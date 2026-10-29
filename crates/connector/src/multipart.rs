//! Sequential, recoverable assignment output parts. Only service receipts settle delivery.
use crate::{
    ErrorKind, Result,
    client::{Client, http_error, parse_json},
    failure,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Instant,
};
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    client: &Client,
    base: &str,
    upload: &str,
    session: Uuid,
    path: &Path,
    file: &crate::artifacts::OutputFile,
    directory: &Path,
    key: Uuid,
    view: &Value,
) -> Result<()> {
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let deadline = Instant::now() + budgets.overall;
    let progress = &view["transfer"]["progress"];
    let phase = progress["phase"]
        .as_str()
        .ok_or_else(|| failure("multipart phase absent"))?;
    if phase == "initializing" {
        return Err(failure("multipart initialization is pending").with_kind(ErrorKind::Retry));
    }
    if matches!(phase, "failed" | "aborted") {
        return Err(failure(
            "multipart verification failed; retain finalized output evidence",
        ));
    }
    let part_size = progress["part_size_bytes"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| failure("invalid multipart size"))?;
    let count = progress["part_count"]
        .as_u64()
        .filter(|n| (1..=10000).contains(n))
        .ok_or_else(|| failure("invalid multipart count"))?;
    if file.size_bytes.div_ceil(part_size) != count {
        return Err(failure("multipart layout conflicts with finalized output"));
    }
    let filename = format!("{key}.multipart.json");
    if directory.join(&filename).exists() {
        let saved: Value = crate::local_process::read_record(&directory.join(&filename))?;
        if saved["upload_id"] != upload
            || saved["size_bytes"] != file.size_bytes
            || saved["sha256"] != file.sha256
            || saved["part_size_bytes"] != part_size
            || saved["part_count"] != count
        {
            return Err(failure("saved multipart output identity conflicts"));
        }
    }
    if phase == "uploading" {
        crate::artifacts::verify_file(path, file.size_bytes, &file.sha256)?;
        let mut parts = BTreeMap::new();
        let mut after = 0;
        loop {
            let (status, body) = client.request(
                Method::GET,
                &format!("{base}/{upload}/parts?after={after}"),
                Some(session),
                None,
                None,
            )?;
            if status != StatusCode::OK {
                return Err(http_error("multipart inventory", status));
            }
            let page = parse_json(&body)?;
            let rows = page["parts"]
                .as_array()
                .filter(|r| r.len() <= 100)
                .ok_or_else(|| failure("multipart inventory exceeds bound"))?;
            for part in rows {
                let number = part["part_number"]
                    .as_u64()
                    .filter(|n| *n > after && *n <= count)
                    .ok_or_else(|| failure("invalid multipart inventory"))?;
                if parts.insert(number, part.clone()).is_some() {
                    return Err(failure("duplicate multipart inventory"));
                }
            }
            match page["next_after"].as_u64() {
                Some(next) if next > after && next <= count => after = next,
                None if page["next_after"].is_null() => break,
                _ => return Err(failure("invalid multipart cursor")),
            }
        }
        let mut source = File::open(path).map_err(|_| failure("finalized source unavailable"))?;
        let mut reused_bytes = 0;
        let mut transferred_bytes = 0;
        for number in 1..=count {
            if Instant::now() >= deadline {
                return Err(
                    failure("multipart attempt deadline reached").with_kind(ErrorKind::Retry)
                );
            }
            let offset = (number - 1) * part_size;
            let size = (file.size_bytes - offset).min(part_size);
            source
                .seek(SeekFrom::Start(offset))
                .map_err(|_| failure("cannot seek finalized output"))?;
            let mut range = (&mut source).take(size);
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 65536];
            let mut measured = 0;
            crate::transfers::begin_file(size);
            loop {
                let n = range
                    .read(&mut buffer)
                    .map_err(|_| failure("cannot read finalized part"))?;
                if n == 0 {
                    break;
                }
                measured += n as u64;
                digest.update(&buffer[..n]);
                crate::transfers::advance(n as u64);
            }
            crate::transfers::end_file();
            if measured != size {
                return Err(failure("finalized output changed during multipart upload"));
            }
            let checksum = STANDARD.encode(digest.finalize());
            let part_base = format!("{base}/{upload}/parts/{number}");
            let mut confirmed = false;
            if let Some(prior) = parts.get(&number) {
                if prior["size_bytes"] != size || prior["checksum_sha256"] != checksum {
                    return Err(failure("multipart part conflicts with finalized content"));
                }
                confirmed = prior["etag"].as_str().is_some_and(|s| !s.is_empty());
                if !confirmed {
                    let (status, _) = client.request(
                        Method::PUT,
                        &format!("{part_base}/receipt"),
                        Some(session),
                        None,
                        None,
                    )?;
                    if status == StatusCode::OK {
                        confirmed = true;
                    } else if status != StatusCode::CONFLICT {
                        return Err(http_error("multipart reconciliation", status));
                    }
                }
            }
            if !confirmed {
                let (status, body) = client.request(
                    Method::PUT,
                    &part_base,
                    Some(session),
                    None,
                    Some(&json!({"checksum_sha256":checksum})),
                )?;
                if status == StatusCode::ACCEPTED {
                    return Err(
                        failure("multipart transport is pending").with_kind(ErrorKind::Retry)
                    );
                }
                if status != StatusCode::OK {
                    return Err(http_error("multipart authorization", status));
                }
                let authorization = parse_json(&body)?;
                if authorization["part_number"] != number
                    || authorization["size_bytes"] != size
                    || authorization["checksum_sha256"] != checksum
                    || authorization["request"]["method"] != "PUT"
                {
                    return Err(failure(
                        "multipart authorization conflicts with local bytes",
                    ));
                }
                let transfer = &authorization["request"];
                let url = transfer["url"]
                    .as_str()
                    .ok_or_else(|| failure("multipart URL absent"))?;
                let headers: Vec<(String, String)> =
                    serde_json::from_value(transfer["headers"].clone())
                        .map_err(|_| failure("invalid multipart headers"))?;
                crate::transfers::begin_file(size);
                enumeray_client::bulk::upload(
                    File::open(path).map_err(|_| failure("output file disappeared"))?,
                    offset,
                    size,
                    url,
                    &headers,
                    deadline.saturating_duration_since(Instant::now()),
                    crate::transfers::advance,
                )
                .map_err(|_| {
                    failure("multipart output interrupted; refresh the same part")
                        .with_kind(ErrorKind::Retry)
                })?;
                crate::transfers::end_file();
                let (status, body) = client.request(
                    Method::PUT,
                    &format!("{part_base}/receipt"),
                    Some(session),
                    None,
                    None,
                )?;
                if status != StatusCode::OK {
                    return Err(http_error("multipart acknowledgement", status));
                }
                let receipt = parse_json(&body)?;
                if receipt["part_number"] != number
                    || receipt["size_bytes"] != size
                    || receipt["checksum_sha256"] != checksum
                    || receipt["etag"].as_str().is_none_or(str::is_empty)
                {
                    return Err(failure("multipart receipt conflicts with content"));
                }
                transferred_bytes += size;
            } else {
                reused_bytes += size;
            }
            crate::transfers::payload_observation(enumeray_client::bulk::TransferProgress {
                resumed_bytes: reused_bytes,
                transferred_bytes,
                verified_bytes: 0,
                total_bytes: file.size_bytes,
            });
            crate::transfers::end_file();
            crate::capture::replace(
                directory,
                &filename,
                &json!({"schema_version":1,"upload_id":upload,"size_bytes":file.size_bytes,"sha256":file.sha256,"part_size_bytes":part_size,"part_count":count,"last_confirmed_part":number,"checksum_sha256":checksum}),
            )?;
        }
    }
    let (status, body) = client.request(
        Method::PUT,
        &format!("{base}/{upload}/completion"),
        Some(session),
        None,
        Some(&json!({"multipart":true})),
    )?;
    if status == StatusCode::ACCEPTED {
        return Err(
            failure("trusted multipart verification is pending").with_kind(ErrorKind::Retry)
        );
    }
    if status != StatusCode::OK {
        return Err(http_error("multipart completion", status));
    }
    let receipt = parse_json(&body)?;
    if receipt["upload_id"] != upload
        || receipt["artifact_id"] != view["reservation"]["artifact_id"]
        || receipt["size_bytes"] != file.size_bytes
        || receipt["checksum"]["value"] != file.sha256
        || receipt["version_id"]
            .as_str()
            .is_none_or(|v| v.is_empty() || v == "null")
    {
        return Err(failure(
            "multipart completion conflicts with retained evidence",
        ));
    }
    Ok(())
}
