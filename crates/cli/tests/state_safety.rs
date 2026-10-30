//! Local state and file-transfer failure boundaries, exercised through the standalone CLI.
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
    time::Duration,
};

fn command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    command
        .args(["--json", "--config"])
        .arg(config)
        .env_remove("ENUMERAY_TOKEN");
    command
}

fn lock(path: &Path) -> File {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).unwrap();
    file.try_lock().unwrap();
    file
}

fn failed(output: Output, message: &str) {
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn serve(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        responses.into_iter().map(|body| {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = vec![];
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]);
        }
        let headers = String::from_utf8(bytes).unwrap();
        let length = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|length| length.trim().parse::<usize>().ok())).unwrap_or(0);
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).unwrap();
        let request = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        let body = body.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        request
    }).collect()
    });
    (origin, worker)
}

#[test]
fn concurrent_profile_and_upload_writers_stop_before_network_or_state_changes() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let _profiles = lock(&directory.path().join("profiles.json.lock"));
    failed(
        command(&config)
            .args(["auth", "configure", "https://example.org"])
            .output()
            .unwrap(),
        "already being changed",
    );
    assert!(!config.exists());
    let upload = directory.path().join("upload.json");
    let _upload = lock(&directory.path().join("upload.json.lock"));
    failed(
        command(&config)
            .args([
                "--api",
                "http://127.0.0.1:1",
                "artifact",
                "resume",
                "--state",
            ])
            .arg(&upload)
            .output()
            .unwrap(),
        "already in use",
    );
    assert!(!upload.exists());
}

#[test]
fn logout_invalidates_an_unfinished_login_even_without_a_preexisting_profile() {
    use std::{process::Stdio, sync::mpsc};
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("GET /api/v1/me ")
        );
        arrived.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(15)).unwrap();
        let body = json!({"user_id":"33333333-3333-4333-8333-333333333333","label":"Researcher"})
            .to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
    });
    let mut login = command(&config)
        .args(["--api", &origin, "auth", "login", "--token-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    login
        .stdin
        .take()
        .unwrap()
        .write_all(b"private-stale-login-token\n")
        .unwrap();
    arrival.recv_timeout(Duration::from_secs(15)).unwrap();
    let logout = command(&config).args(["auth", "logout"]).output().unwrap();
    assert!(
        logout.status.success(),
        "{}",
        String::from_utf8_lossy(&logout.stderr)
    );
    release.send(()).unwrap();
    let result = login.wait_with_output().unwrap();
    failed(result, "session_changed");
    worker.join().unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    assert!(saved["profiles"].as_object().unwrap().is_empty());
    assert!(!saved.to_string().contains("private-stale-login-token"));
}

#[test]
fn ownership_acceptance_always_sends_the_scoped_resource_identity() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    for (kind, noun) in [("model", "model"), ("connector", "connector")] {
        let (origin, worker) = serve(vec![
            json!({"id":"33333333-3333-4333-8333-333333333333","accepted":true}),
        ]);
        let mut invocation = command(&config);
        invocation.args(["--api", &origin, noun]);
        if noun == "connector" {
            invocation.args(["ownership", "accept"]);
        } else {
            invocation.arg("accept-transfer");
        }
        let output = invocation
            .args(["intended-resource", "transfer-id"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            worker.join().unwrap(),
            vec![json!({"resource_kind":kind,"resource_id":"intended-resource"})]
        );
    }
}

#[test]
fn unusable_or_locked_secret_destinations_fail_before_issuing_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let parent = directory.path().join("not-a-directory");
    fs::write(&parent, b"preserve").unwrap();
    let output_path = parent.join("secret.json");
    failed(
        command(&config)
            .args([
                "--api",
                "http://127.0.0.1:1",
                "auth",
                "token-create",
                "test",
                "--scopes",
                "me:read",
                "--output",
            ])
            .arg(&output_path)
            .output()
            .unwrap(),
        "I/O failed",
    );
    assert_eq!(fs::read(&parent).unwrap(), b"preserve");
    let output_path = directory.path().join("secret.json");
    let _output = lock(&directory.path().join("secret.json.lock"));
    failed(
        command(&config)
            .args([
                "--api",
                "http://127.0.0.1:1",
                "access",
                "invite",
                "model",
                "--user",
                "22222222-2222-4222-8222-222222222222",
                "--output",
            ])
            .arg(&output_path)
            .output()
            .unwrap(),
        "already in use",
    );
    assert!(!output_path.exists());
}

#[test]
fn token_creation_never_claims_success_for_an_invalid_secret_response() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let output_path = directory.path().join("secret.json");
    let issued = json!({"id":"22222222-2222-4222-8222-222222222222","token":"private-issued-token","expires_at":"2026-10-17T12:00:00Z"});
    let mut invalid = issued.clone();
    invalid["id"] = Value::Null;
    let (origin, worker) = serve(vec![invalid]);
    let output = command(&config)
        .args([
            "--api",
            &origin,
            "auth",
            "token-create",
            "test",
            "--scopes",
            "me:read",
            "--output",
        ])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!output_path.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-issued-token"));
    failed(output, "invalid secret response");
    worker.join().unwrap();
    let (origin, worker) = serve(vec![issued.clone()]);
    let output = command(&config)
        .args([
            "--api",
            &origin,
            "auth",
            "token-create",
            "test",
            "--scopes",
            "me:read",
            "--output",
        ])
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-issued-token"));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&output_path).unwrap()).unwrap(),
        issued
    );
    worker.join().unwrap();
}

