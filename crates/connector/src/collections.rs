//! Collection data stays in bounded immutable sidecars, never journal member maps.
use crate::{
    Result,
    client::{Client, http_error, parse_json},
    failure, local_process,
};
use enumeray_contracts::collections::*;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Measured {
    pub root: String,
    pub manifest: String,
    pub manifest_sha256: String,
    pub member_count: u32,
    pub total_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputPage {
    pub collection_id: Uuid,
    pub member_count: u32,
    pub manifest_digest: String,
    pub start: u32,
    pub members: Vec<PinnedMember>,
    pub finalize: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutputPage {
    pub name: String,
    pub start: u32,
}
fn bad(_: impl std::fmt::Display) -> crate::ConnectorError {
    failure("collection manifest conflicts with retained evidence")
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    crate::journal::check_private_path(path, false)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(bad)?
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(bad)?;
    parse_document(&bytes, MAX_MANIFEST_BYTES).map_err(bad)
}
pub(crate) fn write<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(bad)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(bad("size"));
    }
    let parent = path.parent().ok_or_else(|| bad("parent"))?;
    private_tree(parent)?;
    if path.exists() {
        let old: Value = read(path)?;
        let new: Value = parse_document(&bytes, MAX_MANIFEST_BYTES).map_err(bad)?;
        if old != new {
            return Err(bad("immutable"));
        }
        return Ok(());
    }
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut f = crate::capture::private_file(&temp).map_err(bad)?;
    f.write_all(&bytes)
        .and_then(|_| f.sync_all())
        .map_err(bad)?;
    fs::hard_link(&temp, path).map_err(bad)?;
    fs::remove_file(temp).map_err(bad)?;
    File::open(parent).and_then(|f| f.sync_all()).map_err(bad)
}
pub(crate) fn private_tree(path: &Path) -> Result<()> {
    if path.exists() {
        return local_process::create_private_directory(path);
    }
    if let Some(parent) = path.parent() {
        private_tree(parent)?;
    }
    local_process::create_private_directory(path)
}
fn slot_key(name: &str) -> String {
    crate::artifacts::transfer_key(Uuid::nil(), name, false, 0).to_string()
}
fn output_evidence(directory: &Path, slot: &str, path: &str) -> PathBuf {
    directory
        .join("collection-output-evidence")
        .join(format!("{}.json", slot_key(&format!("{slot}\0{path}"))))
}
fn base(assignment: &Value) -> Result<String> {
    Ok(format!(
        "/connector/v1/assignments/{}",
        assignment["assignment_id"]
            .as_str()
            .ok_or_else(|| bad("assignment"))?
    ))
}
fn api(
    client: &Client,
    method: Method,
    path: &str,
    session: Option<Uuid>,
    key: Option<Uuid>,
    body: Option<&Value>,
) -> Result<Value> {
    let (status, bytes) = client.request(method, path, session, key, body)?;
    if !status.is_success() {
        return Err(http_error("collection", status));
    }
    parse_json(&bytes)
}
fn input_manifest(directory: &Path, id: Uuid) -> PathBuf {
    directory
        .join("input-collections")
        .join(format!("{id}.json"))
}
fn staged_manifest(journal: &Path, intent: Uuid, id: Uuid) -> PathBuf {
    crate::artifacts::input_directory(journal, intent)
        .join("manifests")
        .join(format!("{id}.json"))
}
pub(crate) fn has_inputs(assignment: &Value) -> Result<bool> {
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    Ok(!references(&definition, &assignment["inputs"])
        .map_err(bad)?
        .is_empty())
}
pub(crate) fn stage_inputs(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
) -> Result<()> {
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    let references = references(&definition, &assignment["inputs"]).map_err(bad)?;
    if references.is_empty() {
        return Ok(());
    }
    let session = crate::journal::Journal::snapshot(journal)?
        .0
        .active_session_id
        .ok_or_else(|| failure("collection staging requires the retained connector session"))?;
    for (_, id, decl) in references {
        let base = format!("{}/collections/{id}", base(assignment)?);
        let view: CollectionView =
            serde_json::from_value(api(client, Method::GET, &base, Some(session), None, None)?)
                .map_err(bad)?;
        if view.state != "ready" || !view.available {
            return Err(failure("input collection unavailable").with_kind(crate::ErrorKind::Retry));
        }
        let mut members = Vec::new();
        let mut cursor = 0;
        loop {
            let page: MemberPage = serde_json::from_value(api(
                client,
                Method::GET,
                &format!("{base}/members?cursor={cursor}"),
                Some(session),
                None,
                None,
            )?)
            .map_err(bad)?;
            for m in page.items {
                if m.index as usize != members.len() {
                    return Err(bad("order"));
                }
                members.push(PinnedMember {
                    member: m.spec,
                    artifact_id: m.artifact_id.ok_or_else(|| bad("artifact"))?,
                    version_id: m.version_id.ok_or_else(|| bad("version"))?,
                });
                if members.len() > MAX_MEMBERS {
                    return Err(bad("count"));
                }
            }
            match page.next_cursor {
                None => break,
                Some(next) if next > cursor => cursor = next,
                _ => return Err(bad("cursor")),
            }
        }
        let specs = members.iter().map(|m| m.member.clone()).collect::<Vec<_>>();
        validate_members(&specs).map_err(bad)?;
        if !decl.permits(&specs)
            || manifest_digest(&members).map_err(bad)?
                != view.manifest_digest.as_deref().unwrap_or("")
        {
            return Err(bad("digest"));
        }
        let directory = crate::artifacts::input_directory(journal, intent)
            .join("collections")
            .join(id.to_string());
        private_tree(&directory)?;
        write(&staged_manifest(journal, intent, id), &members)?;
        let _group = crate::spool::collection(
            journal,
            intent,
            &directory,
            &directory,
            view.receipt.request.total_bytes,
        )?;
        for m in &members {
            let mut one = assignment.clone();
            one["definition"]["input_schema"] = json!({"type":"object","properties":{"member":{"type":"object","additionalProperties":false,"required":["artifact_id"],"properties":{"artifact_id":{"type":"string"}},"x-enumeray-file":{"media_types":[m.member.media_type],"max_bytes":m.member.size_bytes}}},"required":["member"],"additionalProperties":false});
            one["inputs"] = json!({"member":{"artifact_id":m.artifact_id}});
            crate::artifacts::stage_inputs_at(
                client,
                journal,
                intent,
                &one,
                directory.clone(),
                Some(m),
            )?;
        }
    }
    Ok(())
}
pub(crate) fn stage_runner(
    journal: &Path,
    job: &crate::compute::ComputeJob,
    binding: &crate::compute::ComputeInstallation,
) -> Result<()> {
    let definition = serde_json::from_value(job.assignment["definition"].clone()).map_err(bad)?;
    for (_, id, _) in references(&definition, &job.assignment["inputs"]).map_err(bad)? {
        let members: Vec<PinnedMember> = read(&staged_manifest(journal, job.intent, id))?;
        let digest = manifest_digest(&members).map_err(bad)?;
        let mut start = 0;
        while start < members.len() {
            let mut end = start + 1;
            while end < members.len()
                && serde_json::to_vec(&members[start..=end])
                    .map_err(bad)?
                    .len()
                    < MAX_PAGE_BYTES - 2048
            {
                end += 1;
            }
            let page = InputPage {
                collection_id: id,
                member_count: members.len() as u32,
                manifest_digest: digest.clone(),
                start: start as u32,
                members: members[start..end].to_vec(),
                finalize: false,
            };
            crate::compute::invoke(
                journal,
                job,
                "write-collection",
                Some(&serde_json::to_vec(&page).map_err(bad)?),
            )?;
            start = end;
        }
        let page = InputPage {
            collection_id: id,
            member_count: members.len() as u32,
            manifest_digest: digest,
            start: 0,
            members: vec![],
            finalize: true,
        };
        crate::compute::invoke(
            journal,
            job,
            "write-collection",
            Some(&serde_json::to_vec(&page).map_err(bad)?),
        )?;
        for m in members {
            crate::connections::resolve(binding).upload_input(
                binding,
                &crate::compute::remote_directory(&job.execution, job.intent),
                m.artifact_id,
                &crate::artifacts::input_directory(journal, job.intent)
                    .join("collections")
                    .join(id.to_string())
                    .join(m.artifact_id.to_string()),
            )?;
        }
    }
    Ok(())
}
pub(crate) fn install_page(directory: &Path, assignment: &Value, page: InputPage) -> Result<Value> {
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    if !references(&definition, &assignment["inputs"])
        .map_err(bad)?
        .iter()
        .any(|r| r.1 == page.collection_id)
        || page.member_count as usize > MAX_MEMBERS
        || page.start as usize + page.members.len() > page.member_count as usize
        || serde_json::to_vec(&page).map_err(bad)?.len() > MAX_PAGE_BYTES
    {
        return Err(bad("page"));
    }
    let root = directory
        .join("input-collection-pages")
        .join(page.collection_id.to_string());
    private_tree(&root)?;
    write(
        &root.join("header.json"),
        &json!({"member_count":page.member_count,"manifest_digest":page.manifest_digest}),
    )?;
    for (n, m) in page.members.iter().enumerate() {
        write(&root.join(format!("{}.json", page.start + n as u32)), m)?;
        write(
            &directory
                .join("collection-input-evidence")
                .join(format!("{}.json", m.artifact_id)),
            &json!({"size_bytes":m.member.size_bytes,"sha256":m.member.checksum_sha256}),
        )?;
    }
    if page.finalize {
        let mut members = Vec::new();
        for n in 0..page.member_count {
            members.push(read::<PinnedMember>(&root.join(format!("{n}.json")))?);
        }
        let specs = members.iter().map(|m| m.member.clone()).collect::<Vec<_>>();
        validate_members(&specs).map_err(bad)?;
        let decl = references(&definition, &assignment["inputs"])
            .map_err(bad)?
            .into_iter()
            .find(|r| r.1 == page.collection_id)
            .ok_or_else(|| bad("declaration"))?
            .2;
        if !decl.permits(&specs) || manifest_digest(&members).map_err(bad)? != page.manifest_digest
        {
            return Err(bad("digest"));
        }
        write(&input_manifest(directory, page.collection_id), &members)?;
    }
    Ok(json!({"status":"staged"}))
}
pub(crate) fn input_digest(directory: &Path, id: Uuid) -> Result<(u64, String)> {
    let v: Value = read(
        &directory
            .join("collection-input-evidence")
            .join(format!("{id}.json")),
    )?;
    Ok((
        v["size_bytes"].as_u64().ok_or_else(|| bad("size"))?,
        v["sha256"].as_str().ok_or_else(|| bad("hash"))?.into(),
    ))
}
pub(crate) fn verify_inputs(directory: &Path, assignment: &Value) -> Result<()> {
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    for (_, id, _) in references(&definition, &assignment["inputs"]).map_err(bad)? {
        let members: Vec<PinnedMember> = read(&input_manifest(directory, id))?;
        for m in members {
            crate::artifacts::verify_file(
                &directory.join("inputs").join(m.artifact_id.to_string()),
                m.member.size_bytes,
                &m.member.checksum_sha256,
            )?;
        }
    }
    Ok(())
}
pub(crate) fn prepare_workspace(
    directory: &Path,
    workspace: &Path,
    assignment: &Value,
) -> Result<Value> {
    let mut result = json!({});
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    for (name, id, _) in references(&definition, &assignment["inputs"]).map_err(bad)? {
        let members: Vec<PinnedMember> = read(&input_manifest(directory, id))?;
        let relative = format!("collections/{id}");
        let root = workspace.join(&relative);
        private_tree(&root.join("files"))?;
        for m in &members {
            let source = directory.join("inputs").join(m.artifact_id.to_string());
            crate::artifacts::verify_file(&source, m.member.size_bytes, &m.member.checksum_sha256)?;
            let target = root.join("files").join(&m.member.path);
            private_tree(target.parent().ok_or_else(|| bad("parent"))?)?;
            if !target.exists() {
                copy_member(&source, &target, &m.member)?;
            }
            crate::artifacts::verify_file(&target, m.member.size_bytes, &m.member.checksum_sha256)?;
        }
        write(&root.join("manifest.json"), &members)?;
        result[&name] = json!({"collection_id":id,"root":format!("{relative}/files"),"manifest":format!("{relative}/manifest.json")});
    }
    Ok(result)
}
pub(crate) fn measure(workspace: &Path, assignment: &Value) -> Result<BTreeMap<String, Measured>> {
    let result: Value = parse_document(
        &fs::read(workspace.join("result.json")).map_err(bad)?,
        MAX_PAGE_BYTES,
    )
    .map_err(bad)?;
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    let declarations = declarations(&definition, true).map_err(bad)?;
    let mut measured = BTreeMap::new();
    let mut total_count = result["files"].as_object().map_or(0, |m| m.len());
    let Some(outputs) = result.get("collections") else {
        return Ok(measured);
    };
    let outputs = outputs.as_object().ok_or_else(|| bad("outputs"))?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Report {
        root: String,
        manifest: String,
    }
    for (name, value) in outputs {
        let d = declarations.get(name).ok_or_else(|| bad("slot"))?;
        let r: Report = serde_json::from_value(value.clone()).map_err(bad)?;
        if !valid_path(&r.root) || !valid_path(&r.manifest) {
            return Err(bad("path"));
        }
        let mut root = workspace.to_path_buf();
        for component in Path::new(&r.root).components() {
            root.push(component);
            if !fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
                return Err(bad("collection root is not a confined directory"));
            }
        }
        let manifest = crate::artifacts::confined_file(workspace, &r.manifest)?;
        let mut bytes = Vec::new();
        File::open(manifest)
            .map_err(bad)?
            .take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(bad)?;
        let mut declared: Vec<DeclaredMember> =
            parse_document(&bytes, MAX_MANIFEST_BYTES).map_err(bad)?;
        if declared.len() > MAX_MEMBERS || declared.len() > d.max_members as usize {
            return Err(bad("count"));
        }
        declared.sort_by(|a, b| a.path.cmp(&b.path));
        let mut members = Vec::new();
        for m in declared {
            if !valid_path(&m.path) {
                return Err(bad("path"));
            }
            let path = format!("{}/{}", r.root, m.path);
            let source = crate::artifacts::confined_file(workspace, &path)?;
            if !d.media_types.contains(&m.media_type)
                || fs::metadata(&source).map_err(bad)?.len() > d.max_file_bytes
                || !m.scientific_metadata.is_object()
                || enumeray_contracts::models::validate_input_value(&m.scientific_metadata).is_err()
            {
                return Err(bad("declaration"));
            }
            let (size, hash) = crate::artifacts::file_digest(&source)?;
            members.push(MemberSpec {
                path: m.path,
                media_type: m.media_type,
                scientific_metadata: m.scientific_metadata,
                size_bytes: size,
                checksum_sha256: hash,
            });
        }
        validate_members(&members).map_err(bad)?;
        total_count += members.len();
        if total_count > MAX_MEMBERS || !d.permits(&members) {
            return Err(bad("limits"));
        }
        let directory = workspace.parent().ok_or_else(|| bad("parent"))?;
        let relative = format!("collection-outputs/{}.json", slot_key(name));
        write(&directory.join(&relative), &members)?;
        for m in &members {
            let file = crate::artifacts::OutputFile {
                path: format!("{}/{}", r.root, m.path),
                media_type: m.media_type.clone(),
                scientific_metadata: m.scientific_metadata.clone(),
                size_bytes: m.size_bytes,
                sha256: m.checksum_sha256.clone(),
            };
            write(
                &output_evidence(directory, name, &file.path),
                &json!({"slot":name,"file":file}),
            )?;
        }
        measured.insert(
            name.clone(),
            Measured {
                root: r.root,
                manifest: relative,
                manifest_sha256: manifest_digest(&members).map_err(bad)?,
                member_count: members.len() as u32,
                total_bytes: total_bytes(&members).map_err(bad)?,
            },
        );
    }
    Ok(measured)
}
pub(crate) fn output_file(
    directory: &Path,
    completion: &crate::local_process::Completion,
    path: &str,
) -> Result<crate::artifacts::OutputFile> {
    if let Some(f) = completion.files.values().find(|f| f.path == path) {
        return Ok(f.clone());
    }
    for slot in completion.collections.keys() {
        let evidence = output_evidence(directory, slot, path);
        if !evidence.exists() {
            continue;
        }
        let v: Value = read(&evidence)?;
        if v["slot"].as_str() != Some(slot.as_str()) {
            return Err(bad("slot"));
        }
        let f: crate::artifacts::OutputFile =
            serde_json::from_value(v["file"].clone()).map_err(bad)?;
        if f.path != path {
            return Err(bad("path"));
        }
        return Ok(f);
    }
    Err(bad("path"))
}
pub(crate) fn output_page(
    directory: &Path,
    completion: &crate::local_process::Completion,
    request: OutputPage,
) -> Result<Value> {
    let measured = completion
        .collections
        .get(&request.name)
        .ok_or_else(|| bad("slot"))?;
    let members: Vec<MemberSpec> = read(&directory.join(&measured.manifest))?;
    if manifest_digest(&members).map_err(bad)? != measured.manifest_sha256 {
        return Err(bad("digest"));
    }
    let start = request.start as usize;
    if start > members.len() {
        return Err(bad("cursor"));
    }
    let mut end = start;
    while end < members.len()
        && end - start < 100
        && serde_json::to_vec(&members[start..=end])
            .map_err(bad)?
            .len()
            < MAX_PAGE_BYTES - 2048
    {
        end += 1;
    }
    Ok(
        json!({"status":"completed","measured":measured,"members":members[start..end],"next_cursor":if end<members.len(){Some(end)}else{None}}),
    )
}
pub(crate) fn publish(
    client: &Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    session: Uuid,
    name: &str,
) -> Result<Value> {
    let job: crate::compute::ComputeJob = local_process::read_record(
        &crate::compute::evidence_directory(journal, intent).join("job.json"),
    )?;
    let mut members = Vec::new();
    let mut cursor = 0;
    let measured;
    loop {
        let request = OutputPage {
            name: name.into(),
            start: cursor,
        };
        let page = crate::compute::invoke(
            journal,
            &job,
            "read-collection",
            Some(&serde_json::to_vec(&request).map_err(bad)?),
        )?;
        let items: Vec<MemberSpec> =
            serde_json::from_value(page["members"].clone()).map_err(bad)?;
        members.extend(items);
        if members.len() > MAX_MEMBERS {
            return Err(bad("count"));
        }
        if let Some(next) = page["next_cursor"].as_u64() {
            if next <= cursor as u64 {
                return Err(bad("cursor"));
            }
            cursor = next as u32;
        } else {
            measured = serde_json::from_value::<Measured>(page["measured"].clone()).map_err(bad)?;
            break;
        }
    }
    validate_members(&members).map_err(bad)?;
    if manifest_digest(&members).map_err(bad)? != measured.manifest_sha256 {
        return Err(bad("digest"));
    }
    let request = CollectionRequest {
        logical_name: name.into(),
        scientific_metadata: json!({}),
        member_count: measured.member_count,
        total_bytes: measured.total_bytes,
        manifest_sha256: measured.manifest_sha256,
        output_name: Some(name.into()),
    };
    let receipt: CollectionReceipt = serde_json::from_value(api(
        client,
        Method::POST,
        &format!("{}/collections", base(assignment)?),
        Some(session),
        Some(crate::artifacts::transfer_key(
            intent,
            &format!("collection/{name}"),
            false,
            0,
        )),
        Some(&serde_json::to_value(request).map_err(bad)?),
    )?)
    .map_err(bad)?;
    let id = receipt.collection_id;
    let base = format!("{}/collections/{id}", base(assignment)?);
    let mut start = 0;
    while start < members.len() {
        let mut end = start + 1;
        while end < members.len()
            && serde_json::to_vec(&members[start..=end])
                .map_err(bad)?
                .len()
                < MAX_PAGE_BYTES
        {
            end += 1;
        }
        api(
            client,
            Method::PUT,
            &format!("{base}/members/{start}"),
            Some(session),
            None,
            Some(&serde_json::to_value(&members[start..end]).map_err(bad)?),
        )?;
        start = end;
    }
    api(
        client,
        Method::PUT,
        &format!("{base}/freeze"),
        Some(session),
        None,
        Some(&json!({})),
    )?;
    let state: CollectionView =
        serde_json::from_value(api(client, Method::GET, &base, Some(session), None, None)?)
            .map_err(bad)?;
    if state.state == "ready" {
        return Ok(json!({"collection_id":id}));
    }
    OUTPUT_SCOPE.with(|s| *s.borrow_mut() = Some((id, measured.total_bytes)));
    let _scope = OutputScope;
    for (index, m) in members.iter().enumerate() {
        let file = crate::artifacts::OutputFile {
            path: format!("{}/{}", measured.root, m.path),
            media_type: m.media_type.clone(),
            scientific_metadata: m.scientific_metadata.clone(),
            size_bytes: m.size_bytes,
            sha256: m.checksum_sha256.clone(),
        };
        let opaque = member_id(id, index as u32).to_string();
        let files = BTreeMap::from([(opaque.clone(), file)]);
        let report = crate::artifacts::publish(
            client,
            journal,
            intent,
            assignment,
            session,
            &files,
            &json!({"outputs":{},"files":{}}),
        )?;
        api(
            client,
            Method::PUT,
            &format!("{base}/members/{index}/artifact"),
            Some(session),
            None,
            Some(&report["files"][&opaque]),
        )?;
    }
    api(
        client,
        Method::PUT,
        &format!("{base}/completion"),
        Some(session),
        None,
        Some(&json!({})),
    )?;
    Ok(json!({"collection_id":id}))
}
thread_local! {static OUTPUT_SCOPE:std::cell::RefCell<Option<(Uuid,u64)>>=const{std::cell::RefCell::new(None)};}
struct OutputScope;
impl Drop for OutputScope {
    fn drop(&mut self) {
        OUTPUT_SCOPE.with(|s| *s.borrow_mut() = None);
    }
}
pub(crate) fn output_scope(journal: &Path, intent: Uuid) -> Option<(PathBuf, PathBuf, u64)> {
    OUTPUT_SCOPE.with(|s| {
        s.borrow().as_ref().map(|(id, size)| {
            (
                crate::compute::evidence_directory(journal, intent)
                    .join("collection-payloads")
                    .join(id.to_string()),
                journal
                    .join("artifact-transfers")
                    .join(intent.to_string())
                    .join("collection-staging")
                    .join(id.to_string()),
                *size,
            )
        })
    })
}

