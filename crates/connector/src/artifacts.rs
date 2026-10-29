//! Assignment-scoped verified transfers. Storage capability URLs never reach a
//! wrapper, its request file, its environment, or connector diagnostics.
use crate::{
    Result,
    client::{Client, http_error, parse_json},
    failure, local_process,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use enumeray_contracts::models::{self, ScientificDefinition};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Wrapper-declared output. Integrity facts are measured after execution.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputFileDeclaration {
    pub path: String,
    pub media_type: String,
    #[serde(default = "empty_object")]
    pub scientific_metadata: Value,
}

/// Verified output retained in the connector journal and used for publication.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFile {
    pub path: String,
    pub media_type: String,
    pub scientific_metadata: Value,
    pub size_bytes: u64,
    pub sha256: String,
}
fn empty_object() -> Value {
    json!({})
}

pub(crate) fn transfer_key(intent: Uuid, name: &str, archive: bool, generation: u64) -> Uuid {
    let scope = if archive { "archive" } else { "output" };
    let text = if generation == 0 {
        format!("{scope}/{intent}/{name}")
    } else {
        format!("{scope}/{intent}/{name}/{generation}")
    };
    let digest = Sha256::digest(text.as_bytes());
    Uuid::from_bytes(digest[..16].try_into().expect("digest"))
}
fn retained_transfer_record(directory: &Path, legacy: &Path, name: &str) -> PathBuf {
    let scoped = directory.join(name);
    if scoped.exists() {
        scoped
    } else {
        legacy.join(name)
    }
}

