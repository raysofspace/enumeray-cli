use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    thread,
};
use tempfile::TempDir;
fn cli() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_enumeray"));
    c.env_remove("ENUMERAY_TOKEN").arg("--json");
    c
}
fn parsed(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
fn inspect() -> Value {
    parsed(cli().args(["compatibility", "inspect"]).output().unwrap())
}
fn selection(i: &Value) -> Value {
    json!({"schema_version":1,"service_repository":"raysofspace/enumeray","public_repository":"raysofspace/enumeray-cli","service_revision":"1".repeat(40),"public_revision":i["source"]["revision"],"version":i["version"],"public_content_sha256":i["source"]["content_sha256"],"baseline":{"id":"persistent-v1","sha256":"2".repeat(64),"status":"candidate"}})
}
fn write(p: &Path, v: &Value) {
    fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}
fn check(d: &Path, s: &Value, n: &str) -> Command {
    let p = d.join(format!("{n}-selection.json"));
    write(&p, s);
    let mut c = cli();
    c.args(["compatibility", "check", "--development-selection"])
        .arg(p)
        .arg("--receipt")
        .arg(d.join(format!("{n}-receipt.json")));
    c
}
fn failed(o: Output, status: &str) -> Value {
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["status"], status);
    v
}

#[test]
fn inspect_is_credential_free_and_embedded_identity_is_stable() {
    let d = TempDir::new().unwrap();
    let p = d.path().join("invalid-profiles.json");
    fs::write(&p, b"not-json").unwrap();
    let first = parsed(
        cli()
            .arg("--config")
            .arg(&p)
            .env("ENUMERAY_TOKEN", "never-print-this-token")
            .args(["compatibility", "inspect"])
            .output()
            .unwrap(),
    );
    assert_eq!(first["source"], inspect()["source"]);
    assert_eq!(first["wrapper_protocols"], json!([1]));
    assert_eq!(first["executable_sha256"].as_str().unwrap().len(), 64);
    assert!(!first.to_string().contains("never-print-this-token"));
    assert_eq!(fs::read(&p).unwrap(), b"not-json");
}
#[test]
fn unsigned_local_match_saves_private_receipt_and_cannot_overwrite_evidence() {
    let d = TempDir::new().unwrap();
    let s = selection(&inspect());
    let r = parsed(
        check(d.path(), &s, "match")
            .arg("--local-only")
            .output()
            .unwrap(),
    );
    assert_eq!(r["status"], "match");
    assert_eq!(r["trust"], "unsigned_development");
    let p = d.path().join("match-receipt.json");
    let b = fs::read(&p).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&b).unwrap(), r);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        check(d.path(), &s, "match")
            .arg("--local-only")
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(fs::read(p).unwrap(), b);
}
#[test]
fn mismatched_source_unknown_fingerprint_and_unavailable_service_never_pass() {
    let d = TempDir::new().unwrap();
    let i = inspect();
    let mut s = selection(&i);
    s["public_revision"] = json!("f".repeat(40));
    failed(
        check(d.path(), &s, "wrong")
            .arg("--local-only")
            .output()
            .unwrap(),
        "mismatch",
    );
    let mut s = selection(&i);
    failed(
        check(d.path(), &s, "service")
            .arg("--config")
            .arg(d.path().join("absent"))
            .output()
            .unwrap(),
        "missing_evidence",
    );
    if i["source"]["clean"] == false {
        s.as_object_mut().unwrap().remove("public_content_sha256");
        failed(
            check(d.path(), &s, "fingerprint")
                .arg("--local-only")
                .output()
                .unwrap(),
            "missing_evidence",
        );
    }
}
#[test]
fn every_supplied_peer_docs_and_context_is_required() {
    let d = TempDir::new().unwrap();
    let i = inspect();
    let s = selection(&i);
    let mut peer = i.clone();
    peer["wrapper_protocols"] = json!([1, 2]);
    let p = d.path().join("peer.json");
    write(&p, &peer);
    failed(
        check(d.path(), &s, "peer")
            .arg("--local-only")
            .arg("--peer")
            .arg(&p)
            .output()
            .unwrap(),
        "mismatch",
    );
    failed(
        check(d.path(), &s, "docs")
            .arg("--local-only")
            .arg("--docs")
            .arg(d.path().join("absent"))
            .output()
            .unwrap(),
        "missing_evidence",
    );
    let p = d.path().join("context.json");
    let mut b = i.clone();
    b.as_object_mut().unwrap().remove("executable_sha256");
    b.as_object_mut().unwrap().remove("observed_at");
    write(
        &p,
        &json!({"schema_version":1,"context_id":"process-direct","observed_at":i["observed_at"],"build_identity":b}),
    );
    assert_eq!(
        parsed(
            check(d.path(), &s, "context")
                .arg("--local-only")
                .arg("--context")
                .arg(&p)
                .output()
                .unwrap()
        )["status"],
        "match"
    );
}
fn server(bodies: Vec<Value>) -> (String, thread::JoinHandle<Vec<String>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    let w = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in bodies {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut r = Vec::new();
            let mut b = [0];
            while !r.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut b).unwrap();
                r.push(b[0]);
            }
            requests.push(String::from_utf8(r).unwrap());
            let body = body.to_string();
            write!(s,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        }
        requests
    });
    (origin, w)
}
#[test]
fn service_and_reported_connector_match_without_refreshing_credentials() {
    let d = TempDir::new().unwrap();
    let i = inspect();
    let s = selection(&i);
    let mut b = i.clone();
    b.as_object_mut().unwrap().remove("observed_at");
    b.as_object_mut().unwrap().remove("executable_sha256");
    let api = json!({"x-enumeray-build":{"schema_version":1,"version":i["version"],"service_source":{"repository":"https://github.com/raysofspace/enumeray","revision":s["service_revision"],"clean":true},"public_source":i["source"],"public_revision":s["public_revision"],"baseline":s["baseline"]}});
    let report = json!({"connector_id":"selected","build_observation":{"identity":b,"received_at":i["observed_at"],"recent":true,"credential_current":true}});
    let (origin, w) = server(vec![api, report]);
    let r = parsed(
        check(d.path(), &s, "online")
            .arg("--api")
            .arg(origin)
            .arg("--connector")
            .arg("selected")
            .arg("--config")
            .arg(d.path().join("absent"))
            .env("ENUMERAY_TOKEN", "static-do-not-print")
            .output()
            .unwrap(),
    );
    assert_eq!(r["status"], "match");
    assert!(!r.to_string().contains("static-do-not-print"));
    let requests = w.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/v1/openapi.json"));
    assert!(!requests[0].to_lowercase().contains("authorization:"));
    assert!(requests[1].starts_with("GET /api/v1/connectors/selected/diagnostics"));
    assert!(requests[1].contains("static-do-not-print"));
}
#[test]
fn unsigned_receipt_cannot_be_relabelled_as_verified_offline_evidence() {
    let d = TempDir::new().unwrap();
    parsed(
        check(d.path(), &selection(&inspect()), "unsigned")
            .arg("--local-only")
            .output()
            .unwrap(),
    );
    let p = d.path().join("unsigned-receipt.json");
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(fs::read(&p).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let o = cli()
        .args(["compatibility", "check", "--transferred-receipt"])
        .arg(p)
        .arg("--transferred-sha256")
        .arg(h)
        .arg("--receipt")
        .arg(d.path().join("transfer.json"))
        .arg("--local-only")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(!d.path().join("transfer.json").exists());
}

#[test]
fn incomplete_or_unsupported_development_peers_and_wrong_docs_service_do_not_match() {
    let d = TempDir::new().unwrap();
    let i = inspect();
    let s = selection(&i);
    let path = d.path().join("peer.json");
    for (name, field) in [
        ("missing-target", "target"),
        ("missing-tree", "tree"),
        ("missing-lockfile", "lockfile_sha256"),
    ] {
        let mut peer = i.clone();
        if field == "target" {
            peer.as_object_mut().unwrap().remove(field);
        } else {
            peer["source"].as_object_mut().unwrap().remove(field);
        }
        write(&path, &peer);
        failed(
            check(d.path(), &s, name)
                .arg("--local-only")
                .arg("--peer")
                .arg(&path)
                .output()
                .unwrap(),
            "missing_evidence",
        );
    }
    let mut peer = i.clone();
    peer["target"] = json!("unsupported-target");
    write(&path, &peer);
    failed(
        check(d.path(), &s, "unsupported-target")
            .arg("--local-only")
            .arg("--peer")
            .arg(&path)
            .output()
            .unwrap(),
        "mismatch",
    );
    let docs = d.path().join("docs.json");
    write(
        &docs,
        &json!({"schema_version":1,"root":{"repository":"https://github.com/raysofspace/enumeray","revision":"f".repeat(40),"clean":true},"public":i["source"]}),
    );
    failed(
        check(d.path(), &s, "wrong-docs-service")
            .arg("--local-only")
            .arg("--docs")
            .arg(docs)
            .output()
            .unwrap(),
        "mismatch",
    );
}