fn copy_member(source: &Path, target: &Path, member: &MemberSpec) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let budgets = enumeray_contracts::transfers::TransferBudgets::from_env().map_err(failure)?;
    let deadline = std::time::Instant::now() + budgets.overall;
    let evidence = enumeray_client::bulk::DownloadEvidence {
        scope: "collection-workspace-v1".into(),
        source: source.to_string_lossy().into(),
        version_id: None,
        size_bytes: member.size_bytes,
        checksum_sha256: member.checksum_sha256.clone(),
    };
    let mut retained = enumeray_client::bulk::RetainedTransfer::open(
        target,
        evidence,
        deadline,
        crate::transfers::advance,
    )
    .map_err(bad)?;
    retained.discard_uncommitted().map_err(bad)?;
    let mut source = File::open(source).map_err(bad)?;
    source
        .seek(SeekFrom::Start(retained.offset()))
        .map_err(bad)?;
    let mut buffer = [0; 65536];
    loop {
        let n = source.read(&mut buffer).map_err(bad)?;
        if n == 0 {
            break;
        }
        retained.append(&buffer[..n]).map_err(bad)?;
        crate::transfers::payload_progress(retained.progress());
    }
    retained
        .finish_with_progress(crate::transfers::advance)
        .map_err(bad)?;
    Ok(())
}

