//! Filesystem and subprocess mechanics shared by connector integration tests.
//! Scenario setup, HTTP ordering, and credential assertions belong to each suite.

#![allow(
    dead_code,
    reason = "Each integration-test binary uses a different fixture subset."
)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use serde_json::Value;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

static CLI_BINARY_FALLBACK: std::sync::OnceLock<fn() -> PathBuf> = std::sync::OnceLock::new();

/// External integration harnesses can supply their own executable discovery.
pub fn set_cli_binary_fallback(resolve: fn() -> PathBuf) {
    CLI_BINARY_FALLBACK.get_or_init(|| resolve);
}

pub fn cli_binary() -> PathBuf {
    std::env::var_os("ENUMERAY_CLI_BIN")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_enumeray").map(PathBuf::from))
        .or_else(|| CLI_BINARY_FALLBACK.get().map(|resolve| resolve()))
        .expect("Set ENUMERAY_CLI_BIN to the matching public CLI executable")
}

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            static READY: std::sync::LazyLock<()> = std::sync::LazyLock::new(|| {
                assert!(
                    std::process::Command::new(cli_binary())
                        .arg("--help")
                        .stdout(std::process::Stdio::null())
                        .status()
                        .unwrap()
                        .success()
                );
            });
            std::sync::LazyLock::force(&READY);
        }
        let path =
            std::env::temp_dir().join(format!("enumeray-connector-fixture-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }

    pub fn journal(&self) -> PathBuf {
        self.0.join("installation")
    }

    pub fn token(&self, token: &str) {
        let path = self.0.join("token");
        fs::write(&path, token).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    pub fn read(&self) -> Value {
        serde_json::from_slice(&fs::read(self.journal().join("journal.json")).unwrap()).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn command() -> Command {
    let mut command = Command::new(cli_binary());
    command.arg("connector").kill_on_drop(true);
    command
}

pub async fn run(command: &mut Command) -> Output {
    timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap()
        .unwrap()
}

pub fn successful_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Installation inputs for the HTTP publication fixture, never a connector option.
pub fn write_installation_fixture(scratch: &Scratch, installation: Value) {
    #[cfg(target_os = "macos")]
    {
        // Keep macOS first-execution checks outside service/transport deadlines.
        // The readiness flag exits before any scientific fixture code executes.
        let argv = installation["argv"].as_array().unwrap();
        if argv.len() > 1 && argv[0].as_str().unwrap().contains("python") {
            let wrapper = PathBuf::from(argv[1].as_str().unwrap());
            if wrapper.exists() {
                let code = fs::read_to_string(&wrapper).unwrap();
                if !code.contains("--fixture-ready") {
                    let body = if code.starts_with("#!") {
                        code.split_once('\n').map_or("", |(_, body)| body)
                    } else {
                        &code
                    };
                    fs::write(&wrapper, format!("#!{}\nimport sys\nif sys.argv[1:]==['--fixture-ready']:sys.exit(0)\n{body}", argv[0].as_str().unwrap())).unwrap();
                    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
                    assert!(
                        std::process::Command::new(&wrapper)
                            .arg("--fixture-ready")
                            .status()
                            .unwrap()
                            .success()
                    );
                }
            }
        }
    }
    let path = scratch.0.join("installation-fixture.json");
    let config = serde_json::json!({"version":1,"installations":[{
        "provider_binding_id":installation["provider_binding_id"],
        "installation":installation,
        "work_root":scratch.0.join("compute"),
        "driver":{"kind":"process","scratch":{"max_bytes":1073741824,"max_entries":10000,"max_depth":32,"scan_interval_ms":1000,"cache":"per_run","symlinks":"forbid"}},
        "connection":{"kind":"direct","launcher":[cli_binary()]},
        "identity_file":null
    }]});
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
pub fn install_scalar(scratch: &Scratch) {
    let wrapper = scratch.0.join("wrapper.py");
    // The fixture accepts both example publications and emits no incidental logs.
    let code = include_str!("../../../../examples/scalar_model/wrapper.py")
        .replace(" or value[\"publication_id\"] != \"local-v1\"", "")
        .replace(
            "    print(\"Scalar wrapper completed.\", file=sys.stderr)",
            "",
        );
    fs::write(&wrapper, code).unwrap();
    fs::write(
        scratch.0.join("model.py"),
        include_bytes!("../../../../examples/scalar_model/model.py"),
    )
    .unwrap();
    let binding: Value = serde_json::from_str(include_str!(
        "../../../../examples/scalar-double-installation.json"
    ))
    .unwrap();
    let mut installation = binding.clone();
    installation["argv"] = serde_json::json!(["/usr/bin/python3", wrapper, "execute"]);
    installation["environment"] = serde_json::json!({});
    write_installation_fixture(scratch, installation);
}

/// Model/transport inputs become the same desired publication configuration that
/// the API serves. The local launcher simulates account separation during validate;
/// real account isolation is qualified by the real-backends service suite.
pub fn desired_configuration(root: &Path) -> Value {
    use serde_json::json;
    let fixture: Value =
        serde_json::from_slice(&fs::read(root.join("installation-fixture.json")).unwrap()).unwrap();
    let launcher = root.join("publication-runner.py");
    if !launcher.exists() {
        fs::write(&launcher, r#"import json,os,subprocess,sys
command=json.loads(sys.argv[1])+sys.argv[2:]
if sys.argv[3] in ('validate','identity'):
 result=subprocess.run(command,input=sys.stdin.buffer.read(),stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 sys.stderr.buffer.write(result.stderr)
 if result.returncode:sys.exit(result.returncode)
 reply=json.loads(result.stdout)
 reply['payload']['execution_uid']=str(os.getuid()+1)
 print(json.dumps(reply))
else:os.execv(command[0],command)
"#).unwrap();
    }
    let mut contexts = Vec::new();
    let mut publications = Vec::new();
    for installation in fixture["installations"].as_array().unwrap() {
        let argv = installation["installation"]["argv"].as_array().unwrap();
        let (executable, arguments) =
            if argv.len() > 1 && argv[0].as_str().unwrap().contains("python") {
                let wrapper = PathBuf::from(argv[1].as_str().unwrap());
                if wrapper.exists() {
                    let code = fs::read_to_string(&wrapper).unwrap();
                    let expected = format!("#!{}\n", argv[0].as_str().unwrap());
                    if !code.starts_with(&expected) {
                        let body = if code.starts_with("#!") {
                            code.split_once('\n').map_or("", |(_, body)| body)
                        } else {
                            &code
                        };
                        fs::write(&wrapper, format!("{expected}{body}")).unwrap();
                    }
                    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
                }
                (wrapper, argv[2..].to_vec())
            } else {
                (PathBuf::from(argv[0].as_str().unwrap()), argv[1..].to_vec())
            };
        let release = executable.parent().unwrap();
        let mut entrypoint = vec![json!(executable.file_name().unwrap().to_str().unwrap())];
        entrypoint.extend(arguments);
        let mut connection = installation["connection"].clone();
        let development = installation["execution_mode"] == "development";
        if !development && installation["real_identity"] != true {
            connection["launcher"] = json!([
                "/usr/bin/python3",
                launcher,
                serde_json::to_string(&installation["connection"]["launcher"]).unwrap()
            ]);
        }
        contexts.push(json!({"id":"test-context","definition":{
            "execution_mode": if development {"development"} else {"managed"},
            "installation_roots":[root.parent().unwrap()],"work_root":installation["work_root"],
            "driver":installation["driver"],"connection":connection,
            "shared_workspace":installation["shared_workspace"],"max_outstanding_assignments":64
        }}));
        publications.push(json!({"publication_id":"test-publication","context_id":"test-context","generation":1,"requirements":{"driver":installation["driver"]["kind"],"capabilities":["json_inputs","json_outputs"],"wrapper_protocol_version":installation["installation"]["installation"]["wrapper_protocol_version"]},"descriptor":{
            "provider_binding_id":installation["provider_binding_id"],"installation":installation["installation"]["installation"],
            "release_path":release,"entrypoint":entrypoint,
            "environment":installation["installation"]["environment"],
            "slurm_script":installation["slurm_script"].as_str().map(|script|Path::new(script).file_name().unwrap().to_str().unwrap())
        }}));
    }
    json!({"generation":1,"contexts":contexts,"publications":publications,"run_payload_namespace_reserved":true})
}
