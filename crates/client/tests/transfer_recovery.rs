use base64::{Engine as _, engine::general_purpose::STANDARD};
use enumeray_client::{
    Error,
    bulk::{self, DownloadEvidence, RetainedTransfer},
};
use enumeray_contracts::transfers::TransferBudgets;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("enumeray-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> PathBuf {
        self.0.join("result")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn evidence(bytes: &[u8]) -> DownloadEvidence {
    DownloadEvidence {
        scope: "https://api.example/user/one".into(),
        source: "artifact-one".into(),
        version_id: Some("exact-version".into()),
        size_bytes: bytes.len() as u64,
        checksum_sha256: STANDARD.encode(Sha256::digest(bytes)),
    }
}
fn open(fixture: &Fixture, bytes: &[u8]) -> RetainedTransfer {
    RetainedTransfer::open(
        &fixture.path(),
        evidence(bytes),
        Instant::now() + Duration::from_secs(10),
        |_| {},
    )
    .unwrap()
}
fn server(replies: Vec<Vec<u8>>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/secret-capability",
        listener.local_addr().unwrap()
    );
    let worker = thread::spawn(move || {
        replies
            .into_iter()
            .map(|reply| {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let _ = socket.write_all(&reply);
                String::from_utf8(request).unwrap()
            })
            .collect()
    });
    (url, worker)
}
fn response(status: u16, range: &str, length: usize, version: &str, bytes: &[u8]) -> Vec<u8> {
    let mut reply = format!("HTTP/1.1 {status} status\r\nConnection: close\r\nContent-Length: {length}\r\nx-amz-version-id: {version}\r\n{range}\r\n").into_bytes();
    reply.extend(bytes);
    reply
}
fn download(retained: &mut RetainedTransfer, url: &str) -> Result<(), Error> {
    bulk::download_retained(
        retained,
        || Ok(url.into()),
        |_| {},
        TransferBudgets::default(),
        Instant::now() + Duration::from_secs(5),
    )
}

#[test]
fn resumes_an_interrupted_http_body_without_resending_the_prefix() {
    let fixture = Fixture::new();
    let bytes = b"abcdefghij";
    let (url, worker) = server(vec![response(
        206,
        "Content-Range: bytes 0-9/10\r\n",
        10,
        "exact-version",
        &bytes[..4],
    )]);
    let mut retained = open(&fixture, bytes);
    assert!(download(&mut retained, &url).is_err());
    drop(retained);
    worker.join().unwrap();
    assert!(!fixture.path().exists());
    let (url, worker) = server(vec![response(
        206,
        "Content-Range: bytes 4-9/10\r\n",
        6,
        "exact-version",
        &bytes[4..],
    )]);
    let mut retained = open(&fixture, bytes);
    assert_eq!(retained.offset(), 4);
    download(&mut retained, &url).unwrap();
    let progress = retained.finish().unwrap();
    assert_eq!(
        (
            progress.resumed_bytes,
            progress.transferred_bytes,
            progress.verified_bytes
        ),
        (4, 6, 10)
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
    let requests = worker.join().unwrap();
    assert!(requests[0].to_lowercase().contains("range: bytes=4-9"));
    assert!(!requests[0].to_lowercase().contains("authorization:"));
    assert!(!requests[0].to_lowercase().contains("cookie:"));
    // Completion can replay from its compact owned receipt.
    assert_eq!(open(&fixture, bytes).finish().unwrap().resumed_bytes, 10);
}

#[test]
fn rejects_ignored_ranges_wrong_versions_and_inconsistent_lengths() {
    for (status, range, length, version, body) in [
        (200, "", 10, "exact-version", b"abcdefghij".as_slice()),
        (
            206,
            "Content-Range: bytes 1-10/10\r\n",
            10,
            "exact-version",
            b"abcdefghij".as_slice(),
        ),
        (
            206,
            "Content-Range: bytes 0-9/10\r\n",
            10,
            "other-version",
            b"abcdefghij".as_slice(),
        ),
        (
            206,
            "Content-Range: bytes 0-9/10\r\n",
            9,
            "exact-version",
            b"abcdefghi".as_slice(),
        ),
        (416, "", 0, "exact-version", b"".as_slice()),
    ] {
        let fixture = Fixture::new();
        let mut retained = open(&fixture, b"abcdefghij");
        let (url, worker) = server(vec![response(status, range, length, version, body)]);
        assert!(download(&mut retained, &url).is_err());
        worker.join().unwrap();
        assert_eq!(retained.offset(), 0);
        assert!(!fixture.path().exists());
    }
}

#[test]
fn renews_rejected_capabilities_and_verifies_empty_objects() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"");
    let (url, worker) = server(vec![
        response(403, "", 0, "exact-version", b""),
        response(200, "", 0, "exact-version", b""),
    ]);
    let mut authorizations = 0;
    bulk::download_retained(
        &mut retained,
        || {
            authorizations += 1;
            Ok(url.clone())
        },
        |_| {},
        TransferBudgets::default(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(authorizations, 2);
    retained.finish().unwrap();
    worker.join().unwrap();
    assert_eq!(fs::metadata(fixture.path()).unwrap().len(), 0);
    for entry in fs::read_dir(&fixture.0).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert!(
                !String::from_utf8(fs::read(path.join("state.json")).unwrap())
                    .unwrap()
                    .contains("secret-capability")
            );
        }
    }
}

