//! Approved host operations. Privileged writes use pinned directory descriptors.
use crate::{Result, failure, setup};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use enumeray_contracts::{
    execution::{ConnectionConfig, ExecutionMode},
    management::ConnectorEnrollment,
    setup::*,
};
use rustix::fs::{self as rfs, AtFlags, Gid, Mode, OFlags, Uid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

fn digest(spec: &HostSetupSpec) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(spec).map_err(|_| failure("cannot encode host specification"))?
        )
    ))
}
pub fn configuration_digest(spec: &HostSetupSpec) -> std::result::Result<String, String> {
    setup::validate(spec)
        .and_then(|()| digest(spec))
        .map_err(|e| e.to_string())
}
fn os(program: &str, args: &[&str]) -> Result<String> {
    let mut command = Command::new(program);
    command.env_clear();
    if program == "/usr/bin/systemctl" && args.contains(&"--user") {
        command.env("XDG_RUNTIME_DIR", format!("/run/user/{}", uid(None)?));
    }
    let output = command
        .args(args)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir("/")
        .stdin(Stdio::null())
        .output()
        .map_err(|_| failure(format!("cannot run host prerequisite {program}")))?;
    if !output.status.success() {
        return Err(failure(format!(
            "host operation {program} failed; inspect the approved action and host permissions"
        )));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|_| failure("invalid host observation"))
}
fn uid(name: Option<&str>) -> Result<u32> {
    let mut args = vec!["-u"];
    if let Some(name) = name {
        args.push(name);
    }
    os("/usr/bin/id", &args)?
        .parse()
        .map_err(|_| failure("invalid OS identity"))
}
fn ids(account: &HostAccount) -> Result<(u32, u32)> {
    let user = uid(Some(&account.name))?;
    let group = os("/usr/bin/id", &["-g", &account.name])?
        .parse()
        .map_err(|_| failure("invalid OS group"))?;
    if user == 0
        || account.uid.is_some_and(|n| n != user)
        || account.gid.is_some_and(|n| n != group)
        || os("/usr/bin/id", &["-gn", &account.name])? != account.group
    {
        return Err(failure(
            "existing account identity conflicts with the reviewed plan",
        ));
    }
    let home = match account.platform {
        HostPlatform::Linux => os("/usr/bin/getent", &["passwd", &account.name])?
            .split(':')
            .nth(5)
            .unwrap_or("")
            .to_owned(),
        HostPlatform::Macos => os(
            "/usr/bin/dscl",
            &[
                ".",
                "-read",
                &format!("/Users/{}", account.name),
                "NFSHomeDirectory",
            ],
        )?
        .strip_prefix("NFSHomeDirectory: ")
        .unwrap_or("")
        .to_owned(),
    };
    if home != account.home {
        return Err(failure(
            "existing account home differs from the reviewed plan",
        ));
    }
    Ok((user, group))
}
fn directory(
    path: &Path,
    owner: u32,
    group: u32,
    mode: u32,
    create: bool,
    protected: bool,
) -> Result<File> {
    // These are fixed, root-controlled macOS aliases, not user-provided links.
    #[cfg(target_os = "macos")]
    let native_path = if let Ok(rest) = path.strip_prefix("/etc") {
        Path::new("/private/etc").join(rest)
    } else if let Ok(rest) = path.strip_prefix("/var") {
        Path::new("/private/var").join(rest)
    } else {
        path.to_path_buf()
    };
    #[cfg(target_os = "macos")]
    let path = native_path.as_path();
    if !setup::absolute(path.to_str().unwrap_or("")) {
        return Err(failure("host path must be a safe absolute path"));
    }
    let mut current = File::open("/").map_err(|_| failure("cannot open filesystem root"))?;
    let parts = path
        .components()
        .filter_map(|p| {
            if let Component::Normal(p) = p {
                Some(p)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let mut created = false;
        let next = match rfs::openat(
            &current,
            *part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(e) if e == rustix::io::Errno::NOENT && create => {
                rfs::mkdirat(
                    &current,
                    *part,
                    Mode::from_raw_mode(
                        (if last {
                            mode
                        } else {
                            if uid(None)? == 0 { 0o755 } else { 0o700 }
                        }) as _,
                    ),
                )
                .map_err(|_| failure("cannot prepare host directory"))?;
                created = true;
                File::from(
                    rfs::openat(
                        &current,
                        *part,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|_| failure("cannot open prepared host directory"))?,
                )
            }
            Err(_) => return Err(failure("missing, inaccessible or symlinked host directory")),
        };
        if created {
            let (u, g) = if last {
                (owner, group)
            } else {
                (
                    uid(None)?,
                    os("/usr/bin/id", &["-g"])?
                        .parse()
                        .map_err(|_| failure("invalid host group"))?,
                )
            };
            rfs::fchown(&next, Some(Uid::from_raw(u)), Some(Gid::from_raw(g)))
                .map_err(|_| failure("cannot set directory ownership"))?;
            rfs::fchmod(
                &next,
                Mode::from_raw_mode(
                    (if last {
                        mode
                    } else {
                        if uid(None)? == 0 { 0o755 } else { 0o700 }
                    }) as _,
                ),
            )
            .map_err(|_| failure("cannot set directory permissions"))?;
            current
                .sync_all()
                .map_err(|_| failure("cannot persist directory creation"))?;
        }
        let m = next
            .metadata()
            .map_err(|_| failure("cannot inspect directory"))?;
        if protected && (m.uid() != 0 || m.mode() & 0o022 != 0) {
            return Err(failure(
                "managed executable/service ancestors must be root-owned and not group/other writable",
            ));
        }
        if last
            && (m.uid() != owner
                || (!protected && m.gid() != group)
                || (mode == 0o700 && m.mode() & 0o077 != 0))
        {
            return Err(failure(
                "existing directory ownership/access conflicts with the plan; it was preserved",
            ));
        }
        current = next;
    }
    Ok(current)
}
fn owned_file(
    path: &Path,
    bytes: &[u8],
    owner: u32,
    group: u32,
    mode: u32,
    protected: bool,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("host file has no parent"))?;
    let d = directory(
        parent,
        if protected { 0 } else { owner },
        if protected { 0 } else { group },
        0o755,
        true,
        protected,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| failure("invalid host filename"))?;
    match rfs::openat(
        &d,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => {
            let mut f = File::from(fd);
            let m = f
                .metadata()
                .map_err(|_| failure("cannot inspect host file"))?;
            if !m.is_file()
                || m.uid() != owner
                || m.gid() != group
                || m.mode() & 0o777 != mode
                || m.len() != bytes.len() as u64
            {
                return Err(failure(
                    "existing host file conflicts with the plan; it was preserved",
                ));
            }
            let mut old = Vec::new();
            f.read_to_end(&mut old)
                .map_err(|_| failure("cannot read host file"))?;
            if old != bytes {
                return Err(failure(
                    "existing host file content conflicts with the plan; it was preserved",
                ));
            }
            return Ok(());
        }
        Err(e) if e == rustix::io::Errno::NOENT => (),
        Err(_) => return Err(failure("cannot safely inspect host file")),
    }
    let temporary = format!(".enumeray-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut f = File::from(
            rfs::openat(
                &d,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| failure("cannot stage host file"))?,
        );
        f.write_all(bytes)
            .map_err(|_| failure("cannot write host file"))?;
        rfs::fchown(&f, Some(Uid::from_raw(owner)), Some(Gid::from_raw(group)))
            .map_err(|_| failure("cannot set file ownership"))?;
        rfs::fchmod(&f, Mode::from_raw_mode(mode as _))
            .map_err(|_| failure("cannot set file permissions"))?;
        f.sync_all()
            .map_err(|_| failure("cannot persist host file"))?;
        rfs::linkat(&d, temporary.as_str(), &d, name, AtFlags::empty()).map_err(|_| {
            failure("host file appeared during apply; preserve it and repeat inspection")
        })?;
        d.sync_all()
            .map_err(|_| failure("cannot persist host file publication"))?;
        Ok(())
    })();
    let _ = rfs::unlinkat(&d, temporary.as_str(), AtFlags::empty());
    result
}
fn runtime_path(spec: &HostSetupSpec) -> PathBuf {
    Path::new(&spec.journal)
        .parent()
        .unwrap()
        .join("runtime.json")
}

