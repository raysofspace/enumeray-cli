use crate::config;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use enumeray_client::{
    EnumerayClient, Error, UploadChecksum, UploadReceipt, UploadRequest, UploadTransfer,
    UploadView, resource_path,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadOperation {
    pub schema_version: u32,
    pub api_url: String,
    pub user_id: uuid::Uuid,
    pub source: PathBuf,
    pub idempotency_key: String,
    pub request: UploadRequest,
    pub reservation: Option<UploadReceipt>,
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multipart: Option<MultipartCheckpoint>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultipartCheckpoint {
    pub part_size_bytes: u64,
    pub part_count: i32,
    pub confirmed: std::collections::BTreeMap<i32, String>,
}

pub(crate) fn download_authorization_hint(error: Error) -> Error {
    if matches!(
        &error,
        Error::Api {
            status: 401 | 403,
            ..
        }
    ) {
        eprintln!(
            "Download authorization is unavailable. Check scopes/current access and renew the same account with auth refresh/login, then repeat this download or collection; retained progress is unchanged."
        );
    }
    error
}

pub fn prepare(
    client: &EnumerayClient,
    source: &Path,
    name: String,
    media_type: String,
    metadata: Value,
    state: &Path,
) -> Result<Value, Error> {
    let deadline = transfer_deadline()?;
    if !metadata.is_object() {
        return Err(Error::Invalid(
            "Upload metadata must be a JSON object.".into(),
        ));
    }
    enumeray_contracts::models::validate_input_value(&metadata)
        .map_err(|error| Error::Invalid(error.to_string()))?;
    let source = source.canonicalize()?;
    let (size, checksum) = checksum_until(&mut source_file(&source)?, deadline)?;
    let operation = UploadOperation {
        schema_version: 1,
        api_url: client.base_url().to_owned(),
        user_id: client
            .with_timeout_limit(remaining(deadline))?
            .me()?
            .user_id,
        source,
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        request: UploadRequest {
            logical_name: name,
            media_type,
            scientific_metadata: metadata,
            expected_size_bytes: size,
            checksum: UploadChecksum {
                algorithm: "sha256".into(),
                checksum_type: "full_object".into(),
                value: checksum,
            },
        },
        reservation: None,
        version_id: None,
        multipart: None,
    };
    let _lock = config::lock_private(
        state,
        "Upload operation is already in use; resume after the other command exits.",
    )?;
    config::write_private(state, &operation, false)?;
    eprintln!(
        "Upload operation saved to {}. Reuse `artifact resume --state` after an interruption.",
        state.display()
    );
    resume_locked(client, state, deadline, &mut Default::default())
}

pub fn resume(client: &EnumerayClient, state: &Path) -> Result<Value, Error> {
    resume_observed(client, state).map(|v| v.0)
}
pub(crate) fn resume_observed(
    client: &EnumerayClient,
    state: &Path,
) -> Result<(Value, enumeray_client::bulk::TransferProgress), Error> {
    let deadline = transfer_deadline()?;
    let _lock = config::lock_private(
        state,
        "Upload operation is already in use; resume after the other command exits.",
    )?;
    let mut progress = enumeray_client::bulk::TransferProgress::default();
    let receipt = resume_locked(client, state, deadline, &mut progress)?;
    progress.verified_bytes = receipt["size_bytes"]
        .as_u64()
        .ok_or_else(|| Error::Invalid("Missing verified upload bytes.".into()))?;
    if progress
        .resumed_bytes
        .checked_add(progress.transferred_bytes)
        != Some(progress.verified_bytes)
    {
        return Err(Error::Invalid(
            "Upload progress conflicts with the verified payload.".into(),
        ));
    }
    Ok((receipt, progress))
}

fn transfer_deadline() -> Result<std::time::Instant, Error> {
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env()
        .map_err(|message| Error::Invalid(message.into()))?;
    Ok(std::time::Instant::now() + budgets.overall)
}

fn remaining(deadline: std::time::Instant) -> std::time::Duration {
    deadline.saturating_duration_since(std::time::Instant::now())
}

fn resume_locked(
    client: &EnumerayClient,
    state: &Path,
    deadline: std::time::Instant,
    payload: &mut enumeray_client::bulk::TransferProgress,
) -> Result<Value, Error> {
    let authorized = || client.with_timeout_limit(remaining(deadline));
    config::require_private(state)?;
    let mut operation: UploadOperation = config::read_json(state)?;
    if operation.schema_version != 1
        || operation.api_url != client.base_url()
        || operation.user_id != authorized()?.me()?.user_id
    {
        return Err(Error::Invalid(
            "Upload state belongs to another API/user or unsupported format.".into(),
        ));
    }
    if operation.reservation.is_none() {
        let view: UploadView = authorized()?.request(
            Method::POST,
            "/api/v1/uploads",
            Some(&operation.request),
            Some(&operation.idempotency_key),
        )?;
        if view.reservation.upload_id.is_nil() || view.reservation.artifact_id.is_nil() {
            return Err(Error::Invalid(
                "Upload reservation omitted its identity; retain the saved operation and retry."
                    .into(),
            ));
        }
        operation.reservation = Some(view.reservation);
        config::write_private(state, &operation, true)?;
    }
    payload.total_bytes = operation.request.expected_size_bytes;
    let id = operation
        .reservation
        .as_ref()
        .expect("set above")
        .upload_id
        .to_string();
    let view: UploadView = authorized()?.get(&resource_path(&["uploads", &id])?)?;
    let reservation = operation.reservation.as_ref().expect("set above");
    if view.reservation.upload_id != reservation.upload_id
        || view.reservation.artifact_id != reservation.artifact_id
    {
        return Err(Error::Invalid(
            "Upload status conflicts with the saved reservation; retain the saved operation."
                .into(),
        ));
    }
    if view.state == "completed" {
        let completion = view.completion.ok_or_else(|| {
            Error::Invalid(
                "Completed upload omitted its receipt; retain the saved operation and retry."
                    .into(),
            )
        })?;
        validate_completion(&operation, &completion)?;
        payload.resumed_bytes = operation.request.expected_size_bytes;
        return Ok(completion);
    }
    if matches!(view.transfer, UploadTransfer::Multipart { .. }) {
        return resume_multipart(client, state, &mut operation, deadline, payload);
    }
    if operation.version_id.is_none() {
        let UploadTransfer::Available { request } = view.transfer else {
            return Err(Error::Invalid("Upload transfer is unavailable or expired; inspect the saved reservation before retrying.".into()));
        };
        let mut file = source_file(&operation.source)?;
        let (size, digest) = checksum_until(&mut file, deadline)?;
        if size != operation.request.expected_size_bytes
            || digest != operation.request.checksum.value
        {
            return Err(Error::Invalid(
                "Source file changed since this upload was prepared.".into(),
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        eprintln!("Transferring {size} bytes...");
        operation.version_id =
            Some(client.transfer_upload_with_budget(&request, file, remaining(deadline))?);
        config::write_private(state, &operation, true)?;
        payload.transferred_bytes = size;
    } else {
        payload.resumed_bytes = operation.request.expected_size_bytes;
    }
    let completion = authorized()?.request(
        Method::PUT,
        &resource_path(&["uploads", &id, "completion"])?,
        Some(&json!({"version_id":operation.version_id})),
        None,
    )?;
    validate_completion(&operation, &completion)?;
    Ok(completion)
}

fn source_file(path: &Path) -> Result<File, Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::Invalid(
            "Upload source must remain a regular file, not a symlink.".into(),
        ));
    }
    Ok(File::open(path)?)
}

fn validate_completion(operation: &UploadOperation, completion: &Value) -> Result<(), Error> {
    let reservation = operation
        .reservation
        .as_ref()
        .expect("reserved before completion");
    if completion["upload_id"] != reservation.upload_id.to_string()
        || completion["artifact_id"] != reservation.artifact_id.to_string()
        || completion["size_bytes"] != operation.request.expected_size_bytes
        || completion["checksum"] != serde_json::to_value(&operation.request.checksum)?
        || completion["version_id"]
            .as_str()
            .is_none_or(|version| version.is_empty() || version == "null")
        || operation
            .version_id
            .as_ref()
            .is_some_and(|version| completion["version_id"] != *version)
        || completion["completed_at"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return Err(Error::Invalid("Upload completion conflicts with the saved identity or verified content; retain the saved operation and retry.".into()));
    }
    Ok(())
}

pub fn download(
    client: &EnumerayClient,
    path: &str,
    destination: &Path,
    evidence: Option<(u64, String, String)>,
) -> Result<Value, Error> {
    if let Some((size, checksum, version_id)) = evidence {
        use enumeray_client::bulk::{self, DownloadEvidence, RetainedTransfer};
        let budgets = enumeray_contracts::transfers::TransferBudgets::from_env()
            .map_err(|message| Error::Invalid(message.into()))?;
        let deadline = std::time::Instant::now() + budgets.overall;
        let user = client
            .with_timeout_limit(remaining(deadline))?
            .me()?
            .user_id;
        let evidence = DownloadEvidence {
            scope: format!("{}user/{user}", client.base_url()),
            source: path.into(),
            version_id: Some(version_id),
            size_bytes: size,
            checksum_sha256: checksum,
        };
        eprintln!(
            "Retained download checkpoint: {}",
            bulk::checkpoint_directory(destination)?.display()
        );
        let mut retained = RetainedTransfer::open(destination, evidence, deadline, |_| {})?;
        eprintln!("Resuming {} of {size} bytes.", retained.offset());
        let mut reported = std::time::Instant::now();
        bulk::download_retained(
            &mut retained,
            || {
                client
                    .with_timeout_limit(
                        deadline.saturating_duration_since(std::time::Instant::now()),
                    )?
                    .download_location(path)
            },
            |progress| {
                if reported.elapsed() >= std::time::Duration::from_secs(1) {
                    eprintln!(
                        "Downloaded {} of {} bytes ({} reused).",
                        progress.resumed_bytes + progress.transferred_bytes,
                        progress.total_bytes,
                        progress.resumed_bytes
                    );
                    reported = std::time::Instant::now();
                }
            },
            budgets,
            deadline,
        )?;
        let progress = retained.finish()?;
        return Ok(
            json!({"path":destination,"size_bytes":size,"resumed_bytes":progress.resumed_bytes,
            "transferred_bytes":progress.transferred_bytes,"verified_bytes":progress.verified_bytes}),
        );
    }
    if destination.exists() {
        return Err(Error::Invalid(
            "Download destination already exists.".into(),
        ));
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".enumeray-download-{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<Value, Error> {
        let mut file = options.open(&temporary)?;
        let size = client.download(path, &mut file)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, destination)?;
        std::fs::remove_file(&temporary)?;
        File::open(parent)?.sync_all()?;
        Ok(json!({"path":destination,"size_bytes":size}))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub fn checksum(reader: &mut impl Read) -> Result<(u64, String), Error> {
    checksum_until(reader, transfer_deadline()?)
}

fn checksum_until(
    reader: &mut impl Read,
    deadline: std::time::Instant,
) -> Result<(u64, String), Error> {
    let started = std::time::Instant::now();
    let mut reported = started;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(Error::Invalid(
                "File hashing reached its bulk deadline; retain the source and retry.".into(),
            ));
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        size += count as u64;
        if std::time::Instant::now() >= deadline {
            return Err(Error::Invalid(
                "File hashing reached its bulk deadline; retain the source and retry.".into(),
            ));
        }
        if reported.elapsed() >= std::time::Duration::from_secs(5) {
            eprintln!("Hashed {size} bytes.");
            reported = std::time::Instant::now();
        }
    }
    Ok((size, STANDARD.encode(digest.finalize())))
}

fn resume_multipart(
    client: &EnumerayClient,
    state: &Path,
    operation: &mut UploadOperation,
    deadline: std::time::Instant,
    payload: &mut enumeray_client::bulk::TransferProgress,
) -> Result<Value, Error> {
    use enumeray_client::{PartAuthorization, UploadPart, UploadParts};
    use std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    };
    let id = operation
        .reservation
        .as_ref()
        .expect("reserved upload")
        .upload_id
        .to_string();
    let base = resource_path(&["uploads", &id])?;
    let authorized = || client.with_timeout_limit(remaining(deadline));
    let mut file = source_file(&operation.source)?;
    let (size, digest) = checksum_until(&mut file, deadline)?;
    if size != operation.request.expected_size_bytes || digest != operation.request.checksum.value {
        return Err(Error::Invalid(
            "Source file changed since this upload was prepared.".into(),
        ));
    }
    let progress = loop {
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "Bulk upload deadline reached; retain the saved operation and resume.".into(),
            ));
        }
        let view: UploadView = authorized()?.get(&base)?;
        if view.state == "completed" {
            let receipt = view
                .completion
                .ok_or_else(|| Error::Invalid("Completed upload omitted its receipt.".into()))?;
            validate_completion(operation, &receipt)?;
            payload.resumed_bytes = operation.request.expected_size_bytes;
            return Ok(receipt);
        }
        if view.state != "pending" {
            return Err(Error::Invalid(
                "Multipart reservation expired; retain its saved identity.".into(),
            ));
        }
        let UploadTransfer::Multipart { progress } = view.transfer else {
            return Err(Error::Invalid(
                "Multipart transport changed unexpectedly.".into(),
            ));
        };
        match progress.phase.as_str() {
            "initializing" => std::thread::sleep(Duration::from_secs(1)),
            "uploading" | "completing" | "verifying" | "verified" => break progress,
            _ => {
                return Err(Error::Invalid(
                    "Multipart upload failed or was aborted; retain its saved state.".into(),
                ));
            }
        }
    };
    if progress.part_size_bytes == 0
        || !(1..=10000).contains(&progress.part_count)
        || size.div_ceil(progress.part_size_bytes) != progress.part_count as u64
    {
        return Err(Error::Invalid("Invalid multipart layout.".into()));
    }
    if let Some(saved) = &operation.multipart {
        if saved.part_size_bytes != progress.part_size_bytes
            || saved.part_count != progress.part_count
        {
            return Err(Error::Invalid(
                "Multipart layout conflicts with saved evidence.".into(),
            ));
        }
    } else {
        operation.multipart = Some(MultipartCheckpoint {
            part_size_bytes: progress.part_size_bytes,
            part_count: progress.part_count,
            confirmed: BTreeMap::new(),
        });
        config::write_private(state, operation, true)?;
    }
    if progress.phase == "uploading" {
        let mut known = BTreeMap::new();
        let mut after = 0;
        loop {
            let page: UploadParts = authorized()?.get(&format!("{base}/parts?after={after}"))?;
            if page.parts.len() > 100 {
                return Err(Error::Invalid("Multipart page exceeds its bound.".into()));
            }
            for part in page.parts {
                if part.part_number <= after
                    || part.part_number > progress.part_count
                    || known.insert(part.part_number, part).is_some()
                {
                    return Err(Error::Invalid("Invalid multipart part inventory.".into()));
                }
            }
            match page.next_after {
                Some(next) if next > after && next <= progress.part_count => after = next,
                None => break,
                _ => return Err(Error::Invalid("Invalid multipart cursor.".into())),
            }
        }
        for number in 1..=progress.part_count {
            if Instant::now() >= deadline {
                return Err(Error::Invalid(
                    "Bulk upload deadline reached; resume the saved operation.".into(),
                ));
            }
            let offset = (number as u64 - 1) * progress.part_size_bytes;
            let length = (size - offset).min(progress.part_size_bytes);
            file.seek(SeekFrom::Start(offset))?;
            let (measured, part_checksum) =
                checksum_until(&mut (&mut file).take(length), deadline)?;
            if measured != length {
                return Err(Error::Invalid(
                    "Source changed while reading an upload part.".into(),
                ));
            }
            let part_base = format!("{base}/parts/{number}");
            let mut acknowledged = None;
            if let Some(prior) = known.get(&number) {
                if prior.size_bytes != length || prior.checksum_sha256 != part_checksum {
                    return Err(Error::Invalid(
                        "Source part conflicts with retained upload evidence.".into(),
                    ));
                }
                if prior.etag.is_some() {
                    acknowledged = Some(prior.clone());
                } else {
                    match authorized()?.request::<UploadPart, Value>(
                        Method::PUT,
                        &format!("{part_base}/receipt"),
                        None::<&Value>,
                        None,
                    ) {
                        Ok(receipt) => acknowledged = Some(receipt),
                        Err(Error::Api { status: 409, .. }) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            let receipt = if let Some(receipt) = acknowledged {
                payload.resumed_bytes += length;
                eprintln!(
                    "Reusing confirmed part {number}/{} ({length} bytes).",
                    progress.part_count
                );
                receipt
            } else {
                let authorization: PartAuthorization = authorized()?.request(
                    Method::PUT,
                    &part_base,
                    Some(&json!({"checksum_sha256":part_checksum})),
                    None,
                )?;
                if authorization.part_number != number
                    || authorization.size_bytes != length
                    || authorization.checksum_sha256 != part_checksum
                {
                    return Err(Error::Invalid(
                        "Part authorization conflicts with local bytes.".into(),
                    ));
                }
                eprintln!(
                    "Transferring part {number}/{} ({length} bytes)...",
                    progress.part_count
                );
                client.transfer_part(
                    &authorization.request,
                    file.try_clone()?,
                    offset,
                    length,
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                payload.transferred_bytes += length;
                authorized()?.request(
                    Method::PUT,
                    &format!("{part_base}/receipt"),
                    None::<&Value>,
                    None,
                )?
            };
            if receipt.part_number != number
                || receipt.size_bytes != length
                || receipt.checksum_sha256 != part_checksum
                || receipt.etag.as_deref().is_none_or(str::is_empty)
            {
                return Err(Error::Invalid(
                    "Part receipt conflicts with uploaded content.".into(),
                ));
            }
            operation
                .multipart
                .as_mut()
                .expect("saved multipart")
                .confirmed
                .insert(number, part_checksum);
            config::write_private(state, operation, true)?;
        }
    }
    if progress.phase != "uploading" {
        payload.resumed_bytes = size;
    }
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Invalid(
                "Verification is pending; resume the saved upload operation.".into(),
            ));
        }
        let result: Value = authorized()?.request(
            Method::PUT,
            &format!("{base}/completion"),
            Some(&json!({"multipart":true})),
            None,
        )?;
        if result.get("upload_id").is_some() {
            validate_completion(operation, &result)?;
            operation.version_id = result["version_id"].as_str().map(str::to_owned);
            config::write_private(state, operation, true)?;
            return Ok(result);
        }
        let view: UploadView = authorized()?.get(&base)?;
        if view.state != "pending" && view.state != "completed" {
            return Err(Error::Invalid(
                "Multipart upload expired before publication.".into(),
            ));
        }
        if matches!(view.transfer,UploadTransfer::Multipart {ref progress} if matches!(progress.phase.as_str(),"failed"|"aborted"))
        {
            return Err(Error::Invalid(
                "Multipart verification failed; retain saved state.".into(),
            ));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