#[test]
fn artifact_metadata_is_validated_before_source_or_remote_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let metadata = directory.path().join("metadata.json");
    let state = directory.path().join("upload.json");
    for (bytes, reason) in [
        (
            r#"{"scientific":1,"scientific":2}"#,
            "inputs must be valid JSON",
        ),
        ("[]", "metadata must be a JSON object"),
    ] {
        fs::write(&metadata, bytes).unwrap();
        failed(
            command(&config)
                .args([
                    "--api",
                    "http://127.0.0.1:1",
                    "artifact",
                    "upload",
                    "missing-source",
                    "--name",
                    "data",
                    "--media-type",
                    "text/plain",
                    "--metadata",
                ])
                .arg(&metadata)
                .arg("--state")
                .arg(&state)
                .output()
                .unwrap(),
            reason,
        );
        assert!(!state.exists());
    }
}

#[test]
fn artifact_download_rejects_oversized_ranges_and_retains_private_checkpoint() {
    use base64::Engine as _;
    use sha2::Digest;
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let output_path = directory.path().join("artifact");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let metadata=json!({"content":{"version_id":"version-one","size_bytes":1,"checksum":{"value":base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(b"a"))}}}).to_string();
    let responses=vec![
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{metadata}",metadata.len()),
        {
            let user=json!({"user_id":"22222222-2222-4222-8222-222222222222","label":"owner"}).to_string();
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{user}",user.len())
        },
        format!("HTTP/1.1 303 See Other\r\nLocation: {origin}storage\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1\r\nx-amz-version-id: version-one\r\nContent-Length: 2\r\nConnection: close\r\n\r\nab".into(),
    ];
    let worker = thread::spawn(move || {
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            socket.write_all(response.as_bytes()).unwrap();
        }
    });
    failed(
        command(&config)
            .args([
                "--api",
                &origin,
                "artifact",
                "download",
                "artifact-id",
                "--output",
            ])
            .arg(&output_path)
            .output()
            .unwrap(),
        "conflicts with the pinned download",
    );
    worker.join().unwrap();
    assert!(!output_path.exists());
    let state = enumeray_client::bulk::checkpoint_directory(&output_path).unwrap();
    let checkpoint: Value =
        serde_json::from_slice(&fs::read(state.join("state.json")).unwrap()).unwrap();
    assert_eq!(checkpoint["offset"], 0);
    assert_eq!(fs::metadata(state.join("payload.part")).unwrap().len(), 0);
}

#[test]
fn completed_uploads_require_the_original_reservation_and_verified_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("profiles.json");
    let state = directory.path().join("upload.json");
    let user = "22222222-2222-4222-8222-222222222222";
    let upload = "33333333-3333-4333-8333-333333333333";
    let artifact = "44444444-4444-4444-8444-444444444444";
    for (reservation, receipt, expected) in [
        (
            json!({"upload_id":upload,"artifact_id":user}),
            Value::Null,
            "conflicts with the saved reservation",
        ),
        (
            json!({"upload_id":upload,"artifact_id":artifact}),
            Value::Null,
            "omitted its receipt",
        ),
        (
            json!({"upload_id":upload,"artifact_id":artifact}),
            json!({"artifact_id":artifact}),
            "conflicts with the saved identity or verified content",
        ),
    ] {
        let (origin, worker) = serve(vec![
            json!({"user_id":user,"label":"caller"}),
            json!({"state":"completed","reservation":reservation,"transfer":{"status":"completed"},"completion":receipt}),
        ]);
        fs::write(&state, json!({"schema_version":1,"api_url":origin,"user_id":user,"source":directory.path().join("source-no-longer-needed"),"idempotency_key":"original-upload-key","request":{"logical_name":"data","media_type":"text/plain","scientific_metadata":{},"expected_size_bytes":1,"checksum":{"algorithm":"sha256","checksum_type":"full_object","value":"digest"}},"reservation":{"upload_id":upload,"artifact_id":artifact},"version_id":null}).to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let before = fs::read(&state).unwrap();
        failed(
            command(&config)
                .args(["--api", &origin, "artifact", "resume", "--state"])
                .arg(&state)
                .output()
                .unwrap(),
            expected,
        );
        assert_eq!(fs::read(&state).unwrap(), before);
        worker.join().unwrap();
    }
}

#[test]
fn expired_download_authorization_keeps_checkpoint_and_reports_same_account_recovery() {
    use base64::Engine as _;
    use sha2::Digest;
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("result");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let evidence = enumeray_client::bulk::DownloadEvidence {
        scope: format!("{origin}user/22222222-2222-4222-8222-222222222222"),
        source: "/api/v1/artifacts/id/content".into(),
        version_id: Some("version-one".into()),
        size_bytes: 6,
        checksum_sha256: base64::engine::general_purpose::STANDARD
            .encode(sha2::Sha256::digest(b"abcdef")),
    };
    let mut retained = enumeray_client::bulk::RetainedTransfer::open(
        &target,
        evidence,
        std::time::Instant::now() + Duration::from_secs(10),
        |_| {},
    )
    .unwrap();
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    let payload = retained.payload_path();
    drop(retained);
    let state = payload.parent().unwrap().join("state.json");
    let before = fs::read(&state).unwrap();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut byte = [0];
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let body=json!({"code":"invalid_credential","message":"Credential expired.","request_id":"expired-download"}).to_string();
        write!(
            socket,
            "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let output = command(&directory.path().join("profiles.json"))
        .args(["--api", &origin, "artifact", "download", "id", "--output"])
        .arg(&target)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("auth refresh/login")
            && stderr.contains("same account")
            && stderr.contains("401"),
        "{stderr}"
    );
    assert!(!output.status.success());
    worker.join().unwrap();
    assert_eq!(fs::read(state).unwrap(), before);
    assert_eq!(fs::read(payload).unwrap(), b"abc");
    assert!(!target.exists());
}