/// Atomically stage one reviewed setup-project file under the invoking identity.
/// Existing bytes are verified and preserved; directory traversal never follows links.
pub fn stage_setup_file(
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> std::result::Result<(), String> {
    let result = (|| {
        let group = os("/usr/bin/id", &["-g"])?
            .parse()
            .map_err(|_| failure("invalid host group"))?;
        directory(
            path.parent()
                .ok_or_else(|| failure("staged file has no parent"))?,
            uid(None)?,
            group,
            0o700,
            true,
            false,
        )?;
        owned_file(
            path,
            bytes,
            uid(None)?,
            group,
            if executable { 0o500 } else { 0o400 },
            false,
        )
    })();
    result.map_err(|e| e.to_string())
}
fn write_runtime(spec: &HostSetupSpec, consent: bool, owner: u32, group: u32) -> Result<()> {
    let config = ConnectorRuntimeConfiguration {
        manifest_version: 1,
        spec: spec.clone(),
        plan_sha256: digest(spec)?,
        allow_development_execution: consent,
    };
    owned_file(
        &runtime_path(spec),
        &serde_json::to_vec_pretty(&config)
            .map_err(|_| failure("cannot encode runtime configuration"))?,
        owner,
        group,
        0o600,
        false,
    )
}
fn prepare(spec: &HostSetupSpec) -> Result<Value> {
    setup::validate(spec)?;
    if spec.context.definition.execution_mode != ExecutionMode::Development
        || uid(None)? == 0
        || uid(None)? != uid(Some(&spec.connector_account.name))?
    {
        return Err(failure(
            "development preparation requires its existing regular OS user",
        ));
    }
    let (u, g) = ids(&spec.connector_account)?;
    directory(
        runtime_path(spec).parent().unwrap(),
        u,
        g,
        0o700,
        true,
        false,
    )?;
    for p in [
        Path::new(&spec.journal),
        Path::new(&spec.token_file).parent().unwrap(),
        Path::new(&spec.context.definition.work_root),
    ] {
        directory(p, u, g, 0o700, true, false)?;
    }
    for p in &spec.context.definition.installation_roots {
        directory(Path::new(p), u, g, 0o700, true, false)?;
    }
    write_runtime(spec, true, u, g)?;
    Ok(json!({"status":"prepared","configuration":runtime_path(spec)}))
}
pub fn prepare_development(
    spec: &HostSetupSpec,
    allow: bool,
) -> std::result::Result<Value, String> {
    if !allow {
        return Err("Explicit trusted development execution consent is required.".into());
    }
    prepare(spec).map_err(|e| e.to_string())
}
fn ensure_account(account: &HostAccount, ssh: bool, ledger: &Path) -> Result<(u32, u32)> {
    if let Ok(found) = ids(account) {
        return Ok(found);
    }
    if !account.create {
        return Err(failure(
            "required existing account is absent or conflicts with the plan",
        ));
    }
    if uid(Some(&account.name)).is_ok() {
        return Err(failure(
            "account name already exists with a different identity",
        ));
    }
    if account.platform == HostPlatform::Macos
        && !ledger
            .join(format!("account-{}.json", account.name))
            .exists()
        && os(
            "/usr/bin/dscl",
            &[".", "-read", &format!("/Users/{}", account.name)],
        )
        .is_ok()
    {
        return Err(failure(
            "a pre-existing partial macOS account must be reviewed by its administrator; it was preserved",
        ));
    }
    owned_file(
        &ledger.join(format!("account-{}.json", account.name)),
        &serde_json::to_vec(account).map_err(|_| failure("cannot encode account intent"))?,
        0,
        0,
        0o600,
        true,
    )?;
    match account.platform {
        HostPlatform::Linux => {
            if let Ok(group) = os("/usr/bin/getent", &["group", &account.group]) {
                let gid = group
                    .split(':')
                    .nth(2)
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(|| failure("invalid existing group"))?;
                if account.gid.is_some_and(|n| n != gid) {
                    return Err(failure("group ID collision"));
                }
            } else {
                let mut args = vec!["--system".to_owned()];
                if let Some(gid) = account.gid {
                    args.extend(["--gid".into(), gid.to_string()]);
                }
                args.push(account.group.clone());
                os(
                    "/usr/sbin/groupadd",
                    &args.iter().map(String::as_str).collect::<Vec<_>>(),
                )?;
            }
            let mut args = vec![
                "--system".to_owned(),
                "--gid".into(),
                account.group.clone(),
                "--no-create-home".into(),
                "--home-dir".into(),
                account.home.clone(),
                "--shell".into(),
                if ssh {
                    "/bin/sh".into()
                } else {
                    "/usr/sbin/nologin".into()
                },
            ];
            if let Some(user) = account.uid {
                args.extend(["--uid".into(), user.to_string()]);
            }
            args.push(account.name.clone());
            os(
                "/usr/sbin/useradd",
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            )?;
        }
        HostPlatform::Macos => {
            let user = account
                .uid
                .ok_or_else(|| failure("review an unused macOS UID"))?
                .to_string();
            let group = account
                .gid
                .ok_or_else(|| failure("review an unused macOS GID"))?
                .to_string();
            for (collection, attribute, value, own) in [
                ("/Users", "UniqueID", user.as_str(), account.name.as_str()),
                (
                    "/Groups",
                    "PrimaryGroupID",
                    group.as_str(),
                    account.group.as_str(),
                ),
            ] {
                let occupied = os(
                    "/usr/bin/dscl",
                    &[".", "-search", collection, attribute, value],
                )?;
                if occupied
                    .lines()
                    .any(|line| line.split_whitespace().next().is_some_and(|n| n != own))
                {
                    return Err(failure("macOS numeric identity collision"));
                }
            }
            let gp = format!("/Groups/{}", account.group);
            let up = format!("/Users/{}", account.name);
            if let Ok(existing) = os("/usr/bin/dscl", &[".", "-read", &gp, "PrimaryGroupID"]) {
                if existing.split_whitespace().last() != Some(group.as_str()) {
                    return Err(failure("existing macOS group differs"));
                }
            } else {
                os("/usr/bin/dscl", &[".", "-create", &gp])?;
                os(
                    "/usr/bin/dscl",
                    &[".", "-create", &gp, "PrimaryGroupID", &group],
                )?;
            }
            for (attribute, value) in [
                ("UniqueID", user.as_str()),
                ("PrimaryGroupID", group.as_str()),
                ("NFSHomeDirectory", account.home.as_str()),
                ("UserShell", if ssh { "/bin/sh" } else { "/usr/bin/false" }),
                ("IsHidden", "1"),
                ("Password", "*"),
            ] {
                os("/usr/bin/dscl", &[".", "-create", &up, attribute, value])?;
            }
        }
    }
    ids(account)
}
fn install_binary(
    spec: &HostSetupSpec,
    destination: &str,
    override_source: Option<&Path>,
    expected_sha256: Option<&str>,
) -> Result<()> {
    let source = override_source
        .or_else(|| spec.executable_source.as_deref().map(Path::new))
        .ok_or_else(|| {
            failure("managed installation requires a reviewed executable source and checksum")
        })?;
    let source_file = File::from(
        rfs::open(
            source,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("cannot safely open executable source"))?,
    );
    let metadata = source_file
        .metadata()
        .map_err(|_| failure("cannot inspect executable source"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 512 * 1024 * 1024
    {
        return Err(failure("invalid executable source"));
    }
    let mut bytes = Vec::new();
    source_file
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read reviewed executable source"))?;
    if Some(format!("{:x}", Sha256::digest(&bytes))).as_deref() != expected_sha256 {
        return Err(failure("executable source changed after review"));
    }
    owned_file(Path::new(destination), &bytes, 0, 0, 0o755, true)
}
fn read_enrollment(path: &Path, connector: &str) -> Result<ConnectorEnrollment> {
    crate::journal::check_private_path(path, false)?;
    let file = File::from(
        rfs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("cannot safely open enrollment document"))?,
    );
    if !file
        .metadata()
        .map_err(|_| failure("cannot inspect enrollment document"))?
        .is_file()
    {
        return Err(failure("enrollment document must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read enrollment document"))?;
    if bytes.len() > 16 * 1024 {
        return Err(failure("enrollment document exceeds 16 KiB"));
    }
    let document: ConnectorEnrollment =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid enrollment document"))?;
    if document.connector_id != connector
        || document.id.is_nil()
        || document.enrollment_token.len() != 43
        || !URL_SAFE_NO_PAD
            .decode(&document.enrollment_token)
            .is_ok_and(|token| token.len() == 32)
    {
        return Err(failure(
            "enrollment document does not match this connector's credential contract",
        ));
    }
    Ok(document)
}
fn apply(
    path: &Path,
    side: HostSide,
    approved: &str,
    enrollment: Option<&Path>,
    executable_source: Option<&Path>,
) -> Result<Value> {
    if side == HostSide::Execution && enrollment.is_some() {
        return Err(failure(
            "execution-host preparation never accepts connector enrollment credentials",
        ));
    }
    let plan: HostConfigurationPlan = setup::read_json(path)?;
    setup::validate(&plan.spec)?;
    let regenerated = setup::render(plan.spec.clone())?;
    if serde_json::to_value(&plan).ok() != serde_json::to_value(&regenerated).ok()
        || digest(&plan.spec)? != approved
    {
        return Err(failure("host plan changed or its digest was not approved"));
    }
    let spec = &plan.spec;
    if spec.context.definition.execution_mode != ExecutionMode::Managed || uid(None)? != 0 {
        return Err(failure(
            "managed host apply requires an explicit administrator invocation",
        ));
    }
    let account = if side == HostSide::Connector {
        &spec.connector_account
    } else {
        &spec.execution_account
    };
    let native = if cfg!(target_os = "macos") {
        HostPlatform::Macos
    } else {
        HostPlatform::Linux
    };
    if account.platform != native {
        return Err(failure(
            "run this apply step locally on its reviewed host platform",
        ));
    }
    let base = if native == HostPlatform::Macos {
        "/Library/Application Support/EnumeraySetup"
    } else {
        "/var/lib/enumeray-setup"
    };
    let ledger = Path::new(base)
        .join(&spec.context.connector_id)
        .join(if side == HostSide::Connector {
            "connector"
        } else {
            "execution"
        })
        .join(approved);
    directory(&ledger, 0, 0, 0o700, true, true)?;
    // Serialize numeric account allocation across every connector on this host.
    let ld = directory(Path::new(base), 0, 0, 0o755, false, true)?;
    let lock = File::from(
        rfs::openat(
            &ld,
            "apply.lock",
            OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|_| failure("cannot lock host application"))?,
    );
    lock.try_lock()
        .map_err(|_| failure("this host setup is already being applied"))?;
    owned_file(
        &ledger.join("spec.json"),
        &serde_json::to_vec(spec).map_err(|_| failure("cannot encode host specification"))?,
        0,
        0,
        0o600,
        true,
    )?;
    let ssh = matches!(spec.context.definition.connection, ConnectionConfig::Ssh(_));
    let (u, g) = ensure_account(account, side == HostSide::Execution && ssh, &ledger)?;
    let mut local_preflight = Value::Null;
    if side == HostSide::Connector {
        if !ssh {
            let (execution_uid, _) = ensure_account(&spec.execution_account, false, &ledger)?;
            if u == execution_uid {
                return Err(failure(
                    "managed connector and execution accounts resolve to the same UID; no credentials were installed",
                ));
            }
            prepare_execution(spec, &ledger, executable_source)?;
        }
        let home = Path::new(&account.home);
        directory(
            home,
            u,
            g,
            if home.exists() { 0o755 } else { 0o700 },
            true,
            false,
        )?;
        if fs::metadata(home)
            .map_err(|_| failure("cannot inspect connector home"))?
            .mode()
            & 0o022
            != 0
        {
            return Err(failure(
                "connector home is writable by another identity; preserve it and correct site access",
            ));
        }
        directory(
            runtime_path(spec).parent().unwrap(),
            u,
            g,
            0o700,
            true,
            false,
        )?;
        directory(Path::new(&spec.journal), u, g, 0o700, true, false)?;
        directory(
            Path::new(&spec.token_file).parent().unwrap(),
            u,
            g,
            0o700,
            true,
            false,
        )?;
        if let Some(p) = &spec.credentials_file {
            directory(Path::new(p).parent().unwrap(), u, g, 0o700, true, false)?;
        }
        install_binary(
            spec,
            &spec.connector_executable,
            executable_source,
            spec.executable_sha256.as_deref(),
        )?;
        if let Some(source) = enrollment {
            let document = read_enrollment(source, &spec.context.connector_id)?;
            let destination =
                Path::new(&account.home).join(format!("enrollment-{}.json", document.id));
            owned_file(
                &destination,
                &serde_json::to_vec(&document)
                    .map_err(|_| failure("invalid enrollment document"))?,
                u,
                g,
                0o600,
                false,
            )?;
            os(
                "/usr/bin/sudo",
                &[
                    "-n",
                    "-u",
                    &account.name,
                    "--",
                    &spec.connector_executable,
                    "connector",
                    "enroll",
                    "--directory",
                    Path::new(&spec.token_file)
                        .parent()
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "--api-base-url",
                    &spec.api_base_url,
                    "--enrollment-file",
                    destination.to_str().unwrap(),
                ],
            )?;
        }
        write_runtime(spec, false, u, g)?;
        for file in &plan.files {
            if !spec.install_service && !file.name.ends_with(".sudoers") {
                continue;
            }
            let stage = ledger.join(&file.name);
            owned_file(&stage, file.content.as_bytes(), 0, 0, 0o600, true)?;
            if file.name.ends_with(".sudoers") {
                os("/usr/sbin/visudo", &["-cf", stage.to_str().unwrap()])?;
            }
            if file.name.ends_with(".plist") {
                os("/usr/bin/plutil", &["-lint", "--", stage.to_str().unwrap()])?;
            }
            if file.name.ends_with(".service") {
                os(
                    "/usr/bin/systemd-analyze",
                    &["verify", stage.to_str().unwrap()],
                )?;
            }
            owned_file(
                Path::new(&file.destination),
                file.content.as_bytes(),
                0,
                0,
                if file.name.ends_with(".sudoers") {
                    0o440
                } else {
                    0o644
                },
                true,
            )?;
        }
        let private_plan = runtime_path(spec).parent().unwrap().join("host-plan.json");
        owned_file(
            &private_plan,
            &serde_json::to_vec(&regenerated)
                .map_err(|_| failure("cannot encode private host plan"))?,
            u,
            g,
            0o600,
            false,
        )?;
        let observed = Command::new("/usr/bin/sudo")
            .args([
                "-n",
                "-u",
                &account.name,
                "--",
                &spec.connector_executable,
                "connector",
                "preflight",
                "--plan",
                private_plan.to_str().unwrap(),
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", &account.home)
            .output()
            .map_err(|_| failure("cannot run local prerequisites as the connector account"))?;
        local_preflight = serde_json::from_slice(&observed.stdout).map_err(|_| {
            failure("host preparation remains saved; local prerequisites returned no valid report")
        })?;
        owned_file(
            &ledger.join(format!("preflight-{}.json", uuid::Uuid::new_v4())),
            &serde_json::to_vec(&local_preflight)
                .map_err(|_| failure("cannot encode local preflight evidence"))?,
            0,
            0,
            0o600,
            true,
        )?;
    } else {
        prepare_execution(spec, &ledger, executable_source)?;
    }
    owned_file(
        &ledger.join("complete.json"),
        b"{\"host_preparation_complete\":true}",
        0,
        0,
        0o600,
        true,
    )?;
    Ok(
        json!({"status":"prepared","side":side,"plan_sha256":approved,"configuration":runtime_path(spec),"service_started":false,"local_preflight":local_preflight,"next_action":"Review the local prerequisite report and correct blocked or unknown checks before explicit startup. Host preparation alone does not establish connected or ready status."}),
    )
}
fn prepare_execution(
    spec: &HostSetupSpec,
    ledger: &Path,
    executable_source: Option<&Path>,
) -> Result<()> {
    let ssh = matches!(spec.context.definition.connection, ConnectionConfig::Ssh(_));
    let (u, g) = ensure_account(&spec.execution_account, ssh, ledger)?;
    if spec.execution_account.create {
        directory(
            Path::new(&spec.execution_account.home),
            u,
            g,
            0o700,
            true,
            false,
        )?;
    }
    directory(
        Path::new(&spec.context.definition.work_root),
        u,
        g,
        0o700,
        true,
        false,
    )?;
    for root in &spec.context.definition.installation_roots {
        let path = Path::new(root);
        if path.exists() {
            let metadata = fs::symlink_metadata(path)
                .map_err(|_| failure("cannot inspect installation root"))?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || path.canonicalize().ok().as_deref() != Some(path)
            {
                return Err(failure(
                    "installation root must be a real approved directory",
                ));
            }
        } else {
            directory(path, u, g, 0o700, true, false)?;
        }
    }
    if let ConnectionConfig::Ssh(ssh) = &spec.context.definition.connection {
        install_binary(
            spec,
            &ssh.runner_path,
            executable_source,
            spec.execution_executable_sha256
                .as_deref()
                .or(spec.executable_sha256.as_deref()),
        )?;
    }
    Ok(())
}
pub fn apply_host_configuration(
    path: &Path,
    side: HostSide,
    approved: &str,
    enrollment: Option<&Path>,
    executable_source: Option<&Path>,
) -> std::result::Result<Value, String> {
    apply(path, side, approved, enrollment, executable_source).map_err(|e| e.to_string())
}
fn runtime(path: &Path, administrator: bool) -> Result<ConnectorRuntimeConfiguration> {
    crate::journal::check_private_path(path, false)?;
    let config: ConnectorRuntimeConfiguration = setup::read_json(path)?;
    setup::validate(&config.spec)?;
    if config.manifest_version != 1 || digest(&config.spec)? != config.plan_sha256 {
        return Err(failure(
            "runtime configuration does not match its reviewed host plan",
        ));
    }
    if config.spec.context.definition.execution_mode == ExecutionMode::Development
        && !config.allow_development_execution
    {
        return Err(failure("saved runtime lacks explicit development consent"));
    }
    if fs::metadata(path)
        .map_err(|_| failure("cannot inspect runtime configuration owner"))?
        .uid()
        != uid(Some(&config.spec.connector_account.name))?
    {
        return Err(failure(
            "runtime configuration ownership differs from its saved connector account",
        ));
    }
    if uid(None)? != uid(Some(&config.spec.connector_account.name))?
        && !(administrator
            && uid(None)? == 0
            && config.spec.context.definition.execution_mode == ExecutionMode::Managed)
    {
        return Err(failure(format!(
            "run this host command as {} using its configuration; user profiles are not used",
            config.spec.connector_account.name
        )));
    }
    Ok(config)
}
pub(crate) fn start(path: &Path, foreground: bool, recover: bool) -> Result<Value> {
    let config = runtime(path, !foreground)?;
    let spec = &config.spec;
    if !foreground {
        if spec.context.definition.execution_mode == ExecutionMode::Managed && !spec.install_service
        {
            return Err(failure(
                "saved host plan selected foreground operation; review and apply a background service plan before starting it",
            ));
        }
        let plan = setup::render(spec.clone())?;
        let service = plan
            .files
            .iter()
            .find(|f| f.name.ends_with(".service") || f.name.ends_with(".plist"))
            .ok_or_else(|| failure("service configuration unavailable"))?;
        if spec.context.definition.execution_mode == ExecutionMode::Development {
            owned_file(
                Path::new(&service.destination),
                service.content.as_bytes(),
                uid(None)?,
                ids(&spec.connector_account)?.1,
                0o600,
                false,
            )?;
        } else {
            verify_service(service)?;
        }
        if spec.connector_account.platform == HostPlatform::Linux {
            let mut args = vec![];
            if spec.context.definition.execution_mode == ExecutionMode::Development {
                args.push("--user");
            }
            let mut reload = args.clone();
            reload.push("daemon-reload");
            os("/usr/bin/systemctl", &reload)?;
            args.extend(["start", &service.name]);
            os("/usr/bin/systemctl", &args)?;
        } else {
            let domain = if spec.context.definition.execution_mode == ExecutionMode::Managed {
                "system".into()
            } else {
                format!("gui/{}", uid(None)?)
            };
            let label = format!(
                "{domain}/org.enumeray.connector.{}",
                spec.context.connector_id
            );
            if os("/bin/launchctl", &["print", &label]).is_err() {
                os(
                    "/bin/launchctl",
                    &["bootstrap", &domain, &service.destination],
                )?;
            }
        }
        return Ok(
            json!({"status":"start_requested","configuration":path,"readiness":"inspect setup diagnostics"}),
        );
    }
    let mut argv = setup::render(spec.clone())?.foreground_argv;
    if !config.allow_development_execution {
        argv.retain(|s| s != "--allow-development-execution");
    }
    if recover {
        argv.push("--confirm-prior-process-stopped".into());
    }
    use std::os::unix::process::CommandExt;
    let error = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &spec.connector_account.home)
        .exec();
    Err(failure(format!(
        "cannot start the saved connector runtime: {error}"
    )))
}

pub(crate) fn status(path: &Path) -> Result<Value> {
    let config = runtime(path, true)?;
    let snapshot = crate::journal::Journal::snapshot(Path::new(&config.spec.journal));
    let service = native_state(&config.spec);
    Ok(
        json!({"configuration":path,"connector_id":config.spec.context.connector_id,"journal_available":snapshot.is_ok(),"journal":snapshot.ok().map(|(_,s)|s),"service":service,"process_state":"unknown","readiness":"use authenticated setup status; local journal/service observations do not prove session ownership or host readiness"}),
    )
}

fn verify_service(file: &PlannedHostFile) -> Result<()> {
    let path = Path::new(&file.destination);
    let parent = directory(
        path.parent()
            .ok_or_else(|| failure("invalid service destination"))?,
        0,
        0,
        0o755,
        false,
        true,
    )?;
    let mut f = File::from(
        rfs::openat(
            &parent,
            path.file_name()
                .ok_or_else(|| failure("invalid service name"))?,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("install the approved service definition through host apply first"))?,
    );
    let m = f
        .metadata()
        .map_err(|_| failure("cannot inspect installed service"))?;
    if !m.is_file() || m.uid() != 0 || m.mode() & 0o022 != 0 || m.len() != file.content.len() as u64
    {
        return Err(failure(
            "installed service ownership/access conflicts with the plan",
        ));
    }
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read installed service"))?;
    if bytes != file.content.as_bytes() {
        return Err(failure(
            "installed service differs from the saved configuration; inspect it before startup",
        ));
    }
    Ok(())
}
fn native_state(spec: &HostSetupSpec) -> Value {
    if spec.connector_account.platform == HostPlatform::Linux {
        let name = format!("enumeray-{}.service", spec.context.connector_id);
        let mut args = vec![];
        if spec.context.definition.execution_mode == ExecutionMode::Development {
            args.push("--user");
        }
        args.extend([
            "show",
            &name,
            "--property=ActiveState,SubState",
            "--no-pager",
        ]);
        match os("/usr/bin/systemctl", &args) {
            Ok(text) => json!({"observation":text,"source":"systemd"}),
            Err(_) => json!({"state":"unknown","source":"systemd"}),
        }
    } else {
        let domain = if spec.context.definition.execution_mode == ExecutionMode::Managed {
            "system".into()
        } else {
            format!("gui/{}", uid(None).unwrap_or(0))
        };
        let label = format!(
            "{domain}/org.enumeray.connector.{}",
            spec.context.connector_id
        );
        match os("/bin/launchctl", &["print", &label]) {
            Ok(text) => {
                json!({"state":if text.lines().any(|line|line.trim()=="state = running"){"running"}else{"loaded"},"source":"launchd"})
            }
            Err(_) => json!({"state":"unknown","source":"launchd"}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

    #[test]
    fn enrollment_staging_accepts_only_private_typed_credentials_for_this_connector() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("enumeray-host-enrollment-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("enrollment.json");
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let mut document = json!({"id":uuid::Uuid::new_v4(),"connector_id":"example","enrollment_token":token,"expires_at":"2099-01-01T00:00:00Z"});
        let write = |document: &Value| {
            fs::write(&path, serde_json::to_vec(document).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write(&document);
        assert_eq!(
            read_enrollment(&path, "example").unwrap().enrollment_token,
            token
        );
        assert!(read_enrollment(&path, "different").is_err());
        document["unexpected_secret"] = json!("not an enrollment field");
        write(&document);
        assert!(read_enrollment(&path, "example").is_err());
        document
            .as_object_mut()
            .unwrap()
            .remove("unexpected_secret");
        document["enrollment_token"] = json!("invalid");
        write(&document);
        assert!(read_enrollment(&path, "example").is_err());
        let link = root.join("linked.json");
        symlink(&path, &link).unwrap();
        assert!(read_enrollment(&link, "example").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
