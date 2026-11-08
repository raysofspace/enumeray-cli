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
fn choice(label: &str, default: &str, options: &[&str]) -> Result<String, Error> {
    let label = format!("{label} ({})", options.join("/"));
    loop {
        let value = ask(&label, default)?;
        if options.contains(&value.as_str()) {
            return Ok(value);
        }
        eprintln!("Choose {}.", options.join(" or "));
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
    if local(&["-u"])? == "0" {
        return Err(invalid(
            "Run setup as a regular user. A managed installation has a separate administrator step.",
        ));
    }
    let profiles = config::load(&profile_path(args)?)?;
    let profile = profiles.profiles.get(&args.profile);
    let api = EnumerayClient::new(
        config::api_origin(args, profile),
        None,
        Duration::from_secs(args.timeout),
    )?
    .base_url()
    .to_owned();
    if profile
        .is_some_and(|profile| profile.api_url.trim_end_matches('/') != api.trim_end_matches('/'))
    {
        return Err(invalid(
            "The selected profile is saved for a different API. Select a profile for this API and rerun setup.",
        ));
    }
    eprintln!("Using profile {:?} at {api}.", args.profile);
    let mode = match options.mode {
        Some(m) => m,
        None => {
            eprintln!(
                "  development  Run trusted models locally as your current OS user. No administrator step.\n  managed      Separate connector and model accounts. An administrator applies the host plan before startup."
            );
            if choice("Setup mode", "development", &["development", "managed"])? == "development" {
                SetupMode::Development
            } else {
                SetupMode::Managed
            }
        }
    };
    let dev = mode == SetupMode::Development;
    if dev {
        eprintln!(
            "Development uses the local process driver under your account. Model code can read your files and connector credentials. Setup prepares and enrolls the connector after you approve."
        );
    } else {
        eprintln!(
            "Managed setup needs a host administrator to prepare the reviewed accounts, paths and connector enrollment before either startup option."
        );
    }
    let name = local(&["-un"])?;
    let group = local(&["-gn"])?;
    let home = std::env::var("HOME").map_err(|_| invalid("HOME is unset."))?;
    let platform = if cfg!(target_os = "macos") {
        HostPlatform::Macos
    } else {
        HostPlatform::Linux
    };
    let id = ask("Connector ID", "personal-connector")?;
    let context = ask("Context ID", &format!("{id}-cpu"))?;
    let driver = if dev {
        "process".into()
    } else {
        eprintln!(
            "  process  Launch each model directly on the execution host.\n  slurm    Submit each model to the site's Slurm scheduler. Requires an allocation, shared paths and a prepared batch script."
        );
        choice("Execution driver", "process", &["process", "slurm"])?
    };
    let connection = if dev {
        "direct".into()
    } else {
        eprintln!(
            "  direct  Use a separate execution account on this host through restricted sudo.\n  ssh     Use a remote execution host. Requires setup on both hosts, a private SSH credential and a verified host key."
        );
        choice("Execution connection", "direct", &["direct", "ssh"])?
    };
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
        let create = yes(
            "Allow the administrator to create this account if absent",
            true,
        )?;
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
        let execution = ask("Existing model execution account on this host", &name)?;
        if execution == name {
            existing
        } else {
            HostAccount {
                platform,
                name: execution.clone(),
                group: ask("Execution primary group", &execution)?,
                home: path(
                    "Execution account home",
                    &account_home(platform, &execution),
                )?,
                create: false,
                uid: None,
                gid: None,
            }
        }
    } else {
        let remote_platform =
            if choice("Execution host platform", "linux", &["linux", "macos"])? == "linux" {
                HostPlatform::Linux
            } else {
                HostPlatform::Macos
            };
        let remote = ask("Existing remote execution account", &name)?;
        HostAccount {
            platform: remote_platform,
            name: remote.clone(),
            group: ask("Remote primary group", &remote)?,
            home: path(
                "Remote account home",
                &account_home(remote_platform, &remote),
            )?,
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
    } else if execution_account.platform == HostPlatform::Macos {
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
    let workers = number(
        "Maximum simultaneous assignments (1–64)",
        if dev { 1 } else { 4 },
    )?;
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
        execution_executable_sha256: None,
        journal: format!("{state}/journal"),
        token_file: format!("{state}/credentials/token"),
        credentials_file: (connection == "ssh").then(|| format!("{state}/ssh-credentials.json")),
        max_workers: workers,
        max_transfer_workers: 2,
        max_pending_assignments: 64,
        shutdown_timeout_seconds: 30,
        install_service: false,
    };
    if connection == "ssh" {
        eprintln!(
            "Approve the execution host's native Enumeray artifact. Different platforms or CPUs can require different bytes; its administrator supplies that local artifact with --executable-source."
        );
        host.execution_executable_sha256 = Some(ask(
            "SSH execution executable SHA-256",
            if host.connector_account.platform == host.execution_account.platform {
                host.executable_sha256.as_deref().unwrap_or("")
            } else {
                ""
            },
        )?);
    }
    eprintln!("  foreground  Start the connector in a terminal after preparation.");
    if dev {
        eprintln!(
            "  background  Start a user-level systemd service or macOS LaunchAgent from your logged-in account."
        );
    } else {
        eprintln!(
            "  background  Include a systemd service or macOS LaunchDaemon in the administrator's host plan."
        );
    }
    let startup = choice(
        "Connector startup",
        "foreground",
        &["foreground", "background"],
    )?;
    host.install_service = startup == "background";
    eprintln!(
        "  demo     Generate a Python starter and prepare a candidate trial.\n  project  Use a project with publication.json and prepare a candidate trial.\n  later    Finish at connector readiness and prepare a model separately."
    );
    let selected_model = choice("First model", "demo", &["demo", "project", "later"])?;
    let first_model = match selected_model.as_str() {
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
            let examples: Vec<&str> = match &parsed {
                Some((_, source)) => source
                    .definition
                    .examples
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect(),
                None => vec!["starter-example"],
            };
            let (example, inputs) = trial_inputs(&home, &examples)?;
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
        _ => unreachable!("first-model choice is validated"),
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

fn account_home(platform: HostPlatform, name: &str) -> String {
    match platform {
        HostPlatform::Linux => format!("/home/{name}"),
        HostPlatform::Macos => format!("/Users/{name}"),
    }
}

fn trial_inputs(home: &str, examples: &[&str]) -> Result<(Option<String>, Option<PathBuf>), Error> {
    if let Some(first) = examples.first()
        && choice("Trial inputs", "example", &["example", "file"])? == "example"
    {
        return Ok((
            Some(choice("Declared input example", first, examples)?),
            None,
        ));
    }
    let inputs = path(
        "Scientific inputs JSON file",
        &format!("{home}/inputs.json"),
    )?;
    Ok((None, Some(PathBuf::from(inputs))))
}
