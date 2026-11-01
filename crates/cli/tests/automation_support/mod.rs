use serde_json::{Value, json};
use std::{fs, path::Path, process::Output};

pub fn specimen(
    root: &Path,
    origin: &str,
    name: &str,
    contract: Option<&str>,
    args: &[&str],
    output: &Output,
    replacements: &[(&str, &str)],
) -> Value {
    let stdout = if output.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&output.stdout).expect("structured stdout")
    };
    let stderr = String::from_utf8(output.stderr.clone()).expect("UTF-8 diagnostics");
    let mut transcript = json!({"schema_version":1,"contract":contract,"command":["enumeray","--json"],"arguments":args,"exit_code":output.status.code(),"stdout":stdout,"stderr":stderr});
    let mut replacements = replacements.to_vec();
    let root_text = root.to_string_lossy().into_owned();
    let canonical_root = root.canonicalize().unwrap().to_string_lossy().into_owned();
    replacements.extend([
        (canonical_root.as_str(), "/private/enumeray-example"),
        (root_text.as_str(), "/private/enumeray-example"),
        (origin, "https://enumeray.example.org/"),
    ]);
    normalize(&mut transcript, &replacements);
    save(name, &transcript);
    transcript
}

pub fn save(name: &str, value: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/automation")
        .join(name);
    let bytes = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    if std::env::var_os("ENUMERAY_UPDATE_AUTOMATION_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap_or_else(|e| panic!(
            "{}: {e}; refresh with ENUMERAY_UPDATE_AUTOMATION_FIXTURES=1",
            path.display()
        )),
        bytes,
        "CLI transcript changed: {}",
        path.display()
    );
    assert!(!bytes.contains("test-token"));
    assert!(!bytes.contains("fixture-secret"));
}

pub fn normalize(value: &mut Value, replacements: &[(&str, &str)]) {
    match value {
        Value::String(text) => {
            for (from, to) in replacements {
                *text = text.replace(from, to);
            }
            // This local filename hashes the original absolute destination.
            // Redact that derived path identity along with the temporary root.
            let prefix = ".enumeray-download-";
            let mut cursor = 0;
            while let Some(offset) = text[cursor..].find(prefix) {
                let start = cursor + offset + prefix.len();
                let end = start + 64;
                if text
                    .get(start..end)
                    .is_some_and(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    text.replace_range(start..end, &"0".repeat(64));
                    cursor = end;
                } else {
                    cursor = start;
                }
            }
        }
        Value::Object(object) => {
            for child in object.values_mut() {
                normalize(child, replacements);
            }
        }
        Value::Array(values) => {
            for child in values {
                normalize(child, replacements);
            }
        }
        _ => {}
    }
}
