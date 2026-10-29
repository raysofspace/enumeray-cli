//! Runs the packaged credential-free process runner, with a real external model.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[test]
fn remote_runner_executes_once_and_preserves_completion() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-compute-runner-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let job = scalar_job(&root, intent);
    let output = call("prepare", &directory, Some(&job));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = call(
        "request-cancel",
        &directory,
        Some(&json!({"scope":"pending"})),
    );
    assert_eq!(payload(response)["cancellation"]["outcome"], "unsupported");
    let output = call("start", &directory, None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!call("start", &directory, None).status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = call("status", &directory, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        if status["status"] == "completed" {
            assert_eq!(status["completion"]["report"]["outputs"]["doubled"], 6);
            break;
        }
        assert!(Instant::now() < deadline, "runner did not complete");
        std::thread::sleep(Duration::from_millis(30));
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn accepted_process_waits_for_worker_start_without_becoming_unresolved_or_relaunching() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-delayed-worker-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let job = scalar_job(&root, intent);
    assert_eq!(
        payload(call("prepare", &directory, Some(&job)))["status"],
        "prepared"
    );
    // Hold the launch between successful spawn and the worker's first marker.
    // An intent alone still leaves submission unresolved.
    for name in ["start-attempt.json", "launch-intent.json"] {
        let path = directory.join(name);
        fs::write(
            &path,
            serde_json::to_vec(&json!({"intent":intent})).unwrap(),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(
        payload(call("status", &directory, None))["status"],
        "unresolved"
    );
    let outcome = directory.join("launch-outcome.json");
    fs::write(&outcome, br#"{"outcome":"accepted"}"#).unwrap();
    fs::set_permissions(outcome, fs::Permissions::from_mode(0o600)).unwrap();
    for _ in 0..2 {
        assert_eq!(
            payload(call("status", &directory, None))["status"],
            "pending"
        );
        assert!(!call("start", &directory, None).status.success());
        assert!(!directory.join("started").exists());
        assert!(!directory.join("workspace").exists());
    }
    let worker = Command::new(env!("CARGO_BIN_EXE_enumeray"))
        .args(["local-worker", "--directory"])
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        worker.status.success(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );
    let completed = payload(call("status", &directory, None));
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["completion"]["report"]["outputs"]["doubled"], 6);
    assert_eq!(payload(call("status", &directory, None)), completed);
    assert!(!call("start", &directory, None).status.success());
    fs::remove_dir_all(root).unwrap();
}

fn scalar_job(root: &Path, intent: Uuid) -> Value {
    let model: Value = serde_json::to_value(
        enumeray_contracts::models::parse_publication(include_bytes!(
            "../../../examples/local-scalar-model.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let binding: Value = serde_json::from_str(include_str!(
        "../../../examples/local-scalar-installation.json"
    ))
    .unwrap();
    let deployment: Value = serde_json::from_str(include_str!(
        "../../../examples/local-scalar-requirements.json"
    ))
    .unwrap();
    let mut assignment = json!({"assignment_id":Uuid::new_v4(),"run_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),"model_id":"scalar-double","publication_id":"test-publication","context_id":"test-context","definition":model["definition"],"descriptor":binding,"requirements":deployment,"inputs":{"value":3}});
    assignment["requirements"]["driver"] = json!("process");
    let wrapper =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/scalar_model/wrapper.py");
    json!({"version":1,"intent":intent,"assignment":assignment,"execution":{"installation":{"provider_binding_id":"test","installation":binding["installation"],"argv":["/usr/bin/python3",wrapper,"execute"],"environment":{}},"work_root":root,"driver":{"kind":"process"}}})
}

fn cleanup_proof(job: &Value, completion: &Value) -> Value {
    let report = &completion["report"];
    json!({"terminal_result":{"receipt":{"assignment_id":job["assignment"]["assignment_id"],
        "run_id":job["assignment"]["run_id"],"attempt_id":job["assignment"]["attempt_id"],
        "outcome":report["outcome"],"completed_at":completion["observed_at"],"provider_cleanup_acknowledged":true},
        "outputs":report["outputs"],"files":report["files"],"optional_output_failures":report["optional_output_failures"],"failure":report["failure"]},
        "live_logs_delivered":true,"archive_status":null,
        "protection":{"version":1,"generation":1,"run_payload_namespace_reserved":true,"cache_sha256":null,"references":[]}})
}

#[cfg(unix)]
#[test]
fn installation_candidates_and_cached_aliases_pin_payloads_before_readiness_or_retirement() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "enumeray-runner-installation-pins-{}",
        Uuid::new_v4()
    ));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let job = scalar_job(&root, intent);
    assert_eq!(
        payload(call("prepare", &directory, Some(&job)))["status"],
        "prepared"
    );
    assert!(call("start", &directory, None).status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    let completion = loop {
        let reply = payload(call("status", &directory, None));
        if reply["status"] == "completed"
            && payload(call("allocation-status", &directory, None))["status"] == "completed"
        {
            break reply["completion"].clone();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let release = directory.join("workspace/candidate");
    fs::create_dir(&release).unwrap();
    fs::write(release.join("wrapper"), b"not executable yet").unwrap();
    let validation = json!({"installation_roots":[root],"driver":{"kind":"process"},"descriptor":{
        "provider_binding_id":"unready-candidate","installation":job["execution"]["installation"]["installation"],
        "release_path":release,"entrypoint":["wrapper"],"environment":{}}});
    assert!(!call("validate", &root, Some(&validation)).status.success());
    let first: Value =
        serde_json::from_slice(&fs::read(directory.join("release-pins.json")).unwrap()).unwrap();
    assert_eq!(first["references"].as_array().unwrap().len(), 1);
    assert!(!directory.join("cleanup.json").exists());

    let cached = directory.join("workspace/cached-release");
    fs::create_dir(&cached).unwrap();
    fs::write(cached.join("wrapper"), b"retired cached software").unwrap();
    let alias = root.join("old-release-alias");
    symlink(&cached, &alias).unwrap();
    let seed = json!({"references":[{"release_path":alias,"files":[alias.join("wrapper")]}]});
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("payload.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(
        !call("protect-installations", &root, Some(&seed))
            .status
            .success()
    );
    drop(lock);
    assert_eq!(
        payload(call("protect-installations", &root, Some(&seed)))["status"],
        "completed"
    );
    let external = root.join("external-release");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("wrapper"), b"external installed software").unwrap();
    let installed_alias = directory.join("workspace/installed-alias");
    symlink(&external, &installed_alias).unwrap();
    let ancestor_alias = root.join("alternate-work-root");
    symlink(&root, &ancestor_alias).unwrap();
    let alternate_installed_alias = ancestor_alias
        .join(intent.to_string())
        .join("workspace/installed-alias");
    let outgoing = json!({"references":[{"release_path":alternate_installed_alias,"files":[alternate_installed_alias.join("wrapper")]}]});
    assert_eq!(
        payload(call("protect-installations", &root, Some(&outgoing)))["status"],
        "completed"
    );
    let pins = fs::read(directory.join("release-pins.json")).unwrap();
    let references: Value = serde_json::from_slice(&pins).unwrap();
    assert!(
        references["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["release_path"] == json!(installed_alias))
    );
    assert!(
        references["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["release_path"] == json!(cached))
    );
    assert!(
        !call(
            "protect-installations",
            &root,
            Some(&json!({"references":[{"release_path":"relative","files":[]}]}))
        )
        .status
        .success()
    );
    assert!(
        !call(
            "protect-installations",
            &root,
            Some(&json!({"references":vec![seed["references"][0].clone();129]}))
        )
        .status
        .success()
    );
    let proof = cleanup_proof(&job, &completion);
    assert!(
        !call("cleanup-run", &directory, Some(&proof))
            .status
            .success()
    );
    assert_eq!(fs::read(directory.join("release-pins.json")).unwrap(), pins);
    assert!(!directory.join("cleanup.json").exists());
    assert_eq!(
        fs::read(cached.join("wrapper")).unwrap(),
        b"retired cached software"
    );
    assert_eq!(
        fs::read(release.join("wrapper")).unwrap(),
        b"not executable yet"
    );
    assert_eq!(
        fs::read(installed_alias.join("wrapper")).unwrap(),
        b"external installed software"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_payload_cleanup_is_bounded_restartable_and_replay_fenced() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-runner-cleanup-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let job = scalar_job(&root, intent);
    assert_eq!(
        payload(call("prepare", &directory, Some(&job)))["status"],
        "prepared"
    );
    assert!(call("start", &directory, None).status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    let completion = loop {
        let observed = payload(call("status", &directory, None));
        if observed["status"] == "completed"
            && payload(call("allocation-status", &directory, None))["status"] == "completed"
        {
            break observed["completion"].clone();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut proof = cleanup_proof(&job, &completion);
    proof["protection"]["references"] = json!(
        (0..1024)
            .map(|i| {
                let release = root.join(format!("unrelated-release-{i}"));
                json!({"release_path":release,"files":[release.join("wrapper")]})
            })
            .collect::<Vec<_>>()
    );
    let workspace = directory.join("workspace");
    for i in 0..600 {
        fs::write(workspace.join(format!("scratch-{i}")), vec![8; 16]).unwrap();
    }
    let mut missing = proof.clone();
    missing["terminal_result"]["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("provider_cleanup_acknowledged");
    assert!(
        !call("cleanup-run", &directory, Some(&missing))
            .status
            .success()
    );
    assert!(!directory.join("cleanup.json").exists());
    let reader = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("payload.lock"))
        .unwrap();
    reader.lock_shared().unwrap();
    assert!(
        !call("cleanup-run", &directory, Some(&proof))
            .status
            .success()
    );
    assert!(!directory.join("cleanup.json").exists());
    drop(reader);
    let evidence: Vec<_> = [
        "remote.json",
        "job.json",
        "completion.json",
        "start-attempt.json",
        "worker-attempt.json",
        "started",
    ]
    .into_iter()
    .map(|name| (name, fs::read(directory.join(name)).unwrap()))
    .collect();
    // Discard the first reply, then invoke a fresh process for every retry.
    let _lost = call("cleanup-run", &directory, Some(&proof));
    let first: Value =
        serde_json::from_slice(&fs::read(directory.join("cleanup.json")).unwrap()).unwrap();
    assert_eq!(first["complete"], false);
    assert!(first["removed_entries"].as_u64().unwrap() <= 256);
    assert!(first["request"].get("protection").is_none());
    assert!(serde_json::to_vec(&first).unwrap().len() < 16 * 1024);

    // Resume a legacy checkpoint containing the whole catalogue. A different
    // receipt must still be rejected without changing the retained authority.
    let mut legacy = first.clone();
    legacy["request"]["protection"] = proof["protection"].clone();
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(directory.join("cleanup.json"), &legacy_bytes).unwrap();
    let mut changed = proof.clone();
    changed["terminal_result"]["receipt"]["completed_at"] = json!("2001-01-01T00:00:00Z");
    assert!(
        !call("cleanup-run", &directory, Some(&changed))
            .status
            .success()
    );
    assert_eq!(
        fs::read(directory.join("cleanup.json")).unwrap(),
        legacy_bytes
    );
    let resumed = payload(call("cleanup-run", &directory, Some(&proof)));
    assert_eq!(resumed["status"], "pending");
    let compacted: Value =
        serde_json::from_slice(&fs::read(directory.join("cleanup.json")).unwrap()).unwrap();
    assert!(compacted["request"].get("protection").is_none());
    assert!(serde_json::to_vec(&compacted).unwrap().len() < 16 * 1024);
    for _ in 0..8 {
        if payload(call("cleanup-run", &directory, Some(&proof)))["status"] == "completed" {
            break;
        }
    }
    let retired: Value =
        serde_json::from_slice(&fs::read(directory.join("cleanup.json")).unwrap()).unwrap();
    assert_eq!(retired["complete"], true);
    assert!(retired["request"].get("protection").is_none());
    assert!(!workspace.exists());
    assert!(!directory.join("inputs").exists());
    for (name, bytes) in evidence {
        assert_eq!(fs::read(directory.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(
        payload(call("status", &directory, None))["completion"],
        completion
    );
    assert!(!call("prepare", &directory, Some(&job)).status.success());
    assert!(!call("start", &directory, None).status.success());
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_enumeray"))
            .args(["local-worker", "--directory"])
            .arg(&directory)
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut changed = proof.clone();
    changed["terminal_result"]["receipt"]["completed_at"] = json!("2001-01-01T00:00:00Z");
    assert!(
        !call("cleanup-run", &directory, Some(&changed))
            .status
            .success()
    );
    assert_eq!(
        payload(call("cleanup-run", &directory, Some(&proof)))["status"],
        "completed"
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
struct EnvironmentFixture {
    root: std::path::PathBuf,
    directory: std::path::PathBuf,
    job: Value,
}

#[cfg(unix)]
impl EnvironmentFixture {
    fn new(driver: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("enumeray-environment-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let intent = Uuid::new_v4();
        let directory = root.join(intent.to_string());
        let mut job = scalar_job(&root, intent);
        if driver == "slurm" {
            let script = root.join("model.sbatch");
            fs::write(
                &script,
                "#!/bin/sh\nexec \"$1\" local-worker --directory \"$2\"\n",
            )
            .unwrap();
            job["execution"]["driver"] =
                json!({"kind":"slurm","worker_path":env!("CARGO_BIN_EXE_enumeray")});
            job["execution"]["slurm_script"] = json!(script);
            job["assignment"]["requirements"]["driver"] = json!("slurm");
        }
        Self {
            root,
            directory,
            job,
        }
    }

    fn prepare(&self) -> Value {
        payload(call("prepare", &self.directory, Some(&self.job)))
    }

    fn record(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.directory.join(name)).unwrap()).unwrap()
    }

    fn replace(&self, name: &str, value: &Value) {
        fs::write(
            self.directory.join(name),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    fn worker(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
        command
            .args(["local-worker", "--directory"])
            .arg(&self.directory);
        command
    }
}

#[cfg(unix)]
impl Drop for EnvironmentFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn configured_policy() -> Value {
    json!({"max_bytes":8 * 1024 * 1024,"max_entries":10000,"max_depth":32,"scan_interval_ms":1000,"cache":"per_run","symlinks":"workspace_only"})
}
fn wait_for_completion(fixture: &EnvironmentFixture) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let reply = payload(call("status", &fixture.directory, None));
        if reply["status"] == "completed" {
            return reply["completion"].clone();
        }
        assert!(Instant::now() < deadline, "runner did not complete");
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[cfg(unix)]
#[test]
fn process_scratch_snapshot_recovers_and_large_linked_workspaces_clean_up() {
    let mut fixture = EnvironmentFixture::new("process");
    let script = fixture.root.join("scratch.py");
    fs::write(&script, "import json,os\nfrom pathlib import Path\nPath('intermediate').write_bytes(b'x' * (4 * 1024 * 1024))\nfor i in range(1000): Path(str(i)).touch()\nPath('deep/' + '/'.join(['d'] * 12)).mkdir(parents=True)\nPath('alias').symlink_to('intermediate')\nPath(os.environ['XDG_CACHE_HOME'], 'cached').write_bytes(b'cache')\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':6},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n").unwrap();
    fixture.job["execution"]["installation"]["argv"] = json!(["/usr/bin/python3", script]);
    fixture.job["execution"]["driver"]["scratch"] = configured_policy();
    assert_eq!(fixture.prepare()["status"], "prepared");
    assert_eq!(fixture.record("job.json")["scratch"], configured_policy());
    let prepared = fs::read(fixture.directory.join("remote.json")).unwrap();
    let mut changed = fixture.job.clone();
    changed["execution"]["driver"]["scratch"]["max_bytes"] = json!(16 * 1024 * 1024);
    assert!(
        !call("prepare", &fixture.directory, Some(&changed))
            .status
            .success()
    );
    assert_eq!(
        fs::read(fixture.directory.join("remote.json")).unwrap(),
        prepared
    );
    assert!(call("start", &fixture.directory, None).status.success());
    let completion = wait_for_completion(&fixture);
    assert_eq!(completion["report"]["outcome"], "succeeded");
    assert_eq!(wait_for_completion(&fixture), completion);
    assert!(!call("start", &fixture.directory, None).status.success());
    let proof = cleanup_proof(&fixture.job, &completion);
    let mut completed = false;
    let mut removed = 0;
    for _ in 0..10 {
        let reply = payload(call("cleanup-run", &fixture.directory, Some(&proof)));
        let progress = fixture.record("cleanup.json");
        let total = progress["removed_entries"].as_u64().unwrap();
        assert!(total - removed <= 256);
        removed = total;
        if reply["status"] == "completed" {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert!(!fixture.directory.join("workspace").exists());
    assert_eq!(wait_for_completion(&fixture), completion);
    assert!(
        !call("prepare", &fixture.directory, Some(&fixture.job))
            .status
            .success()
    );
    assert!(!call("start", &fixture.directory, None).status.success());
}
#[cfg(unix)]
#[test]
fn legacy_process_environment_replays_while_configured_paths_are_rechecked() {
    let mut legacy = EnvironmentFixture::new("process");
    let script = legacy.root.join("legacy-env.py");
    fs::write(&script, "import json,os\nfrom pathlib import Path\nfor key in ['TMPDIR','TMP','TEMP','XDG_CACHE_HOME']: assert os.environ[key] == '/site/' + key\nr=json.loads(Path('request.json').read_text())\nfor key in ['inputs','model_id','publication_id','input_files']: r.pop(key,None)\nr.update(outcome='succeeded',outputs={'doubled':6},failure=None,files={})\nPath('result.json').write_text(json.dumps(r))\n").unwrap();
    legacy.job["execution"]["installation"]["argv"] = json!(["/usr/bin/python3", script]);
    let environment = json!({"TMPDIR":"/site/TMPDIR","TMP":"/site/TMP","TEMP":"/site/TEMP","XDG_CACHE_HOME":"/site/XDG_CACHE_HOME"});
    legacy.job["execution"]["installation"]["environment"] = environment.clone();
    legacy.job["assignment"]["descriptor"]["environment"] = environment;
    assert_eq!(legacy.prepare()["status"], "prepared");
    assert!(call("start", &legacy.directory, None).status.success());
    let completion = wait_for_completion(&legacy);
    assert_eq!(completion["report"]["outcome"], "succeeded");
    assert_eq!(wait_for_completion(&legacy), completion);
    assert!(!legacy.directory.join("workspace/.enumeray").exists());
    for key in ["TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME"] {
        for source in ["execution", "descriptor"] {
            let mut prepared = EnvironmentFixture::new("process");
            prepared.job["execution"]["driver"]["scratch"] = configured_policy();
            assert_eq!(prepared.prepare()["status"], "prepared");
            let mut retained = prepared.record("job.json");
            if source == "execution" {
                retained["installation"]["environment"] = json!({key:"/site/tmp"});
            } else {
                retained["assignment"]["descriptor"]["environment"] = json!({key:"/site/tmp"});
            }
            prepared.replace("job.json", &retained);
            assert!(prepared.worker().output().unwrap().status.success());
            assert_eq!(
                prepared.record("startup-failure.json")["stage"],
                "worker_validation"
            );
            assert!(!prepared.directory.join("wrapper-started.json").exists());
            let mut before = EnvironmentFixture::new("process");
            before.job["execution"]["driver"]["scratch"] = configured_policy();
            if source == "execution" {
                before.job["execution"]["installation"]["environment"] = json!({key:"/site/tmp"});
            } else {
                before.job["assignment"]["descriptor"]["environment"] = json!({key:"/site/tmp"});
            }
            assert_eq!(before.prepare()["status"], "completed");
            assert_eq!(
                before.record("startup-failure.json")["stage"],
                "launch_preparation"
            );
            assert!(!before.directory.join("wrapper-started.json").exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn configured_large_workspace_remains_cancellable_during_scanning() {
    let mut fixture = EnvironmentFixture::new("process");
    let script = fixture.root.join("scratch.py");
    fs::write(&script, "import time\nfrom pathlib import Path\nfor i in range(2000): Path(str(i)).touch()\nPath('ready').touch()\ntime.sleep(60)\n").unwrap();
    fixture.job["execution"]["installation"]["argv"] = json!(["/usr/bin/python3", script]);
    fixture.job["execution"]["driver"]["scratch"] = configured_policy();
    assert_eq!(fixture.prepare()["status"], "prepared");
    assert!(call("start", &fixture.directory, None).status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fixture.directory.join("workspace/ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let response = payload(call(
        "request-cancel",
        &fixture.directory,
        Some(&json!({"scope":"running"})),
    ));
    assert_eq!(response["cancellation"]["outcome"], "accepted");
    let completion = wait_for_completion(&fixture);
    assert_eq!(completion["report"]["outcome"], "cancelled");
    assert_eq!(completion["report"]["execution_terminated"], true);
    assert_eq!(wait_for_completion(&fixture), completion);
    assert!(!call("start", &fixture.directory, None).status.success());
}

#[cfg(unix)]
#[test]
fn reserved_environment_fails_prepare_and_start_without_execution() {
    const VALUE: &str = "private-override-value";
    for driver in ["process", "slurm"] {
        for key in ["HOME", "SLURM_JOB_ID"] {
            for (boundary, source) in [
                ("prepare", "execution"),
                ("prepare", "descriptor"),
                ("prepare-existing", "execution"),
                ("prepare-existing", "descriptor"),
                ("start", "execution"),
                ("start", "descriptor"),
            ] {
                let mut fixture = EnvironmentFixture::new(driver);
                if boundary != "prepare" {
                    assert_eq!(fixture.prepare()["status"], "prepared");
                }
                if source == "execution" {
                    fixture.job["execution"]["installation"]["environment"] = json!({key:VALUE});
                } else {
                    fixture.job["assignment"]["descriptor"]["environment"] = json!({key:VALUE});
                }
                let failed = if boundary == "prepare" {
                    fixture.prepare()
                } else {
                    let mut retained = fixture.record("remote.json");
                    retained["execution"] = fixture.job["execution"].clone();
                    retained["assignment"] = fixture.job["assignment"].clone();
                    fixture.replace("remote.json", &retained);
                    if boundary == "prepare-existing" {
                        fixture.job = retained;
                        fixture.prepare()
                    } else {
                        payload(call("start", &fixture.directory, None))
                    }
                };
                assert_eq!(
                    failed["status"], "completed",
                    "{driver}/{boundary}/{source}/{key}"
                );
                assert_eq!(
                    failed["completion"]["report"]["failure"]["code"],
                    "launch_preparation_failed"
                );
                let diagnostic = failed["completion"]["report"]["failure"]["message"]
                    .as_str()
                    .unwrap();
                assert!(diagnostic.contains(key), "{diagnostic}");
                assert!(!diagnostic.contains(VALUE));
                assert!(!fixture.directory.join("launch-intent.json").exists());
                assert!(!fixture.directory.join("wrapper-started.json").exists());
                assert!(!fixture.directory.join("workspace").exists());
                assert_eq!(payload(call("status", &fixture.directory, None)), failed);
                assert!(!call("start", &fixture.directory, None).status.success());
                assert_eq!(
                    payload(call(
                        "request-cancel",
                        &fixture.directory,
                        Some(&json!({"scope":"running"}))
                    ))["cancellation"]["outcome"],
                    "already_terminal"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn reserved_environment_is_rechecked_by_the_compute_node_worker() {
    for driver in ["process", "slurm"] {
        for key in ["HOME", "SLURM_JOB_ID"] {
            for source in ["execution", "descriptor"] {
                let fixture = EnvironmentFixture::new(driver);
                assert_eq!(fixture.prepare()["status"], "prepared");
                let mut retained = fixture.record("job.json");
                if source == "execution" {
                    retained["installation"]["environment"] = json!({key:"private-override-value"});
                } else {
                    retained["assignment"]["descriptor"]["environment"] =
                        json!({key:"private-override-value"});
                }
                fixture.replace("job.json", &retained);
                assert_eq!(
                    fixture.worker().output().unwrap().status.success(),
                    driver == "process"
                );
                let evidence = fixture.record("startup-failure.json");
                assert_eq!(evidence["stage"], "worker_validation");
                let diagnostic = evidence["message"].as_str().unwrap();
                assert!(diagnostic.contains(key), "{diagnostic}");
                assert!(!diagnostic.contains("private-override-value"));
                assert!(!fixture.directory.join("wrapper-started.json").exists());
                assert!(!fixture.directory.join("workspace").exists());
                assert!(!fixture.worker().output().unwrap().status.success());
                assert_eq!(fixture.record("startup-failure.json"), evidence);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn runtime_environment_inheritance_preserves_batch_setup_and_runner_defaults() {
    let code = r#"import json, os
from pathlib import Path
assert Path(os.environ['HOME']).resolve() == Path.cwd()
assert os.environ['PYTHONDONTWRITEBYTECODE'] == '1'
assert os.environ['MODEL_MODE'] == 'descriptor'
assert os.environ['OMP_NUM_THREADS'] == '2'
if os.environ['EXPECT_INHERITANCE'] == '1':
    assert os.environ['PATH'] == '/batch/bin:/usr/bin:/bin'
    for key, value in {'SLURM_JOB_ID': '2468', 'PMI_RANK': '3', 'PMI_SIZE': '8',
                       'PMIX_RANK': '4', 'PMIX_NAMESPACE': 'batch-job',
                       'MODULEPATH': '/batch/modules', 'LD_LIBRARY_PATH': '/batch/libs',
                       'PYTHONPATH': '/batch/python', 'BATCH_SETUP': 'ready'}.items():
        assert os.environ.get(key) == value, (key, os.environ.get(key))
else:
    assert os.environ['PATH'] == '/usr/bin:/bin'
    for key in ['SLURM_JOB_ID', 'PMI_RANK', 'PMIX_RANK', 'BATCH_SETUP',
                'ENUMERAY_S3_ISOLATION_SENTINEL']:
        assert key not in os.environ, key
r = json.loads(Path('request.json').read_text())
for key in ['inputs', 'model_id', 'publication_id', 'input_files']:
    r.pop(key, None)
r.update(outcome='succeeded', outputs={'doubled': 6}, failure=None, files={})
Path('result.json').write_text(json.dumps(r))
print('environment checked', flush=True)
"#;
    for driver in ["process", "slurm"] {
        let mut fixture = EnvironmentFixture::new(driver);
        let wrapper = fixture.root.join("environment.py");
        fs::write(&wrapper, code).unwrap();
        fixture.job["execution"]["installation"]["argv"][1] = json!(wrapper);
        fixture.job["execution"]["installation"]["environment"] = json!({
            "MODEL_MODE":"descriptor", "OMP_NUM_THREADS":"2",
            "EXPECT_INHERITANCE":if driver == "slurm" { "1" } else { "0" }
        });
        assert_eq!(fixture.prepare()["status"], "prepared");
        let output = fixture
            .worker()
            .env_clear()
            .envs([
                ("PATH", "/batch/bin:/usr/bin:/bin"),
                ("HOME", "/batch/home"),
                ("PYTHONDONTWRITEBYTECODE", "0"),
                ("SLURM_JOB_ID", "2468"),
                ("PMI_RANK", "3"),
                ("PMI_SIZE", "8"),
                ("PMIX_RANK", "4"),
                ("PMIX_NAMESPACE", "batch-job"),
                ("MODULEPATH", "/batch/modules"),
                ("LD_LIBRARY_PATH", "/batch/libs"),
                ("PYTHONPATH", "/batch/python"),
                ("BATCH_SETUP", "ready"),
                ("OMP_NUM_THREADS", "8"),
            ])
            .envs((driver == "process").then_some(("ENUMERAY_S3_ISOLATION_SENTINEL", "private")))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let completion = fixture.record("completion.json");
        assert_eq!(completion["report"]["outcome"], "succeeded", "{completion}");
        assert_eq!(completion["report"]["outputs"]["doubled"], 6);
        let original = payload(call("status", &fixture.directory, None));
        let mut retained = fixture.record("remote.json");
        // Previously accepted installations must remain observable after this
        // validation policy changes, including their immutable result and logs.
        let historical = json!({"HOME":"/old/home","SLURM_JOB_ID":"old-allocation"});
        retained["execution"]["installation"]["environment"] = historical.clone();
        retained["assignment"]["descriptor"]["environment"] = historical;
        fixture.replace("remote.json", &retained);
        let mut worker = fixture.record("job.json");
        worker["assignment"] = retained["assignment"].clone();
        fixture.replace("job.json", &worker);
        assert_eq!(payload(call("status", &fixture.directory, None)), original);
        assert_eq!(
            payload(call(
                "read-logs",
                &fixture.directory,
                Some(&json!({"source":"stdout","epoch":1,"offset":0,"complete":true}))
            ))["data"],
            "ZW52aXJvbm1lbnQgY2hlY2tlZAo="
        );
        assert_eq!(
            payload(call(
                "request-cancel",
                &fixture.directory,
                Some(&json!({"scope":"running"}))
            ))["cancellation"]["outcome"],
            "already_terminal"
        );
    }
}

#[test]
fn runner_stages_verified_inputs_and_reads_only_declared_outputs() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-runner-files-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let artifact = Uuid::new_v4();
    let wrapper = root.join("wrapper.py");
    fs::write(
        &wrapper,
        r#"import json
from pathlib import Path
r = json.loads(Path('request.json').read_text())
value = int(Path(r['input_files']['dataset']['path']).read_text())
Path('answer.txt').write_text(str(value * 2))
for key in ['inputs', 'model_id', 'publication_id', 'input_files']:
    r.pop(key, None)
r.update(outcome='succeeded', outputs={}, failure=None, files={
    'result': {'path': 'answer.txt', 'media_type': 'text/plain', 'scientific_metadata': {}}
})
Path('result.json').write_text(json.dumps(r))
"#,
    )
    .unwrap();
    let mut job = scalar_job(&root, intent);
    let input_model: Value = serde_json::to_value(
        enumeray_contracts::models::parse_publication(include_bytes!(
            "../../../examples/artifact-input.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let output_model: Value = serde_json::to_value(
        enumeray_contracts::models::parse_publication(include_bytes!(
            "../../../examples/artifact-output.json"
        ))
        .unwrap(),
    )
    .unwrap();
    job["assignment"]["definition"]["input_schema"] =
        input_model["definition"]["input_schema"].clone();
    job["assignment"]["definition"]["output_schema"] =
        output_model["definition"]["output_schema"].clone();
    job["assignment"]["inputs"] = json!({"dataset":{"artifact_id":artifact}});
    job["execution"]["installation"]["argv"][1] = json!(wrapper);
    job["input_digests"] =
        json!({artifact.to_string(): [2, STANDARD.encode(Sha256::digest(b"42"))]});

    assert!(call("prepare", &directory, Some(&job)).status.success());
    let first = json!({"artifact_id":artifact,"offset":0,"data":STANDARD.encode(b"4")});
    assert!(
        call("write-input", &directory, Some(&first))
            .status
            .success()
    );
    let probe = json!({"target":{"kind":"input","artifact_id":artifact},"offset":0,"length":1});
    let proof = payload(call("transfer-probe", &directory, Some(&probe)));
    assert_eq!(proof["transfer"]["retained_bytes"], 1);
    assert_eq!(
        proof["transfer"]["range_sha256"],
        STANDARD.encode(Sha256::digest(b"4"))
    );
    assert!(!call("transfer-probe", &directory, Some(&json!({"target":{"kind":"input","artifact_id":Uuid::new_v4()},"offset":0,"length":0}))).status.success());
    assert!(
        !call(
            "write-input",
            &directory,
            Some(&json!({"artifact_id":artifact,"offset":0,"data":STANDARD.encode(b"x")}))
        )
        .status
        .success()
    );
    // Staging can resume before the first start attempt. A separate regression
    // verifies that an early start seals an incomplete launch permanently.
    let second = json!({"artifact_id":artifact,"offset":1,"data":STANDARD.encode(b"2")});
    assert!(
        call("write-input", &directory, Some(&second))
            .status
            .success()
    );
    assert!(
        call("write-input", &directory, Some(&first))
            .status
            .success()
    );
    assert_eq!(
        fs::read(directory.join("inputs").join(artifact.to_string())).unwrap(),
        b"42"
    );
    assert!(call("start", &directory, None).status.success());
    assert!(
        !call("transfer-probe", &directory, Some(&probe))
            .status
            .success()
    );
    assert!(
        !call("write-input", &directory, Some(&first))
            .status
            .success()
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = call("status", &directory, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        if status["status"] == "completed" {
            assert_eq!(status["completion"]["report"]["outcome"], "succeeded");
            assert_eq!(
                status["completion"]["files"]["result"]["path"],
                "answer.txt"
            );
            break;
        }
        assert!(Instant::now() < deadline, "runner did not complete");
        std::thread::sleep(Duration::from_millis(30));
    }
    for path in ["../wrapper.py", "request.json"] {
        assert!(
            !call(
                "read-output",
                &directory,
                Some(&json!({"path":path,"offset":0}))
            )
            .status
            .success()
        );
    }
    let retained_path = directory.join("remote.json");
    let mut historical: Value = serde_json::from_slice(&fs::read(&retained_path).unwrap()).unwrap();
    historical["execution"]["installation"]["environment"] = json!({"HOME":"/old/home"});
    historical["assignment"]["descriptor"]["environment"] = json!({"HOME":"/old/home"});
    fs::write(&retained_path, serde_json::to_vec(&historical).unwrap()).unwrap();
    let worker_path = directory.join("job.json");
    let mut worker: Value = serde_json::from_slice(&fs::read(&worker_path).unwrap()).unwrap();
    worker["assignment"] = historical["assignment"].clone();
    fs::write(&worker_path, serde_json::to_vec(&worker).unwrap()).unwrap();
    for (offset, expected) in [(0, b"84".as_slice()), (1, b"4".as_slice())] {
        let output = call(
            "read-output",
            &directory,
            Some(&json!({"path":"answer.txt","offset":offset})),
        );
        assert!(output.status.success());
        let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            STANDARD.decode(payload["data"].as_str().unwrap()).unwrap(),
            expected
        );
    }
    for scope in ["pending", "running"] {
        assert_eq!(
            payload(call(
                "request-cancel",
                &directory,
                Some(&json!({"scope":scope}))
            ))["cancellation"]["outcome"],
            "already_terminal"
        );
    }
    assert!(!call("prepare", &directory, Some(&job)).status.success());
    fs::remove_dir_all(root).unwrap();
}

fn call(action: &str, directory: &Path, input: Option<&Value>) -> std::process::Output {
    call_with_scheduler(action, directory, input, None)
}

fn call_with_scheduler(
    action: &str,
    directory: &Path,
    input: Option<&Value>,
    scheduler: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    if let Some(scheduler) = scheduler {
        command
            .env("SLURM_CLUSTERS", "outside")
            .env("SBATCH_CLUSTERS", "outside");
        let path = std::env::var_os("PATH").unwrap_or_default();
        #[cfg(target_os = "macos")]
        let executables = {
            command.env("SLURM_CONF", scheduler);
            scheduler_executables()
        };
        #[cfg(not(target_os = "macos"))]
        let executables = scheduler;
        command.env(
            "PATH",
            std::env::join_paths(
                std::iter::once(executables.to_path_buf()).chain(std::env::split_paths(&path)),
            )
            .unwrap(),
        );
    }
    let mut child = command
        .args(["compute-runner", action, "--directory"])
        .arg(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&serde_json::to_vec(&json!({"version":1,"action":action,"payload":input.cloned().unwrap_or(Value::Null)})).unwrap()).unwrap();
    let mut output = child.wait_with_output().unwrap();
    if output.status.success() {
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["version"], 1);
        output.stdout = serde_json::to_vec(&value["payload"]).unwrap();
    }
    output
}

#[cfg(target_os = "macos")]
fn scheduler_executables() -> &'static Path {
    use std::{os::unix::fs::PermissionsExt, sync::LazyLock};
    static EXECUTABLES: LazyLock<std::path::PathBuf> = LazyLock::new(|| {
        // First execution of a new script can exceed the bounded scheduler call
        // on macOS. Keep that host startup outside the evidence/timeout tests.
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "enumeray-scheduler-executables-{}",
            std::process::id(),
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["squeue", "sacct", "scancel", "sbatch"] {
            let path = root.join(name);
            fs::write(
                &path,
                include_str!("../../connector/tests/fixtures/cancellation-scheduler.py"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                Command::new(&path)
                    .arg("--fixture-ready")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        root
    });
    &EXECUTABLES
}

fn payload(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

// Controlled scheduler replies exercise uncertainty and races without changing
// the test process's environment. The real-backends suite qualifies real Slurm.
#[cfg(unix)]
struct SchedulerFixture {
    root: std::path::PathBuf,
    directory: std::path::PathBuf,
    scheduler: std::path::PathBuf,
}
#[cfg(unix)]
impl SchedulerFixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Self::unlaunched(
            "#!/bin/sh\n#SBATCH --clusters=cluster-a\nexec \"$1\" local-worker --directory \"$2\"\n",
        );
        let job: Value =
            serde_json::from_slice(&fs::read(fixture.directory.join("remote.json")).unwrap())
                .unwrap();
        let script = job["execution"]["slurm_script"].as_str().unwrap();
        for (name, value) in [
            (
                "launch-intent.json",
                json!({"intent":job["intent"],"script_digest":job["execution"]["installation"]["file_digests"][script],"execution_uid":fixture.uid(),"prepared_at":1700000000}),
            ),
            ("slurm.json", json!({"job_id":123,"cluster":"cluster-a"})),
        ] {
            let path = fixture.directory.join(name);
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fixture
    }
    fn unlaunched(script: &str) -> Self {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("enumeray-slurm-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let intent = Uuid::new_v4();
        let directory = root.join(intent.to_string());
        let script_path = root.join("model.sbatch");
        fs::write(&script_path, script).unwrap();
        let mut job = scalar_job(&root, intent);
        job["execution"]["driver"] = json!({"kind":"slurm","worker_path":"/unused/worker"});
        job["execution"]["slurm_script"] = json!(script_path);
        job["execution"]["installation"]["file_digests"] = json!({script_path.to_str().unwrap():STANDARD.encode(Sha256::digest(script.as_bytes()))});
        job["assignment"]["requirements"]["driver"] = json!("slurm");
        payload(call("prepare", &directory, Some(&job)));
        let scheduler = root.join("scheduler");
        fs::create_dir(&scheduler).unwrap();
        for name in ["squeue", "sacct", "scancel", "sbatch"] {
            let path = scheduler.join(name);
            fs::write(
                &path,
                include_str!("../../connector/tests/fixtures/cancellation-scheduler.py"),
            )
            .unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(scheduler.join("intent"), intent.to_string()).unwrap();
        fs::write(scheduler.join("state"), "PENDING").unwrap();
        fs::write(scheduler.join("mode"), "keep-pending").unwrap();
        fs::write(scheduler.join("recovery"), "missing").unwrap();
        fs::write(scheduler.join("reply"), "123;cluster-a").unwrap();
        Self {
            root,
            directory,
            scheduler,
        }
    }
    fn uid(&self) -> String {
        String::from_utf8(
            Command::new("/usr/bin/id")
                .arg("-u")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    }
    fn output(&self, action: &str, input: Option<&Value>) -> std::process::Output {
        call_with_scheduler(action, &self.directory, input, Some(&self.scheduler))
    }
    fn record(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.directory.join(name)).unwrap()).unwrap()
    }
    fn lose_receipt(&self) {
        for name in ["slurm.json", "slurm-receipt.json"] {
            fs::remove_file(self.directory.join(name)).unwrap();
        }
    }
    fn call(&self, action: &str, input: Option<&Value>) -> Value {
        payload(call_with_scheduler(
            action,
            &self.directory,
            input,
            Some(&self.scheduler),
        ))
    }
    fn cancel(&self, scope: &str) -> Value {
        let response = self.call("request-cancel", Some(&json!({"scope":scope})));
        assert!(response.get("status").is_none());
        assert!(response.get("completion").is_none());
        response["cancellation"].clone()
    }
    fn set(&self, name: &str, value: &str) {
        fs::write(self.scheduler.join(name), value).unwrap();
    }
    fn calls(&self, name: &str) -> Vec<Value> {
        fs::read_to_string(self.scheduler.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["command"] == name)
            .collect()
    }
    fn accounting_row(&self, id: &str, state: &str, exit: &str) -> String {
        let intent = self.record("remote.json")["intent"]
            .as_str()
            .unwrap()
            .to_owned();
        let name = if id == "123" {
            format!("enumeray-{intent}")
        } else {
            "batch".into()
        };
        let now = time::OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        format!(
            "{id}|{name}|{}|{state}|{exit}|Unknown|{}",
            self.uid(),
            now.trim_end_matches('Z')
        )
    }
    fn write_record(&self, name: &str, value: &Value) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.directory.join(name);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn stopped_worker(&self) {
        self.write_record("started", &json!(true));
        self.write_record("worker.lock", &json!(true));
    }
}
#[cfg(unix)]
impl Drop for SchedulerFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn slurm_observation_does_not_cancel_and_acceptance_needs_observation() {
    let fixture = SchedulerFixture::new();
    for _ in 0..2 {
        assert_eq!(fixture.call("status", None)["status"], "pending");
    }
    assert!(fixture.calls("scancel").is_empty());
    for _ in 0..2 {
        assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
        assert_eq!(fixture.call("status", None)["status"], "pending");
        assert!(!fixture.directory.join("completion.json").exists());
    }
    let calls = fixture.calls("scancel");
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(
            call["args"],
            json!([
                "--ctld",
                "--quiet",
                "--state=PENDING",
                "123",
                "--clusters",
                "cluster-a"
            ])
        );
    }
    fixture.set("state", "COMPLETED");
    assert_eq!(fixture.cancel("pending")["outcome"], "already_terminal");
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    assert!(!fixture.directory.join("completion.json").exists());
    fixture.set("state", "CANCELLED");
    assert_eq!(fixture.cancel("pending")["outcome"], "already_terminal");
    assert!(!fixture.directory.join("completion.json").exists());
    let done = fixture.call("status", None);
    assert_eq!(
        done["completion"]["report"]["failure"]["code"],
        "access_revoked"
    );
    assert_eq!(fixture.call("status", None), done);
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn historical_reserved_environment_keeps_slurm_observation_and_cancellation_available() {
    let fixture = SchedulerFixture::new();
    let mut retained = fixture.record("remote.json");
    let historical = json!({"HOME":"/old/home","SLURM_JOB_ID":"old-allocation"});
    retained["execution"]["installation"]["environment"] = historical.clone();
    retained["assignment"]["descriptor"]["environment"] = historical;
    fixture.write_record("remote.json", &retained);
    let mut worker = fixture.record("job.json");
    worker["assignment"] = retained["assignment"].clone();
    fixture.write_record("job.json", &worker);
    assert_eq!(fixture.call("status", None)["status"], "pending");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    fixture.set("state", "CANCELLED");
    let cancelled = fixture.call("status", None);
    assert_eq!(cancelled["status"], "completed");
    assert_eq!(
        cancelled["completion"]["report"]["failure"]["code"],
        "access_revoked"
    );
    assert_eq!(fixture.call("status", None), cancelled);
    assert_eq!(fixture.cancel("pending")["outcome"], "already_terminal");
    assert!(!fixture.output("start", None).status.success());
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn lost_cancellation_reply_retries_the_persisted_handle_during_accounting_lag() {
    let fixture = SchedulerFixture::new();
    fixture.set("mode", "lost-reply");
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    assert!(!fixture.directory.join("completion.json").exists());
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert_eq!(fixture.calls("scancel").len(), 2);
    for command in ["squeue", "sacct", "scancel"] {
        for call in fixture.calls(command) {
            assert!(
                call["args"]
                    .as_array()
                    .unwrap()
                    .windows(2)
                    .any(|pair| pair == [json!("--clusters"), json!("cluster-a")])
            );
        }
    }
    fixture.set("state", "CANCELLED");
    assert_eq!(fixture.call("status", None)["status"], "completed");
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn pending_cancellation_racing_with_start_preserves_the_drain_policy() {
    let fixture = SchedulerFixture::new();
    fixture.set("mode", "start-race");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert_eq!(fixture.call("status", None)["status"], "running");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert_eq!(fixture.call("status", None)["status"], "running");
    assert!(!fixture.directory.join("completion.json").exists());
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn stopped_worker_without_completion_leaves_cancellation_unresolved() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = SchedulerFixture::new();
    for name in ["started", "worker.lock"] {
        let path = fixture.directory.join(name);
        fs::write(&path, b"").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert!(!fixture.directory.join("completion.json").exists());
    assert!(fixture.calls("scancel").is_empty());
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn cancellation_without_unambiguous_identity_never_targets_or_submits_a_job() {
    let fixture = SchedulerFixture::new();
    fs::remove_file(fixture.directory.join("slurm.json")).unwrap();
    for evidence in ["missing", "ambiguous"] {
        fixture.set("recovery", evidence);
        assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
        assert_eq!(fixture.call("status", None)["status"], "unresolved");
        assert!(!fixture.directory.join("slurm.json").exists());
    }
    assert!(fixture.calls("scancel").is_empty());
    assert!(fixture.calls("sbatch").is_empty());
    fixture.set("recovery", "unique");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    let handle: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("slurm.json")).unwrap()).unwrap();
    assert_eq!(handle["job_id"], 123);
    assert_eq!(handle["cluster"], "cluster-a");
    assert_eq!(fixture.calls("scancel").len(), 1);
}

#[cfg(unix)]
#[test]
fn scheduler_cluster_is_durable_after_submission_and_lost_receipt() {
    let fixture =
        SchedulerFixture::unlaunched("#!/bin/sh\n#SBATCH --clusters=cluster-b\nexec worker\n");
    fixture.set("reply", "123;cluster-b");
    fixture.set("expected-cancel-cluster", "cluster-b");
    assert_eq!(fixture.call("start", None)["status"], "pending");
    let intent = fixture.record("launch-intent.json");
    assert!(intent.get("clusters").is_none());
    assert!(intent["script_digest"].is_string());
    assert_eq!(intent["execution_uid"], fixture.uid());
    assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-b");
    fixture.lose_receipt();
    fixture.set("recovery-cluster-b", "unique");
    // Recovery uses scheduler evidence even when the installed file has disappeared.
    fs::remove_file(fixture.root.join("model.sbatch")).unwrap();
    assert_eq!(fixture.call("status", None)["status"], "pending");
    assert_eq!(
        fixture.record("slurm.json"),
        json!({"job_id":123,"cluster":"cluster-b"})
    );
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert!(!fixture.output("start", None).status.success());
    assert_eq!(fixture.calls("sbatch").len(), 1);
    let submitted = fixture.calls("sbatch");
    assert!(
        !submitted[0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--clusters")
    );
    for command in ["squeue", "sacct", "scancel"] {
        for call in fixture.calls(command) {
            let args = call["args"].as_array().unwrap();
            let recovery = args.iter().any(|arg| arg == "--name");
            let cluster = if recovery { "all" } else { "cluster-b" };
            assert!(
                args.windows(2)
                    .any(|pair| pair == [json!("--clusters"), json!(cluster)])
            );
            assert!(call["routing_environment"].is_null());
        }
    }
}

#[cfg(unix)]
#[test]
fn lost_submission_reply_never_permits_another_launch() {
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\n#SBATCH -M cluster-b\nexec worker\n");
    fixture.set("mode", "lost-submit-reply");
    assert_eq!(fixture.call("start", None)["status"], "unresolved");
    assert_eq!(
        fixture.record("launch-outcome.json")["outcome"],
        "uncertain"
    );
    assert!(fixture.directory.join("launch-intent.json").exists());
    fixture.set("recovery-cluster-b", "unique");
    assert_eq!(fixture.call("status", None)["status"], "pending");
    assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-b");
    assert!(!fixture.output("start", None).status.success());
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn equal_job_numbers_on_different_clusters_remain_ambiguous() {
    let fixture = SchedulerFixture::unlaunched(
        "#!/bin/sh\n#SBATCH --clusters=cluster-a,cluster-b\nexec worker\n",
    );
    fixture.call("start", None);
    fixture.lose_receipt();
    fixture.set("recovery-cluster-a", "unique");
    fixture.set("recovery-cluster-b", "unique");
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert!(!fixture.directory.join("slurm.json").exists());
    assert!(!fixture.output("start", None).status.success());
    assert!(fixture.calls("scancel").is_empty());
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn complete_scheduler_searches_are_required_before_cluster_selection() {
    let fixture = SchedulerFixture::unlaunched(
        "#!/bin/sh\n#SBATCH --clusters=cluster-a,cluster-b\nexec worker\n",
    );
    fixture.call("start", None);
    fixture.lose_receipt();
    fixture.set("recovery-cluster-a", "unique");
    fixture.set("recovery-cluster-b", "unavailable");
    assert!(!fixture.output("status", None).status.success());
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert!(!fixture.directory.join("slurm.json").exists());
    fixture.set("recovery-cluster-a", "missing");
    fixture.set("recovery-cluster-b", "unique");
    fixture.set("expected-cancel-cluster", "cluster-b");
    assert_eq!(fixture.call("status", None)["status"], "pending");
    assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-b");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert_eq!(fixture.calls("scancel").len(), 1);
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn missing_cluster_receipts_require_scheduler_evidence() {
    for script in [
        "#!/bin/sh\nexec worker\n",
        "#!/bin/sh\n#SBATCH --clusters=cluster-a,cluster-b\nexec worker\n",
    ] {
        let fixture = SchedulerFixture::unlaunched(script);
        fixture.set("reply", "123");
        assert_eq!(fixture.call("start", None)["status"], "unresolved");
        assert!(!fixture.directory.join("slurm.json").exists());
        fixture.set("recovery-cluster-b", "accounting-only");
        assert_eq!(fixture.call("status", None)["status"], "pending");
        assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-b");
        assert_eq!(fixture.calls("sbatch").len(), 1);
    }
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
    fixture.set("reply", "123");
    fixture.set("recovery-cluster-a", "queue-only");
    assert_eq!(fixture.call("start", None)["status"], "pending");
    assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-a");
}

#[cfg(unix)]
#[test]
fn scheduler_receipt_supplies_the_actual_cluster_without_a_connector_allowlist() {
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\n#SBATCH --clusters=all\nexec worker\n");
    fixture.set("reply", "123;model-cluster");
    fixture.set("expected-cancel-cluster", "model-cluster");
    assert_eq!(fixture.call("start", None)["status"], "pending");
    assert_eq!(fixture.record("slurm.json")["cluster"], "model-cluster");
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn contradictory_receipts_preserve_uncertainty() {
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
    fixture.set("reply", "456");
    assert_eq!(fixture.call("start", None)["status"], "unresolved");
    fixture.set("recovery-cluster-b", "unique");
    assert!(!fixture.output("status", None).status.success());
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert!(fixture.directory.join("slurm-receipt.json").exists());
    assert!(!fixture.directory.join("slurm.json").exists());
    assert!(!fixture.output("start", None).status.success());
    assert!(fixture.calls("scancel").is_empty());
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn recovery_checks_identity_and_uses_the_original_accounting_window() {
    let fixture = SchedulerFixture::new();
    fs::remove_file(fixture.directory.join("slurm.json")).unwrap();
    for evidence in [
        "wrong-user",
        "wrong-name",
        "malformed",
        "conflicting-clusters",
    ] {
        fixture.set("recovery", evidence);
        assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
        assert!(!fixture.directory.join("slurm.json").exists());
    }
    fixture.set("recovery", "accounting-only");
    assert_eq!(fixture.call("status", None)["status"], "pending");
    assert_eq!(fixture.record("slurm.json")["cluster"], "cluster-a");
    for call in fixture.calls("sacct") {
        if call["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--name")
        {
            assert!(
                call["args"]
                    .as_array()
                    .unwrap()
                    .windows(2)
                    .any(|pair| pair == [json!("--starttime"), json!("2023-11-14T22:08:20")])
            );
        }
    }
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn model_directives_are_passed_to_slurm_without_connector_policy() {
    for directive in [
        "--clusters=model-cluster",
        "--clusters=all",
        "--clusters=cluster-a,cluster-a",
        "--clusters=cluster-a\n#SBATCH -M cluster-b",
        "--clust=cluster-b",
        "--cluster-constraint=x",
        "hetjob",
        "--account=model --partition=science --qos=long --nodes=8 --ntasks=32 --cpus-per-task=4 --mem-per-cpu=4G --time=14-00:00:00",
    ] {
        let script = format!("#!/bin/sh\n#SBATCH {directive}\nexec worker\n");
        let fixture = SchedulerFixture::unlaunched(&script);
        assert_eq!(fixture.call("start", None)["status"], "pending");
        let submissions = fixture.calls("sbatch");
        assert_eq!(submissions.len(), 1);
        let args = submissions[0]["args"].as_array().unwrap();
        assert!(!args.iter().filter_map(Value::as_str).any(|arg| {
            arg.starts_with("--clusters")
                || arg.starts_with("--account")
                || arg.starts_with("--partition")
                || arg.starts_with("--qos")
                || arg.starts_with("--nodes")
                || arg.starts_with("--ntasks")
                || arg.starts_with("--cpus")
                || arg.starts_with("--mem")
                || arg.starts_with("--time")
        }));
        assert_eq!(
            fs::read_to_string(fixture.root.join("model.sbatch")).unwrap(),
            script
        );
    }
    let fixture =
        SchedulerFixture::unlaunched("#!/bin/sh\n#SBATCH --partition=invalid\nexec worker\n");
    fixture.set("mode", "reject-submit");
    let failed = fixture.call("start", None);
    assert_eq!(failed["status"], "completed");
    assert_eq!(
        failed["completion"]["report"]["failure"]["code"],
        "launch_rejected"
    );
    assert_eq!(fixture.call("status", None), failed);
    assert!(fixture.directory.join("launch-intent.json").exists());
    assert!(!fixture.output("start", None).status.success());
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn changed_batch_files_fail_before_launch() {
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
    fs::write(
        fixture.root.join("model.sbatch"),
        "#!/bin/sh\n#SBATCH --clusters=cluster-b\n",
    )
    .unwrap();
    let failed = fixture.call("start", None);
    assert_eq!(
        failed["completion"]["report"]["failure"]["code"],
        "launch_preparation_failed"
    );
    assert_eq!(fixture.call("status", None), failed);
    assert!(!fixture.output("start", None).status.success());
    assert!(fixture.directory.join("start-attempt.json").exists());
    assert!(!fixture.directory.join("launch-intent.json").exists());
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn incomplete_launch_records_never_fall_back_to_an_implicit_cluster() {
    let fixture = SchedulerFixture::new();
    fs::remove_file(fixture.directory.join("slurm.json")).unwrap();
    fs::write(
        fixture.directory.join("launch-intent.json"),
        serde_json::to_vec(&json!({"intent":fixture.record("remote.json")["intent"]})).unwrap(),
    )
    .unwrap();
    assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
    assert!(!fixture.output("status", None).status.success());
    assert!(!fixture.output("start", None).status.success());
    for command in ["sbatch", "squeue", "sacct", "scancel"] {
        assert!(fixture.calls(command).is_empty());
    }
}

#[cfg(unix)]
#[test]
fn installation_validation_checks_monitoring_access_without_interpreting_model_directives() {
    use std::os::unix::fs::PermissionsExt;
    for script in [
        "#!/bin/sh\n#SBATCH --clusters=model-cluster\n#SBATCH --account=research --qos=long\nexec worker\n",
        "#!/bin/sh\n#SBATCH --clusters=all\nsbatch another-model.sbatch\n",
    ] {
        let fixture = SchedulerFixture::unlaunched(script);
        let wrapper = fixture.root.join("wrapper.sh");
        fs::write(&wrapper, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let job = fixture.record("remote.json");
        let validation = json!({"installation_roots":[fixture.root.parent().unwrap()],"driver":job["execution"]["driver"],"descriptor":{
            "provider_binding_id":"test","installation":job["execution"]["installation"]["installation"],"release_path":fixture.root,
            "entrypoint":["wrapper.sh"],"slurm_script":"model.sbatch","environment":{}
        }});
        assert_eq!(
            payload(fixture.output("validate", Some(&validation)))["status"],
            "ready"
        );
        assert_eq!(fixture.calls("sacct").len(), 1);
        for call in fixture.calls("sacct") {
            assert!(
                !call["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "--clusters")
            );
        }
        assert!(!fixture.directory.join("launch-intent.json").exists());
        assert!(fixture.calls("sbatch").is_empty());
    }
}

#[cfg(unix)]
#[test]
fn invalid_persisted_handles_cannot_select_another_destination() {
    for handle in [
        json!({"job_id":123,"cluster":null}),
        json!({"job_id":123,"cluster":"all"}),
        json!({"job_id":123,"cluster":"a,b"}),
        json!({"job_id":0,"cluster":"cluster-a"}),
    ] {
        let fixture = SchedulerFixture::new();
        fs::write(
            fixture.directory.join("slurm.json"),
            serde_json::to_vec(&handle).unwrap(),
        )
        .unwrap();
        assert_eq!(fixture.cancel("pending")["outcome"], "unresolved");
        assert!(!fixture.output("status", None).status.success());
        for command in ["sbatch", "squeue", "sacct", "scancel"] {
            assert!(fixture.calls(command).is_empty());
        }
    }
}

#[test]
fn user_cancellation_before_start_prevents_submission_and_survives_runner_restart() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-cancel-{}", Uuid::new_v4()));
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    payload(call(
        "prepare",
        &directory,
        Some(&scalar_job(&root, intent)),
    ));
    for _ in 0..2 {
        assert_eq!(
            payload(call(
                "request-cancel",
                &directory,
                Some(&json!({"scope":"running"}))
            ))["cancellation"]["outcome"],
            "already_terminal"
        );
    }
    let done = payload(call("status", &directory, None));
    assert_eq!(done["completion"]["report"]["outcome"], "cancelled");
    assert_eq!(payload(call("start", &directory, None)), done);
    assert!(!directory.join("launch-intent.json").exists());
    assert!(!directory.join("started").exists());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn user_cancellation_stops_running_process_descendants_and_retains_logs() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-cancel-running-{}", Uuid::new_v4()));
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let mut job = scalar_job(&root, intent);
    job["assignment"]["definition"]["log_files"] =
        json!({"solver":{"path":"solver.log"},"missing":{"path":"missing.log"}});
    job["execution"]["installation"]["argv"] = json!([
        "/usr/bin/python3",
        "-u",
        "-c",
        "import subprocess,time; from pathlib import Path; p=subprocess.Popen(['/bin/sleep','600']); Path('solver.log').write_text('solver before cancellation'); Path('descendant').write_text(str(p.pid)); print('diagnostic before cancellation',flush=True); time.sleep(600)"
    ]);
    payload(call("prepare", &directory, Some(&job)));
    payload(call("start", &directory, None));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !directory.join("workspace/descendant").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    // Discard the reply, then reconnect through another runner process.
    assert!(
        call(
            "request-cancel",
            &directory,
            Some(&json!({"scope":"running"}))
        )
        .status
        .success()
    );
    let done = loop {
        let value = payload(call("status", &directory, None));
        if value["status"] == "completed" {
            break value;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(done["completion"]["report"]["outcome"], "cancelled");
    assert_eq!(done["completion"]["report"]["execution_terminated"], true);
    let log = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        payload(call(
            "read-logs",
            &directory,
            Some(&json!({"source":"stdout","epoch":1,"offset":0,"complete":true})),
        ))["data"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(
        String::from_utf8(log)
            .unwrap()
            .contains("diagnostic before cancellation")
    );
    let named = payload(call(
        "read-logs",
        &directory,
        Some(&json!({"source":"model.solver","epoch":1,"offset":0,"complete":true})),
    ));
    assert_eq!(named["closed"], true);
    assert_eq!(
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            named["data"].as_str().unwrap()
        )
        .unwrap(),
        b"solver before cancellation"
    );
    let missing = payload(call(
        "read-logs",
        &directory,
        Some(&json!({"source":"model.missing","epoch":1,"offset":0,"complete":true})),
    ));
    assert_eq!(missing["closed"], true);
    assert_eq!(missing["unavailable"], true);
    assert!(
        !call(
            "read-logs",
            &directory,
            Some(&json!({"source":"model.undeclared","epoch":1,"offset":0,"complete":true}))
        )
        .status
        .success()
    );
    let pid = fs::read_to_string(directory.join("workspace/descendant")).unwrap();
    let output = Command::new("/bin/ps")
        .args(["-p", &pid, "-o", "stat="])
        .output()
        .unwrap();
    assert!(
        output.stdout.is_empty()
            || String::from_utf8_lossy(&output.stdout)
                .trim()
                .starts_with('Z')
    );
    assert_eq!(
        payload(call(
            "request-cancel",
            &directory,
            Some(&json!({"scope":"running"}))
        ))["cancellation"]["outcome"],
        "already_terminal"
    );
    assert_eq!(payload(call("status", &directory, None)), done);
    assert!(!call("start", &directory, None).status.success());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn running_slurm_cancellation_waits_for_termination_after_lost_reply_and_worker_death() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = SchedulerFixture::new();
    fixture.set("state", "RUNNING");
    fixture.set("mode", "lost-reply");
    for name in ["started", "worker.lock"] {
        let path = fixture.directory.join(name);
        fs::write(&path, b"").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(fixture.cancel("running")["outcome"], "unresolved");
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    assert_eq!(fixture.cancel("running")["outcome"], "accepted");
    fixture.set("state", "COMPLETING");
    assert_eq!(fixture.call("status", None)["status"], "running");
    fixture.set("state", "CANCELLED");
    fixture.set(
        "accounting",
        &format!(
            "{}\n{}",
            fixture.accounting_row("123", "CANCELLED", "0:15"),
            fixture.accounting_row("123", "RUNNING", "0:0")
        ),
    );
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    fixture.set(
        "accounting",
        &fixture.accounting_row("123", "CANCELLED", "0:15"),
    );
    let done = fixture.call("status", None);
    assert_eq!(done["completion"]["report"]["outcome"], "cancelled");
    assert_eq!(fixture.call("status", None), done);
    for call in fixture.calls("scancel") {
        assert!(
            !call["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--state=PENDING"))
        );
        assert!(
            call["args"]
                .as_array()
                .unwrap()
                .contains(&json!("cluster-a"))
        );
    }
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn cancellation_during_submission_serializes_with_the_original_launch() {
    let fixture =
        SchedulerFixture::unlaunched("#!/bin/sh\nexec \"$1\" local-worker --directory \"$2\"\n");
    fixture.set("mode", "block-submit");
    std::thread::scope(|scope| {
        let start = scope.spawn(|| fixture.output("start", None));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.scheduler.join("submit-blocked").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let cancel = scope.spawn(|| fixture.cancel("running"));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!fixture.directory.join("user-cancellation.json").exists());
        fixture.set("release-submit", "");
        assert!(start.join().unwrap().status.success());
        assert_eq!(cancel.join().unwrap()["outcome"], "accepted");
    });
    assert_eq!(fixture.calls("sbatch").len(), 1);
    fixture.set("state", "CANCELLED");
    assert_eq!(
        fixture.call("status", None)["completion"]["report"]["outcome"],
        "cancelled"
    );
    assert!(!fixture.output("start", None).status.success());
    assert_eq!(fixture.calls("sbatch").len(), 1);
}

#[cfg(unix)]
#[test]
fn confirmed_slurm_failures_finish_before_or_during_worker_execution_without_relaunch() {
    for started in [false, true] {
        for state in [
            "FAILED",
            "TIMEOUT",
            "OUT_OF_MEMORY",
            "NODE_FAIL",
            "BOOT_FAIL",
            "DEADLINE",
            "PREEMPTED",
            "CANCELLED",
        ] {
            let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
            assert_eq!(fixture.call("start", None)["status"], "pending");
            if started {
                fixture.stopped_worker();
            }
            fixture.set("state", state);
            fixture.set("exit-code", "1:9");
            let done = fixture.call("status", None);
            let report = &done["completion"]["report"];
            assert_eq!(report["outcome"], "failed", "{state}/{started}");
            assert_eq!(report["execution_terminated"], true);
            assert_eq!(
                report["failure"]["code"],
                format!("slurm_{}", state.to_ascii_lowercase())
            );
            assert_eq!(report["outputs"], json!({}));
            assert_eq!(report["files"], json!({}));
            assert!(done.get("logs").is_none());
            fs::write(
                fixture.directory.join("slurm.log"),
                "batch diagnostic after worker shutdown\n",
            )
            .unwrap();
            let chunk = fixture.call(
                "read-logs",
                Some(&json!({"source":"slurm_batch","epoch":1,"offset":0,"complete":true})),
            );
            assert_eq!(chunk["closed"], true);
            assert_eq!(
                base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    chunk["data"].as_str().unwrap()
                )
                .unwrap(),
                b"batch diagnostic after worker shutdown\n"
            );
            assert_eq!(
                done["completion"]["scheduler_evidence"]["worker_started"],
                started
            );
            assert_eq!(
                done["completion"]["scheduler_evidence"]["allocation"]["exit_status"],
                1
            );
            assert_eq!(fixture.call("status", None), done);
            assert!(!fixture.output("start", None).status.success());
            assert_eq!(fixture.calls("sbatch").len(), 1);
            assert!(fixture.calls("scancel").is_empty());
        }
    }
}

#[cfg(unix)]
#[test]
fn scheduler_accounting_lag_conflicts_and_requeue_cannot_create_completion() {
    let fixture = SchedulerFixture::new();
    fixture.stopped_worker();
    for state in ["LAG", "RUNNING", "COMPLETING"] {
        fixture.set("state", state);
        assert_ne!(fixture.call("status", None)["status"], "completed");
        assert!(!fixture.directory.join("completion.json").exists());
    }
    fixture.set("state", "FAILED");
    for accounting in [
        String::new(),
        "malformed".into(),
        fixture.accounting_row("123.batch", "FAILED", "1:0"),
        format!(
            "{}\n{}",
            fixture.accounting_row("123", "FAILED", "1:0"),
            fixture.accounting_row("123.0", "RUNNING", "0:0")
        ),
        format!(
            "{}\n{}",
            fixture.accounting_row("123", "FAILED", "1:0"),
            fixture.accounting_row("123", "COMPLETED", "0:0")
        ),
        fixture
            .accounting_row("123", "FAILED", "1:0")
            .replace("enumeray-", "other-"),
        fixture
            .accounting_row("123", "FAILED", "1:0")
            .replace("|Unknown|", "|bad-date|"),
        format!(
            "{}\n{}",
            fixture.accounting_row("123", "COMPLETED", "0:0"),
            fixture.accounting_row("123.0", "FAILED", "1:0")
        ),
    ] {
        fixture.set("accounting", &accounting);
        assert_eq!(
            fixture.call("status", None)["status"],
            "unresolved",
            "{accounting}"
        );
        assert!(!fixture.directory.join("completion.json").exists());
    }
    fixture.set(
        "accounting",
        &fixture.accounting_row("123", "FAILED", "1:0"),
    );
    fixture.set("queue-after-accounting", "RUNNING");
    assert_eq!(fixture.call("status", None)["status"], "unresolved");
    assert!(!fixture.directory.join("completion.json").exists());
    fs::remove_file(fixture.scheduler.join("queue-after-accounting")).unwrap();
    fixture.set("state", "FAILED");
    assert_eq!(fixture.call("status", None)["status"], "completed");
    assert!(fixture.calls("sbatch").is_empty());
    assert!(fixture.calls("scancel").is_empty());
}

#[cfg(unix)]
#[test]
fn terminal_accounting_conflicting_with_live_worker_waits_for_ownership_release() {
    for started in [false, true] {
        let fixture = SchedulerFixture::new();
        fixture.stopped_worker();
        if !started {
            fs::remove_file(fixture.directory.join("started")).unwrap();
        }
        let owner = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.directory.join("worker.lock"))
            .unwrap();
        owner.lock().unwrap();
        fixture.set("state", "TIMEOUT");
        assert_eq!(fixture.call("status", None)["status"], "unresolved");
        assert!(!fixture.directory.join("completion.json").exists());
        drop(owner);
        let done = fixture.call("status", None);
        assert_eq!(
            done["completion"]["report"]["failure"]["code"],
            "slurm_timeout"
        );
    }
}

#[cfg(unix)]
fn successful_completion(fixture: &SchedulerFixture) -> Value {
    json!({"observed_at":"2026-10-07T12:00:00Z", "report":{
        "attempt_id":fixture.record("remote.json")["assignment"]["attempt_id"],
        "outcome":"succeeded","execution_terminated":true,"outputs":{"doubled":6},
        "files":{},"optional_output_failures":{},"failure":null}})
}

#[cfg(unix)]
#[test]
fn retained_or_concurrently_published_wrapper_completion_wins_over_scheduler_failure() {
    use std::os::unix::fs::PermissionsExt;
    for late in [false, true] {
        let fixture = SchedulerFixture::new();
        fixture.stopped_worker();
        fixture.set("state", "FAILED");
        let completion = successful_completion(&fixture);
        if late {
            let retained = fixture.scheduler.join("retained-completion.json");
            fs::write(&retained, serde_json::to_vec(&completion).unwrap()).unwrap();
            fs::set_permissions(&retained, fs::Permissions::from_mode(0o600)).unwrap();
            fixture.set(
                "publish-completion",
                &serde_json::to_string(&fixture.directory.join("completion.json")).unwrap(),
            );
        } else {
            fixture.write_record("completion.json", &completion);
        }
        let done = fixture.call("status", None);
        assert_eq!(done["completion"], completion);
        assert_eq!(fixture.call("status", None), done);
        assert!(done["completion"].get("scheduler_evidence").is_none());
        assert!(fixture.calls("sbatch").is_empty());
    }
}

#[cfg(unix)]
#[test]
fn invalid_worker_evidence_is_never_replaced_by_scheduler_failure() {
    for evidence in ["completion", "job", "ownership", "cancellation"] {
        let fixture = SchedulerFixture::new();
        fixture.stopped_worker();
        fixture.set("state", "FAILED");
        let path = match evidence {
            "completion" => "completion.json",
            "job" => "job.json",
            "ownership" => "worker.lock",
            _ => "user-cancellation.json",
        };
        if evidence == "ownership" {
            fs::remove_file(fixture.directory.join(path)).unwrap();
        } else {
            fixture.write_record(path, &json!({"conflicting":true}));
        }
        let before = fs::read(fixture.directory.join(path)).ok();
        assert!(!fixture.output("status", None).status.success());
        assert_eq!(fs::read(fixture.directory.join(path)).ok(), before);
        if evidence != "completion" {
            assert!(!fixture.directory.join("completion.json").exists());
        }
        assert!(fixture.calls("sbatch").is_empty());
    }
}

#[cfg(unix)]
#[test]
fn cancellation_intent_does_not_relabel_failures_or_started_pending_revocation() {
    for scope in ["pending", "running"] {
        for state in ["FAILED", "CANCELLED"] {
            let fixture = SchedulerFixture::new();
            assert_eq!(fixture.cancel(scope)["outcome"], "accepted");
            fixture.stopped_worker();
            fixture.set("state", state);
            let done = fixture.call("status", None);
            let report = &done["completion"]["report"];
            let expected = match (scope, state) {
                (_, "FAILED") => "slurm_failed",
                ("running", _) => "user_cancelled",
                _ => "slurm_cancelled",
            };
            assert_eq!(report["failure"]["code"], expected);
            assert_eq!(
                report["outcome"],
                if expected == "user_cancelled" {
                    "cancelled"
                } else {
                    "failed"
                }
            );
        }
    }
    let fixture = SchedulerFixture::new();
    assert_eq!(fixture.cancel("pending")["outcome"], "accepted");
    fixture.set("state", "CANCELLED");
    // Batch setup started, even though the scientific worker did not.
    fixture.set("started-at", "2023-11-14T22:13:21");
    assert_eq!(
        fixture.call("status", None)["completion"]["report"]["failure"]["code"],
        "slurm_cancelled"
    );
}

#[cfg(unix)]
#[test]
fn ambiguous_nonzero_submission_and_malformed_receipts_remain_unresolved() {
    for mode in [
        "ambiguous-submit",
        "lost-submit-reply",
        "truncated-rejection",
        "conflicting-submit",
    ] {
        let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
        fixture.set("mode", mode);
        let initial = fixture.call("start", None);
        assert_ne!(initial["status"], "completed", "{mode}");
        assert_eq!(
            fixture.record("launch-outcome.json")["outcome"],
            "uncertain"
        );
        assert!(!fixture.directory.join("startup-failure.json").exists());
        assert!(!fixture.directory.join("completion.json").exists());
        assert!(!fixture.output("start", None).status.success());
        assert_eq!(fixture.calls("sbatch").len(), 1);
    }
    for reply in ["", "malformed", "0;cluster-a"] {
        let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
        fixture.set("reply", reply);
        assert_eq!(fixture.call("start", None)["status"], "unresolved");
        assert_eq!(
            fixture.record("launch-outcome.json")["outcome"],
            "uncertain"
        );
        assert!(!fixture.directory.join("completion.json").exists());
        assert!(!fixture.output("start", None).status.success());
        assert_eq!(fixture.calls("sbatch").len(), 1);
    }
}

#[test]
fn unavailable_executable_preparation_failure_is_durable_and_never_starts() {
    let root = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("enumeray-preparation-failure-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let intent = Uuid::new_v4();
    let directory = root.join(intent.to_string());
    let mut job = scalar_job(&root, intent);
    job["execution"]["installation"]["argv"][0] = json!(root.join("missing-executable"));
    let failed = payload(call("prepare", &directory, Some(&job)));
    assert_eq!(failed["status"], "completed");
    assert_eq!(failed["completion"]["report"]["outcome"], "failed");
    assert_eq!(
        failed["completion"]["report"]["failure"]["code"],
        "launch_preparation_failed"
    );
    assert_eq!(payload(call("status", &directory, None)), failed);
    assert_eq!(payload(call("prepare", &directory, Some(&job))), failed);
    assert!(!call("start", &directory, None).status.success());
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_enumeray"))
            .args(["local-worker", "--directory"])
            .arg(&directory)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(payload(call("status", &directory, None)), failed);
    assert_eq!(
        payload(call(
            "request-cancel",
            &directory,
            Some(&json!({"scope":"running"}))
        ))["cancellation"]["outcome"],
        "already_terminal"
    );
    assert!(!directory.join("started").exists());
    assert!(!directory.join("workspace").exists());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn worker_startup_failure_waits_for_stopped_slurm_and_can_explain_completed_accounting() {
    for validation in [true, false] {
        for state in ["FAILED", "COMPLETED"] {
            let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
            assert_eq!(fixture.call("start", None)["status"], "pending");
            if validation {
                fs::remove_file(fixture.root.join("model.sbatch")).unwrap();
            } else {
                fs::write(fixture.directory.join("workspace"), b"not a directory").unwrap();
            }
            let output = Command::new(env!("CARGO_BIN_EXE_enumeray"))
                .args(["local-worker", "--directory"])
                .arg(&fixture.directory)
                .output()
                .unwrap();
            assert!(!output.status.success());
            let retained = fixture.record("startup-failure.json");
            assert_eq!(
                retained["stage"],
                if validation {
                    "worker_validation"
                } else {
                    "worker_preparation"
                }
            );
            assert!(!fixture.directory.join("wrapper-started.json").exists());
            assert_ne!(fixture.call("status", None)["status"], "completed");
            assert!(!fixture.directory.join("completion.json").exists());
            fixture.set("state", state);
            fixture.set(
                "exit-code",
                if state == "COMPLETED" { "0:0" } else { "1:0" },
            );
            let failed = fixture.call("status", None);
            assert_eq!(failed["status"], "completed");
            assert_eq!(
                failed["completion"]["report"]["failure"]["code"],
                if validation {
                    "worker_validation_failed"
                } else {
                    "worker_preparation_failed"
                }
            );
            assert_eq!(failed["completion"]["startup_evidence"], retained);
            assert_eq!(
                failed["completion"]["scheduler_evidence"]["allocation"]["state"],
                state
            );
            assert_eq!(fixture.call("status", None), failed);
            assert!(!fixture.output("start", None).status.success());
            assert_eq!(fixture.calls("sbatch").len(), 1);
        }
    }
}

#[cfg(unix)]
#[test]
fn worker_failure_published_during_scheduler_observation_keeps_original_evidence() {
    for state in ["FAILED", "COMPLETED"] {
        let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
        assert_eq!(fixture.call("start", None)["status"], "pending");
        fs::write(fixture.directory.join("workspace"), b"not a directory").unwrap();
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_enumeray"))
                .args(["local-worker", "--directory"])
                .arg(&fixture.directory)
                .output()
                .unwrap()
                .status
                .success()
        );
        let retained = fixture.record("startup-failure.json");
        // Publish the real worker's evidence only while accounting is in flight.
        // This forces status's initial startup read to precede its visibility.
        fs::rename(
            fixture.directory.join("startup-failure.json"),
            fixture.scheduler.join("retained-startup-failure.json"),
        )
        .unwrap();
        fixture.set("state", state);
        fixture.set(
            "exit-code",
            if state == "COMPLETED" { "0:0" } else { "1:0" },
        );
        fixture.set(
            "publish-startup-failure",
            &serde_json::to_string(&fixture.directory.join("startup-failure.json")).unwrap(),
        );
        let failed = fixture.call("status", None);
        assert_eq!(failed["status"], "completed");
        assert_eq!(
            failed["completion"]["report"]["failure"]["code"],
            "worker_preparation_failed"
        );
        assert_eq!(failed["completion"]["startup_evidence"], retained);
        assert_eq!(failed["completion"]["observed_at"], retained["observed_at"]);
        assert_eq!(
            failed["completion"]["scheduler_evidence"]["allocation"]["state"],
            state
        );
        assert_eq!(fixture.call("status", None), failed);
        assert!(!fixture.output("start", None).status.success());
        assert_eq!(fixture.calls("sbatch").len(), 1);
        assert!(!fixture.directory.join("wrapper-started.json").exists());
    }
}

#[cfg(unix)]
#[test]
fn incomplete_inputs_at_first_start_are_terminal_and_cannot_reopen_staging() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    let fixture = SchedulerFixture::unlaunched("#!/bin/sh\nexec worker\n");
    let mut remote = fixture.record("remote.json");
    let artifact = Uuid::new_v4();
    remote["input_digests"] =
        json!({artifact.to_string():[2,STANDARD.encode(Sha256::digest(b"42"))]});
    fixture.write_record("remote.json", &remote);
    let failed = fixture.call("start", None);
    assert_eq!(
        failed["completion"]["report"]["failure"]["code"],
        "launch_preparation_failed"
    );
    assert_eq!(fixture.call("status", None), failed);
    assert!(
        !fixture
            .output(
                "write-input",
                Some(&json!({"artifact_id":artifact,"offset":0,"data":STANDARD.encode(b"42")}))
            )
            .status
            .success()
    );
    assert!(!fixture.output("start", None).status.success());
    assert!(fixture.calls("sbatch").is_empty());
}

#[cfg(unix)]
#[test]
fn finalized_wrapper_output_waits_for_actual_slurm_allocation_release() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = SchedulerFixture::new();
    for name in ["remote.json", "job.json"] {
        let mut job = fixture.record(name);
        job["assignment"]["definition"]["log_files"] = json!({"solver":{"path":"solver.log"}});
        fixture.write_record(name, &job);
    }
    let workspace = fixture.directory.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.write_record("workspace/solver.log", &json!("final solver diagnostic"));
    let log_request = json!({"source":"model.solver","epoch":1,"offset":0,"complete":true});
    fixture.stopped_worker();
    let completion = successful_completion(&fixture);
    fixture.write_record("completion.json", &completion);
    let proof = cleanup_proof(&fixture.record("remote.json"), &completion);
    for state in ["RUNNING", "SUSPENDED", "LAG"] {
        fixture.set("state", state);
        assert_eq!(fixture.call("status", None)["completion"], completion);
        assert_eq!(fixture.call("allocation-status", None)["status"], "running");
        let log = fixture.call("read-logs", Some(&log_request));
        assert_eq!(log["closed"], false);
        assert_eq!(log["unavailable"], false);
        assert!(!fixture.output("cleanup-run", Some(&proof)).status.success());
        assert!(!fixture.directory.join("cleanup.json").exists());
    }
    fixture.set("state", "COMPLETED");
    assert_eq!(
        fixture.call("allocation-status", None)["status"],
        "completed"
    );
    assert_eq!(fixture.call("status", None)["completion"], completion);
    let log = fixture.call("read-logs", Some(&log_request));
    assert_eq!(log["closed"], true);
    assert_eq!(log["unavailable"], false);
    assert_eq!(
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            log["data"].as_str().unwrap()
        )
        .unwrap(),
        serde_json::to_vec(&json!("final solver diagnostic")).unwrap()
    );
    assert_eq!(
        fixture.call("cleanup-run", Some(&proof))["status"],
        "completed"
    );
    fixture.set("state", "LAG");
    assert_eq!(
        fixture.call("cleanup-run", Some(&proof))["status"],
        "completed"
    );
    assert!(fixture.calls("sbatch").is_empty());
    assert!(fixture.calls("scancel").is_empty());
}

#[cfg(unix)]
#[test]
fn archive_recovery_uses_accounting_after_the_controller_purges_its_job() {
    let fixture = SchedulerFixture::new();
    fixture.stopped_worker();
    fixture.write_record("completion.json", &successful_completion(&fixture));
    let mut job = fixture.record("remote.json");
    job["log_policy"] = serde_json::to_value(enumeray_contracts::logs::LogPolicy {
        archive_enabled: true,
        ..Default::default()
    })
    .unwrap();
    fixture.write_record("remote.json", &job);
    fixture.set("queue-purged", "1");
    fixture.set("other-queue-job", "999|RUNNING");
    fixture.set("state", "COMPLETED");
    assert_eq!(
        fixture.call("allocation-status", None)["status"],
        "completed"
    );
    let archive = fixture.call("build-log-archive", None);
    assert_eq!(archive["status"], "completed");
    assert_eq!(archive["archive"]["manifest"]["complete"], false);
    for call in fixture.calls("squeue") {
        let args = call["args"].as_array().unwrap();
        assert!(!args.iter().any(|arg| arg == "--jobs"));
        assert!(args.iter().any(|arg| arg == "--states=all"));
        assert!(
            args.windows(2)
                .any(|pair| pair == [json!("--user"), json!(fixture.uid())])
        );
    }
    assert!(fixture.calls("sbatch").is_empty());
    assert!(fixture.calls("scancel").is_empty());
}
