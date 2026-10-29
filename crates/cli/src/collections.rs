//! Private, restartable collection upload and selective verified downloads.
use crate::{args::Collection, config, transfers};
use enumeray_client::{EnumerayClient, Error, UploadChecksum, UploadRequest, resource_path};
use enumeray_contracts::collections::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;
fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
fn contract(e: InvalidCollection) -> Error {
    invalid(e.to_string())
}
pub(crate) fn private_dir(path: &Path) -> Result<(), Error> {
    if !path.try_exists()? {
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(path)?;
    }
    crate::analysis::state::private_dir(path)
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    config::require_private(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    parse_document(&bytes, MAX_MANIFEST_BYTES).map_err(contract)
}
pub(crate) fn write<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    replace: bool,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("Collection metadata exceeds 16 MiB."));
    }
    config::write_private_bytes(path, &bytes, replace)
}
pub(crate) fn confined(root: &Path, relative: &str) -> Result<PathBuf, Error> {
    if !valid_path(relative) {
        return Err(invalid("Unsafe collection member path."));
    }
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        current.push(part);
        let m = fs::symlink_metadata(&current)?;
        if m.file_type().is_symlink() {
            return Err(invalid("Collection members cannot traverse symlinks."));
        }
    }
    if !fs::metadata(&current)?.is_file() {
        return Err(invalid("Collection members must be regular files."));
    }
    Ok(current)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadState {
    schema_version: u32,
    api_url: String,
    user_id: Option<Uuid>,
    root: PathBuf,
    key: String,
    request: CollectionRequest,
    receipt: Option<CollectionReceipt>,
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    client: &EnumerayClient,
    directory: Option<&Path>,
    manifest: Option<&Path>,
    root: Option<&Path>,
    name: &str,
    media: Option<&str>,
    state: &Path,
    upload: bool,
) -> Result<Value, Error> {
    if state.try_exists()? {
        return Err(invalid(
            "Collection state already exists; use collection resume.",
        ));
    }
    let source = directory
        .or(root)
        .or_else(|| manifest.and_then(Path::parent))
        .ok_or_else(|| invalid("Collection source root is required."))?;
    if !fs::symlink_metadata(source)?.is_dir() {
        return Err(invalid(
            "Collection root must be a directory without symlinks.",
        ));
    }
    let source = source.canonicalize()?;
    let parent = state
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    if parent.starts_with(&source) {
        return Err(invalid("Keep collection state outside the source tree."));
    }
    let mut declared: Vec<DeclaredMember> = if let Some(manifest) = manifest {
        let mut bytes = Vec::new();
        File::open(manifest)?
            .take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        parse_document(&bytes, MAX_MANIFEST_BYTES).map_err(contract)?
    } else {
        let media = media.ok_or_else(|| invalid("Directory upload requires --media-type."))?;
        let mut result = Vec::new();
        let mut pending = vec![source.clone()];
        let mut visited = 0;
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir)? {
                visited += 1;
                if visited > 100_000 {
                    return Err(invalid("Collection directory traversal exceeds its bound."));
                }
                let entry = entry?;
                let m = entry.file_type()?;
                if m.is_symlink() {
                    return Err(invalid("Collection directory contains a symlink."));
                }
                if m.is_dir() {
                    pending.push(entry.path());
                } else if m.is_file() {
                    let path = entry
                        .path()
                        .strip_prefix(&source)
                        .map_err(|_| invalid("Invalid source root."))?
                        .to_str()
                        .ok_or_else(|| invalid("Member paths require UTF-8."))?
                        .to_owned();
                    result.push(DeclaredMember {
                        path,
                        media_type: media.into(),
                        scientific_metadata: json!({}),
                    });
                    if result.len() > MAX_MEMBERS {
                        return Err(invalid("Collection exceeds 10,000 members."));
                    }
                } else {
                    return Err(invalid("Collection contains a non-regular file."));
                }
            }
        }
        result
    };
    if declared.len() > MAX_MEMBERS {
        return Err(invalid("Collection exceeds 10,000 members."));
    }
    declared.sort_by(|a, b| a.path.cmp(&b.path));
    let mut members = Vec::new();
    for d in declared {
        let path = confined(&source, &d.path)?;
        let (size, checksum) = transfers::checksum(&mut File::open(path)?)?;
        members.push(MemberSpec {
            path: d.path,
            media_type: d.media_type,
            scientific_metadata: d.scientific_metadata,
            size_bytes: size,
            checksum_sha256: checksum,
        });
    }
    validate_members(&members).map_err(contract)?;
    let request = CollectionRequest {
        logical_name: name.into(),
        scientific_metadata: json!({}),
        member_count: members.len() as u32,
        total_bytes: total_bytes(&members).map_err(contract)?,
        manifest_sha256: manifest_digest(&members).map_err(contract)?,
        output_name: None,
    };
    let saved = UploadState {
        schema_version: 1,
        api_url: client.base_url().into(),
        user_id: None,
        root: source,
        key: Uuid::new_v4().to_string(),
        request,
        receipt: None,
    };
    private_dir(state)?;
    private_dir(&state.join("uploads"))?;
    write(&state.join("manifest.json"), &members, false)?;
    write(&state.join("operation.json"), &saved, false)?;
    if upload {
        resume(client, state)
    } else {
        Ok(json!({"state":state,"request":saved.request}))
    }
}
pub(crate) fn resume(client: &EnumerayClient, state: &Path) -> Result<Value, Error> {
    private_dir(state)?;
    let _lock = config::lock_private(
        &state.join("operation.json"),
        "Collection operation is already in use.",
    )?;
    let mut saved: UploadState = read(&state.join("operation.json"))?;
    if saved.schema_version != 1 || saved.api_url != client.base_url() {
        return Err(invalid(
            "Collection operation belongs to another account/API or format.",
        ));
    }
    let members: Vec<MemberSpec> = read(&state.join("manifest.json"))?;
    validate_members(&members).map_err(contract)?;
    if manifest_digest(&members).map_err(contract)? != saved.request.manifest_sha256
        || members.len() != saved.request.member_count as usize
        || total_bytes(&members).map_err(contract)? != saved.request.total_bytes
        || saved
            .receipt
            .as_ref()
            .is_some_and(|r| r.collection_id.is_nil() || r.request != saved.request)
    {
        return Err(invalid(
            "Saved collection manifest conflicts with frozen evidence.",
        ));
    }
    // Freeze the complete local operation before the first network request.
    // Bind its account durably before reserving any remote identities.
    let user_id = client.me()?.user_id;
    if saved.user_id.is_some_and(|id| id != user_id) {
        return Err(invalid("Collection operation belongs to another account."));
    }
    if saved.user_id.is_none() {
        saved.user_id = Some(user_id);
        write(&state.join("operation.json"), &saved, true)?;
    }
    if saved.receipt.is_none() {
        saved.receipt = Some(client.reserve_collection(&saved.key, &saved.request)?);
        write(&state.join("operation.json"), &saved, true)?;
    }
    let id = saved.receipt.as_ref().expect("receipt").collection_id;
    let mut start = 0;
    while start < members.len() {
        let mut end = start + 1;
        while end < members.len()
            && serde_json::to_vec(&members[start..=end])?.len() <= MAX_PAGE_BYTES
        {
            end += 1;
        }
        client.collection_page(id, start as u32, &members[start..end])?;
        start = end;
    }
    let frozen = client.freeze_collection(id)?;
    if frozen["collection_id"] != id.to_string()
        || frozen["manifest_sha256"] != saved.request.manifest_sha256
    {
        return Err(invalid(
            "Collection freeze conflicts with the saved operation.",
        ));
    }
    let view = client.collection(id)?;
    if view.receipt.request != saved.request {
        return Err(invalid(
            "Collection reservation conflicts with frozen source evidence.",
        ));
    }
    if view.state == "ready" {
        let mut result = serde_json::to_value(client.collection(id)?)?;
        result["progress"] = json!({"verified_members":members.len(),"total_members":members.len(),"resumed_bytes":saved.request.total_bytes,"transferred_bytes":0,"verified_bytes":saved.request.total_bytes});
        return Ok(result);
    }
    let mut verified = 0;
    let mut bytes = 0;
    let mut resumed_bytes = 0;
    let mut transferred_bytes = 0;
    for (index, m) in members.iter().enumerate() {
        let path = state.join("uploads").join(format!("{index}.json"));
        if !path.try_exists()? {
            let source = confined(&saved.root, &m.path)?;
            let op = transfers::UploadOperation {
                schema_version: 1,
                api_url: saved.api_url.clone(),
                user_id,
                source,
                idempotency_key: Uuid::new_v4().to_string(),
                request: UploadRequest {
                    logical_name: member_id(id, index as u32).to_string(),
                    media_type: m.media_type.clone(),
                    scientific_metadata: m.scientific_metadata.clone(),
                    expected_size_bytes: m.size_bytes,
                    checksum: UploadChecksum {
                        algorithm: "sha256".into(),
                        checksum_type: "full_object".into(),
                        value: m.checksum_sha256.clone(),
                    },
                },
                reservation: None,
                version_id: None,
                multipart: None,
            };
            config::write_private(&path, &op, false)?;
        }
        let mut operation: transfers::UploadOperation = config::read_json(&path)?;
        if operation.source != saved.root.join(&m.path)
            || operation.request.logical_name != member_id(id, index as u32).to_string()
            || operation.request.expected_size_bytes != m.size_bytes
            || operation.request.media_type != m.media_type
            || operation.request.scientific_metadata != m.scientific_metadata
            || operation.request.checksum.value != m.checksum_sha256
        {
            return Err(invalid(
                "Member upload operation conflicts with its frozen manifest.",
            ));
        }
        if let Some(reservation) = &operation.reservation {
            let view: enumeray_client::UploadView = client.get(&resource_path(&[
                "uploads",
                &reservation.upload_id.to_string(),
            ])?)?;
            if view.reservation.upload_id != reservation.upload_id
                || view.reservation.artifact_id != reservation.artifact_id
            {
                return Err(invalid("Member upload reservation changed."));
            }
            if view.state == "expired" {
                let archive = state
                    .join("uploads")
                    .join(format!("{index}.expired-{}.json", reservation.upload_id));
                if archive.try_exists()? {
                    let retained: transfers::UploadOperation = read(&archive)?;
                    if serde_json::to_value(&retained)? != serde_json::to_value(&operation)? {
                        return Err(invalid("Expired upload evidence conflicts."));
                    }
                } else {
                    write(&archive, &operation, false)?;
                }
                operation.idempotency_key = Uuid::new_v4().to_string();
                operation.reservation = None;
                operation.version_id = None;
                operation.multipart = None;
                config::write_private(&path, &operation, true)?;
            }
        }
        let (receipt, progress) = transfers::resume_observed(client, &path)?;
        resumed_bytes += progress.resumed_bytes;
        transferred_bytes += progress.transferred_bytes;
        let artifact = receipt["artifact_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| invalid("Invalid member upload receipt."))?;
        client.bind_collection_member(id, index as u32, artifact)?;
        verified += 1;
        bytes += m.size_bytes;
        eprintln!(
            "Collection: {verified}/{} members verified; {bytes}/{} bytes.",
            members.len(),
            saved.request.total_bytes
        );
    }
    let mut result = serde_json::to_value(client.complete_collection(id)?)?;
    result["progress"] = json!({"verified_members":verified,"total_members":members.len(),"resumed_bytes":resumed_bytes,"transferred_bytes":transferred_bytes,"verified_bytes":bytes});
    Ok(result)
}
pub(crate) fn manifest(
    client: &EnumerayClient,
    id: Uuid,
) -> Result<(CollectionView, Vec<PinnedMember>), Error> {
    let view = client.collection(id)?;
    if view.state != "ready" {
        return Err(invalid("Collection is not sealed."));
    }
    let mut members = Vec::new();
    let mut cursor = 0;
    loop {
        let page = client.collection_members(id, cursor)?;
        if page.items.is_empty() && page.next_cursor.is_some() {
            return Err(invalid("Collection pagination made no progress."));
        }
        for m in page.items {
            if m.index as usize != members.len() {
                return Err(invalid("Collection page conflicts with member ordering."));
            }
            members.push(PinnedMember {
                member: m.spec,
                artifact_id: m
                    .artifact_id
                    .ok_or_else(|| invalid("Member has no artifact."))?,
                version_id: m
                    .version_id
                    .ok_or_else(|| invalid("Member has no exact version."))?,
            });
            if members.len() > MAX_MEMBERS {
                return Err(invalid("Collection exceeds its count bound."));
            }
        }
        match page.next_cursor {
            None => break,
            Some(n) if n > cursor => cursor = n,
            _ => return Err(invalid("Collection pagination conflicts.")),
        }
    }
    let specs = members.iter().map(|m| m.member.clone()).collect::<Vec<_>>();
    validate_members(&specs).map_err(contract)?;
    if members.len() != view.receipt.request.member_count as usize
        || manifest_digest(&members).map_err(contract)?
            != view.manifest_digest.as_deref().unwrap_or("")
    {
        return Err(invalid(
            "Collection manifest conflicts with its sealed digest.",
        ));
    }
    Ok((view, members))
}
pub(crate) fn download(
    client: &EnumerayClient,
    id: Uuid,
    output: &Path,
    selected: &[String],
    evidence_only: bool,
) -> Result<Value, Error> {
    private_dir(output)?;
    let _lock = config::lock_private(
        &output.join("download.json"),
        "Collection download is already in use.",
    )?;
    let (view, members) = manifest(client, id)?;
    let identity = json!({"schema_version":1,"api_url":client.base_url(),"user_id":client.me()?.user_id,"collection_id":id,"manifest_digest":view.manifest_digest});
    let state = output.join("download.json");
    if state.try_exists()? {
        if read::<Value>(&state)? != identity {
            return Err(invalid(
                "Download directory belongs to different collection evidence.",
            ));
        }
    } else {
        write(&state, &identity, false)?;
    }
    let receipt_path = output.join("collection.json");
    let receipt = serde_json::to_value(&view.receipt)?;
    if receipt_path.try_exists()? {
        if read::<Value>(&receipt_path)? != receipt {
            return Err(invalid(
                "Saved collection metadata conflicts with immutable evidence.",
            ));
        }
    } else {
        write(&receipt_path, &receipt, false)?;
    }
    let manifest_path = output.join("manifest.json");
    if manifest_path.try_exists()? {
        if manifest_digest(&read::<Vec<PinnedMember>>(&manifest_path)?).map_err(contract)?
            != view.manifest_digest.as_deref().unwrap_or("")
        {
            return Err(invalid("Saved manifest conflicts."));
        }
    } else {
        write(&manifest_path, &members, false)?;
    }
    let selection = selected.iter().collect::<BTreeSet<_>>();
    if selection.len() != selected.len()
        || selection
            .iter()
            .any(|s| !members.iter().any(|m| &m.member.path == *s))
    {
        return Err(invalid(
            "Selection contains an unknown or duplicate member path.",
        ));
    }
    private_dir(&output.join("files"))?;
    let mut verified = 0;
    let mut bytes = 0;
    let mut resumed_bytes = 0;
    let mut transferred_bytes = 0;
    if !evidence_only {
        for m in &members {
            if !selection.is_empty() && !selection.contains(&m.member.path) {
                continue;
            }
            let target = output.join("files").join(&m.member.path);
            let mut current = output.join("files");
            for part in Path::new(&m.member.path)
                .parent()
                .into_iter()
                .flat_map(|p| p.components())
            {
                current.push(part);
                private_dir(&current)?;
            }
            if target.try_exists()? {
                config::require_private(&target)?;
                let (size, hash) = transfers::checksum(&mut File::open(&target)?)?;
                if size != m.member.size_bytes || hash != m.member.checksum_sha256 {
                    return Err(invalid(
                        "Existing destination conflicts with immutable member evidence; preserve it before retrying.",
                    ));
                }
                resumed_bytes += size;
            } else {
                let artifact: Value =
                    client.get(&resource_path(&["artifacts", &m.artifact_id.to_string()])?)?;
                if artifact["content"]["version_id"] != m.version_id
                    || artifact["content"]["checksum"]["value"] != m.member.checksum_sha256
                    || artifact["content"]["size_bytes"] != m.member.size_bytes
                {
                    return Err(invalid(
                        "Member artifact conflicts with the pinned version.",
                    ));
                }
                let progress = transfers::download(
                    client,
                    &resource_path(&["artifacts", &m.artifact_id.to_string(), "content"])?,
                    &target,
                    Some((
                        m.member.size_bytes,
                        m.member.checksum_sha256.clone(),
                        m.version_id.clone(),
                    )),
                )?;
                resumed_bytes += progress["resumed_bytes"]
                    .as_u64()
                    .ok_or_else(|| invalid("Missing resumed byte count."))?;
                transferred_bytes += progress["transferred_bytes"]
                    .as_u64()
                    .ok_or_else(|| invalid("Missing transferred byte count."))?;
            }
            verified += 1;
            bytes += m.member.size_bytes;
            eprintln!("Collection download: {verified} members verified; {bytes} bytes.");
        }
    }
    Ok(
        json!({"collection_id":id,"manifest_digest":view.manifest_digest,"manifest":manifest_path,"verified_members":verified,"verified_bytes":bytes,"resumed_bytes":resumed_bytes,"transferred_bytes":transferred_bytes,"total_members":members.len(),"complete":!evidence_only&&verified==members.len()}),
    )
}
pub(crate) fn execute(client: &EnumerayClient, command: &Collection) -> Result<Value, Error> {
    match command {
        Collection::Upload {
            directory,
            manifest,
            root,
            name,
            media_type,
            state,
        } => prepare(
            client,
            directory.as_deref(),
            manifest.as_deref(),
            root.as_deref(),
            name,
            media_type.as_deref(),
            state,
            true,
        ),
        Collection::Resume { state } => resume(client, state),
        Collection::Get { id } => Ok(serde_json::to_value(client.collection(*id)?)?),
        Collection::Members { id, cursor } => Ok(serde_json::to_value(
            client.collection_members(*id, *cursor)?,
        )?),
        Collection::Download { id, output, member } => download(client, *id, output, member, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_evidence_is_saved_without_network_and_manifest_selection_is_exact() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/a.csv"), b"a,b\n").unwrap();
        fs::write(root.join("notes.txt"), b"notes").unwrap();
        fs::write(root.join("unselected.bin"), b"unselected").unwrap();
        let manifest = temp.path().join("members.json");
        fs::write(&manifest, br#"[{"path":"notes.txt","media_type":"text/plain"},{"path":"nested/a.csv","media_type":"text/csv","scientific_metadata":{"unit":"K"}}]"#).unwrap();
        let client = EnumerayClient::new(
            "http://127.0.0.1:1",
            None,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let state = temp.path().join("operation");
        prepare(
            &client,
            None,
            Some(&manifest),
            Some(&root),
            "dataset",
            None,
            &state,
            false,
        )
        .unwrap();
        let saved: UploadState = read(&state.join("operation.json")).unwrap();
        let members: Vec<MemberSpec> = read(&state.join("manifest.json")).unwrap();
        assert!(saved.user_id.is_none());
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].path, "nested/a.csv");
        assert_eq!(members[1].media_type, "text/plain");
        assert_eq!(saved.request.total_bytes, 9);
        assert_eq!(
            saved.request.manifest_sha256,
            manifest_digest(&members).unwrap()
        );
        assert!(
            prepare(
                &client,
                Some(&root),
                None,
                None,
                "dataset",
                Some("text/csv"),
                &root.join("state"),
                false
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("notes.txt"), root.join("link")).unwrap();
            assert!(
                prepare(
                    &client,
                    Some(&root),
                    None,
                    None,
                    "dataset",
                    Some("text/csv"),
                    &temp.path().join("links"),
                    false
                )
                .is_err()
            );
        }
    }
}
