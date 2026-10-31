//! Reviewable host preparation. Rendering never applies the generated actions.
use crate::{Result, failure};
use enumeray_contracts::{
    execution::{ConnectionConfig, ExecutionMode},
    setup::*,
};
use serde_json::{Value, json};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

pub(crate) fn absolute(path: &str) -> bool {
    Path::new(path).is_absolute()
        && path.len() <= 4096
        && path != "/"
        && !path.chars().any(char::is_control)
        && !Path::new(path)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
}
fn account_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s != "root"
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}
pub(crate) fn validate(spec: &HostSetupSpec) -> Result<()> {
    if spec.manifest_version != 1 {
        return Err(failure("unsupported host setup version"));
    }
    spec.context
        .definition
        .validate_execution()
        .map_err(failure)?;
    spec.context.metadata.validate().map_err(failure)?;
    for id in [&spec.context.id, &spec.context.connector_id] {
        if id.is_empty()
            || id.len() > 64
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        {
            return Err(failure("invalid connector or context identifier"));
        }
    }
    crate::client::validate_origin(&spec.api_base_url, false)?;
    if !(1..=64).contains(&spec.max_workers)
        || !(1..=64).contains(&spec.max_transfer_workers)
        || !(spec.max_workers..=64).contains(&spec.max_pending_assignments)
        || !(1..=3600).contains(&spec.shutdown_timeout_seconds)
        || !(1..=1024).contains(&spec.context.definition.max_outstanding_assignments)
    {
        return Err(failure("invalid host capacity or shutdown bounds"));
    }
    for account in [&spec.connector_account, &spec.execution_account] {
        if !account_name(&account.name)
            || !account_name(&account.group)
            || !absolute(&account.home)
            || account.uid == Some(0)
            || account.gid == Some(0)
            || (account.create
                && account.platform == HostPlatform::Macos
                && (account.uid.is_none() || account.gid.is_none()))
        {
            return Err(failure(
                "host accounts require safe names, absolute homes and explicit non-root macOS creation IDs",
            ));
        }
        if account.platform == HostPlatform::Linux
            && account
                .home
                .ends_with(|c: char| c.is_whitespace() || c == '\\')
        {
            return Err(failure(
                "generated Linux service homes cannot end with whitespace or a backslash",
            ));
        }
    }
    for path in [&spec.connector_executable, &spec.journal, &spec.token_file] {
        if !absolute(path) {
            return Err(failure("host configuration requires safe absolute paths"));
        }
    }
    if spec.credentials_file.as_ref().is_some_and(|p| !absolute(p)) {
        return Err(failure("invalid credentials file path"));
    }
    let context = &spec.context.definition;
    if matches!(context.connection, ConnectionConfig::Direct { .. })
        && spec.connector_account.platform != spec.execution_account.platform
    {
        return Err(failure("direct execution uses one native host platform"));
    }
    if context.execution_mode == ExecutionMode::Development {
        if spec.connector_account.create
            || spec.execution_account.create
            || spec.connector_account.name != spec.execution_account.name
            || spec.connector_account.platform != spec.execution_account.platform
        {
            return Err(failure(
                "development uses one existing regular OS account on one host",
            ));
        }
    } else if matches!(context.connection, ConnectionConfig::Direct { .. })
        && spec.connector_account.name == spec.execution_account.name
    {
        return Err(failure(
            "managed direct execution requires separate accounts",
        ));
    }
    if let ConnectionConfig::Ssh(ssh) = &context.connection
        && (ssh.user != spec.execution_account.name || spec.credentials_file.is_none())
    {
        return Err(failure(
            "SSH account and private credential map must match the context",
        ));
    }
    if let enumeray_contracts::execution::DriverConfig::Process { scratch: None } = context.driver {
        return Err(failure(
            "new process setup requires an explicit scratch policy",
        ));
    }
    Ok(())
}