pub fn input_directory(journal: &Path, intent: Uuid) -> PathBuf {
    journal.join("input-staging").join(intent.to_string())
}
pub fn stage_inputs(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<()> {
    crate::collections::stage_inputs(client, journal, intent, assignment)?;
    stage_inputs_at(
        client,
        journal,
        intent,
        assignment,
        input_directory(journal, intent),
        None,
    )
}
pub(crate) fn stage_inputs_at(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    directory: PathBuf,
    expected: Option<&enumeray_contracts::collections::PinnedMember>,
) -> Result<()> {
    let definition: ScientificDefinition = serde_json::from_value(assignment["definition"].clone())
        .map_err(|_| failure("invalid assigned contract"))?;
    let declarations = models::files::declarations(&definition)
        .map_err(|_| failure("invalid input file declarations"))?;
    if declarations.is_empty() {
        return Ok(());
    }
    local_process::create_private_directory(&journal.join("input-staging"))?;
    local_process::create_private_directory(&directory)?;
    for (name, declaration) in declarations {
        let Some(id) = assignment["inputs"][&name]["artifact_id"].as_str() else {
            continue;
        };
        let id = Uuid::parse_str(id).map_err(|_| failure("invalid input artifact reference"))?;
        let path = directory.join(id.to_string());
        let base = format!(
            "/connector/v1/assignments/{}/artifacts/{id}",
            assignment["assignment_id"]
                .as_str()
                .ok_or_else(|| failure("invalid assignment identity"))?,
        );
        let (status, bytes) = client.request(Method::GET, &base, None, None, None)?;
        if status != StatusCode::OK {
            return Err(http_error("input metadata", status));
        }
        let metadata = parse_json(&bytes)?;
        if let Some(expected) = expected
            && (metadata["content"]["version_id"] != expected.version_id
                || metadata["content"]["size_bytes"] != expected.member.size_bytes
                || metadata["content"]["checksum"]["value"] != expected.member.checksum_sha256)
        {
            return Err(failure(
                "collection member differs from its pinned evidence",
            ));
        }
        let size = metadata["content"]["size_bytes"]
            .as_u64()
            .filter(|v| *v <= declaration.max_bytes)
            .ok_or_else(|| failure("input artifact exceeds declared size"))?;
        let legacy = directory.join(format!(".{id}.part"));
        if legacy
            .try_exists()
            .map_err(|_| failure("cannot inspect legacy input partial"))?
        {
            return Err(failure(format!(
                "Legacy unverified input partial retained at {}; inspect and explicitly move it aside before restarting staging",
                legacy.display()
            )));
        }
        let partial = enumeray_client::bulk::checkpoint_directory(&path)
            .map_err(|e| failure(e.to_string()))?
            .join("payload.part");
        crate::spool::reserve_copy(journal, intent, &path, size, &partial)?;
        let checksum = metadata["content"]["checksum"]["value"]
            .as_str()
            .ok_or_else(|| failure("input artifact has no verified checksum"))?;
        if path.exists() {
            verify_file(&path, size, checksum)?;
            continue;
        }
        let version = metadata["content"]["version_id"]
            .as_str()
            .ok_or_else(|| failure("input artifact has no verified version"))?;
        let budgets =
            enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
        let deadline = std::time::Instant::now() + budgets.overall;
        let evidence = enumeray_client::bulk::DownloadEvidence {
            scope: format!(
                "{}/assignment/{}",
                client.origin(),
                assignment["assignment_id"]
            ),
            source: id.to_string(),
            version_id: Some(version.into()),
            size_bytes: size,
            checksum_sha256: checksum.into(),
        };
        crate::transfers::begin_file(size);
        let mut retained = enumeray_client::bulk::RetainedTransfer::open(
            &path,
            evidence,
            deadline,
            crate::transfers::advance,
        )
        .map_err(|e| failure(format!("Input checkpoint beside {}: {e}", path.display())))?;
        enumeray_client::bulk::download_retained(
            &mut retained,
            || {
                client
                    .transfer_location(&format!("{base}/content"))
                    .map_err(|e| enumeray_client::Error::Invalid(e.message))
            },
            crate::transfers::payload_progress,
            budgets,
            deadline,
        )
        .map_err(|e| {
            failure(format!(
                "Input range transfer beside {}: {e}; retain its original checkpoint",
                path.display()
            ))
            .with_kind(crate::ErrorKind::Retry)
        })?;
        let progress = retained
            .finish_with_progress(crate::transfers::advance)
            .map_err(|_| failure("input bytes conflict with pinned evidence"))?;
        crate::transfers::payload_progress(progress);
        crate::transfers::end_file();
    }
    Ok(())
}

pub fn copy_inputs(source: &Path, target: &Path, assignment: &Value) -> Result<()> {
    local_process::create_private_directory(target)?;
    for id in input_ids(assignment)? {
        let from = source.join(id.to_string());
        let to = target.join(id.to_string());
        if !from.is_file() {
            return Err(failure("verified input staging is missing"));
        }
        let (size, checksum) = file_digest(&from)?;
        if !to.exists() {
            use std::io::{Seek, SeekFrom};
            let budgets =
                enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
            let deadline = std::time::Instant::now() + budgets.overall;
            let evidence = enumeray_client::bulk::DownloadEvidence {
                scope: format!("local/{}", assignment["assignment_id"]),
                source: from.to_string_lossy().into_owned(),
                version_id: None,
                size_bytes: size,
                checksum_sha256: checksum.clone(),
            };
            let mut retained = enumeray_client::bulk::RetainedTransfer::open(
                &to,
                evidence,
                deadline,
                crate::transfers::advance,
            )
            .map_err(|e| failure(e.to_string()))?;
            retained
                .discard_uncommitted()
                .map_err(|e| failure(e.to_string()))?;
            let mut source =
                File::open(&from).map_err(|_| failure("cannot open local staging source"))?;
            source
                .seek(SeekFrom::Start(retained.offset()))
                .map_err(|_| failure("cannot seek local staging source"))?;
            let mut buffer = [0; 65536];
            loop {
                let n = source
                    .read(&mut buffer)
                    .map_err(|_| failure("cannot read local staging source"))?;
                if n == 0 {
                    break;
                }
                retained
                    .append(&buffer[..n])
                    .map_err(|e| failure(e.to_string()))?;
                crate::transfers::payload_progress(retained.progress());
            }
            retained
                .finish_with_progress(crate::transfers::advance)
                .map_err(|e| failure(e.to_string()))?;
        }
        verify_file(&to, size, &checksum)?;
    }
    Ok(())
}
pub fn input_ids(assignment: &Value) -> Result<Vec<Uuid>> {
    let definition: ScientificDefinition = serde_json::from_value(assignment["definition"].clone())
        .map_err(|_| failure("invalid assigned contract"))?;
    models::files::declarations(&definition)
        .map_err(|_| failure("invalid file contract"))?
        .keys()
        .filter_map(|name| assignment["inputs"][name]["artifact_id"].as_str())
        .map(|id| Uuid::parse_str(id).map_err(|_| failure("invalid artifact reference")))
        .collect()
}
pub fn request_files(assignment: &Value) -> Result<Value> {
    let definition: ScientificDefinition = serde_json::from_value(assignment["definition"].clone())
        .map_err(|_| failure("invalid assigned contract"))?;
    let mut files = serde_json::Map::new();
    for name in models::files::declarations(&definition)
        .map_err(|_| failure("invalid file contract"))?
        .keys()
    {
        if let Some(id) = assignment["inputs"][name]["artifact_id"].as_str() {
            let id = Uuid::parse_str(id).map_err(|_| failure("invalid input reference"))?;
            files.insert(
                name.clone(),
                json!({"path":format!("inputs/{id}"),"artifact_id":id}),
            );
        }
    }
    Ok(Value::Object(files))
}

pub fn confined_file(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(failure(
            "file path is not confined to the assignment workspace",
        ));
    }
    let mut current = root.to_path_buf();
    for part in path.components() {
        current.push(part);
        let metadata =
            fs::symlink_metadata(&current).map_err(|_| failure("declared file is missing"))?;
        if metadata.file_type().is_symlink() {
            return Err(failure("declared file traverses a symbolic link"));
        }
    }
    if !fs::metadata(&current).is_ok_and(|m| m.is_file()) {
        return Err(failure("declared output is not a regular file"));
    }
    Ok(current)
}
pub fn file_digest(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).map_err(|_| failure("cannot read artifact bytes"))?;
    crate::transfers::begin_file(
        file.metadata()
            .map_err(|_| failure("cannot inspect artifact bytes"))?
            .len(),
    );
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    let mut size = 0;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| failure("cannot read artifact bytes"))?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        crate::transfers::advance(n as u64);
        size += n as u64;
    }
    crate::transfers::end_file();
    Ok((size, STANDARD.encode(digest.finalize())))
}
pub fn verify_file(path: &Path, size: u64, checksum: &str) -> Result<()> {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
        return Err(failure("artifact is not a regular retained file"));
    }
    let actual = file_digest(path)?;
    if actual.0 != size || actual.1 != checksum {
        return Err(failure("artifact bytes differ from verified evidence"));
    }
    Ok(())
}