#[test]
fn rejects_conflicting_sources_corrupt_and_truncated_prefixes() {
    for truncate in [false, true] {
        let fixture = Fixture::new();
        let mut retained = open(&fixture, b"abcdef");
        retained.append(b"abc").unwrap();
        retained.checkpoint().unwrap();
        let path = retained.payload_path();
        drop(retained);
        fs::write(
            &path,
            if truncate {
                b"a".as_slice()
            } else {
                b"xyz".as_slice()
            },
        )
        .unwrap();
        assert!(
            RetainedTransfer::open(
                &fixture.path(),
                evidence(b"abcdef"),
                Instant::now() + Duration::from_secs(1),
                |_| {}
            )
            .is_err()
        );
    }
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    drop(retained);
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"ABCDEF"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
}

#[test]
fn locks_transfers_discards_only_uncommitted_tails_and_protects_destinations() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"abcdef"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    retained.append(b"bad").unwrap();
    drop(retained);
    let mut retained = open(&fixture, b"abcdef");
    assert_eq!(retained.offset(), 3);
    retained.discard_uncommitted().unwrap();
    retained.append(b"def").unwrap();
    fs::write(fixture.path(), b"existing").unwrap();
    assert!(retained.finish().is_err());
    assert_eq!(fs::read(fixture.path()).unwrap(), b"existing");
}

#[test]
fn recovers_a_lost_final_publication_acknowledgement() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abc");
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    fs::hard_link(retained.payload_path(), fixture.path()).unwrap();
    drop(retained);
    assert_eq!(open(&fixture, b"abc").finish().unwrap().verified_bytes, 3);
}

#[test]
fn external_transfer_tail_requires_source_proof_and_final_disk_checksum() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    fs::write(retained.payload_path(), b"abc").unwrap();
    retained
        .reconcile_external(
            |offset, length| {
                assert_eq!((offset, length), (0, 3));
                Ok(STANDARD.encode(Sha256::digest(b"abc")))
            },
            |_| {},
        )
        .unwrap();
    retained.mark_resumed();
    retained.append(b"def").unwrap();
    // The final checksum measures disk bytes rather than trusting streaming state.
    fs::write(retained.payload_path(), b"xxxxxx").unwrap();
    assert!(retained.finish().is_err());
    assert!(!fixture.path().exists());
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    fs::write(retained.payload_path(), b"bad").unwrap();
    assert!(
        retained
            .reconcile_external(|_, _| Ok(STANDARD.encode(Sha256::digest(b"abc"))), |_| {})
            .is_err()
    );
}

#[test]
fn checks_overall_deadlines_during_local_verification() {
    let fixture = Fixture::new();
    let mut retained =
        RetainedTransfer::open(&fixture.path(), evidence(b"abc"), Instant::now(), |_| {}).unwrap();
    assert!(retained.append(b"abc").is_err());
    assert!(retained.finish().is_err());
}

#[test]
fn connection_budget_bounds_a_stalled_tls_handshake_and_keeps_the_prefix() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("https://{}/bounded", listener.local_addr().unwrap());
    let (done, finished) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        // Accept TCP but never answer TLS. The connection budget must win over
        // the longer stall and overall budgets, without discarding prior bytes.
        let _ = finished.recv_timeout(Duration::from_secs(3));
    });
    let started = Instant::now();
    let result = bulk::download_retained(
        &mut retained,
        || Ok(url.clone()),
        |_| {},
        TransferBudgets {
            connect: Duration::from_millis(100),
            stall: Duration::from_secs(2),
            overall: Duration::from_secs(3),
        },
        Instant::now() + Duration::from_secs(3),
    );
    done.send(()).unwrap();
    server.join().unwrap();
    assert!(matches!(result, Err(Error::Transport(error)) if error.is_timeout()));
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(retained);
    assert_eq!(open(&fixture, b"abcdef").offset(), 3);
}