pub(crate) fn render(spec: HostSetupSpec) -> Result<HostConfigurationPlan> {
    validate(&spec)?;
    let managed = spec.context.definition.execution_mode == ExecutionMode::Managed;
    let ssh = matches!(spec.context.definition.connection, ConnectionConfig::Ssh(_));
    let mut actions = vec![];
    account_actions(
        &mut actions,
        &spec.connector_account,
        HostSide::Connector,
        false,
    );
    if managed {
        account_actions(
            &mut actions,
            &spec.execution_account,
            HostSide::Execution,
            ssh,
        );
    }
    directory(
        &mut actions,
        HostSide::Connector,
        &spec.connector_account,
        &spec.connector_account.home,
        "0700",
    );
    directory(
        &mut actions,
        HostSide::Connector,
        &spec.connector_account,
        Path::new(&spec.token_file)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| failure("invalid token parent"))?,
        "0700",
    );
    directory(
        &mut actions,
        HostSide::Connector,
        &spec.connector_account,
        &spec.journal,
        "0700",
    );
    if let Some(path) = &spec.credentials_file {
        directory(
            &mut actions,
            HostSide::Connector,
            &spec.connector_account,
            Path::new(path)
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(|| failure("invalid credential parent"))?,
            "0700",
        );
    }
    directory(
        &mut actions,
        HostSide::Execution,
        &spec.execution_account,
        &spec.context.definition.work_root,
        "0700",
    );
    for root in &spec.context.definition.installation_roots {
        actions.push(HostAction { host: HostSide::Execution, responsible_role: ResponsibleRole::HostAdministrator,
            kind: HostActionKind::Directory, target: root.clone(), prerequisites: vec![], instructions: vec![format!("Prepare {} as a site-controlled readable installation root; grant the execution account traversal/read access. Authors install releases through the approved site route.", sh(root))] });
    }
    actions.push(HostAction { host: HostSide::Connector, responsible_role: ResponsibleRole::HostAdministrator,
        kind: HostActionKind::Executable, target: spec.connector_executable.clone(), prerequisites: vec![], instructions: vec![
            "Install the selected qualified Enumeray executable at this exact path. In managed mode the execution account must not be able to replace the file or any ancestor. Preserve existing credentials and journals.".into()] });
    let mut files = vec![];
    if managed && !ssh {
        let ConnectionConfig::Direct { launcher } = &spec.context.definition.connection else {
            unreachable!()
        };
        let expected = vec![
            "/usr/bin/sudo".to_owned(),
            "-n".into(),
            "-u".into(),
            spec.execution_account.name.clone(),
            "--".into(),
            spec.connector_executable.clone(),
        ];
        if launcher != &expected {
            return Err(failure(
                "generated managed direct launcher must use sudo -n, the execution account and the configured site executable",
            ));
        }
        files.push(PlannedHostFile {
            name: "enumeray-runner.sudoers".into(),
            host: HostSide::Connector,
            destination: format!("/etc/sudoers.d/enumeray-{}", spec.context.connector_id),
            content: format!(
                "{} ALL=({}) NOPASSWD: {} compute-runner *\n",
                spec.connector_account.name,
                spec.execution_account.name,
                sudo_path(&spec.connector_executable)
            ),
        });
        actions.push(HostAction { host: HostSide::Connector, responsible_role: ResponsibleRole::HostAdministrator,
            kind: HostActionKind::Launcher, target: files.last().unwrap().destination.clone(), prerequisites: vec![], instructions: vec![
                "Review this runner-only sudoers rule with visudo -cf before installing it root-owned with mode 0440. Keep the executable and its ancestors site-controlled.".into()] });
    }
    if ssh {
        let ConnectionConfig::Ssh(target) = &spec.context.definition.connection else {
            unreachable!()
        };
        actions.push(HostAction { host: HostSide::Execution, responsible_role: ResponsibleRole::SiteOperator,
            kind: HostActionKind::Ssh, target: target.host.clone(), prerequisites: vec![], instructions: vec![
                format!("Install matching Enumeray at {} for {}; enable the site's approved SSH login and SFTP access.", sh(&target.runner_path), target.user),
                "Install only the approved public key in the execution account. Verify the host key through a trusted site channel; never automatically trust ssh-keyscan output.".into()] });
        actions.push(HostAction { host: HostSide::Connector, responsible_role: ResponsibleRole::ConnectorOwner,
            kind: HostActionKind::Ssh, target: target.credential_reference.clone(), prerequisites: vec![], instructions: vec![
                "Privately populate the named SSH credential reference and trusted known_hosts entry. Key values are never generated, placed in arguments or sent to the API.".into()] });
    }
    let mut argv = vec![
        spec.connector_executable.clone(),
        "connector".into(),
        "run".into(),
        "--journal".into(),
        spec.journal.clone(),
        "--api-base-url".into(),
        spec.api_base_url.clone(),
        "--connector-id".into(),
        spec.context.connector_id.clone(),
        "--token-file".into(),
        spec.token_file.clone(),
        "--max-workers".into(),
        spec.max_workers.to_string(),
        "--max-transfer-workers".into(),
        spec.max_transfer_workers.to_string(),
        "--max-pending-assignments".into(),
        spec.max_pending_assignments.to_string(),
        "--shutdown-timeout-seconds".into(),
        spec.shutdown_timeout_seconds.to_string(),
        "--recovery-mode".into(),
        "manual".into(),
    ];
    if let Some(path) = &spec.credentials_file {
        argv.extend(["--credentials-file".into(), path.clone()]);
    }
    if !managed {
        argv.push("--allow-development-execution".into());
    }
    let label = format!("org.enumeray.connector.{}", spec.context.connector_id);
    let (name, destination, content) = match spec.connector_account.platform {
        HostPlatform::Linux => {
            let name = format!("enumeray-{}.service", spec.context.connector_id);
            let destination = if managed {
                format!("/etc/systemd/system/{name}")
            } else {
                format!(
                    "{}/.config/systemd/user/{name}",
                    spec.connector_account.home
                )
            };
            let identity = if managed {
                format!(
                    "User={}\nGroup={}\n",
                    spec.connector_account.name, spec.connector_account.group
                )
            } else {
                String::new()
            };
            let command = argv
                .iter()
                .map(|s| systemd(s))
                .collect::<Vec<_>>()
                .join(" ");
            let content = format!(
                "[Unit]\nDescription=Enumeray connector {}\nWants=network-online.target\nAfter=network-online.target\n\n[Service]\nType=simple\n{identity}WorkingDirectory={}\nUMask=0077\nExecStart={command}\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec={}\n\n[Install]\nWantedBy={}\n",
                spec.context.connector_id,
                systemd_value(&spec.connector_account.home),
                spec.shutdown_timeout_seconds + 10,
                if managed {
                    "multi-user.target"
                } else {
                    "default.target"
                }
            );
            (name, destination, content)
        }
        HostPlatform::Macos => {
            let name = format!("{label}.plist");
            let destination = if managed {
                format!("/Library/LaunchDaemons/{name}")
            } else {
                format!(
                    "{}/Library/LaunchAgents/{name}",
                    spec.connector_account.home
                )
            };
            let identity = if managed {
                format!(
                    "<key>UserName</key><string>{}</string><key>GroupName</key><string>{}</string>",
                    xml(&spec.connector_account.name),
                    xml(&spec.connector_account.group)
                )
            } else {
                String::new()
            };
            let arguments = argv
                .iter()
                .map(|s| format!("<string>{}</string>", xml(s)))
                .collect::<String>();
            let content = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string>{identity}<key>ProgramArguments</key><array>{arguments}</array><key>WorkingDirectory</key><string>{}</string><key>Umask</key><integer>63</integer><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>5</integer><key>ExitTimeOut</key><integer>{}</integer></dict></plist>\n",
                xml(&spec.connector_account.home),
                spec.shutdown_timeout_seconds + 10
            );
            (name, destination, content)
        }
    };
    files.push(PlannedHostFile {
        name,
        host: HostSide::Connector,
        destination: destination.clone(),
        content,
    });
    actions.push(HostAction { host: HostSide::Connector, responsible_role: if managed {ResponsibleRole::HostAdministrator} else {ResponsibleRole::ConnectorOwner},
        kind: HostActionKind::Service, target: destination, prerequisites: vec![], instructions: vec!["Choose foreground or background startup explicitly. Review and validate the service definition before installation; generation does not install or start it. Enroll separately, then run local preflight as the connector account. Retain the journal on every restart; use the existing explicit recovery procedure after an unclosed session.".into()] });
    for action in &mut actions {
        let account = match action.host {
            HostSide::Connector => &spec.connector_account,
            HostSide::Execution => &spec.execution_account,
        };
        let prerequisite = |kind, target: String| HostPrerequisite {
            host: action.host,
            kind,
            target,
        };
        action.prerequisites.push(prerequisite(
            if matches!(action.kind, HostActionKind::Account) {
                HostPrerequisiteKind::ReviewedIdentity
            } else {
                HostPrerequisiteKind::Account
            },
            account.name.clone(),
        ));
        if matches!(
            action.kind,
            HostActionKind::Launcher | HostActionKind::Service
        ) {
            action.prerequisites.push(prerequisite(
                HostPrerequisiteKind::Executable,
                spec.connector_executable.clone(),
            ));
        }
        if matches!(action.kind, HostActionKind::Service) {
            action.prerequisites.push(prerequisite(
                HostPrerequisiteKind::PrivateState,
                spec.journal.clone(),
            ));
            action.prerequisites.push(prerequisite(
                HostPrerequisiteKind::Enrollment,
                spec.context.connector_id.clone(),
            ));
        }
        if matches!(action.kind, HostActionKind::Ssh)
            && let ConnectionConfig::Ssh(target) = &spec.context.definition.connection
        {
            action.prerequisites.push(prerequisite(
                HostPrerequisiteKind::TrustedHostKey,
                target.host.clone(),
            ));
        }
    }
    Ok(HostConfigurationPlan {
        manifest_version: 1,
        spec,
        actions,
        files,
        foreground_argv: argv,
    })
}

