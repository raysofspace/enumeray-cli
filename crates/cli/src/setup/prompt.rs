//! Terminal input stays separate from the deterministic saved workflow.
use super::*;
use enumeray_contracts::{
    execution::{ConnectionConfig, DriverConfig, SshTarget},
    management::ContextCreate,
    models::validation::valid_model_id,
};
use std::{
    io::{BufRead, Write},
    net::IpAddr,
    ops::RangeInclusive,
    path::Component,
};

struct Prompt<R, W> {
    input: R,
    output: W,
}

impl<R: BufRead, W: Write> Prompt<R, W> {
    fn note(&mut self, text: &str) -> Result<(), Error> {
        writeln!(self.output, "{text}")?;
        Ok(())
    }

    fn answer(&mut self, default: Option<&str>) -> Result<String, Error> {
        match default {
            Some(default) => writeln!(self.output, "Press Enter to use {default:?}.")?,
            None => writeln!(self.output, "An answer is required.")?,
        }
        write!(self.output, "> ")?;
        self.output.flush()?;

        let mut value = String::new();
        if self.input.read_line(&mut value)? == 0 {
            return Err(invalid(
                "Setup input ended. Run the same connector setup command again. Unfinished questionnaires restart.",
            ));
        }
        let value = value.trim();
        Ok(if value.is_empty() {
            default.unwrap_or("").to_owned()
        } else {
            value.to_owned()
        })
    }