pub fn publish(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    session: Uuid,
    files: &BTreeMap<String, OutputFile>,
    report: &Value,
) -> Result<Value> {
    if files.is_empty() {
        return Ok(report.clone());
    }
    let workspace = crate::compute::collect_files(journal, intent, assignment, files)?;
    publish_from(
        client, journal, intent, assignment, session, files, report, &workspace, false,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_from(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    session: Uuid,
    files: &BTreeMap<String, OutputFile>,
    report: &Value,
    workspace: &Path,
    archive: bool,
) -> Result<Value> {
    let mut report = report.clone();
    let legacy_directory = journal.join("artifact-transfers");
    local_process::create_private_directory(&legacy_directory)?;
    let directory = legacy_directory.join(intent.to_string());
    local_process::create_private_directory(&directory)?;
    for (name, file) in files {
        let path = confined_file(workspace, &file.path)?;
        let original_key = transfer_key(intent, name, archive, 0);
        let generation_name = format!("reservation-{original_key}.json");
        let generation_record =
            retained_transfer_record(&directory, &legacy_directory, &generation_name);
        let generation = if generation_record.exists() {
            let record: Value = local_process::read_record(&generation_record)?;
            record["generation"]
                .as_u64()
                .filter(|v| *v <= u32::MAX as u64)
                .ok_or_else(|| failure("invalid retained output reservation generation"))?
        } else {
            0
        };
        let key = transfer_key(intent, name, archive, generation);
        let base = format!(
            "/connector/v1/assignments/{}/{suffix}",
            assignment["assignment_id"]
                .as_str()
                .ok_or_else(|| failure("invalid assignment identity"))?,
            suffix = if archive {
                "log-archive/uploads"
            } else {
                "uploads"
            },
        );
        let request = json!({"logical_name":name,"media_type":file.media_type,"scientific_metadata":file.scientific_metadata,"expected_size_bytes":file.size_bytes,"checksum":{"algorithm":"sha256","checksum_type":"full_object","value":file.sha256}});
        let (status, bytes) = client.request(
            Method::POST,
            &base,
            Some(session),
            Some(key),
            Some(&request),
        )?;
        if status != StatusCode::CREATED {
            return Err(http_error("output reservation", status));
        }
        let view = parse_json(&bytes)?;
        if view["state"] == "expired" {
            let next = generation
                .checked_add(1)
                .filter(|v| *v <= u32::MAX as u64)
                .ok_or_else(|| failure("output reservation generation exhausted"))?;
            crate::capture::replace(&directory, &generation_name, &json!({"generation":next}))?;
            return Err(failure(
                "output reservation expired; retain finalized bytes and reserve a replacement",
            )
            .with_kind(crate::ErrorKind::Retry));
        }
        let upload = view["reservation"]["upload_id"]
            .as_str()
            .ok_or_else(|| failure("invalid output reservation"))?;
        let artifact = view["reservation"]["artifact_id"]
            .as_str()
            .ok_or_else(|| failure("invalid output artifact identity"))?;
        if view["state"] != "completed" && view["transfer"]["status"] == "multipart" {
            crate::multipart::publish(
                client, &base, upload, session, &path, file, &directory, key, &view,
            )?;
        } else if view["state"] != "completed" {
            verify_file(&path, file.size_bytes, &file.sha256)?;
            let record =
                retained_transfer_record(&directory, &legacy_directory, &format!("{key}.json"));
            let version: Value = if record.exists() {
                local_process::read_record(&record)?
            } else {
                let transfer = &view["transfer"]["request"];
                if view["transfer"]["status"] != "available" {
                    return Err(failure("output transfer is not available")
                        .with_kind(crate::ErrorKind::Retry));
                }
                if view["transfer"]["expires_at"]
                    .as_str()
                    .and_then(|t| {
                        time::OffsetDateTime::parse(
                            t,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .ok()
                    })
                    .is_some_and(|t| t <= time::OffsetDateTime::now_utc())
                {
                    return Err(failure(
                        "output transfer capability expired; refresh instructions",
                    )
                    .with_kind(crate::ErrorKind::Retry));
                }
                let url = transfer["url"]
                    .as_str()
                    .ok_or_else(|| failure("invalid output transfer"))?;
                crate::transfers::begin_file(file.size_bytes);
                let mut request = transfer_client()?
                    .put(url)
                    .body(reqwest::blocking::Body::sized(
                        crate::transfers::ProgressReader(
                            File::open(&path).map_err(|_| failure("output file disappeared"))?,
                        ),
                        file.size_bytes,
                    ));
                if transfer["method"] != "PUT" {
                    return Err(failure("unsupported storage transfer method"));
                }
                let headers: Vec<(String, String)> =
                    serde_json::from_value(transfer["headers"].clone())
                        .map_err(|_| failure("invalid transfer headers"))?;
                for (name, value) in headers {
                    request = request.header(name, value);
                }
                let response = request.send().map_err(|_| {
                    failure("output transfer interrupted").with_kind(crate::ErrorKind::Retry)
                })?;
                if !response.status().is_success() {
                    return Err(failure(
                        "output transfer capability rejected; refresh instructions",
                    )
                    .with_kind(crate::ErrorKind::Retry));
                }
                crate::transfers::payload_observation(enumeray_client::bulk::TransferProgress {
                    transferred_bytes: file.size_bytes,
                    total_bytes: file.size_bytes,
                    ..Default::default()
                });
                crate::transfers::end_file();
                let version = response
                    .headers()
                    .get("x-amz-version-id")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| failure("output transfer omitted immutable storage version"))?
                    .to_owned();
                let value = json!({"version_id":version});
                local_process::write_record(&directory, &format!("{key}.json"), &value)?;
                value
            };
            let (status, _) = client.request(
                Method::PUT,
                &format!("{base}/{upload}/completion"),
                Some(session),
                None,
                Some(&version),
            )?;
            if status != StatusCode::OK {
                return Err(http_error("output verification", status));
            }
        }
        crate::transfers::verified_payload(file.size_bytes);
        let reference = json!({"artifact_id":artifact});
        report["outputs"][name] = reference.clone();
        report["files"][name] = reference;
    }
    Ok(report)
}
fn transfer_client() -> Result<reqwest::blocking::Client> {
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(budgets.connect)
        .timeout(budgets.overall)
        .build()
        .map_err(|_| failure("cannot create storage transfer client"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_and_symlinks() {
        let root = std::env::temp_dir().join(format!("enumeray-confinement-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("output"), b"abc").unwrap();
        assert!(confined_file(&root, "output").is_ok());
        for path in ["../secret", "/etc/passwd", "a/../../secret", ""] {
            assert!(confined_file(&root, path).is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap();
            assert!(confined_file(&root, "escape").is_err());
        }
        assert!(verify_file(&root.join("output"), 3, "bad").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