fn account_actions(
    actions: &mut Vec<HostAction>,
    account: &HostAccount,
    host: HostSide,
    ssh: bool,
) {
    let mut instructions = if !account.create {
        vec![format!(
            "Verify existing account {} and group {}; preserve its identity and home {}.",
            account.name,
            account.group,
            sh(&account.home)
        )]
    } else if account.platform == HostPlatform::Linux {
        let shell = if ssh { "/bin/sh" } else { "/usr/sbin/nologin" };
        let uid = account
            .uid
            .map(|v| format!(" --uid {v}"))
            .unwrap_or_default();
        let gid = account
            .gid
            .map(|v| format!(" --gid {v}"))
            .unwrap_or_default();
        vec![format!(
            "First verify account/group names and selected numeric IDs are unused. Review: groupadd --system{gid} {}; useradd --system{uid} --gid {} --create-home --home-dir {} --shell {} {}. Never rewrite an existing identity.",
            account.group,
            account.group,
            sh(&account.home),
            shell,
            account.name
        )]
    } else {
        let uid = account.uid.unwrap();
        let gid = account.gid.unwrap();
        let shell = if ssh { "/bin/sh" } else { "/usr/bin/false" };
        vec![
            format!(
                "Before any changes, verify names and numeric IDs are unused: dscl . -read /Users/{}; dscl . -read /Groups/{}; dscl . -search /Users UniqueID {uid}; dscl . -search /Groups PrimaryGroupID {gid}. Stop on a collision; never modify an existing identity.",
                account.name, account.group
            ),
            format!(
                "After review, create /Groups/{} with PrimaryGroupID {gid}; create /Users/{} with UniqueID {uid}, PrimaryGroupID {gid}, NFSHomeDirectory {}, UserShell {shell}, IsHidden 1 and a disabled password. Use dscl . -create for these explicitly reviewed local records. No administrator group membership or interactive password is needed.",
                account.group,
                account.name,
                sh(&account.home)
            ),
        ]
    };
    instructions.push(if ssh {
        "The execution account requires a usable approved SSH login shell and SFTP access; a login-disabled shell prevents remote preflight and execution."
    } else {
        "For managed direct service identities, keep interactive login and passwords disabled. Existing development users retain their normal login."
    }.into());
    actions.push(HostAction {
        host,
        responsible_role: ResponsibleRole::HostAdministrator,
        kind: HostActionKind::Account,
        target: account.name.clone(),
        prerequisites: vec![],
        instructions,
    });
}
fn directory(
    actions: &mut Vec<HostAction>,
    host: HostSide,
    account: &HostAccount,
    path: &str,
    mode: &str,
) {
    actions.push(HostAction {host, responsible_role: ResponsibleRole::HostAdministrator, kind: HostActionKind::Directory, target: path.into(), prerequisites: vec![], instructions: vec![format!("Create only if missing; inspect existing ownership/access before changing it. Reviewed command: install -d -m {mode} -o {} -g {} {}", account.name, account.group, sh(path))]});
}
fn sh(s: &str) -> String {
    crate::connections::quote(s)
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn systemd(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
fn systemd_value(s: &str) -> String {
    // WorkingDirectory is a single literal path; systemd does not unquote it.
    s.replace('%', "%%")
}
fn sudo_path(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if " \\,:=\t!*?[]#\"()".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| failure("cannot read host configuration"))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read host configuration"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure("host configuration exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("invalid typed host configuration"))
}