pub(crate) fn validate_measured(
    assignment: &Value,
    report: &Value,
    files: usize,
    collections: &BTreeMap<String, Measured>,
) -> Result<()> {
    if collections.is_empty() && report.get("collections").is_none() {
        return Ok(());
    }
    let definition = serde_json::from_value(assignment["definition"].clone()).map_err(bad)?;
    if report["outcome"] != "succeeded" {
        if !collections.is_empty() {
            return Err(bad("failed outputs"));
        }
        return Ok(());
    }
    let bytes = serde_json::to_vec(report).map_err(bad)?;
    let parsed = enumeray_contracts::assignments::results::parse_request(&bytes).map_err(bad)?;
    enumeray_contracts::assignments::results::validate_outputs(&definition, &parsed)
        .map_err(|_| bad("result"))?;
    if parsed.collections.len() != collections.len()
        || parsed.files.as_object().map_or(0, |f| f.len()) != files
    {
        return Err(bad("members"));
    }
    let declarations = declarations(&definition, true).map_err(bad)?;
    let mut count = files;
    for (name, c) in collections {
        let d = declarations.get(name).ok_or_else(|| bad("declaration"))?;
        count += c.member_count as usize;
        if !parsed.collections.contains_key(name)
            || !valid_path(&c.root)
            || c.manifest != format!("collection-outputs/{}.json", slot_key(name))
            || !valid_checksum(&c.manifest_sha256)
            || c.member_count > d.max_members
            || c.total_bytes > d.max_total_bytes
            || count > MAX_MEMBERS
        {
            return Err(bad("descriptor"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("enumeray-collections-{}", Uuid::new_v4()));
            local_process::create_private_directory(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn assignment() -> Value {
        let m: enumeray_contracts::models::ParsedPublication =
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/collections/publication.json"
            ))
            .unwrap();
        json!({"definition":m.definition,"inputs":{}})
    }
    #[test]
    fn output_measurement_is_frozen_and_does_not_trust_integrity_claims() {
        let s = Scratch::new();
        let workspace = s.0.join("workspace");
        private_tree(&workspace.join("snapshots/nested")).unwrap();
        fs::write(workspace.join("snapshots/nested/a"), b"abc").unwrap();
        fs::write(workspace.join("members.json"),br#"[{"path":"nested/a","media_type":"application/octet-stream","scientific_metadata":{"step":1}}]"#).unwrap();
        fs::write(workspace.join("result.json"),br#"{"files":{},"collections":{"snapshots":{"root":"snapshots","manifest":"members.json"}}}"#).unwrap();
        let measured = measure(&workspace, &assignment()).unwrap();
        assert_eq!(measured["snapshots"].total_bytes, 3);
        let members: Vec<MemberSpec> = read(&s.0.join(&measured["snapshots"].manifest)).unwrap();
        assert_eq!(members[0].size_bytes, 3);
        assert_eq!(
            measure(&workspace, &assignment()).unwrap()["snapshots"].manifest_sha256,
            measured["snapshots"].manifest_sha256
        );
        fs::write(workspace.join("snapshots/nested/a"), b"changed").unwrap();
        assert!(measure(&workspace, &assignment()).is_err());
        fs::write(
            workspace.join("members.json"),
            br#"[{"path":"nested/a","media_type":"application/octet-stream","sha256":"forged"}]"#,
        )
        .unwrap();
        assert!(measure(&workspace, &assignment()).is_err());
    }
    #[test]
    fn separate_collection_slots_can_share_a_workspace_member() {
        let s = Scratch::new();
        let workspace = s.0.join("workspace");
        private_tree(&workspace.join("snapshots")).unwrap();
        fs::write(workspace.join("snapshots/a"), b"abc").unwrap();
        fs::write(
            workspace.join("members.json"),
            br#"[{"path":"a","media_type":"application/octet-stream"}]"#,
        )
        .unwrap();
        fs::write(workspace.join("result.json"), br#"{"files":{},"collections":{"snapshots":{"root":"snapshots","manifest":"members.json"},"alternate":{"root":"snapshots","manifest":"members.json"}}}"#).unwrap();
        let mut a = assignment();
        a["definition"]["output_schema"]["properties"]["alternate"] =
            a["definition"]["output_schema"]["properties"]["snapshots"].clone();
        let measured = measure(&workspace, &a).unwrap();
        assert_eq!(measured.len(), 2);
        assert_eq!(
            measured["snapshots"].manifest_sha256,
            measured["alternate"].manifest_sha256
        );
        let mut completion = local_process::failure_completion(
            &json!({"attempt_id":Uuid::new_v4()}),
            "fixture",
            "fixture",
        )
        .unwrap();
        completion.collections = measured;
        let file = output_file(&s.0, &completion, "snapshots/a").unwrap();
        assert_eq!(file.size_bytes, 3);
        assert_eq!(measure(&workspace, &a).unwrap().len(), 2);
        assert!(output_file(&s.0, &completion, "snapshots/missing").is_err());
    }
    #[test]
    fn empty_collections_still_require_a_real_confined_root() {
        let s = Scratch::new();
        let workspace = s.0.join("workspace");
        private_tree(&workspace).unwrap();
        fs::write(workspace.join("members.json"), b"[]").unwrap();
        fs::write(workspace.join("result.json"), br#"{"files":{},"collections":{"snapshots":{"root":"snapshots","manifest":"members.json"}}}"#).unwrap();
        assert!(measure(&workspace, &assignment()).is_err());
        private_tree(&workspace.join("snapshots")).unwrap();
        assert_eq!(
            measure(&workspace, &assignment()).unwrap()["snapshots"].member_count,
            0
        );
        #[cfg(unix)]
        {
            fs::rename(workspace.join("snapshots"), workspace.join("original")).unwrap();
            std::os::unix::fs::symlink("original", workspace.join("snapshots")).unwrap();
            assert!(measure(&workspace, &assignment()).is_err());
        }
    }
    #[test]
    fn staged_collection_metadata_survives_page_replay_and_materializes_paths() {
        let s = Scratch::new();
        let id = Uuid::new_v4();
        let artifact = Uuid::new_v4();
        let mut a = assignment();
        a["inputs"] = json!({"dataset":{"collection_id":id}});
        assert!(has_inputs(&a).unwrap());
        assert!(!has_inputs(&assignment()).unwrap());
        let (size, sha) = {
            fs::write(s.0.join("data"), b"abc").unwrap();
            crate::artifacts::file_digest(&s.0.join("data")).unwrap()
        };
        let member = PinnedMember {
            member: MemberSpec {
                path: "nested/a".into(),
                media_type: "application/octet-stream".into(),
                scientific_metadata: json!({}),
                size_bytes: size,
                checksum_sha256: sha,
            },
            artifact_id: artifact,
            version_id: "v1".into(),
        };
        let digest = manifest_digest(std::slice::from_ref(&member)).unwrap();
        for _ in 0..2 {
            install_page(
                &s.0,
                &a,
                InputPage {
                    collection_id: id,
                    member_count: 1,
                    manifest_digest: digest.clone(),
                    start: 0,
                    members: vec![member.clone()],
                    finalize: true,
                },
            )
            .unwrap();
        }
        private_tree(&s.0.join("inputs")).unwrap();
        fs::copy(
            s.0.join("data"),
            s.0.join("inputs").join(artifact.to_string()),
        )
        .unwrap();
        verify_inputs(&s.0, &a).unwrap();
        let workspace = s.0.join("workspace");
        private_tree(&workspace).unwrap();
        let files = prepare_workspace(&s.0, &workspace, &a).unwrap();
        assert_eq!(
            fs::read(
                workspace
                    .join(files["dataset"]["root"].as_str().unwrap())
                    .join("nested/a")
            )
            .unwrap(),
            b"abc"
        );
        fs::write(s.0.join("inputs").join(artifact.to_string()), b"corrupt").unwrap();
        assert!(verify_inputs(&s.0, &a).is_err());
    }
    #[test]
    fn ten_thousand_members_use_one_recoverable_spool_reservation() {
        let s = Scratch::new();
        let intent = Uuid::new_v4();
        let root =
            s.0.join("input-staging")
                .join(intent.to_string())
                .join("collections")
                .join(Uuid::new_v4().to_string());
        private_tree(&root).unwrap();
        let guard = crate::spool::collection(&s.0, intent, &root, &root, 10000).unwrap();
        for n in 0..10000 {
            let path = root.join(n.to_string());
            crate::spool::reserve_copy(&s.0, intent, &path, 1, &path).unwrap();
            let _ = crate::capture::private_file(&path).unwrap();
        }
        drop(guard);
        let ledger: Value = read(&s.0.join("spool/reservations.json")).unwrap();
        assert_eq!(ledger["entries"].as_object().unwrap().len(), 1);
        fs::remove_file(s.0.join("spool/reservations.json")).unwrap();
        let _guard = crate::spool::collection(&s.0, intent, &root, &root, 10000).unwrap();
        let ledger: Value = read(&s.0.join("spool/reservations.json")).unwrap();
        assert_eq!(ledger["entries"].as_object().unwrap().len(), 1);
    }
}