#[test]
fn stall_and_overall_budgets_preserve_received_bytes() {
    for overall in [false, true] {
        let fixture = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/bounded", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut byte = [0];
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            socket
                .write_all(&response(
                    206,
                    "Content-Range: bytes 0-2/3\r\n",
                    3,
                    "exact-version",
                    b"a",
                ))
                .unwrap();
            socket.flush().unwrap();
            thread::sleep(Duration::from_millis(350));
        });
        let mut retained = open(&fixture, b"abc");
        let budgets = TransferBudgets {
            connect: Duration::from_secs(1),
            stall: if overall {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(100)
            },
            overall: Duration::from_secs(1),
        };
        let deadline = Instant::now()
            + if overall {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            };
        assert!(
            bulk::download_retained(&mut retained, || Ok(url.clone()), |_| {}, budgets, deadline)
                .is_err()
        );
        drop(retained);
        server.join().unwrap();
        assert_eq!(open(&fixture, b"abc").offset(), 1);
    }
}

#[test]
fn failed_authorization_never_replaces_progress_or_changes_identity() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    drop(retained);
    let mut retained = open(&fixture, b"abcdef");
    assert!(
        bulk::download_retained(
            &mut retained,
            || Err(Error::Invalid("authorization revoked".into())),
            |_| {},
            TransferBudgets::default(),
            Instant::now() + Duration::from_secs(1)
        )
        .is_err()
    );
    assert_eq!(retained.offset(), 3);
    assert_eq!(fs::read(retained.payload_path()).unwrap(), b"abc");
}

#[test]
fn missing_checkpoints_do_not_adopt_unowned_partials() {
    let fixture = Fixture::new();
    let mut retained = open(&fixture, b"abcdef");
    retained.append(b"abc").unwrap();
    retained.checkpoint().unwrap();
    let payload = retained.payload_path();
    drop(retained);
    fs::remove_file(payload.parent().unwrap().join("state.json")).unwrap();
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"abcdef"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fs::read(payload).unwrap(), b"abc");
    assert!(!fixture.path().exists());
}

#[test]
fn oversized_stream_chunks_still_commit_exact_checkpoint_intervals() {
    let fixture = Fixture::new();
    let bytes = vec![7; 9 * 1024 * 1024];
    let mut retained = open(&fixture, &bytes);
    retained.append(&bytes).unwrap();
    let state = retained.payload_path().parent().unwrap().join("state.json");
    let record: serde_json::Value = serde_json::from_slice(&fs::read(state).unwrap()).unwrap();
    assert_eq!(record["offset"], bulk::CHECKPOINT_BYTES);
    drop(retained);
    let mut retained = open(&fixture, &bytes);
    assert_eq!(retained.offset(), bulk::CHECKPOINT_BYTES);
    retained.discard_uncommitted().unwrap();
    retained
        .append(&bytes[bulk::CHECKPOINT_BYTES as usize..])
        .unwrap();
    let result = retained.finish().unwrap();
    assert_eq!(
        (result.resumed_bytes, result.transferred_bytes),
        (8 * 1024 * 1024, 1024 * 1024)
    );
}

#[cfg(unix)]
#[test]
fn refuses_to_truncate_a_partial_with_an_unowned_hard_link() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let retained = open(&fixture, b"abc");
    let payload = retained.payload_path();
    drop(retained);
    let other = fixture.0.join("other");
    fs::write(&other, b"original").unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&payload).unwrap();
    fs::hard_link(&other, payload).unwrap();
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"abc"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fs::read(other).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn rejects_dangling_checkpoint_symlinks_without_replacing_them() {
    let fixture = Fixture::new();
    let retained = open(&fixture, b"abc");
    let state = retained.payload_path().parent().unwrap().join("state.json");
    drop(retained);
    fs::remove_file(&state).unwrap();
    std::os::unix::fs::symlink(fixture.0.join("missing"), &state).unwrap();
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"abc"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
    assert!(
        fs::symlink_metadata(state)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_transfer_payloads() {
    let fixture = Fixture::new();
    let retained = open(&fixture, b"abc");
    let payload = retained.payload_path();
    drop(retained);
    fs::remove_file(&payload).unwrap();
    let other = fixture.0.join("other");
    fs::write(&other, b"abc").unwrap();
    std::os::unix::fs::symlink(&other, payload).unwrap();
    assert!(
        RetainedTransfer::open(
            &fixture.path(),
            evidence(b"abc"),
            Instant::now() + Duration::from_secs(1),
            |_| {}
        )
        .is_err()
    );
    assert_eq!(fs::read(other).unwrap(), b"abc");
}