pub(crate) fn generate(file: &Path, directory: &Path) -> Result<Value> {
    let plan = render(read_json(file)?)?;
    let parent = directory
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = parent.join(format!(".enumeray-plan-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|_| failure("cannot create private plan staging directory"))?;
    let result = (|| {
        write(
            &staging.join("plan.json"),
            &serde_json::to_vec_pretty(&plan).map_err(|_| failure("cannot encode host plan"))?,
        )?;
        write(
            &staging.join("context.json"),
            &serde_json::to_vec_pretty(
                &json!({"manifest_version":1,"kind":"execution-context","spec":plan.spec.context}),
            )
            .map_err(|_| failure("cannot encode context"))?,
        )?;
        let mut review = String::from(
            "# Review host configuration\n\nNo host changes have been applied. Review each action with its responsible role. Install only the selected matching executable. Scientific software and cluster qualification remain site-owned.\n\n",
        );
        for action in &plan.actions {
            review.push_str(&format!(
                "## {:?}: {:?} — {}\n\n",
                action.host, action.responsible_role, action.target
            ));
            for prerequisite in &action.prerequisites {
                review.push_str(&format!(
                    "Prerequisite: {:?} {:?} {}.\n\n",
                    prerequisite.host, prerequisite.kind, prerequisite.target
                ));
            }
            for instruction in &action.instructions {
                review.push_str(instruction);
                review.push_str("\n\n");
            }
        }
        review.push_str("Foreground startup (after preparation and enrollment):\n\n```sh\n");
        review.push_str(
            &plan
                .foreground_argv
                .iter()
                .map(|s| sh(s))
                .collect::<Vec<_>>()
                .join(" "),
        );
        review.push_str("\n```\n\nLocal preflight never inherits development consent from this plan. Add --allow-development-execution explicitly for trusted same-user checks.\n");
        write(&staging.join("REVIEW.md"), review.as_bytes())?;
        for file in &plan.files {
            write(&staging.join(&file.name), file.content.as_bytes())?;
        }
        fs::File::open(&staging)
            .and_then(|f| f.sync_all())
            .map_err(|_| failure("cannot sync host plan"))?;
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &staging,
            rustix::fs::CWD,
            directory,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| {
            failure("plan destination exists or cannot be published; existing files were preserved")
        })?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| {
                failure("plan published but parent sync failed; inspect the existing destination")
            })?;
        Ok(
            json!({"status":"generated","directory":directory,"context_id":plan.spec.context.id,"applied":false}),
        )
    })();
    if staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| failure("cannot create plan file"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot persist plan file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(platform: &str) -> HostSetupSpec {
        serde_json::from_str(match platform {
            "macos" => include_str!("../../cli/examples/host-setup-macos.json"),
            "development" => include_str!("../../cli/examples/host-setup-development.json"),
            _ => include_str!("../../cli/examples/host-setup-linux.json"),
        })
        .unwrap()
    }
    #[test]
    fn native_templates_preserve_account_isolation_recovery_and_argument_boundaries() {
        let linux = render(spec("linux")).unwrap();
        let unit = &linux
            .files
            .iter()
            .find(|f| f.name.ends_with(".service"))
            .unwrap()
            .content;
        assert!(unit.contains("User=enumeray-connect"));
        assert!(unit.contains("TimeoutStopSec=40"));
        assert!(unit.contains("\"manual\""));
        assert!(
            linux
                .actions
                .iter()
                .all(|action| !action.prerequisites.is_empty())
        );
        assert!(!unit.contains("NoNewPrivileges=true"));
        assert!(
            !linux
                .foreground_argv
                .iter()
                .any(|a| a == "--allow-development-execution")
        );
        let mac = render(spec("macos")).unwrap();
        let plist = &mac
            .files
            .iter()
            .find(|f| f.name.ends_with(".plist"))
            .unwrap()
            .content;
        assert!(plist.contains("<key>UserName</key>"));
        assert!(plist.contains("<integer>63</integer>"));
        assert!(plist.contains("Application Support"));
        assert!(mac.actions.iter().any(|a| {
            a.instructions
                .iter()
                .any(|s| s.contains("Stop on a collision"))
        }));
        let dev = render(spec("development")).unwrap();
        assert!(
            dev.foreground_argv
                .iter()
                .any(|a| a == "--allow-development-execution")
        );
        assert!(dev.files.iter().all(|f| !f.name.ends_with(".sudoers")));
    }
    #[test]
    fn managed_plans_cover_both_platforms_and_all_four_host_paths() {
        for platform in [HostPlatform::Linux, HostPlatform::Macos] {
            for context in [
                include_str!("../../cli/examples/process-direct-context.json"),
                include_str!("../../cli/examples/process-ssh-context.json"),
                include_str!("../../cli/examples/slurm-direct-context.json"),
                include_str!("../../cli/examples/slurm-ssh-context.json"),
            ] {
                let mut value = spec("linux");
                value.context = serde_json::from_value(
                    serde_json::from_str::<Value>(context).unwrap()["spec"].clone(),
                )
                .unwrap();
                value.connector_account.platform = platform;
                value.execution_account.platform = platform;
                value.connector_account.create = false;
                value.execution_account.create = false;
                value.connector_executable = "/usr/local/bin/enumeray".into();
                if matches!(
                    value.context.definition.connection,
                    ConnectionConfig::Ssh(_)
                ) {
                    value.credentials_file =
                        Some("/var/lib/enumeray-connect/credentials/ssh.json".into());
                }
                let plan = render(value).unwrap();
                assert!(plan.actions.iter().all(|a| !a.prerequisites.is_empty()));
                assert!(
                    plan.actions
                        .iter()
                        .filter(|a| matches!(a.kind, HostActionKind::Account))
                        .all(|a| a.instructions[0].contains("Verify existing account"))
                );
                let encoded = serde_json::to_vec(&plan).unwrap();
                serde_json::from_slice::<HostConfigurationPlan>(&encoded).unwrap();
            }
        }
    }
    #[test]
    fn setup_refuses_ambiguous_accounts_modes_and_host_paths() {
        let mut value = spec("macos");
        value.connector_account.uid = None;
        assert!(render(value).is_err());
        let mut value = spec("linux");
        value.execution_account.name = value.connector_account.name.clone();
        assert!(render(value).is_err());
        let mut value = spec("development");
        value.connector_account.create = true;
        assert!(render(value).is_err());
        let mut value = spec("linux");
        value.journal = "/tmp/../private".into();
        assert!(render(value).is_err());
        let mut value = spec("linux");
        value.max_workers = 65;
        assert!(render(value).is_err());
        assert_eq!(xml("<&\""), "&lt;&amp;&quot;");
        assert!(systemd("%n $x").contains("%%n $$x"));
        assert_eq!(systemd_value("/srv/a$b %n"), "/srv/a$b %%n");
        assert_eq!(
            sudo_path("/opt/a b*?[x]#(y)/enumeray"),
            "/opt/a\\ b\\*\\?\\[x\\]\\#\\(y\\)/enumeray"
        );
    }
}