    fn validated<T>(
        &mut self,
        question: &str,
        default: Option<&str>,
        guidance: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<T, Error> {
        loop {
            self.note(question)?;
            self.note(guidance)?;
            let value = self.answer(default)?;
            if let Some(parsed) = parse(&value) {
                return Ok(parsed);
            }
            self.note("That answer is invalid. Try again using the choices or format below.")?;
        }
    }

    fn yes(
        &mut self,
        question: &str,
        yes_meaning: &str,
        no_meaning: &str,
        default: bool,
    ) -> Result<bool, Error> {
        self.validated(
            question,
            Some(if default { "yes" } else { "no" }),
            &format!("  yes or y — {yes_meaning}\n  no or n — {no_meaning}\nUppercase and lowercase both work."),
            |value| match value.to_ascii_lowercase().as_str() {
                "yes" | "y" => Some(true),
                "no" | "n" => Some(false),
                _ => None,
            },
        )
    }

    fn choice<'a>(
        &mut self,
        question: &str,
        default: &'a str,
        options: &[(&'a str, &str)],
    ) -> Result<&'a str, Error> {
        let guidance = options
            .iter()
            .map(|(option, meaning)| format!("  {option} — {meaning}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.validated(question, Some(default), &guidance, |value| {
            options
                .iter()
                .map(|(option, _)| *option)
                .find(|option| *option == value)
        })
    }

    fn text(
        &mut self,
        question: &str,
        default: Option<&str>,
        guidance: &str,
        valid: impl Fn(&str) -> bool,
    ) -> Result<String, Error> {
        self.validated(question, default, guidance, |value| {
            valid(value).then(|| value.to_owned())
        })
    }

    fn path(
        &mut self,
        question: &str,
        default: &str,
        purpose: &str,
        valid: impl Fn(&str) -> bool,
    ) -> Result<String, Error> {
        self.note(purpose)?;
        self.text(
            question,
            Some(default),
            "Enter a full path below /, up to 4096 bytes, without control characters or .. components. Shell shortcuts such as ~ and $HOME do not expand here.",
            |value| {
                let path = Path::new(value);
                value.len() <= 4096
                    && path.is_absolute()
                    && path != Path::new("/")
                    && !value.chars().any(char::is_control)
                    && !path.components().any(|part| part == Component::ParentDir)
                    && valid(value)
            },
        )
    }

    fn number(
        &mut self,
        question: &str,
        default: Option<u32>,
        allowed: RangeInclusive<u32>,
    ) -> Result<u32, Error> {
        let default = default.map(|value| value.to_string());
        let guidance = format!(
            "Enter a whole number from {} to {}.",
            allowed.start(),
            allowed.end()
        );
        self.validated(question, default.as_deref(), &guidance, |value| {
            value.parse().ok().filter(|value| allowed.contains(value))
        })
    }
}

pub(super) fn yes(
    question: &str,
    yes_meaning: &str,
    no_meaning: &str,
    default: bool,
) -> Result<bool, Error> {
    Prompt {
        input: io::stdin().lock(),
        output: io::stderr().lock(),
    }
    .yes(question, yes_meaning, no_meaning, default)
}

fn local(args: &[&str]) -> Result<String, Error> {
    let out = Command::new("/usr/bin/id").args(args).output()?;
    if !out.status.success() {
        return Err(invalid("Cannot identify the current regular OS account."));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}

pub(super) fn input(args: &Args, options: &Setup) -> Result<Input, Error> {
    if local(&["-u"])? == "0" {
        return Err(invalid(
            "Run connector setup as a regular user. A managed installation has a separate administrator step.",
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
            "The selected profile is saved for a different API. Select a profile for this API and rerun connector setup.",
        ));
    }
    let current_account = HostAccount {
        platform: if cfg!(target_os = "macos") {
            HostPlatform::Macos
        } else {
            HostPlatform::Linux
        },
        name: local(&["-un"])?,
        group: local(&["-gn"])?,
        home: std::env::var("HOME").map_err(|_| invalid("HOME is unset."))?,
        create: false,
        uid: None,
        gid: None,
    };
    let mut prompt = Prompt {
        input: io::stdin().lock(),
        output: io::stderr().lock(),
    };
    prompt.note(&format!("Using profile {:?} at {api}.", args.profile))?;
    collect(
        &mut prompt,
        options.mode,
        current_account,
        &std::env::current_exe()?.canonicalize()?,
        api,
    )
}

fn collect(
    prompt: &mut Prompt<impl BufRead, impl Write>,
    mode: Option<SetupMode>,
    current_account: HostAccount,
    executable: &Path,
    api: String,
) -> Result<Input, Error> {
    let mode = match mode {
        Some(mode) => mode,
        None => match prompt.choice(
            "How should this connector and its jobs be isolated?",
            "development",
            &[
                ("development", "Run the connector and trusted jobs as your current OS user. No administrator step is needed."),
                ("managed", "Use a separate OS account for jobs. A host administrator reviews and applies the host plan."),
            ],
        )? {
            "development" => SetupMode::Development,
            _ => SetupMode::Managed,
        },
    };
    let dev = mode == SetupMode::Development;
    if dev {
        prompt.note(
            "Development uses the process driver and a direct connection on this computer. Jobs can read your files and connector credentials. Setup prepares the directories and enrolls the connector after you approve.",
        )?;
    } else {
        prompt.note(
            "Managed setup uses a dedicated connector account and a separate execution account. The host administrator applies the reviewed plan before you start the connector.",
        )?;
    }
    let platform = current_account.platform;

    const ID_GUIDANCE: &str = "Use 1–64 characters, starting with a lowercase letter. Use lowercase letters, digits, hyphens, or underscores.";
    let id = prompt.text(
        "What stable ID should this connector use?",
        Some("personal-connector"),
        ID_GUIDANCE,
        valid_model_id,
    )?;
    let context_default = format!("{}-cpu", &id[..id.len().min(60)]);
    let context = prompt.text(
        "What stable ID should this execution context use?",
        Some(&context_default),
        &format!("Authors use this ID to select the environment for their jobs. {ID_GUIDANCE}"),
        valid_model_id,
    )?;
    let driver = if dev {
        "process"
    } else {
        prompt.choice(
            "How should the execution host run jobs?",
            "process",
            &[
                ("process", "Launch jobs as foreground processes under the execution account."),
                ("slurm", "Submit jobs to the site's Slurm scheduler. The site needs a Slurm allocation, shared paths, and a batch script for each model."),
            ],
        )?
    };
    let connection = if dev {
        "direct"
    } else {
        prompt.choice(
            "How should the connector reach the execution account?",
            "direct",
            &[
                ("direct", "Use the execution account on this host through a restricted sudo rule."),
                ("ssh", "Use SSH to reach a separate host. Its administrator must prepare the runner. The connector host needs a private key and a verified host key."),
            ],
        )?
    };
    let template = match (driver, connection) {
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
        prompt.path(
            "Which directory should hold local development files?",
            &format!("{}/enumeray-development", current_account.home),
            "The default keeps connector state, model installations, and job work under your home. Choose a directory you control.",
            |_| true,
        )?
    } else if platform == HostPlatform::Macos {
        format!("/Library/Application Support/Enumeray/{id}")
    } else {
        format!("/var/lib/{id}")
    };
    let connector_account = if dev {
        current_account.clone()
    } else {
        let service = account_field(
            prompt,
            "Which OS user should run the connector on this host?",
            Some(&format!("enumeray-{}", &id[..id.len().min(20)])),
            "Use a dedicated user. This user owns the connector credential and journal.",
            |_| true,
        )?;
        let group = account_field(
            prompt,
            "What is the connector user's primary group on this host?",
            Some(&service),
            "Use the OS group name that owns the connector's files.",
            |_| true,
        )?;
        let create = prompt.yes(
            "May the host administrator create the connector account if it is missing?",
            "Include account creation in the reviewed host plan if needed.",
            "Require the account to exist before the plan is applied.",
            true,
        )?;
        let (uid, gid) = if create && platform == HostPlatform::Macos {
            prompt.note(
                "Ask the host administrator for unused numeric macOS IDs. Host apply checks them again before creating an account or group.",
            )?;
            (
                Some(prompt.number(
                    "What numeric user ID (UID) should the connector account use?",
                    None,
                    1..=u32::MAX,
                )?),
                Some(prompt.number(
                    "What numeric group ID (GID) should its primary group use?",
                    None,
                    1..=u32::MAX,
                )?),
            )
        } else {
            (None, None)
        };
        HostAccount {
            platform,
            name: service,
            group,
            home: account_home_path(
                prompt,
                "What is the connector user's home directory on this host?",
                &base,
                "The connector keeps its private journal and credentials under this directory.",
                platform,
            )?,
            create,
            uid,
            gid,
        }
    };
    let execution_account = if dev {
        current_account
    } else if connection == "direct" {
        let same_user = current_account.name == connector_account.name;
        let execution = account_field(
            prompt,
            "Which existing OS user should run jobs on this host?",
            (!same_user).then_some(current_account.name.as_str()),
            "Choose a different user from the connector account.",
            |name| name != connector_account.name,
        )?;
        if execution == current_account.name {
            current_account
        } else {
            execution_identity(prompt, platform, execution, "this host")?
        }
    } else {
        let remote_platform = match prompt.choice(
            "Which operating system runs on the SSH execution host?",
            "linux",
            &[
                (
                    "linux",
                    "The execution host runs Linux and needs the Linux build of Enumeray.",
                ),
                (
                    "macos",
                    "The execution host runs macOS and needs the macOS build of Enumeray.",
                ),
            ],
        )? {
            "linux" => HostPlatform::Linux,
            _ => HostPlatform::Macos,
        };
        let remote = account_field(
            prompt,
            "Which existing OS user should run jobs on the SSH host?",
            None,
            "Enter the username the connector will use to sign in on the execution host.",
            |_| true,
        )?;
        execution_identity(prompt, remote_platform, remote, "the SSH host")?
    };

    let connector_executable = if dev {
        executable.to_string_lossy().into_owned()
    } else {
        prompt.path(
            "Where should the administrator install the connector executable on this host?",
            "/opt/enumeray/bin/enumeray",
            "Choose a site-controlled location. The execution account must be unable to replace the executable or modify its parent directories.",
            |_| true,
        )?
    };
    context_spec.definition.execution_mode = if dev {
        ExecutionMode::Development
    } else {
        ExecutionMode::Managed
    };
    let default_models = if dev {
        format!("{base}/models")
    } else {
        "/opt/enumeray/models".into()
    };
    let default_work = if dev {
        format!("{base}/work")
    } else if driver == "slurm" {
        "/shared/enumeray/jobs".into()
    } else if execution_account.platform == HostPlatform::Macos {
        format!("/Library/Application Support/EnumerayWork/{id}")
    } else {
        format!("/var/lib/{id}-model-work")
    };
    let installation_root = prompt.path(
        "Which directory on the execution host may hold installed model releases?",
        &default_models,
        "The execution account must be able to read installed releases under this directory.",
        |_| true,
    )?;
    context_spec.definition.work_root = prompt.path(
        "Which private directory on the execution host should hold job files?",
        &default_work,
        if driver == "slurm" {
            "Use a different directory from the installation root. It must be accessible from the Slurm submission host and compute nodes under the same path."
        } else {
            "Use a different directory from the installation root. Each job runs in a private workspace under this directory."
        },
        |value| Path::new(value) != Path::new(&installation_root),
    )?;
    context_spec.definition.installation_roots = vec![installation_root];
    context_spec.definition.connection = if connection == "direct" {
        let launcher = if dev {
            vec![connector_executable.clone()]
        } else {
            vec![
                "/usr/bin/sudo".into(),
                "-n".into(),
                "-u".into(),
                execution_account.name.clone(),
                "--".into(),
                connector_executable.clone(),
            ]
        };
        ConnectionConfig::Direct { launcher }
    } else {
        let host = prompt.text(
            "Which hostname or IP address should the connector use for SSH?",
            None,
            "Use a hostname such as login.example.edu or a bare IP address. Enter the port separately.",
            valid_ssh_host,
        )?;
        let port = prompt.number("Which TCP port does SSH listen on?", Some(22), 1..=65535)?;
        let credential_reference = prompt.text(
            "What name should this context use for its SSH credential?",
            Some("cluster-key"),
            "Use a name that identifies the key in the private credential map on the connector host, such as cluster-key. The key itself and its file path stay in that host's private configuration.",
            |value| valid_text(value, 4096),
        )?;
        let runner_path = prompt.path(
            "Where should the administrator install the runner on the SSH host?",
            "/opt/enumeray/bin/enumeray",
            "Enter the full path to the Enumeray executable called over SSH on that host.",
            |_| true,
        )?;
        prompt.note(
            "Before startup, arrange SSH access for the execution account. The connector account needs the matching private key and a host key verified by the site.",
        )?;

        ConnectionConfig::Ssh(SshTarget {
            host,
            user: execution_account.name.clone(),
            port: port as u16,
            credential_reference,
            runner_path,
        })
    };
    if let DriverConfig::Slurm(config) = &mut context_spec.definition.driver {
        config.worker_path = prompt.path(
            "Where can the Slurm compute nodes find the Enumeray worker?",
            "/opt/enumeray/bin/enumeray",
            "Enter the path visible to the allocated compute nodes. The site supplies the allocation, shared paths, and job runtime.",
            |_| true,
        )?;
    }
    context_spec.metadata.location = prompt.text(
        "What execution location should users see?",
        Some(if dev {
            "Personal compute host"
        } else {
            "Research compute site"
        }),
        "Use a short description of the site or system where jobs run, up to 200 bytes.",
        |value| valid_text(value, 200),
    )?;
    context_spec.metadata.disclosure = prompt.text(
        "Who can access submitted data at this location?",
        Some(if dev {
            "Trusted jobs run as my OS user and can access its credentials and files. Anyone with access to that account or administrator access to this host can read submitted data."
        } else {
            "The execution account and administrators of the execution host can access submitted data."
        }),
        "Users see this disclosure before submitting. Use up to 3800 bytes to name the people or accounts that can read their data here.",
        |value| valid_text(value, 3800),
    )?;
    prompt.note(
        "This value sets the number of connector workers and the initial limits for outstanding assignments on the connector and this context. Queued Slurm jobs count against those limits.",
    )?;
    let workers = prompt.number(
        "How many assignments may this connector handle at once?",
        Some(if dev { 1 } else { 4 }),
        1..=64,
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
        host.execution_executable_sha256 = Some(prompt.text(
            "What is the SHA-256 checksum of the Enumeray executable for the SSH host?",
            None,
            "Copy the approved 64-character checksum in lowercase hexadecimal for the SSH host's OS and CPU. Use the connector executable's checksum only if the SSH host will use the exact same binary. The execution host administrator supplies the matching file with --executable-source.",
            |value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        )?);
    }
    let background = match (dev, platform) {
        (true, HostPlatform::Linux) => {
            "Start explicitly through a user systemd service under your regular account."
        }
        (true, HostPlatform::Macos) => {
            "Start explicitly through a macOS LaunchAgent under your regular account."
        }
        (false, HostPlatform::Linux) => {
            "The administrator reviews and installs a systemd service, then starts it explicitly."
        }
        (false, HostPlatform::Macos) => {
            "The administrator reviews and installs a macOS LaunchDaemon, then starts it explicitly."
        }
    };
    let startup = prompt.choice(
        "How should you start the connector after preparation?",
        "foreground",
        &[
            (
                "foreground",
                "Run the printed start command in a terminal and leave that terminal open.",
            ),
            ("background", background),
        ],
    )?;
    host.install_service = startup == "background";
    let label = prompt.text(
        "What display name should this connector have?",
        Some("Personal connector"),
        "This name appears in connector listings. Use up to 200 bytes. Commands use the stable connector ID.",
        |value| valid_text(value, 200),
    )?;
    Ok(Input {
        manifest_version: INPUT_VERSION,
        connector: ConnectorCreate {
            id,
            label,
            max_outstanding_assignments: workers,
        },
        host,
        startup: startup.to_owned(),
    })
}

fn account_field(
    prompt: &mut Prompt<impl BufRead, impl Write>,
    question: &str,
    default: Option<&str>,
    purpose: &str,
    valid: impl Fn(&str) -> bool,
) -> Result<String, Error> {
    prompt.note(purpose)?;
    prompt.text(
        question,
        default,
        "Use an OS name of 1–64 lowercase letters, digits, underscores, or hyphens, starting with a lowercase letter or underscore. The name root is reserved.",
        |value| {
            value.len() <= 64
                && value != "root"
                && value.bytes().next().is_some_and(|first| first.is_ascii_lowercase() || first == b'_')
                && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
                && valid(value)
        },
    )
}

fn account_home_path(
    prompt: &mut Prompt<impl BufRead, impl Write>,
    question: &str,
    default: &str,
    purpose: &str,
    platform: HostPlatform,
) -> Result<String, Error> {
    if platform == HostPlatform::Linux {
        prompt.note("On Linux, the home path may not end with whitespace or a backslash.")?;
    }
    prompt.path(question, default, purpose, |value| {
        platform != HostPlatform::Linux
            || !value.ends_with(|ch: char| ch.is_whitespace() || ch == '\\')
    })
}

fn execution_identity(
    prompt: &mut Prompt<impl BufRead, impl Write>,
    platform: HostPlatform,
    name: String,
    host: &str,
) -> Result<HostAccount, Error> {
    Ok(HostAccount {
        platform,
        group: account_field(
            prompt,
            &format!("What is the execution user's primary group on {host}?"),
            Some(&name),
            "Use the existing OS group name for this user.",
            |_| true,
        )?,
        home: account_home_path(
            prompt,
            &format!("What is the execution user's home directory on {host}?"),
            &account_home(platform, &name),
            "Use the account's existing home path.",
            platform,
        )?,
        name,
        create: false,
        uid: None,
        gid: None,
    })
}

fn account_home(platform: HostPlatform, name: &str) -> String {
    match platform {
        HostPlatform::Linux => format!("/home/{name}"),
        HostPlatform::Macos => format!("/Users/{name}"),
    }
}

fn valid_ssh_host(value: &str) -> bool {
    if value.parse::<IpAddr>().is_ok() {
        return true;
    }
    let hostname = value.strip_suffix('.').unwrap_or(value);
    hostname.len() <= 253
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> HostAccount {
        HostAccount {
            platform: HostPlatform::Linux,
            name: "researcher".into(),
            group: "researcher".into(),
            home: "/home/researcher".into(),
            create: false,
            uid: None,
            gid: None,
        }
    }

    #[test]
    fn confirms_aliases_and_default_with_their_effects() {
        for (answer, expected) in [
            ("yes", true),
            ("Y", true),
            ("NO", false),
            ("n", false),
            ("", false),
        ] {
            let answers = format!("maybe\n{answer}\n");
            let mut prompt = Prompt {
                input: answers.as_bytes(),
                output: Vec::new(),
            };
            assert_eq!(
                prompt
                    .yes("Keep the plan?", "Keep it.", "Pause here.", false)
                    .unwrap(),
                expected
            );
            let transcript = String::from_utf8(prompt.output).unwrap();
            assert!(transcript.contains("yes or y — Keep it."));
            assert!(transcript.contains("no or n — Pause here."));
            assert!(transcript.contains("Press Enter to use \"no\"."));
            assert_eq!(transcript.matches("Keep the plan?").count(), 2);
        }
    }

    #[test]
    fn development_questionnaire_retries_invalid_paths_and_capacity() {
        let connector_id = "a".repeat(64);
        let answers = [
            "", // Development mode
            &connector_id,
            "", // Context ID
            "relative/path",
            "", // Development base retry
            "",
            "/home/researcher/enumeray-development/models/",
            "", // Work root must differ from installation root
            "",
            "", // Location and disclosure
            "65",
            "2", // Capacity retry
            "daemon",
            "background", // Startup retry
            "",
            "", // Label and final newline
        ]
        .join("\n");
        let mut prompt = Prompt {
            input: answers.as_bytes(),
            output: Vec::new(),
        };
        let input = collect(
            &mut prompt,
            None,
            account(),
            Path::new("/bin/enumeray"),
            "https://api.enumeray.com".into(),
        )
        .unwrap();
        assert_eq!(input.connector.id, connector_id);
        assert_eq!(input.host.context.id, format!("{}-cpu", "a".repeat(60)));
        assert_eq!(
            input.host.context.definition.execution_mode,
            ExecutionMode::Development
        );
        assert_eq!(input.host.max_workers, 2);
        assert_eq!(input.connector.max_outstanding_assignments, 2);
        assert_eq!(input.host.context.definition.max_outstanding_assignments, 2);
        assert_eq!(input.host.connector_executable, "/bin/enumeray");
        assert!(input.host.executable_source.is_none());
        assert_eq!(
            input.host.context.definition.connection,
            ConnectionConfig::Direct {
                launcher: vec!["/bin/enumeray".into()]
            }
        );
        assert_eq!(input.startup, "background");
        assert!(input.host.install_service);
        let transcript = String::from_utf8(prompt.output).unwrap();
        assert!(transcript.contains("development — Run the connector"));
        assert!(transcript.contains("managed — Use a separate OS account"));
        assert!(transcript.contains("foreground — Run the printed start command"));
        assert!(
            transcript.contains("background — Start explicitly through a user systemd service")
        );
        assert_eq!(transcript.matches("That answer is invalid.").count(), 4);
    }

    #[test]
    fn managed_ssh_questionnaire_shows_choices_and_requires_real_host_inputs() {
        let checksum = "a".repeat(64);
        let answers = [
            "",
            "", // IDs
            "slurm",
            "ssh",
            "",
            "",
            "n",
            "", // Connector user, group, creation consent, and home
            "macos",
            "runner",
            "staff",
            "", // Remote OS, user, group, and home
            "",
            "",
            "", // Connector binary, installation root, work root
            ".",
            "login.example.edu:22",
            "login.example.edu", // SSH address retry
            "0",
            "65535", // SSH port retry
            "",
            "",
            "", // Key reference, runner path, compute worker path
            "",
            "",
            "", // Location, disclosure, and capacity
            "",
            &checksum, // Checksum required
            "",
            "",
            "", // Startup, label, and final newline
        ]
        .join("\n");
        let mut prompt = Prompt {
            input: answers.as_bytes(),
            output: Vec::new(),
        };
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/process-direct-context.json");
        let input = collect(
            &mut prompt,
            Some(SetupMode::Managed),
            account(),
            &source,
            "https://api.enumeray.com".into(),
        )
        .unwrap();
        let ConnectionConfig::Ssh(target) = input.host.context.definition.connection else {
            panic!("expected SSH")
        };
        assert_eq!(target.user, "runner");
        assert_eq!(target.host, "login.example.edu");
        assert_eq!(target.port, 65535);
        assert_eq!(target.credential_reference, "cluster-key");
        assert_eq!(target.runner_path, "/opt/enumeray/bin/enumeray");
        assert!(!input.host.connector_account.create);
        assert_eq!(input.host.execution_account.platform, HostPlatform::Macos);
        assert_eq!(input.host.execution_account.home, "/Users/runner");
        assert_eq!(input.host.execution_account.group, "staff");
        assert_eq!(
            input.host.connector_executable,
            "/opt/enumeray/bin/enumeray"
        );
        assert_eq!(
            input.host.executable_source.as_deref().map(Path::new),
            Some(source.as_path())
        );
        assert_eq!(
            input.host.execution_executable_sha256.as_deref(),
            Some(checksum.as_str())
        );
        assert_eq!(input.startup, "foreground");
        let transcript = String::from_utf8(prompt.output).unwrap();
        for choice in [
            "process",
            "slurm",
            "direct",
            "ssh",
            "linux",
            "macos",
            "foreground",
            "background",
        ] {
            assert!(
                transcript.contains(&format!("  {choice} — ")),
                "missing explanation of {choice}"
            );
        }
        assert_eq!(transcript.matches("That answer is invalid.").count(), 4);
    }
}
