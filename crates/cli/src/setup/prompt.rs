//! Terminal input stays separate from the deterministic saved workflow.
use super::*;
use std::io::Write;
pub(super) fn ask(label: &str, default: &str) -> Result<String, Error> {
    eprint!("{label} [{}]: ", default);
    io::stderr().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        return Err(invalid(
            "Setup input ended; retain the setup directory and resume.",
        ));
    }
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}
pub(super) fn yes(label: &str, default: bool) -> Result<bool, Error> {
    loop {
        match ask(label, if default { "yes" } else { "no" })?
            .to_ascii_lowercase()
            .as_str()
        {
            "yes" | "y" => return Ok(true),
            "no" | "n" => return Ok(false),
            _ => eprintln!("Enter yes or no."),
        }
    }
}
fn local(args: &[&str]) -> Result<String, Error> {
    let out = Command::new("/usr/bin/id").args(args).output()?;
    if !out.status.success() {
        return Err(invalid("Cannot identify the current regular OS account."));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}
fn path(label: &str, default: &str) -> Result<String, Error> {
    let selected = ask(label, default)?;
    let p = PathBuf::from(selected);
    if !p.is_absolute() {
        return Err(invalid(
            "Enter an absolute path; shell variables are not expanded.",
        ));
    }
    Ok(p.to_string_lossy().into_owned())
}
fn bindings(label: &str) -> Result<Vec<String>, Error> {
    let mut values = vec![];
    loop {
        let value = ask(label, "")?;
        if value.is_empty() {
            return Ok(values);
        }
        values.push(value);
    }
}
fn number(label: &str, default: u32) -> Result<u32, Error> {
    ask(label, &default.to_string())?
        .parse()
        .map_err(|_| invalid("Enter a positive numeric ID or capacity."))
}
pub(super) fn input(args: &Args, options: &Setup) -> Result<Input, Error> {
    let mode = match options.mode {
        Some(m) => m,
        None => match ask("Setup mode: development or managed", "development")?.as_str() {
            "development" => SetupMode::Development,
            "managed" => SetupMode::Managed,
            _ => return Err(invalid("Choose development or managed.")),
        },
    };
    if local(&["-u"])? == "0" {
        return Err(invalid(
            "Run the owner setup as a regular user; administrator apply is a separate step.",
        ));
    }
    let name = local(&["-un"])?;
    let group = local(&["-gn"])?;
    let home = std::env::var("HOME").map_err(|_| invalid("HOME is unset."))?;
    let platform = if cfg!(target_os = "macos") {
        HostPlatform::Macos
    } else {
        HostPlatform::Linux
    };
    let profiles = config::load(&profile_path(args)?)?;
    let api = ask(
        "API origin",
        args.api
            .as_deref()
            .or_else(|| {
                profiles
                    .profiles
                    .get(&args.profile)
                    .map(|p| p.api_url.as_str())
            })
            .unwrap_or("https://enumeray.example.org"),
    )?;
    let api = EnumerayClient::new(&api, None, Duration::from_secs(args.timeout))?
        .base_url()
        .to_owned();
    let id = ask("Connector ID", "personal-connector")?;
    let context = ask("Context ID", &format!("{id}-cpu"))?;
    let dev = mode == SetupMode::Development;
    let driver = if dev {
        "process".into()
    } else {
        ask("Driver: process or slurm", "process")?
    };
    let connection = if dev {
        "direct".into()
    } else {
        ask("Connection: direct or ssh", "direct")?
    };
    if !matches!(driver.as_str(), "process" | "slurm")
        || !matches!(connection.as_str(), "direct" | "ssh")
    {
        return Err(invalid("Choose process/slurm and direct/ssh."));
    }
    let template = match (driver.as_str(), connection.as_str()) {
        ("process", "direct") => include_str!("../../examples/process-direct-context.json"),
        ("process", "ssh") => include_str!("../../examples/process-ssh-context.json"),
        ("slurm", "direct") => include_str!("../../examples/slurm-direct-context.json"),
        _ => include_str!("../../examples/slurm-ssh-context.json"),
    };
    let template: Value = serde_json::from_str(template)?;
    let mut context_spec: ContextCreate = serde_json::from_value(template["spec"].clone())?;
    context_spec.id = context;
    context_spec.connector_id = id.clone();
    let base = if dev {
        path(
            "Development directory",
            &format!("{home}/enumeray-development"),
        )?
    } else if platform == HostPlatform::Macos {
        format!("/Library/Application Support/Enumeray/{id}")
    } else {
        format!("/var/lib/{id}")
    };
    let existing = HostAccount {
        platform,
        name: name.clone(),
        group: group.clone(),
        home: home.clone(),
        create: false,
        uid: None,
        gid: None,
    };
    let connector_account = if dev {
        existing.clone()
    } else {
        let svc = ask(
            "Dedicated connector account",
            &format!("enumeray-{}", id.chars().take(20).collect::<String>()),
        )?;
        let svc_group = ask("Connector primary group", &svc)?;
        let create = yes("Create the connector account if absent", true)?;
        let (uid, gid) = if create && platform == HostPlatform::Macos {
            eprintln!(
                "Review unused numeric IDs; host apply rechecks collisions before creating any account."
            );
            (
                Some(number("Connector UID", 450)?),
                Some(number("Connector GID", 450)?),
            )
        } else {
            (None, None)
        };
        HostAccount {
            platform,
            name: svc,
            group: svc_group,
            home: path("Connector account home", &base)?,
            create,
            uid,
            gid,
        }
    };
    let execution_account = if dev {
        existing
    } else if connection == "direct" {
        let execution = ask("Existing model execution account", &name)?;
        if execution == name {
            existing
        } else {
            HostAccount {
                platform,
                name: execution.clone(),
                group: ask("Execution primary group", &execution)?,
                home: path("Execution account home", &format!("/home/{execution}"))?,
                create: false,
                uid: None,
                gid: None,
            }
        }
    } else {
        let remote_platform =
            match ask("Execution host platform: linux or macos", "linux")?.as_str() {
                "linux" => HostPlatform::Linux,
                "macos" => HostPlatform::Macos,
                _ => return Err(invalid("Choose linux or macos.")),
            };
        let remote = ask("Existing remote execution account", &name)?;
        HostAccount {
            platform: remote_platform,
            name: remote.clone(),
            group: ask("Remote primary group", &remote)?,
            home: path("Remote account home", &format!("/home/{remote}"))?,
            create: false,
            uid: None,
            gid: None,
        }
    };
    let executable = std::env::current_exe()?.canonicalize()?;
    let connector_executable = if dev {
        executable.to_string_lossy().into_owned()
    } else {
        path(
            "Protected connector executable",
            "/opt/enumeray/bin/enumeray",
        )?
    };
    context_spec.definition.execution_mode = if dev {
        ExecutionMode::Development
    } else {
        ExecutionMode::Managed
    };
    let default_models = format!("{base}/models");
    let default_work = if dev {
        format!("{base}/work")
    } else if platform == HostPlatform::Macos {
        format!("/Library/Application Support/EnumerayWork/{id}")
    } else {
        format!("/var/lib/{id}-model-work")
    };
    context_spec.definition.installation_roots = vec![path(
        "Approved model installation root",
        if dev {
            &default_models
        } else {
            "/opt/enumeray/models"
        },
    )?];
    context_spec.definition.work_root = path(
        "Execution work root",
        if dev {
            &default_work
        } else if driver == "slurm" {
            "/shared/enumeray/jobs"
        } else {
            &default_work
        },
    )?;
    let mut definition = serde_json::to_value(&context_spec.definition)?;
    if connection == "direct" {
        definition["connection"] = json!({"kind":"direct","launcher":if dev{vec![connector_executable.clone()]}else{vec!["/usr/bin/sudo".into(),"-n".into(),"-u".into(),execution_account.name.clone(),"--".into(),connector_executable.clone()]}});
    } else {
        definition["connection"]["host"] = json!(ask("SSH host", "login.cluster.example")?);
        definition["connection"]["user"] = json!(execution_account.name);
        definition["connection"]["port"] = json!(number("SSH port", 22)?);
        definition["connection"]["credential_reference"] =
            json!(ask("Private SSH credential reference", "cluster-key")?);
        definition["connection"]["runner_path"] = json!(path(
            "Protected remote runner executable",
            "/opt/enumeray/bin/enumeray"
        )?);
        eprintln!(
            "Prepare the existing execution account's SSH access. Privately install the referenced key for the connector account and verify known_hosts through the site's trusted channel."
        );
    }
    if driver == "slurm" {
        definition["driver"]["worker_path"] = json!(path(
            "Enumeray executable visible on compute nodes",
            "/opt/enumeray/bin/enumeray"
        )?);
        eprintln!(
            "The site must provide Slurm allocation, shared paths, runtime setup and compute-node visibility."
        );
    }
    context_spec.definition = serde_json::from_value(definition)?;
    context_spec.metadata.location = ask("Execution location", "Personal compute host")?;
    context_spec.metadata.disclosure = ask(
        "Data access disclosure",
        if dev {
            "Trusted development: model code runs as the connector OS user and can access its credentials and files."
        } else {
            "Models run under the selected execution account on this site."
        },
    )?;
    let workers = number("Maximum simultaneous assignments", if dev { 1 } else { 4 })?;
    context_spec.definition.max_outstanding_assignments = workers;
    let state = if dev {
        format!("{base}/connector")
    } else {
        connector_account.home.clone()
    };
    let mut host = HostSetupSpec {
        manifest_version: 1,
        api_base_url: api,
        context: context_spec,
        connector_account,
        execution_account,
        connector_executable,
        executable_source: (!dev).then(|| executable.to_string_lossy().into_owned()),
        executable_sha256: if dev {
            None
        } else {
            Some(digest_bytes(fs::read(executable)?))
        },
        journal: format!("{state}/journal"),
        token_file: format!("{state}/credentials/token"),
        credentials_file: (connection == "ssh").then(|| format!("{state}/ssh-credentials.json")),
        max_workers: workers,
        max_transfer_workers: 2,
        max_pending_assignments: 64,
        shutdown_timeout_seconds: 30,
        install_service: false,
    };
    let startup = ask("Startup: foreground or background", "foreground")?;
    host.install_service = startup == "background";
    let first_model = match ask("First model: demo, project or later", "demo")?.as_str() {
        "later" => None,
        choice @ ("demo" | "project") => {
            let project = if choice == "project" {
                Some(PathBuf::from(path(
                    "Existing author project directory",
                    &std::env::current_dir()?.to_string_lossy(),
                )?))
            } else {
                None
            };
            let parsed = project
                .as_ref()
                .map(|p| crate::manifest::publication_source(&p.join("publication.json")))
                .transpose()?;
            let model_id = if let Some((_, source)) = &parsed {
                eprintln!("Project model: {}", source.model.id);
                source.model.id.clone()
            } else {
                ask(
                    "Model ID",
                    &format!("setup-example-{}", &Uuid::new_v4().to_string()[..8]),
                )?
            };
            let example = Some(ask(
                "Declared input example",
                if choice == "demo" {
                    "starter-example"
                } else {
                    parsed
                        .as_ref()
                        .and_then(|(_, source)| source.definition.examples.first())
                        .map(|e| e.name.as_str())
                        .unwrap_or("")
                },
            )?)
            .filter(|s| !s.is_empty());
            let inputs = if example.is_none() {
                Some(PathBuf::from(path(
                    "Scientific inputs JSON",
                    &format!("{home}/inputs.json"),
                )?))
            } else {
                None
            };
            let installed_release = if choice == "project"
                && yes("Use an already installed release directory", false)?
            {
                Some(PathBuf::from(path("Installed release directory", "")?))
            } else {
                None
            };
            let (files, collections, collection_manifests, media_types, metadata) = if yes(
                "Bind local input files or collections for this trial",
                false,
            )? {
                eprintln!(
                    "Enter each SLOT=VALUE separately; leave blank to finish each list. Local paths must be absolute."
                );
                (
                    bindings("Input file SLOT=/absolute/path")?,
                    bindings("Input collection SLOT=/absolute/directory")?,
                    bindings("Input collection manifest SLOT=/absolute/manifest.json")?,
                    bindings("Input media type SLOT=TYPE")?,
                    bindings("Input metadata SLOT=JSON")?,
                )
            } else {
                (vec![], vec![], vec![], vec![], vec![])
            };
            Some(FirstModel {
                model_id,
                project,
                demo: choice == "demo",
                installed_release,
                inputs,
                example,
                files,
                collections,
                collection_manifests,
                media_types,
                metadata,
            })
        }
        _ => return Err(invalid("Choose demo, project or later.")),
    };
    Ok(Input {
        manifest_version: 1,
        connector: ConnectorCreate {
            id,
            label: ask("Connector label", "Personal connector")?,
            max_outstanding_assignments: workers,
        },
        host,
        startup,
        first_model,
    })
}
