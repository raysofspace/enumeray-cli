#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};
use uuid::Uuid;

#[tokio::test]
async fn enrollment_retry_preserves_machine_and_generated_credential_after_lost_receipt() {
    let root = std::env::temp_dir().join(format!("enumeray-enrollment-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let enrollment = root.join("enrollment.json");
    fs::write(
        &enrollment,
        serde_json::to_vec(&json!({"enrollment_token":"test-single-use-token"})).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&enrollment, fs::Permissions::from_mode(0o600)).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let received = requests.clone();
    let server = tokio::spawn(async move {
        for attempt in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(headers).unwrap();
            assert!(headers.starts_with("POST /connector/v1/enroll HTTP/1.1"));
            assert!(!headers.contains("user-profile-token-must-not-reach-enrollment"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            received.lock().unwrap().push(body.clone());
            if attempt == 0 {
                // Receive the complete request, then lose the reply, rather than
                // merely returning a synthetic 503 response.
                continue;
            }
            if attempt == 1 {
                // An oversized success response must not allocate an
                // unbounded body or replace the identity used by the retry.
                let receipt =
                    json!({"connector_id":"self-service","credential_id":body["credential_id"]});
                let body = format!("{}{receipt}", " ".repeat(64 * 1024 + 1));
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await;
                continue;
            }
            let receipt =
                json!({"connector_id":"self-service","credential_id":body["credential_id"]})
                    .to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{receipt}", receipt.len()).as_bytes()).await.unwrap();
        }
    });
    let state = root.join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let journal = state.join("journal");
    fs::create_dir(&journal).unwrap();
    fs::write(
        journal.join("original-evidence"),
        b"original backend identity",
    )
    .unwrap();
    let mut durable_identity = None;
    for attempt in 0..3 {
        let output = Command::new(env!("CARGO_BIN_EXE_enumeray"))
            .arg("--config")
            .arg(&root)
            .env(
                "ENUMERAY_TOKEN",
                "user-profile-token-must-not-reach-enrollment",
            )
            .arg("connector")
            .args(["enroll", "--api-base-url", &origin, "--directory"])
            .arg(&state)
            .arg("--enrollment-file")
            .arg(&enrollment)
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.success(),
            attempt == 2,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let identity = fs::read(state.join("identity.json")).unwrap();
        if let Some(original) = &durable_identity {
            assert_eq!(&identity, original);
        } else {
            durable_identity = Some(identity);
        }
        assert_eq!(
            fs::read(journal.join("original-evidence")).unwrap(),
            b"original backend identity"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("test-single-use-token"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("test-single-use-token"));
    }
    server.await.unwrap();
    let records = requests.lock().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0], records[1]);
    assert_eq!(records[0], records[2]);
    let token = fs::read_to_string(state.join("token")).unwrap();
    assert!(token.starts_with("enr_c1_"));
    assert_eq!(
        records[0]["token_verifier"],
        Sha256::digest(token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let identity: Value =
        serde_json::from_slice(&fs::read(state.join("identity.json")).unwrap()).unwrap();
    assert_eq!(records[0]["machine_id"], identity["machine_id"]);
    assert_eq!(identity["token"], token);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn enrollment_creates_a_relative_directory_and_bounds_the_credential_read() {
    let root = std::env::temp_dir().join(format!("enumeray-enrollment-{}", Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("oversized-enrollment.json");
    let file = fs::File::create(&source).unwrap();
    file.set_len(1024 * 1024).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_enumeray"))
        .arg("connector")
        .current_dir(&root)
        .args([
            "enroll",
            "--api-base-url",
            "http://127.0.0.1:9",
            "--directory",
            "new-state",
            "--enrollment-file",
            "oversized-enrollment.json",
        ])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exceeds its byte limit"));
    assert!(root.join("new-state/identity.json").is_file());
    assert!(!root.join("new-state/token").exists());
    fs::remove_dir_all(root).unwrap();
}
