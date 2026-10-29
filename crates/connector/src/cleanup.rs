//! Evidence-gated retirement of run payloads. History, replay fences and lock
//! inodes remain in place; neither a timeout nor a missing host is authority.
use crate::{Result, failure, journal::PreparedAssignment, local_process};
use enumeray_contracts::{
    assignments::results::ResultReceipt,
    logs::{ArchiveState, LogArchiveStatus},
};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{self as rfs, AtFlags, Dir, FileType, Mode, OFlags},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "linux"))]
use std::collections::VecDeque;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Write,
    path::{Component, Path},
    time::{Duration, Instant},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub(crate) const ENTRY_BUDGET: usize = 256;
const TIME_BUDGET: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseReference {
    pub release_path: String,
    pub files: Vec<String>,
}
impl ReleaseReference {
    pub(crate) fn from_descriptor(
        descriptor: &enumeray_contracts::execution::InstallationDescriptor,
    ) -> Self {
        let root = Path::new(&descriptor.release_path);
        let mut files = Vec::new();
        if let Some(entry) = descriptor.entrypoint.first() {
            files.push(root.join(entry).to_string_lossy().into_owned());
        }
        if let Some(script) = &descriptor.slurm_script {
            files.push(root.join(script).to_string_lossy().into_owned());
        }
        files.sort();
        files.dedup();
        Self {
            release_path: descriptor.release_path.clone(),
            files,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseProtection {
    version: u32,
    generation: u64,
    run_payload_namespace_reserved: bool,
    #[serde(default)]
    cache_sha256: Option<String>,
    references: Vec<ReleaseReference>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleasePins {
    references: Vec<ReleaseReference>,
}
static PROTECTION_DISABLED: std::sync::LazyLock<std::sync::Mutex<HashSet<std::path::PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));
fn cache_digest(directory: &Path) -> Result<Option<String>> {
    let path = directory.join("compiled-installations.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{:x}",
        Sha256::digest(local_process::read_private(
            &path,
            local_process::MAX_RECORD_BYTES
        )?)
    )))
}
fn references_valid(references: &[ReleaseReference]) -> Result<()> {
    for r in references {
        if r.files.len() > 2 {
            return Err(failure(
                "installation protection exceeds its declared file bound",
            ));
        }
        for path in std::iter::once(&r.release_path).chain(&r.files) {
            if !Path::new(path).is_absolute()
                || path.contains(['\0', '\n', '\r'])
                || Path::new(path)
                    .components()
                    .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(failure(
                    "installation protection contains an unsafe location",
                ));
            }
        }
    }
    Ok(())
}
fn pins(directory: &Path, name: &str) -> Result<ReleasePins> {
    let path = directory.join(name);
    if !path.exists() {
        return Ok(ReleasePins::default());
    }
    let pins: ReleasePins = local_process::read_record(&path)?;
    references_valid(&pins.references)?;
    Ok(pins)
}
fn in_namespace(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|root|Path::new(path).strip_prefix(root).ok().and_then(|tail|tail.components().next()).is_some_and(|c|
        matches!(c,Component::Normal(name) if name.to_str().is_some_and(|s|Uuid::parse_str(s).is_ok()))))
}
pub(crate) fn record_release_protection(
    directory: &Path,
    generation: u64,
    reserved: bool,
    work_roots: &[String],
    references: &[ReleaseReference],
) -> Result<()> {
    let identity = fs::canonicalize(directory)
        .map_err(|_| failure("cannot resolve installation protection journal"))?;
    PROTECTION_DISABLED
        .lock()
        .expect("release protection")
        .insert(identity.clone());
    // Invalidate the durable permission before attempting a new catalog. If a
    // write fails, the cache digest also prevents using a stale old catalog.
    replace(
        directory,
        "release-protection.json",
        &ReleaseProtection {
            version: 1,
            generation,
            run_payload_namespace_reserved: false,
            cache_sha256: cache_digest(directory)?,
            references: vec![],
        },
    )?;
    let all = retain_release_references(directory, work_roots, references)?;
    if !reserved {
        return Err(failure(
            "API has not reserved run-owned installation namespaces; payload cleanup is disabled",
        ));
    }
    let protection = ReleaseProtection {
        version: 1,
        generation,
        run_payload_namespace_reserved: true,
        cache_sha256: cache_digest(directory)?,
        references: all,
    };
    let bytes = serde_json::to_vec(&protection)
        .map_err(|_| failure("cannot encode installation protection"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(failure(
            "installation protection exceeds its 1 MiB bound; retain payloads",
        ));
    }
    replace(directory, "release-protection.json", &protection)?;
    PROTECTION_DISABLED
        .lock()
        .expect("release protection")
        .remove(&identity);
    Ok(())
}
/// Save known local overlap pins before a cache reference can disappear, even
/// when another host has withheld global payload-cleanup authority.
pub(crate) fn retain_release_references(
    directory: &Path,
    work_roots: &[String],
    references: &[ReleaseReference],
) -> Result<Vec<ReleaseReference>> {
    let canonical = fs::canonicalize(directory)
        .map_err(|_| failure("cannot resolve installation retention journal"))?;
    let directory = canonical.as_path();
    references_valid(references)?;
    let mut permanent = pins(directory, "release-retention-pins.json")?;
    let mut roots = work_roots.to_vec();
    roots.extend(
        [
            "local-jobs",
            "compute-jobs",
            "input-staging",
            "artifact-transfers",
        ]
        .map(|name| directory.join(name).to_string_lossy().into_owned()),
    );
    let roots = roots
        .iter()
        .map(|root| resolved_path(Path::new(root)).map(|p| p.to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>>>()?;
    for r in references {
        let variants = reference_variants(r)?;
        if variants.iter().any(|r| {
            std::iter::once(&r.release_path)
                .chain(&r.files)
                .any(|p| in_namespace(p, work_roots) || in_namespace(p, &roots))
        }) {
            permanent.references.extend(variants);
        }
    }
    permanent.references.sort();
    permanent.references.dedup();
    replace(directory, "release-retention-pins.json", &permanent)?;
    let mut all = references.to_vec();
    all.extend(permanent.references);
    all.sort();
    all.dedup();
    Ok(all)
}
fn release_protection(directory: &Path) -> Result<ReleaseProtection> {
    let identity = fs::canonicalize(directory)
        .map_err(|_| failure("cannot resolve installation protection journal"))?;
    if PROTECTION_DISABLED
        .lock()
        .expect("release protection")
        .contains(&identity)
    {
        return Err(failure(
            "installation protection refresh failed; retain payloads",
        ));
    }
    let protection:ReleaseProtection=local_process::read_record(&directory.join("release-protection.json"))
        .map_err(|_|failure("installation reference protection is unavailable; refresh connector configuration before cleanup"))?;
    protection.validate()?;
    if protection.cache_sha256 != cache_digest(directory)? {
        return Err(failure(
            "installation reference protection is stale; refresh connector configuration",
        ));
    }
    Ok(protection)
}
pub(crate) fn protection_ready(directory: &Path) -> bool {
    release_protection(directory).is_ok()
}
impl ReleaseProtection {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || !self.run_payload_namespace_reserved {
            return Err(failure(
                "run payload namespace reservation is unavailable; retain payloads",
            ));
        }
        references_valid(&self.references)
    }
}
fn resolved_path(path: &Path) -> Result<std::path::PathBuf> {
    for ancestor in path.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(root) => {
                let suffix = path
                    .strip_prefix(ancestor)
                    .map_err(|_| failure("cannot resolve release reference"))?;
                return Ok(if suffix.as_os_str().is_empty() {
                    root
                } else {
                    root.join(suffix)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => {
                return Err(failure(
                    "release reference cannot be resolved safely; retain payloads",
                ));
            }
        }
    }
    Err(failure(
        "release reference has no resolvable filesystem root",
    ))
}
fn resolved_reference(reference: &ReleaseReference) -> Result<ReleaseReference> {
    Ok(ReleaseReference {
        release_path: resolved_path(Path::new(&reference.release_path))?
            .to_string_lossy()
            .into_owned(),
        files: reference
            .files
            .iter()
            .map(|p| resolved_path(Path::new(p)).map(|p| p.to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?,
    })
}
/// Preserve the installed alias entry as well as its final target. Resolving
/// each symlink prefix's parent exposes aliases reached through another alias
/// (for example an OS temporary-directory alias) without dereferencing the
/// installed link itself. Missing suffixes stay attached to an existing prefix.
fn location_variants(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut variants = vec![path.to_owned(), resolved_path(path)?];
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let parent = ancestor
                    .parent()
                    .ok_or_else(|| failure("installation alias has no filesystem parent"))?;
                let prefix = resolved_path(parent)?;
                let suffix = path
                    .strip_prefix(parent)
                    .map_err(|_| failure("cannot resolve installation alias prefix"))?;
                variants.push(prefix.join(suffix));
                let prefix = resolved_path(ancestor)?;
                let suffix = path
                    .strip_prefix(ancestor)
                    .map_err(|_| failure("cannot resolve installation alias suffix"))?;
                variants.push(if suffix.as_os_str().is_empty() {
                    prefix
                } else {
                    prefix.join(suffix)
                });
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => {
                return Err(failure(
                    "installation alias cannot be inspected safely; retain payloads",
                ));
            }
        }
    }
    variants.sort();
    variants.dedup();
    Ok(variants)
}
fn reference_variants(reference: &ReleaseReference) -> Result<Vec<ReleaseReference>> {
    let mut variants = vec![reference.clone(), resolved_reference(reference)?];
    for path in location_variants(Path::new(&reference.release_path))? {
        variants.push(ReleaseReference {
            release_path: path.to_string_lossy().into_owned(),
            files: reference.files.clone(),
        });
    }
    for file in &reference.files {
        for path in location_variants(Path::new(file))? {
            variants.push(ReleaseReference {
                release_path: reference.release_path.clone(),
                files: vec![path.to_string_lossy().into_owned()],
            });
        }
    }
    variants.sort();
    variants.dedup();
    Ok(variants)
}
fn payload_locations(directory: &Path) -> Vec<std::path::PathBuf> {
    [
        "workspace",
        "inputs",
        "outputs",
        "archive-copy",
        "log-captures",
        "log-archives",
        "slurm.log",
    ]
    .into_iter()
    .map(|name| directory.join(name))
    .collect()
}
fn selected_paths(
    references: &[ReleaseReference],
    locations: &[std::path::PathBuf],
) -> Result<Vec<std::path::PathBuf>> {
    let mut resolved_locations = locations
        .iter()
        .map(|path| resolved_path(path))
        .collect::<Result<Vec<_>>>()?;
    let cwd = std::env::current_dir().map_err(|_| failure("cannot resolve cleanup locations"))?;
    resolved_locations.extend(locations.iter().map(|path| {
        if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        }
    }));
    let mut selected = Vec::new();
    for reference in references {
        for reference in reference_variants(reference)? {
            let release = Path::new(&reference.release_path);
            // Broad installation ancestors do not claim unrelated run scratch.
            if resolved_locations.iter().any(|root| {
                release.starts_with(root)
                    || root.parent().is_some_and(|owned| {
                        release == owned
                            && owned
                                .file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| Uuid::parse_str(name).is_ok())
                    })
            }) {
                selected.push(release.to_owned());
            }
            for file in &reference.files {
                let path = Path::new(file);
                if resolved_locations.iter().any(|root| path.starts_with(root)) {
                    selected.push(path.to_owned());
                }
            }
        }
    }
    selected.sort();
    selected.dedup();
    Ok(selected)
}
fn retained_paths(
    directory: &Path,
    references: &[ReleaseReference],
) -> Result<Vec<std::path::PathBuf>> {
    let root = fs::canonicalize(directory)
        .map_err(|_| failure("cannot resolve retained installation payload root"))?;
    let locations = payload_locations(&root);
    let mut retained = pins(directory, "release-pins.json")?;
    for reference in references {
        let variants = reference_variants(reference)?;
        if !selected_paths(std::slice::from_ref(reference), &locations)?.is_empty() {
            retained.references.extend(variants);
        }
    }
    retained.references.sort();
    retained.references.dedup();
    if !retained.references.is_empty() {
        replace(directory, "release-pins.json", &retained)?;
    }
    selected_paths(&retained.references, &locations)
}
fn local_protection(directory: &Path, intent: Uuid) -> Result<ReleaseProtection> {
    let protection = release_protection(directory)?;
    let mut locations = payload_locations(&local_process::job_directory(directory, intent));
    locations.extend(payload_locations(
        &directory.join("compute-jobs").join(intent.to_string()),
    ));
    locations.push(directory.join("input-staging").join(intent.to_string()));
    locations.push(
        directory
            .join("artifact-transfers")
            .join(intent.to_string()),
    );
    if !selected_paths(&protection.references, &locations)?.is_empty() {
        return Err(failure(
            "run payload contains a retained installation reference; preserve its workspace",
        ));
    }
    Ok(protection)
}
pub(crate) fn pin_installation(
    descriptor: &enumeray_contracts::execution::InstallationDescriptor,
) -> Result<Vec<File>> {
    pin_references(&[ReleaseReference::from_descriptor(descriptor)])
}
pub(crate) fn pin_references(references: &[ReleaseReference]) -> Result<Vec<File>> {
    references_valid(references)?;
    let mut all = Vec::new();
    for reference in references {
        all.extend(reference_variants(reference)?);
    }
    all.sort();
    all.dedup();
    let references = all;
    let mut roots = std::collections::BTreeSet::new();
    for reference in &references {
        for path in std::iter::once(&reference.release_path).chain(&reference.files) {
            for ancestor in Path::new(path).ancestors() {
                let Some(intent) = ancestor
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    continue;
                };
                let record = ancestor.join("remote.json");
                if !record.exists() {
                    continue;
                }
                let job: crate::compute::ComputeJob = local_process::read_record(&record)?;
                if job.version != 1
                    || job.intent != intent
                    || resolved_path(ancestor)?
                        != resolved_path(Path::new(&crate::compute::remote_directory(
                            &job.execution,
                            intent,
                        )))?
                {
                    return Err(failure(
                        "installation reference conflicts with run ownership",
                    ));
                }
                roots.insert(resolved_path(ancestor)?);
            }
        }
    }
    let mut fences = Vec::new();
    for root in roots {
        fences.push(payload_fence(&root, false)?);
        let _ = retained_paths(&root, &references)?;
    }
    Ok(fences)
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    // Keep the exact acknowledged record, including additive evidence fields.
    pub terminal_result: Value,
    pub live_logs_delivered: bool,
    pub archive_status: Option<LogArchiveStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protection: Option<ReleaseProtection>,
}
impl Request {
    pub(crate) fn same_delivery(&self, other: &Self) -> Result<bool> {
        Ok(
            crate::numeric::equivalent(&self.terminal_result, &other.terminal_result)
                && self.live_logs_delivered == other.live_logs_delivered
                && crate::numeric::equivalent(
                    &serde_json::to_value(&self.archive_status)
                        .map_err(|_| failure("invalid archive proof"))?,
                    &serde_json::to_value(&other.archive_status)
                        .map_err(|_| failure("invalid archive proof"))?,
                ),
        )
    }
    pub(crate) fn retained_paths(&self, directory: &Path) -> Result<Vec<std::path::PathBuf>> {
        let protection = self
            .protection
            .as_ref()
            .ok_or_else(|| failure("cleanup request lacks installation reference protection"))?;
        protection.validate()?;
        retained_paths(directory, &protection.references)
    }
    pub(crate) fn validate(&self, assignment: &Value, archive_enabled: bool) -> Result<()> {
        let receipt: ResultReceipt =
            serde_json::from_value(self.terminal_result["receipt"].clone())
                .map_err(|_| failure("cleanup lacks a valid terminal receipt"))?;
        if !receipt.provider_cleanup_acknowledged
            || !self.live_logs_delivered
            || self.terminal_result["receipt"]["assignment_id"] != assignment["assignment_id"]
            || self.terminal_result["receipt"]["run_id"] != assignment["run_id"]
            || self.terminal_result["receipt"]["attempt_id"] != assignment["attempt_id"]
            || receipt.assignment_id.is_nil()
            || receipt.run_id.is_nil()
            || receipt.attempt_id.is_nil()
            || (archive_enabled
                && self
                    .archive_status
                    .as_ref()
                    .is_none_or(|s| !archive_finished(s)))
            || self
                .archive_status
                .as_ref()
                .is_some_and(|s| !archive_finished(s))
            || self
                .archive_status
                .as_ref()
                .and_then(|s| s.manifest.as_ref())
                .is_some_and(|m| {
                    m.version != 1
                        || m.assignment_id != receipt.assignment_id
                        || m.attempt_id != receipt.attempt_id
                        || m.launch_intent_id.is_nil()
                })
        {
            return Err(failure(
                "cleanup authority or delivery evidence is unavailable",
            ));
        }
        OffsetDateTime::parse(&receipt.completed_at, &Rfc3339)
            .map_err(|_| failure("cleanup receipt has invalid completion time"))?;
        Ok(())
    }
}
fn archive_finished(status: &LogArchiveStatus) -> bool {
    matches!(
        status.state,
        ArchiveState::Unavailable | ArchiveState::Expired
    ) || status.state == ArchiveState::Ready
        && status.artifact_id.is_some_and(|id| !id.is_nil())
        && status.manifest.is_some()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Archived {
    version: u32,
    journal_id: Uuid,
    entry: PreparedAssignment,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    assignment_id: Uuid,
    launch_intent_id: Uuid,
    archive_sha256: String,
    request: Request,
    compute_complete: bool,
    connector_complete: bool,
    removed_entries: u64,
    removed_bytes: u64,
    #[serde(default)]
    compute_removed_entries: u64,
    #[serde(default)]
    compute_removed_bytes: u64,
    #[serde(default)]
    legacy_cursors: BTreeMap<String, u64>,
    diagnostic: Option<String>,
}

/// A projection over version-1 checkpoints, never a new saved format. Compute
/// retirement (including allocation release) must precede connector retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupProgress {
    ComputePending,
    ConnectorPending,
    Complete,
}
impl State {
    fn progress(&self) -> Result<CleanupProgress> {
        match (self.compute_complete, self.connector_complete) {
            (false, false) => Ok(CleanupProgress::ComputePending),
            (true, false) => Ok(CleanupProgress::ConnectorPending),
            (true, true) => Ok(CleanupProgress::Complete),
            (false, true) => Err(failure(
                "connector cleanup has no completed compute retirement",
            )),
        }
    }

    fn complete(&self) -> bool {
        self.progress()
            .is_ok_and(|p| p == CleanupProgress::Complete)
    }

    fn record_compute_reply(&mut self, reply: &Value) -> Result<()> {
        let complete = match reply["status"].as_str() {
            Some("completed") => true,
            Some("pending") => false,
            _ => {
                return Err(failure(
                    "compute cleanup returned an invalid progress state",
                ));
            }
        };
        let removed_entries = reply["removed_entries"]
            .as_u64()
            .ok_or_else(|| failure("compute cleanup lacks removed entry progress"))?;
        let removed_bytes = reply["removed_bytes"]
            .as_u64()
            .ok_or_else(|| failure("compute cleanup lacks removed byte progress"))?;
        if self.progress()? != CleanupProgress::ComputePending {
            return if complete
                && removed_entries == self.compute_removed_entries
                && removed_bytes == self.compute_removed_bytes
            {
                Ok(())
            } else {
                Err(failure(
                    "compute retirement conflicts with completed progress",
                ))
            };
        }
        if removed_entries < self.compute_removed_entries
            || removed_bytes < self.compute_removed_bytes
        {
            return Err(failure(
                "compute cleanup progress conflicts with its durable checkpoint",
            ));
        }
        self.compute_complete = complete;
        self.compute_removed_entries = removed_entries;
        self.compute_removed_bytes = removed_bytes;
        Ok(())
    }

    fn record_connector_pass(&mut self, complete: bool, budget: &Budget) -> Result<()> {
        if self.progress()? != CleanupProgress::ConnectorPending {
            return Err(failure(
                "connector cleanup requires completed compute retirement",
            ));
        }
        self.removed_entries = self.removed_entries.saturating_add(budget.removed_entries);
        self.removed_bytes = self.removed_bytes.saturating_add(budget.removed_bytes);
        self.connector_complete = complete;
        self.diagnostic = None;
        Ok(())
    }

    fn record_local_release(
        &mut self,
        entry: &PreparedAssignment,
        completion: &local_process::Completion,
    ) -> Result<()> {
        self.progress()?;
        completion.validate(self.launch_intent_id, &entry.assignment)?;
        if entry.launch_intent_id != Some(self.launch_intent_id)
            || entry.assignment.decoded().assignment_id != self.assignment_id
            || entry.execution.as_ref().is_none_or(|execution| {
                !crate::numeric::equivalent(&completion.report, &execution.report)
            })
        {
            return Err(failure(
                "allocation release evidence conflicts with the retained execution report",
            ));
        }
        self.compute_complete = true;
        Ok(())
    }
}
#[derive(Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct Scan {
    offset: i64,
    #[serde(default)]
    complete: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanMode {
    Inspect,
    Discover,
    Pending,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    version: u32,
    assignment_id: Uuid,
}

/// Queue publication follows the archive directory fsync and precedes removal
/// from the active journal. A marker selects work; it never authorizes deletion.
pub(crate) fn enqueue(directory: &Path, assignment_id: Uuid) -> Result<()> {
    if assignment_id.is_nil() {
        return Err(failure("cleanup queue has an invalid assignment identity"));
    }
    // Journal publication also accepts OS directory aliases (such as /var on
    // macOS). Resolve its private metadata root; payload traversal remains
    // descriptor-relative and rejects symbolic-link ancestors.
    let canonical =
        fs::canonicalize(directory).map_err(|_| failure("cannot resolve cleanup queue journal"))?;
    let directory = canonical.as_path();
    let cleanup = directory.join("cleanup");
    local_process::create_private_directory(&cleanup)?;
    let pending = cleanup.join("pending");
    local_process::create_private_directory(&pending)?;
    let name = format!("{assignment_id}.json");
    if pending
        .join(&name)
        .try_exists()
        .map_err(|_| failure("cannot inspect cleanup queue"))?
    {
        queued_archive(directory, &pending.join(name))?;
    } else {
        replace(
            &pending,
            &name,
            &Pending {
                version: 1,
                assignment_id,
            },
        )?;
    }
    // Invalidate cached inventories even on filesystems with coarse directory
    // timestamps, and durably publish a newly created pending directory.
    replace(
        &cleanup,
        "pins-revision.json",
        &json!({"revision":Uuid::new_v4()}),
    )
}
fn queued_archive(directory: &Path, path: &Path) -> Result<std::path::PathBuf> {
    let marker: Pending = local_process::read_record(path)?;
    if marker.version != 1
        || marker.assignment_id.is_nil()
        || path.file_stem().and_then(|s| s.to_str())
            != Some(marker.assignment_id.to_string().as_str())
    {
        return Err(failure(
            "cleanup queue has conflicting assignment identities",
        ));
    }
    Ok(directory
        .join("archive")
        .join(format!("{}.json", marker.assignment_id)))
}
fn dequeue(directory: &Path, id: Uuid) -> Result<()> {
    let pending = directory.join("cleanup/pending");
    let root = open_directory(&pending)?;
    rfs::unlinkat(&root, format!("{id}.json"), AtFlags::empty())
        .or_else(|error| {
            if error == rustix::io::Errno::NOENT {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|_| failure("cannot finish cleanup queue entry"))?;
    rfs::fsync(&root).map_err(|_| failure("cannot sync cleanup queue completion"))
}
fn discovery_complete(directory: &Path) -> Result<bool> {
    let path = directory.join("cleanup/discovery.json");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect cleanup discovery"))?
    {
        return Ok(false);
    }
    let _root = open_directory(&directory.join("cleanup"))?;
    let complete = local_process::read_record::<Scan>(&path)?.complete;
    if complete {
        let _pending = open_directory(&directory.join("cleanup/pending"))?;
    }
    Ok(complete)
}
#[cfg(not(target_os = "linux"))]
struct Walk {
    entries: Dir,
    position: i64,
    target: i64,
}
#[cfg(not(target_os = "linux"))]
static WALKS: std::sync::LazyLock<std::sync::Mutex<VecDeque<(std::path::PathBuf, Walk)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(VecDeque::new()));
#[cfg(not(target_os = "linux"))]
fn take_walk(root: &Path) -> Option<Walk> {
    let mut walks = WALKS.lock().expect("cleanup directory walkers");
    let index = walks.iter().position(|(path, _)| path == root)?;
    walks.remove(index).map(|(_, walk)| walk)
}
#[cfg(not(target_os = "linux"))]
fn keep_walk(root: &Path, entries: Dir, position: i64, target: i64) {
    let mut walks = WALKS.lock().expect("cleanup directory walkers");
    cache_walk(&mut walks, root, entries, position, target);
}
#[cfg(not(target_os = "linux"))]
fn cache_walk(
    walks: &mut VecDeque<(std::path::PathBuf, Walk)>,
    root: &Path,
    entries: Dir,
    position: i64,
    target: i64,
) {
    // Evict the least recently retained iterator, not the smallest path. Path
    // order can evict a just-saved archive replay when its sibling pending scan
    // is saved, repeatedly restarting a prefix longer than ENTRY_BUDGET.
    walks.retain(|(path, _)| path != root);
    if walks.len() >= 16 {
        walks.pop_front();
    }
    walks.push_back((
        root.to_owned(),
        Walk {
            entries,
            position,
            target,
        },
    ));
}

fn archive_entry(path: &Path, journal_id: Uuid, connector: &str) -> Result<(Archived, String)> {
    let bytes = local_process::read_private(path, 16 * 1024 * 1024)?;
    let archived: Archived = serde_json::from_slice(&bytes)
        .map_err(|_| failure("malformed archived cleanup evidence"))?;
    let a = archived.entry.assignment.decoded();
    if archived.version != 1
        || archived.journal_id != journal_id
        || a.journal_id != journal_id
        || a.connector_id != connector
        || path.file_stem().and_then(|s| s.to_str()) != Some(a.assignment_id.to_string().as_str())
    {
        return Err(failure(
            "archived cleanup identities conflict with this journal",
        ));
    }
    crate::execution::validate_history(&archived.entry)?;
    crate::logs::validate_history(&archived.entry)?;
    if let Some(transfers) = &archived.entry.transfers {
        transfers.validate(&archived.entry)?;
    }
    Ok((archived, format!("{:x}", Sha256::digest(bytes))))
}
/// Borrowed authority combines acknowledged local delivery and retention only.
/// It deliberately does not imply allocation release or installation safety;
/// those are checked under their fences before any payload can be deleted.
struct CleanupAuthority<'a> {
    intent: Uuid,
    terminal_result: &'a crate::records::TerminalResult,
    archive_status: Option<&'a LogArchiveStatus>,
}
enum CleanupEligibility<'a> {
    Blocked(crate::ConnectorError),
    Eligible(CleanupAuthority<'a>),
}
impl<'a> CleanupEligibility<'a> {
    fn derive(entry: &'a PreparedAssignment, retention_seconds: u64) -> Self {
        match cleanup_authority(entry, retention_seconds) {
            Ok(authority) => Self::Eligible(authority),
            Err(error) => Self::Blocked(error),
        }
    }
}
impl CleanupAuthority<'_> {
    fn request(&self) -> Request {
        Request {
            terminal_result: (**self.terminal_result).clone(),
            live_logs_delivered: true,
            archive_status: self.archive_status.cloned(),
            protection: None,
        }
    }
}
fn cleanup_authority(
    entry: &PreparedAssignment,
    retention_seconds: u64,
) -> Result<CleanupAuthority<'_>> {
    let result = match crate::state::settlement(entry) {
        crate::state::SettlementView::Pending => {
            return Err(failure("terminal delivery is pending"));
        }
        crate::state::SettlementView::ConnectorAcknowledged(result) => result,
        crate::state::SettlementView::Retrieved(result) => {
            return Err(failure(format!(
                "retrieved terminal receipt for {} lacks local execution acknowledgement",
                result["receipt"]["assignment_id"],
            )));
        }
        crate::state::SettlementView::OperatorResolved { result, resolution } => {
            return Err(failure(format!(
                "operator resolution ({}) for {} cannot authorize provider payload cleanup",
                resolution.decoded().execution_status.as_str(),
                result["receipt"]["assignment_id"],
            )));
        }
    };
    let intent = entry
        .launch_intent_id
        .ok_or_else(|| failure("run has no owned launch intent"))?;
    if intent.is_nil()
        || !result
            .decoded()
            .receipt
            .decoded()
            .provider_cleanup_acknowledged
    {
        return Err(failure(
            "provider cleanup was not acknowledged; retain execution evidence",
        ));
    }
    let execution = entry
        .execution
        .as_ref()
        .ok_or_else(|| failure("local execution evidence is unavailable"))?;
    if !execution.publication_ready()
        || execution
            .acknowledged_receipt()
            .is_none_or(|receipt| !crate::numeric::equivalent(receipt, &result["receipt"]))
    {
        return Err(failure(
            "terminal receipt does not acknowledge the retained delivery",
        ));
    }
    let report = execution.published_report();
    for key in [
        "outputs",
        "files",
        "collections",
        "optional_output_failures",
        "failure",
    ] {
        if !crate::numeric::equivalent(&report[key], &result[key]) {
            return Err(failure(
                "retained terminal delivery conflicts with cleanup receipt",
            ));
        }
    }
    let delivered = entry
        .logs
        .as_ref()
        .is_some_and(|p| p.delivered(entry.assignment.decoded()));
    let archive_status = match &entry.log_archive {
        Some(a) if a.finished => a.status.as_ref(),
        Some(_) => return Err(failure("log archive delivery is pending")),
        None => None,
    };
    if !delivered {
        return Err(failure(
            "cleanup authority or delivery evidence is unavailable",
        ));
    }
    let authority = CleanupAuthority {
        intent,
        terminal_result: result,
        archive_status,
    };
    let request = authority.request();
    request.validate(&entry.assignment, entry.log_archive.is_some())?;
    if request
        .archive_status
        .as_ref()
        .and_then(|s| s.manifest.as_ref())
        .is_some_and(|m| m.launch_intent_id != intent)
    {
        return Err(failure(
            "archive delivery belongs to a different launch intent",
        ));
    }
    let completed = result["receipt"]["completed_at"]
        .as_str()
        .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
        .ok_or_else(|| failure("cleanup has no completion time"))?;
    if OffsetDateTime::now_utc().unix_timestamp()
        < completed
            .unix_timestamp()
            .saturating_add(retention_seconds.min(i64::MAX as u64) as i64)
    {
        return Err(failure(
            "completed run is within its payload retention period",
        ));
    }
    Ok(authority)
}
fn request(entry: &PreparedAssignment, retention_seconds: u64) -> Result<Request> {
    cleanup_authority(entry, retention_seconds).map(|authority| authority.request())
}
fn state(directory: &Path, id: Uuid) -> Result<Option<State>> {
    let path = directory.join("cleanup").join(format!("{id}.json"));
    if path
        .try_exists()
        .map_err(|_| failure("cannot inspect cleanup checkpoint"))?
    {
        let _root = open_directory(&directory.join("cleanup"))?;
        let state: State = local_process::read_record(&path)?;
        if state.version != 1 || state.assignment_id != id || state.launch_intent_id.is_nil() {
            return Err(failure("invalid cleanup checkpoint identities"));
        }
        Ok(Some(state))
    } else {
        Ok(None)
    }
}
fn persist(directory: &Path, state: &State) -> Result<()> {
    let target = directory.join("cleanup");
    local_process::create_private_directory(&target)?;
    // Protection is checked afresh for every deletion attempt and sent only in
    // the live runner request. Permanent checkpoints keep delivery/replay proof.
    let mut compact = state.clone();
    compact.request.protection = None;
    replace(&target, &format!("{}.json", state.assignment_id), &compact)
}

pub(crate) fn replace(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let _root = open_directory(directory)?;
    let bytes =
        serde_json::to_vec(value).map_err(|_| failure("cannot encode cleanup checkpoint"))?;
    if bytes.len() as u64 > local_process::MAX_RECORD_BYTES {
        return Err(failure("cleanup checkpoint exceeds its byte bound"));
    }
    let temporary = directory.join(format!(".cleanup-{}.tmp", Uuid::new_v4()));
    let mut file = crate::capture::private_file(&temporary)
        .map_err(|_| failure("cannot create cleanup checkpoint"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot persist cleanup checkpoint"))?;
    fs::rename(temporary, directory.join(name))
        .map_err(|_| failure("cannot publish cleanup checkpoint"))?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| failure("cannot sync cleanup checkpoint"))
}

/// A bounded page. Inspection never creates records or changes replay evidence.
pub(crate) fn inspect(
    directory: &Path,
    journal_id: Uuid,
    connector: &str,
    limit: usize,
    retention_seconds: u64,
) -> Result<Value> {
    scan(
        directory,
        journal_id,
        connector,
        limit,
        retention_seconds,
        ScanMode::Inspect,
    )
}
/// Called by the service's independent cleanup worker or a journal-owning manual
/// command. Only archived entries are considered; active work cannot be selected.
pub(crate) fn run(
    directory: &Path,
    journal_id: Uuid,
    connector: &str,
    limit: usize,
    retention_seconds: u64,
) -> Result<Value> {
    let discovery = if discovery_complete(directory)? {
        None
    } else {
        Some(scan(
            directory,
            journal_id,
            connector,
            128,
            retention_seconds,
            ScanMode::Discover,
        )?)
    };
    let mut report = scan(
        directory,
        journal_id,
        connector,
        limit,
        retention_seconds,
        ScanMode::Pending,
    )?;
    if let Some(discovery) = discovery {
        report["discovery"] = discovery.clone();
        report["more"] = json!(report["more"] == true || discovery["more"] == true);
    }
    Ok(report)
}
fn unscoped_transfers(directory: &Path) -> Result<Value> {
    let parent = directory.join("artifact-transfers");
    if !parent
        .try_exists()
        .map_err(|_| failure("cannot inspect transfer diagnostics"))?
    {
        return Ok(json!({"entries":[],"truncated":false}));
    }
    let root = open_directory(&parent)?;
    let mut iterator =
        Dir::read_from(&root).map_err(|_| failure("cannot list transfer diagnostics"))?;
    let mut entries = Vec::new();
    let mut scanned = 0;
    let deadline = Instant::now() + TIME_BUDGET;
    while scanned < ENTRY_BUDGET && Instant::now() < deadline {
        let Some(entry) = iterator.read() else {
            return Ok(json!({"entries":entries,"truncated":false}));
        };
        let entry = entry.map_err(|_| failure("cannot read transfer diagnostics"))?;
        let name = entry.file_name().to_string_lossy();
        if name == "." || name == ".." {
            continue;
        }
        scanned += 1;
        if entry.file_type() == FileType::Directory && Uuid::parse_str(&name).is_ok() {
            continue;
        }
        if entries.len() >= 64 {
            return Ok(json!({"entries":entries,"truncated":true}));
        }
        entries.push(name.into_owned());
    }
    Ok(json!({"entries":entries,"truncated":true}))
}
fn scan(
    directory: &Path,
    journal_id: Uuid,
    connector: &str,
    limit: usize,
    retention_seconds: u64,
    mode: ScanMode,
) -> Result<Value> {
    let execute = mode != ScanMode::Inspect;
    if mode == ScanMode::Discover {
        let cleanup = directory.join("cleanup");
        local_process::create_private_directory(&cleanup)?;
        local_process::create_private_directory(&cleanup.join("pending"))?;
    }
    let root = directory.join(if mode == ScanMode::Pending {
        "cleanup/pending"
    } else {
        "archive"
    });
    let mut reports = Vec::new();
    let unscoped_transfers = unscoped_transfers(directory)?;
    let mut unattributed_archives = Vec::new();
    let mut unattributed_archive_count = 0;
    if !root
        .try_exists()
        .map_err(|_| failure("cannot inspect archived runs"))?
    {
        if mode == ScanMode::Discover {
            let cleanup = directory.join("cleanup");
            local_process::create_private_directory(&cleanup)?;
            local_process::create_private_directory(&cleanup.join("pending"))?;
            replace(
                &cleanup,
                "discovery.json",
                &Scan {
                    complete: true,
                    ..Scan::default()
                },
            )?;
        }
        return Ok(
            json!({"status":"completed","runs":reports,"scanned":0,"more":false,
            "unscoped_transfer_entries":unscoped_transfers["entries"],"unscoped_transfer_entries_truncated":unscoped_transfers["truncated"],
            "unattributed_archive_entries":[],"unattributed_archive_entry_count":0}),
        );
    }
    let scan_name = match mode {
        ScanMode::Discover => "discovery.json",
        ScanMode::Pending => "pending-scan.json",
        ScanMode::Inspect => "scan.json",
    };
    let scan_path = directory.join("cleanup").join(scan_name);
    let mut scan: Scan = if execute && scan_path.exists() {
        local_process::read_record(&scan_path)?
    } else {
        Scan::default()
    };
    let root_fd = open_directory(&root)?;
    let mut entries = Dir::read_from(&root_fd).map_err(|_| failure("cannot list archived runs"))?;
    let deadline = Instant::now() + TIME_BUDGET;
    #[cfg(target_os = "linux")]
    if scan.offset != 0 {
        entries
            .seek(scan.offset)
            .map_err(|_| failure("cannot resume archived run scan"))?;
    }
    // Darwin lacks entry cookies. Keep the live iterator between passes; after
    // restart replay the saved prefix in bounded pieces, retaining that replay
    // iterator too, so a large prefix cannot repeatedly starve later runs.
    #[cfg(not(target_os = "linux"))]
    if execute {
        let mut position = 0;
        if let Some(walk) = take_walk(&root)
            && walk.target == scan.offset
        {
            entries = walk.entries;
            position = walk.position;
        }
        let mut replayed = 0;
        while position < scan.offset && replayed < ENTRY_BUDGET && Instant::now() < deadline {
            if entries
                .read()
                .transpose()
                .map_err(|_| failure("cannot resume archived run scan"))?
                .is_none()
            {
                scan.offset = 0;
                entries.rewind();
                position = 0;
                break;
            }
            position += 1;
            replayed += 1;
        }
        if position < scan.offset {
            keep_walk(&root, entries, position, scan.offset);
            return Ok(
                json!({"status":"completed","runs":reports,"scanned":0,"more":true,"completed":0,"blocked":0,"pending":0,
                "replayed":replayed,"diagnostic":"resuming the retained archive scan"}),
            );
        }
    }
    let mut scanned = 0;
    let mut more = true;
    let mut wrap_pending = mode == ScanMode::Pending && scan.offset != 0;
    while scanned < ENTRY_BUDGET && reports.len() < limit.clamp(1, 128) && Instant::now() < deadline
    {
        let Some(entry) = entries.read() else {
            // A full service page can leave the cursor exactly at the end.
            // Retry the queue's head within this pass's remaining budget rather
            // than reporting no work while retained runs are still queued.
            if wrap_pending && reports.is_empty() {
                wrap_pending = false;
                scan.offset = 0;
                // Reopen from the validated root so the retry cannot retain
                // filesystem iteration state from the seek to the old cursor.
                entries = Dir::read_from(&root_fd)
                    .map_err(|_| failure("cannot restart pending cleanup scan"))?;
                continue;
            }
            scan.offset = 0;
            scan.complete = mode == ScanMode::Discover;
            more = false;
            break;
        };
        let entry = entry.map_err(|_| failure("cannot read archived run directory"))?;
        #[cfg(target_os = "linux")]
        {
            scan.offset = entry.offset();
        }
        #[cfg(not(target_os = "linux"))]
        {
            scan.offset += 1;
        }
        let name = entry.file_name().to_string_lossy();
        if name == "." || name == ".." {
            continue;
        }
        scanned += 1;
        let Some(id) = name
            .strip_suffix(".json")
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            unattributed_archive_count += 1;
            if unattributed_archives.len() < 64 {
                unattributed_archives.push(name.into_owned());
            }
            continue;
        };
        // Publishing the marker must succeed before discovery can advance its
        // durable cursor. Per-run evidence errors still leave the marker pinned.
        if mode == ScanMode::Discover {
            enqueue(directory, id)?;
        }
        let outcome = (|| -> Result<Value> {
            let path = if mode == ScanMode::Pending {
                queued_archive(directory, &root.join(name.as_ref()))?
            } else {
                root.join(name.as_ref())
            };
            let (archive, digest) = archive_entry(&path, journal_id, connector)?;
            let existing = state(directory, id)?;
            let was_complete = existing.as_ref().is_some_and(State::complete);
            if existing
                .as_ref()
                .is_some_and(|s| s.archive_sha256 != digest)
            {
                return Err(failure(
                    "cleanup checkpoint conflicts with immutable archive",
                ));
            }
            if let Some(state) = &existing {
                validate_state(state, &archive.entry, &digest)?;
            }
            if mode == ScanMode::Discover {
                if was_complete {
                    persist(directory, existing.as_ref().expect("state"))?;
                    dequeue(directory, id)?;
                }
                return Ok(
                    json!({"assignment_id":id,"state":if was_complete {"completed"} else {"queued"}}),
                );
            }
            let usage = if was_complete && mode != ScanMode::Inspect {
                json!({})
            } else {
                usage(directory, archive.entry.launch_intent_id)?
            };
            let mut value = if was_complete {
                if mode == ScanMode::Pending {
                    persist(directory, existing.as_ref().expect("state"))?;
                }
                summary(existing.as_ref().expect("state"), "completed")
            } else {
                match CleanupEligibility::derive(&archive.entry, retention_seconds) {
                    CleanupEligibility::Blocked(error)=>json!({"assignment_id":id,"state":"blocked","diagnostic":error.message}),
                    CleanupEligibility::Eligible(authority) if !execute=>match local_protection(directory, authority.intent) {
                        Err(error)=>json!({"assignment_id":id,"state":"blocked","diagnostic":error.message}),
                        Ok(_)=>existing.as_ref().map(|s| summary(s,"pending")).unwrap_or_else(|| json!({"assignment_id":id,"launch_intent_id":authority.intent,"state":"eligible","compute_check_required":crate::compute::has_evidence(directory,authority.intent)})),
                    },
                    CleanupEligibility::Eligible(authority)=>retire(directory,&archive.entry,digest,authority,existing)?,
                }
            };
            value["storage"] = usage;
            value["newly_completed"] =
                json!(execute && !was_complete && value["state"] == "completed");
            if value["state"] == "blocked" {
                value["storage"]["retained_bytes"] = value["storage"]["local_bytes"].clone();
                value["storage"]["reclaimable_bytes"] = json!(0);
            }
            if mode == ScanMode::Pending && value["state"] == "completed" {
                dequeue(directory, id)?;
            }
            Ok(value)
        })();
        reports.push(match outcome {
            Ok(value) => value,
            Err(error) => json!({"assignment_id":id,"state":"blocked","diagnostic":error.message}),
        });
    }
    if execute {
        let target = directory.join("cleanup");
        local_process::create_private_directory(&target)?;
        replace(&target, scan_name, &scan)?;
        #[cfg(not(target_os = "linux"))]
        {
            if !more {
                entries.rewind();
            }
            keep_walk(&root, entries, scan.offset, scan.offset);
        }
    }
    let completed = reports.iter().filter(|r| r["state"] == "completed").count();
    let blocked = reports.iter().filter(|r| r["state"] == "blocked").count();
    let pending = reports.len() - completed - blocked;
    let newly_completed = reports
        .iter()
        .filter(|r| r["newly_completed"] == true)
        .count();
    Ok(
        json!({"status":"completed","runs":reports,"scanned":scanned,"more":more,"completed":completed,"newly_completed":newly_completed,"blocked":blocked,"pending":pending,"retention_seconds":retention_seconds,"entry_budget":ENTRY_BUDGET,
            "unscoped_transfer_entries":unscoped_transfers["entries"],"unscoped_transfer_entries_truncated":unscoped_transfers["truncated"],
            "unattributed_archive_entries":unattributed_archives,"unattributed_archive_entry_count":unattributed_archive_count,
            "unattributed_archive_entries_truncated":more || unattributed_archive_count>64}),
    )
}
fn summary(s: &State, status: &str) -> Value {
    json!({"assignment_id":s.assignment_id,"launch_intent_id":s.launch_intent_id,"state":status,
        "compute_complete":s.compute_complete,"connector_complete":s.connector_complete,
        "removed_entries":s.removed_entries,"removed_bytes":s.removed_bytes,
        "compute_removed_entries":s.compute_removed_entries,"compute_removed_bytes":s.compute_removed_bytes,"diagnostic":s.diagnostic})
}
fn retire(
    directory: &Path,
    entry: &PreparedAssignment,
    digest: String,
    authority: CleanupAuthority<'_>,
    prior: Option<State>,
) -> Result<Value> {
    let intent = authority.intent;
    let proof = authority.request();
    let _bulk = named_lock(directory, "bulk-locks", intent, false)?;
    let _reads = named_lock(directory, "payload-locks", intent, false)?;
    let _configuration = configuration_fence(directory, true)?;
    let mut state = prior.unwrap_or_else(|| State {
        version: 1,
        assignment_id: entry.assignment.decoded().assignment_id,
        launch_intent_id: intent,
        archive_sha256: digest,
        request: proof.clone(),
        compute_complete: false,
        connector_complete: false,
        removed_entries: 0,
        removed_bytes: 0,
        compute_removed_entries: 0,
        compute_removed_bytes: 0,
        legacy_cursors: BTreeMap::new(),
        diagnostic: None,
    });
    if !state.request.same_delivery(&proof)? {
        return Err(failure("cleanup proof conflicts with its durable intent"));
    }
    state.request.protection = Some(local_protection(directory, intent)?);
    persist(directory, &state)?; // durable intent always precedes any payload deletion
    let outcome = (|| -> Result<()> {
        let compute = crate::compute::has_evidence(directory, intent);
        if state.progress()? == CleanupProgress::ComputePending {
            if compute {
                let reply = crate::compute::cleanup_run(
                    directory,
                    intent,
                    &entry.assignment,
                    &state.request,
                )?;
                state.record_compute_reply(&reply)?;
            } else {
                let local = local_process::job_directory(directory, intent);
                let completion = local_process::read_completion(&local, intent, &entry.assignment)?
                    .ok_or_else(|| failure("owned execution has no allocation release evidence"))?;
                state.record_local_release(entry, &completion)?;
            }
            persist(directory, &state)?;
            if state.progress()? == CleanupProgress::ComputePending {
                return Ok(());
            }
        }
        if state.progress()? == CleanupProgress::Complete {
            return Ok(());
        }
        let local = local_process::job_directory(directory, intent);
        let _worker = worker_fence(&local)?;
        let mut budget = Budget::new();
        let mut done = payloads(&local, &mut budget)?;
        let compute_directory = directory.join("compute-jobs").join(intent.to_string());
        done &= payloads(&compute_directory, &mut budget)?;
        done &= remove_tree(
            &directory.join("input-staging"),
            &intent.to_string(),
            &mut budget,
        )?;
        done &= remove_tree(
            &directory.join("artifact-transfers"),
            &intent.to_string(),
            &mut budget,
        )?;
        done &= legacy_transfers(
            directory,
            intent,
            entry,
            &mut state.legacy_cursors,
            &mut budget,
        )?;
        state.record_connector_pass(done, &budget)?;
        Ok(())
    })();
    if let Err(error) = outcome {
        state.diagnostic = Some(error.message);
    }
    persist(directory, &state)?;
    if state.complete() {
        crate::spool::release(directory, intent)?;
        replace(
            &directory.join("cleanup"),
            "pins-revision.json",
            &json!({"revision":Uuid::new_v4()}),
        )?;
    }
    Ok(summary(
        &state,
        if state.diagnostic.is_some() {
            "blocked"
        } else if state.complete() {
            "completed"
        } else {
            "pending"
        },
    ))
}

/// Full reference inventory for the configuration background worker. The journal
/// owner still unions active assignments before applying a pruning proposal.
#[derive(Clone, PartialEq, Eq)]
struct PinSignature {
    archive_modified: std::time::SystemTime,
    revision: Option<Uuid>,
    journal_id: Uuid,
    connector_id: String,
    pending: bool,
}
struct PinInventory {
    signature: PinSignature,
    entries: fs::ReadDir,
    keys: HashSet<(String, String)>,
    complete: bool,
    expires_at: Instant,
}
static PIN_INVENTORIES: std::sync::LazyLock<
    std::sync::Mutex<BTreeMap<std::path::PathBuf, PinInventory>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));
fn pin_signature(directory: &Path) -> Result<PinSignature> {
    #[derive(Deserialize)]
    struct Identity {
        version: u32,
        journal_id: Uuid,
        connector_id: String,
    }
    let bytes = local_process::read_private(&directory.join("journal.json"), 16 * 1024 * 1024)?;
    let identity: Identity = serde_json::from_slice(&bytes)
        .map_err(|_| failure("invalid journal identity for installation reference inventory"))?;
    if identity.version != 1 || identity.journal_id.is_nil() || identity.connector_id.is_empty() {
        return Err(failure(
            "installation inventory has invalid journal ownership",
        ));
    }
    let pending = discovery_complete(directory)?;
    let archive_modified = fs::metadata(directory.join(if pending {
        "cleanup/pending"
    } else {
        "archive"
    }))
    .and_then(|m| m.modified())
    .map_err(|_| failure("cannot inspect archive inventory revision"))?;
    let path = directory.join("cleanup/pins-revision.json");
    let revision = if path.exists() {
        let _root = open_directory(&directory.join("cleanup"))?;
        let value: Value = local_process::read_record(&path)?;
        Some(
            value["revision"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .filter(|id| !id.is_nil())
                .ok_or_else(|| failure("malformed installation pin revision"))?,
        )
    } else {
        None
    };
    Ok(PinSignature {
        archive_modified,
        revision,
        journal_id: identity.journal_id,
        connector_id: identity.connector_id,
        pending,
    })
}
pub(crate) fn retained_binding_keys(directory: &Path) -> Result<HashSet<(String, String)>> {
    let root = directory.join(if discovery_complete(directory)? {
        "cleanup/pending"
    } else {
        "archive"
    });
    if !root.exists() {
        return Ok(HashSet::new());
    }
    let _root = open_directory(&root)?;
    let signature = pin_signature(directory)?;
    let mut cache = PIN_INVENTORIES
        .lock()
        .expect("installation reference inventories");
    if cache
        .get(directory)
        .is_some_and(|p| p.complete && (p.signature != signature || Instant::now() >= p.expires_at))
    {
        cache.remove(directory);
    }
    if cache.len() >= 16 && !cache.contains_key(directory) {
        cache.pop_first();
    }
    if !cache.contains_key(directory) {
        cache.insert(
            directory.to_owned(),
            PinInventory {
                signature: signature.clone(),
                entries: fs::read_dir(&root)
                    .map_err(|_| failure("cannot inspect retained installation references"))?,
                keys: HashSet::new(),
                complete: false,
                expires_at: Instant::now() + Duration::from_secs(300),
            },
        );
    }
    let inventory = cache.get_mut(directory).expect("inventory");
    if inventory.complete {
        return Ok(inventory.keys.clone());
    }
    let deadline = Instant::now() + TIME_BUDGET;
    let mut scanned = 0;
    while scanned < ENTRY_BUDGET && Instant::now() < deadline {
        let Some(entry) = inventory.entries.next() else {
            let current = pin_signature(directory)?;
            if current.archive_modified != inventory.signature.archive_modified
                || current.revision != inventory.signature.revision
                || current.journal_id != inventory.signature.journal_id
                || current.connector_id != inventory.signature.connector_id
                || current.pending != inventory.signature.pending
            {
                cache.remove(directory);
                return Err(failure(
                    "installation reference inventory changed; retain configuration until the next pass",
                ));
            }
            inventory.complete = true;
            inventory.expires_at = Instant::now() + Duration::from_secs(300);
            return Ok(inventory.keys.clone());
        };
        scanned += 1;
        let outcome = (|| {
            let path = entry
                .map_err(|_| failure("cannot inspect retained installation reference"))?
                .path();
            pin_from_archive(directory, &path, &inventory.signature)
        })();
        match outcome {
            Ok(Some(key)) => {
                inventory.keys.insert(key);
            }
            Ok(None) => (),
            Err(error) => {
                cache.remove(directory);
                return Err(error);
            }
        }
    }
    Err(failure(
        "installation reference inventory is incomplete; retain configuration while bounded scanning resumes",
    ))
}
fn pin_from_archive(
    directory: &Path,
    path: &Path,
    owner: &PinSignature,
) -> Result<Option<(String, String)>> {
    if path.extension().is_none_or(|e| e != "json") {
        return Ok(None);
    }
    let archive_path;
    let path = if owner.pending {
        archive_path = queued_archive(directory, path)?;
        archive_path.as_path()
    } else {
        path
    };
    let bytes = local_process::read_private(path, 16 * 1024 * 1024)?;
    let archived: Archived = serde_json::from_slice(&bytes)
        .map_err(|_| failure("malformed retained installation evidence"))?;
    if archived.version != 1
        || archived.journal_id != owner.journal_id
        || archived.entry.assignment.decoded().journal_id != owner.journal_id
        || archived.entry.assignment.decoded().connector_id != owner.connector_id
        || path.file_stem().and_then(|s| s.to_str())
            != Some(
                archived
                    .entry
                    .assignment
                    .decoded()
                    .assignment_id
                    .to_string()
                    .as_str(),
            )
    {
        return Err(failure(
            "retained installation archive has conflicting identities",
        ));
    }
    crate::execution::validate_history(&archived.entry)?;
    crate::logs::validate_history(&archived.entry)?;
    let assignment = archived.entry.assignment.decoded();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let state = state(directory, assignment.assignment_id)?;
    if let Some(s) = &state {
        validate_state(s, &archived.entry, &digest)?;
    }
    if state.is_none_or(|s| !s.complete()) {
        Ok(Some((
            assignment.context_id.clone(),
            assignment.descriptor.provider_binding_id.clone(),
        )))
    } else {
        Ok(None)
    }
}
fn validate_state(state: &State, entry: &PreparedAssignment, digest: &str) -> Result<()> {
    state.progress()?;
    let proof = request(entry, 0)?;
    if state.archive_sha256 != digest
        || Some(state.launch_intent_id) != entry.launch_intent_id
        || state.assignment_id != entry.assignment.decoded().assignment_id
        || !state.request.same_delivery(&proof)?
    {
        return Err(failure(
            "cleanup checkpoint does not match archived authority",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Usage {
    bytes: u64,
    reclaimable: u64,
    entries: usize,
    truncated: bool,
}
fn usage(directory: &Path, intent: Option<Uuid>) -> Result<Value> {
    let mut usage = Usage::default();
    if let Some(intent) = intent {
        let deadline = Instant::now() + TIME_BUDGET;
        for (root, all_payload) in [
            (local_process::job_directory(directory, intent), false),
            (
                directory.join("compute-jobs").join(intent.to_string()),
                false,
            ),
            (
                directory.join("input-staging").join(intent.to_string()),
                true,
            ),
            (
                directory
                    .join("artifact-transfers")
                    .join(intent.to_string()),
                true,
            ),
        ] {
            if !root.exists() {
                continue;
            }
            let fd = open_directory(&root)?;
            measure(
                &fd,
                mount(&fd)?,
                all_payload,
                false,
                &mut usage,
                deadline,
                0,
            )?;
        }
    }
    Ok(
        json!({"local_bytes":usage.bytes,"retained_bytes":usage.bytes.saturating_sub(usage.reclaimable),"reclaimable_bytes":usage.reclaimable,"entries":usage.entries,
        "truncated":usage.truncated,"remote_bytes_available":false}),
    )
}
fn measure(
    root: &OwnedFd,
    root_mount: u64,
    payload: bool,
    metadata_source: bool,
    usage: &mut Usage,
    deadline: Instant,
    depth: usize,
) -> Result<()> {
    if depth > 128 {
        usage.truncated = true;
        return Ok(());
    }
    let mut entries =
        Dir::read_from(root).map_err(|_| failure("cannot inspect retained payload capacity"))?;
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(|_| failure("cannot read retained payload capacity"))?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if usage.entries >= ENTRY_BUDGET || Instant::now() >= deadline {
            usage.truncated = true;
            return Ok(());
        }
        usage.entries += 1;
        let stat = rfs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| failure("cannot inspect retained payload entry"))?;
        let kind = FileType::from_raw_mode(stat.st_mode);
        let child_payload = payload
            || [
                b"workspace".as_slice(),
                b"inputs",
                b"outputs",
                b"archive-copy",
                b"log-captures",
                b"log-archives",
                b"slurm.log",
            ]
            .contains(&name.to_bytes());
        let metadata_source = metadata_source
            || [b"log-captures".as_slice(), b"log-archives"].contains(&name.to_bytes());
        if kind == FileType::Directory {
            let fd = rfs::openat(
                root,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| failure("retained capacity directory changed"))?;
            if mount(&fd)? != root_mount {
                usage.truncated = true;
                continue;
            }
            measure(
                &fd,
                root_mount,
                child_payload,
                metadata_source,
                usage,
                deadline,
                depth + 1,
            )?;
        } else {
            let bytes = stat.st_size.max(0) as u64;
            usage.bytes = usage.bytes.saturating_add(bytes);
            if child_payload
                && !(metadata_source
                    && [b"manifest.json".as_slice(), b"archive.json"].contains(&name.to_bytes()))
            {
                usage.reclaimable = usage.reclaimable.saturating_add(bytes);
            }
        }
    }
    Ok(())
}

pub(crate) fn read_fence(directory: &Path, intent: Uuid) -> Result<File> {
    named_lock(directory, "payload-locks", intent, true)
}
fn named_lock(directory: &Path, subdirectory: &str, intent: Uuid, shared: bool) -> Result<File> {
    let _root = open_directory(directory)?;
    let root = directory.join(subdirectory);
    local_process::create_private_directory(&root)?;
    lock_file(&root, &format!("{intent}.lock"), shared)
}
pub(crate) fn payload_fence(directory: &Path, shared: bool) -> Result<File> {
    lock_file(directory, "payload.lock", shared)
}
/// Local configuration publication and payload deletion share the protection
/// inventory. Writers yield rather than block the journal-owning service loop.
pub(crate) fn configuration_fence(directory: &Path, shared: bool) -> Result<File> {
    let canonical = fs::canonicalize(directory)
        .map_err(|_| failure("cannot resolve installation protection journal"))?;
    lock_file(&canonical, "configuration-cleanup.lock", shared)
}
fn lock_file(directory: &Path, name: &str, shared: bool) -> Result<File> {
    let root = open_directory(directory)?;
    let fd = rfs::openat(
        &root,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| failure("cannot open cleanup ownership fence"))?;
    let metadata =
        rfs::fstat(&fd).map_err(|_| failure("cannot inspect cleanup ownership fence"))?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile
        || metadata.st_mode & 0o077 != 0
    {
        return Err(failure(
            "cleanup ownership fence is not a private regular file",
        ));
    }
    let file = File::from(fd);
    if shared {
        file.lock_shared()
            .map_err(|_| failure("cannot acquire payload read fence"))?;
    } else {
        file.try_lock().map_err(|_| {
            failure("payload is still owned by a transfer or reader")
                .with_kind(crate::ErrorKind::Waiting)
        })?;
    }
    Ok(file)
}
pub(crate) fn worker_fence(directory: &Path) -> Result<Option<File>> {
    match fs::symlink_metadata(directory.join("worker.lock")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(failure("cannot inspect worker ownership fence")),
        Ok(_) => (),
    }
    let file = lock_file(directory, "worker.lock", false)?;
    Ok(Some(file))
}

/// Descriptor-relative traversal rejects symlink ancestors and never follows a
/// symlink entry. Linux mount IDs additionally catch same-device bind mounts.
fn open_directory(path: &Path) -> Result<OwnedFd> {
    let absolute;
    let path = if path.is_absolute() {
        path
    } else {
        absolute = std::env::current_dir()
            .map_err(|_| failure("cannot resolve relative cleanup root"))?
            .join(path);
        &absolute
    };
    let mut fd = rfs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| failure("cannot open cleanup root"))?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => {
                fd = rfs::openat(
                    &fd,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| failure("cleanup root is unavailable or traverses a symbolic link"))?
            }
            _ => return Err(failure("cleanup root contains an unsafe component")),
        }
    }
    Ok(fd)
}
pub(crate) struct Budget {
    deadline: Instant,
    remaining: usize,
    pub removed_entries: u64,
    pub removed_bytes: u64,
}
impl Budget {
    pub(crate) fn new() -> Self {
        Self {
            deadline: Instant::now() + TIME_BUDGET,
            remaining: ENTRY_BUDGET,
            removed_entries: 0,
            removed_bytes: 0,
        }
    }
    fn available(&self) -> bool {
        self.remaining != 0 && Instant::now() < self.deadline
    }
    fn removed(&mut self, bytes: u64) {
        self.remaining -= 1;
        self.removed_entries += 1;
        self.removed_bytes = self.removed_bytes.saturating_add(bytes);
    }
}
fn mount(fd: impl AsFd) -> Result<u64> {
    #[cfg(target_os = "linux")]
    {
        match rfs::statx(fd.as_fd(), "", AtFlags::EMPTY_PATH, rfs::StatxFlags::MNT_ID) {
            Ok(stat) if stat.stx_mask & rfs::StatxFlags::MNT_ID.bits() != 0 => {
                return Ok(stat.stx_mnt_id);
            }
            Ok(_)
            | Err(
                rustix::io::Errno::NOSYS
                | rustix::io::Errno::INVAL
                | rustix::io::Errno::NOTSUP
                | rustix::io::Errno::PERM,
            ) => (),
            Err(_) => return Err(failure("cleanup cannot verify mount confinement")),
        }
        proc_mount(fd)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(rfs::fstat(fd)
            .map_err(|_| failure("cleanup cannot verify filesystem identity"))?
            .st_dev as u64)
    }
}
#[cfg(target_os = "linux")]
fn proc_mount(fd: impl AsFd) -> Result<u64> {
    use rustix::fd::AsRawFd;
    // fdinfo has exposed the same mount IDs since Linux 3.15. Keep the
    // descriptor alive while reading it; device IDs cannot detect bind mounts.
    let info = File::open(format!("/proc/self/fdinfo/{}", fd.as_fd().as_raw_fd()))
        .map_err(|_| failure("cleanup mount identity is unavailable"))?;
    read_proc_mount(info)
}
#[cfg(target_os = "linux")]
fn read_proc_mount(info: File) -> Result<u64> {
    use std::io::Read;
    if rfs::fstatfs(&info)
        .map_err(|_| failure("cleanup cannot verify procfs identity"))?
        .f_type
        != rfs::PROC_SUPER_MAGIC
    {
        return Err(failure("cleanup mount identity did not come from procfs"));
    }
    let mut text = String::new();
    info.take(4097)
        .read_to_string(&mut text)
        .map_err(|_| failure("cleanup cannot read mount identity"))?;
    parse_proc_mount(&text)
}
#[cfg(target_os = "linux")]
fn parse_proc_mount(text: &str) -> Result<u64> {
    let unavailable = || failure("cleanup mount identity is unavailable");
    if text.len() > 4096 {
        return Err(unavailable());
    }
    let mut fields = text.lines().filter_map(|line| line.strip_prefix("mnt_id:"));
    let value = fields.next().ok_or_else(unavailable)?.trim();
    if fields.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(unavailable());
    }
    value.parse().map_err(|_| unavailable())
}
fn remove_entry(
    parent: &OwnedFd,
    name: &std::ffi::CStr,
    root_mount: u64,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool> {
    if !budget.available() {
        return Ok(false);
    }
    if depth > 128 {
        return Err(failure("payload exceeds safe cleanup traversal depth"));
    }
    let stat = match rfs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(true),
        Err(_) => return Err(failure("cannot inspect owned payload entry")),
    };
    let directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
    if directory {
        let fd = rfs::openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| failure("owned payload directory changed or is unavailable"))?;
        if mount(&fd)? != root_mount {
            return Err(failure("cleanup will not cross a nested mount"));
        }
        let mut entries = Dir::read_from(&fd).map_err(|_| failure("cannot list owned payload"))?;
        while let Some(entry) = entries.read() {
            if !budget.available() {
                return Ok(false);
            }
            let entry = entry.map_err(|_| failure("cannot read owned payload entry"))?;
            if entry.file_name().to_bytes() == b"." || entry.file_name().to_bytes() == b".." {
                continue;
            }
            if !remove_entry(&fd, entry.file_name(), root_mount, budget, depth + 1)? {
                return Ok(false);
            }
        }
    }
    if !budget.available() {
        return Ok(false);
    }
    let flags = if directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    match rfs::unlinkat(parent, name, flags) {
        Ok(()) => (),
        Err(rustix::io::Errno::NOENT) => return Ok(true),
        Err(_) => return Err(failure("cannot retire owned payload entry")),
    }
    rfs::fsync(parent).map_err(|_| failure("cannot sync payload retirement"))?;
    budget.removed(if directory {
        0
    } else {
        stat.st_size.max(0) as u64
    });
    Ok(true)
}
fn remove_tree(parent: &Path, name: &str, budget: &mut Budget) -> Result<bool> {
    if !parent
        .try_exists()
        .map_err(|_| failure("cannot inspect owned payload root"))?
    {
        return Ok(true);
    }
    let root = open_directory(parent)?;
    let name = std::ffi::CString::new(name).map_err(|_| failure("unsafe payload name"))?;
    remove_entry(&root, &name, mount(&root)?, budget, 0)
}
fn contents(
    directory: &Path,
    preserve: &str,
    root_mount: u64,
    budget: &mut Budget,
) -> Result<bool> {
    if !directory
        .try_exists()
        .map_err(|_| failure("cannot inspect payload capture"))?
    {
        return Ok(true);
    }
    let root = open_directory(directory)?;
    if mount(&root)? != root_mount {
        return Err(failure("cleanup will not cross a nested capture mount"));
    }
    let mut entries = Dir::read_from(&root).map_err(|_| failure("cannot list payload capture"))?;
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(|_| failure("cannot read payload capture"))?;
        if [b".".as_slice(), b"..", preserve.as_bytes()].contains(&entry.file_name().to_bytes()) {
            continue;
        }
        if !remove_entry(&root, entry.file_name(), root_mount, budget, 0)? {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(crate) fn payloads(directory: &Path, budget: &mut Budget) -> Result<bool> {
    if !directory
        .try_exists()
        .map_err(|_| failure("cannot inspect owned payload root"))?
    {
        return Ok(true);
    }
    let root = open_directory(directory)?;
    let root_mount = mount(&root)?;
    if let Some(parent) = directory.parent()
        && mount(open_directory(parent)?)? != root_mount
    {
        return Err(failure("cleanup will not cross a mounted execution root"));
    }
    let mut done = true;
    for name in [
        "workspace",
        "inputs",
        "outputs",
        "archive-copy",
        "collection-payloads",
    ] {
        done &= remove_tree(directory, name, budget)?;
    }
    let captures = directory.join("log-captures");
    let mut sources = Vec::new();
    if captures.exists() {
        for entry in fs::read_dir(&captures).map_err(|_| failure("cannot inspect captures"))? {
            let name = entry
                .map_err(|_| failure("cannot inspect capture"))?
                .file_name();
            let name = name
                .to_str()
                .ok_or_else(|| failure("invalid capture source"))?;
            let source: enumeray_contracts::logs::LogSource =
                serde_json::from_value(json!(name))
                    .map_err(|_| failure("invalid capture source"))?;
            sources.push(source);
            if sources.len() > enumeray_contracts::logs::MAX_MODEL_LOG_FILES + 3 {
                return Err(failure("too many capture sources"));
            }
        }
    }
    for source in sources {
        done &= contents(
            &directory.join("log-captures").join(source.as_str()),
            "manifest.json",
            root_mount,
            budget,
        )?;
    }
    done &= contents(
        &directory.join("log-archives"),
        "archive.json",
        root_mount,
        budget,
    )?;
    done &= remove_tree(directory, "slurm.log", budget)?;
    Ok(done)
}
fn legacy_transfers(
    directory: &Path,
    intent: Uuid,
    entry: &PreparedAssignment,
    cursors: &mut BTreeMap<String, u64>,
    budget: &mut Budget,
) -> Result<bool> {
    let parent = directory.join("artifact-transfers");
    if !parent.exists() {
        return Ok(true);
    }
    let mut names: Vec<_> = entry
        .execution
        .as_ref()
        .map(|e| e.files.keys().map(|n| (false, n.clone())).collect())
        .unwrap_or_default();
    if entry.log_archive.is_some() {
        names.push((true, "log-archive.tar.gz".into()));
    }
    let mut done = true;
    for (archive, name) in names {
        let original = crate::artifacts::transfer_key(intent, &name, archive, 0);
        let reservation = format!("reservation-{original}.json");
        let label = format!("{}/{name}", if archive { "archive" } else { "output" });
        let generation = if parent.join(&reservation).exists() {
            let record: Value = local_process::read_record(&parent.join(&reservation))?;
            record["generation"]
                .as_u64()
                .filter(|g| *g <= u32::MAX as u64)
                .ok_or_else(|| failure("legacy transfer generation is invalid"))?
        } else {
            0
        };
        let cursor = cursors.entry(label).or_default();
        while *cursor <= generation {
            if !budget.available() {
                done = false;
                break;
            }
            let filename = format!(
                "{}.json",
                crate::artifacts::transfer_key(intent, &name, archive, *cursor)
            );
            let before = budget.removed_entries;
            if !remove_tree(&parent, &filename, budget)? {
                done = false;
                break;
            }
            if before == budget.removed_entries {
                budget.remaining -= 1;
            }
            *cursor += 1;
        }
        if *cursor > generation && !remove_tree(&parent, &reservation, budget)? {
            done = false;
        }
    }
    Ok(done)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteState {
    pub version: u32,
    pub launch_intent_id: Uuid,
    pub request: Request,
    pub allocation_released: bool,
    pub complete: bool,
    pub removed_entries: u64,
    pub removed_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    // Run by scripts/cleanup-mounts-smoke.sh with two same-device bind mounts.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn cleanup_rejects_same_filesystem_bind_mount() {
        let root = Path::new("/fixture");
        assert_eq!(
            fs::read(root.join("fixture-kind")).unwrap(),
            b"enumeray-cleanup-bind-mount-v1\n"
        );
        let parent = root.join("owned");
        let owned = open_directory(&parent).unwrap();
        let nested = open_directory(&parent.join("payload/nested")).unwrap();
        assert_eq!(
            rfs::fstat(&owned).unwrap().st_dev,
            rfs::fstat(&nested).unwrap().st_dev
        );
        assert_ne!(mount(&owned).unwrap(), mount(&nested).unwrap());
        let error = remove_tree(&parent, "payload", &mut Budget::new()).unwrap_err();
        assert!(error.to_string().contains("nested mount"), "{error}");
        assert_eq!(
            fs::read(root.join("outside/sentinel")).unwrap(),
            b"preserve\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_mount_identity_rejects_missing_ambiguous_and_malformed_fields() {
        assert_eq!(
            parse_proc_mount("pos:\t0\nflags:\t02300000\nmnt_id:\t638\n").unwrap(),
            638
        );
        for text in [
            "",
            "mnt_id:\n",
            "mount_id: 3\n",
            "mnt_id: -1\n",
            "mnt_id: +1\n",
            "mnt_id: 1 2\n",
            "mnt_id: 18446744073709551616\n",
            "mnt_id: 1\nmnt_id: 1\n",
            "mnt_id: 1\nmnt_id: 2\n",
        ] {
            assert!(parse_proc_mount(text).is_err(), "{text:?}");
        }
        assert!(parse_proc_mount(&format!("mnt_id: 1\n{}", "x".repeat(4096))).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_mount_identity_uses_the_open_descriptor_and_matches_statx() {
        let scratch = Scratch::new();
        let directory = scratch.directory("held");
        let held = open_directory(&directory).unwrap();
        let expected = mount(&held).unwrap();
        fs::rename(&directory, scratch.0.join("moved")).unwrap();
        std::os::unix::fs::symlink("/proc", &directory).unwrap();
        assert_eq!(proc_mount(&held).unwrap(), expected);
        let proc = open_directory(Path::new("/proc")).unwrap();
        assert_ne!(proc_mount(&proc).unwrap(), expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_mount_identity_refuses_spoofed_regular_files() {
        let scratch = Scratch::new();
        let path = scratch.0.join("fake-fdinfo");
        fs::write(&path, b"mnt_id: 638\n").unwrap();
        assert!(read_proc_mount(File::open(path).unwrap()).is_err());
    }

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let base = std::env::temp_dir().canonicalize().unwrap();
            let root = base.join(format!("enumeray-cleanup-{}", Uuid::new_v4()));
            local_process::create_private_directory(&root).unwrap();
            Self(root)
        }
        fn directory(&self, relative: &str) -> std::path::PathBuf {
            let mut directory = self.0.clone();
            for component in relative.split('/') {
                directory.push(component);
                local_process::create_private_directory(&directory).unwrap();
            }
            directory
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn entry() -> PreparedAssignment {
        let mut document: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/journal-minimal-evidence.json"
        ))
        .unwrap();
        let e = &mut document["prepared_assignments"][0];
        let report = json!({"attempt_id":e["assignment"]["attempt_id"],"outcome":"succeeded","execution_terminated":true,
            "outputs":{"doubled":14},"files":{},"optional_output_failures":{},"failure":null});
        e["execution"]["report"] = report.clone();
        e["execution"]["receipt"]["provider_cleanup_acknowledged"] = json!(true);
        e["terminal_result"] = json!({"receipt":e["execution"]["receipt"],"outputs":report["outputs"],"files":{},"optional_output_failures":{},"failure":null});
        for event in e["execution"]["events"].as_array_mut().unwrap() {
            event["request"]["backend_handle"]["namespace"] = json!("enumeray-process-v1");
        }
        let stream =
            json!({"epoch":1,"byte_offset":0,"next_sequence":2,"closed":true,"pending":null});
        e["logs"] = json!({"streams":{"stdout":stream,"stderr":stream},"cursor":0});
        serde_json::from_value(e.clone()).unwrap()
    }
    fn archived(s: &Scratch, e: &PreparedAssignment) {
        if !s.0.join("journal.json").exists() {
            local_process::write_record(&s.0,"journal.json",&json!({"version":1,
            "journal_id":e.assignment.decoded().journal_id,"connector_id":e.assignment.decoded().connector_id,"api_base_url":"http://127.0.0.1:8080",
            "active_session_id":null,"pending_claim_key":null,"prepared_assignments":[]})).unwrap();
        }
        record_release_protection(&s.0, 1, true, &[], &[]).unwrap();
        let directory = s.directory("archive");
        local_process::write_record(
            &directory,
            &format!("{}.json", e.assignment.decoded().assignment_id),
            &json!({"version":1,"journal_id":e.assignment.decoded().journal_id,"entry":e}),
        )
        .unwrap();
    }
    fn local(s: &Scratch, e: &PreparedAssignment) -> std::path::PathBuf {
        let intent = e.launch_intent_id.unwrap();
        let directory = s.directory(&format!("local-jobs/{intent}"));
        let report = e.execution.as_ref().unwrap().report.clone();
        local_process::write_record(
            &directory,
            "completion.json",
            &json!({"observed_at":"2026-09-21T12:00:00Z","report":report,"files":{}}),
        )
        .unwrap();
        local_process::write_record(&directory, "started", &true).unwrap();
        local_process::write_record(&directory, "worker.lock", &json!({})).unwrap();
        directory
    }
    fn checkpoint(e: &PreparedAssignment) -> State {
        State {
            version: 1,
            assignment_id: e.assignment.decoded().assignment_id,
            launch_intent_id: e.launch_intent_id.unwrap(),
            archive_sha256: "retained-archive".into(),
            request: request(e, 0).unwrap(),
            compute_complete: false,
            connector_complete: false,
            removed_entries: 0,
            removed_bytes: 0,
            compute_removed_entries: 0,
            compute_removed_bytes: 0,
            legacy_cursors: BTreeMap::new(),
            diagnostic: None,
        }
    }
    #[test]
    fn cleanup_eligibility_keeps_settlement_delivery_and_retention_independent() {
        let original = serde_json::to_value(entry()).unwrap();
        let cases = [
            ("/terminal_result", Value::Null),
            ("/launch_intent_id", Value::Null),
            ("/execution", Value::Null),
            ("/execution/receipt", Value::Null),
            ("/execution/report/outputs/doubled", json!(15)),
            (
                "/execution/receipt/completed_at",
                json!("2026-09-22T12:00:00Z"),
            ),
            (
                "/terminal_result/receipt/provider_cleanup_acknowledged",
                json!(false),
            ),
            ("/logs/streams/stdout/closed", json!(false)),
        ];
        for (pointer, replacement) in cases {
            let mut value = original.clone();
            *value.pointer_mut(pointer).unwrap() = replacement;
            let entry: PreparedAssignment = serde_json::from_value(value).unwrap();
            let before = serde_json::to_vec(&entry).unwrap();
            assert!(
                matches!(
                    CleanupEligibility::derive(&entry, 0),
                    CleanupEligibility::Blocked(_)
                ),
                "{pointer} must not grant cleanup authority",
            );
            assert_eq!(serde_json::to_vec(&entry).unwrap(), before, "{pointer}");
        }
        let mut historical = original.clone();
        historical.as_object_mut().unwrap().remove("transfers");
        historical
            .as_object_mut()
            .unwrap()
            .remove("delivery_completion");
        historical["terminal_result"]["retained_addition"] =
            serde_json::from_str("{\"precise\":9007199254740993}").unwrap();
        let entry: PreparedAssignment = serde_json::from_value(historical).unwrap();
        let before = serde_json::to_vec(&entry).unwrap();
        let CleanupEligibility::Eligible(authority) = CleanupEligibility::derive(&entry, 0) else {
            panic!("historical acknowledged delivery remains eligible for allocation inspection");
        };
        assert_eq!(
            authority.request().terminal_result,
            **entry.terminal_result.as_ref().unwrap()
        );
        assert_eq!(serde_json::to_vec(&entry).unwrap(), before);
        assert!(matches!(
            CleanupEligibility::derive(&entry, u64::MAX),
            CleanupEligibility::Blocked(_)
        ));

        let mut resolved = original;
        resolved["operator_resolution"] = json!({
            "assignment_id":resolved["assignment"]["assignment_id"],
            "attempt_id":resolved["assignment"]["attempt_id"],
            "execution_status":"terminated","resolved_at":"2026-09-21T12:00:00Z",
        });
        let resolved: PreparedAssignment = serde_json::from_value(resolved).unwrap();
        assert!(matches!(
            CleanupEligibility::derive(&resolved, 0),
            CleanupEligibility::Blocked(_)
        ));
    }
    #[test]
    fn cleanup_progress_replays_match_and_conflicts_preserve_the_checkpoint() {
        let entry = entry();
        for (compute, connector, expected) in [
            (false, false, Some(CleanupProgress::ComputePending)),
            (true, false, Some(CleanupProgress::ConnectorPending)),
            (true, true, Some(CleanupProgress::Complete)),
            (false, true, None),
        ] {
            let mut state = checkpoint(&entry);
            state.compute_complete = compute;
            state.connector_complete = connector;
            assert_eq!(state.progress().ok(), expected);
        }
        let mut state = checkpoint(&entry);
        let budget = Budget::new();
        assert!(state.record_connector_pass(true, &budget).is_err());
        state
            .record_compute_reply(
                &json!({"status":"pending","removed_entries":5,"removed_bytes":20}),
            )
            .unwrap();
        let before = serde_json::to_vec(&state).unwrap();
        for reply in [
            json!({"status":"running","removed_entries":5,"removed_bytes":20}),
            json!({"status":"completed","removed_entries":4,"removed_bytes":20}),
            json!({"status":"completed","removed_entries":5,"removed_bytes":19}),
            json!({"status":"completed","removed_entries":5}),
        ] {
            assert!(state.record_compute_reply(&reply).is_err());
            assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        }
        let completed = json!({"status":"completed","removed_entries":6,"removed_bytes":30});
        state.record_compute_reply(&completed).unwrap();
        let before = serde_json::to_vec(&state).unwrap();
        state.record_compute_reply(&completed).unwrap();
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        assert!(
            state
                .record_compute_reply(
                    &json!({"status":"pending","removed_entries":6,"removed_bytes":30})
                )
                .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
        state.record_connector_pass(true, &budget).unwrap();
        assert_eq!(state.progress().unwrap(), CleanupProgress::Complete);
    }
    #[test]
    fn conflicting_local_completion_cannot_release_connector_payloads() {
        let s = Scratch::new();
        let entry = entry();
        archived(&s, &entry);
        let local = local(&s, &entry);
        let workspace = s.directory(&format!(
            "local-jobs/{}/workspace",
            entry.launch_intent_id.unwrap()
        ));
        fs::write(workspace.join("payload"), b"retained").unwrap();
        let mut completion: Value =
            local_process::read_record(&local.join("completion.json")).unwrap();
        completion["report"]["outputs"]["doubled"] = json!(15);
        replace(&local, "completion.json", &completion).unwrap();
        let evidence = fs::read(local.join("completion.json")).unwrap();
        let report = run(
            &s.0,
            entry.assignment.decoded().journal_id,
            "test-connector",
            1,
            0,
        )
        .unwrap();
        assert_eq!(report["runs"][0]["state"], "blocked");
        assert!(
            report["runs"][0]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("retained execution report")
        );
        assert_eq!(fs::read(workspace.join("payload")).unwrap(), b"retained");
        assert_eq!(fs::read(local.join("completion.json")).unwrap(), evidence);
        assert_eq!(
            state(&s.0, entry.assignment.decoded().assignment_id)
                .unwrap()
                .unwrap()
                .progress()
                .unwrap(),
            CleanupProgress::ComputePending
        );
    }
    #[test]
    fn metadata_aliases_share_configuration_ownership_without_weakening_payload_traversal() {
        let s = Scratch::new();
        let alias = s.0.join("journal-alias");
        std::os::unix::fs::symlink(&s.0, &alias).unwrap();
        assert_eq!(fs::canonicalize(&alias).unwrap(), s.0);
        let reader = configuration_fence(&alias, true).unwrap();
        assert_eq!(
            configuration_fence(&s.0, false).unwrap_err().kind,
            crate::ErrorKind::Waiting
        );
        retain_release_references(&alias, &[], &[]).unwrap();
        drop(reader);
        configuration_fence(&s.0, false).unwrap();
        assert!(open_directory(&alias).is_err());
    }
    #[test]
    fn checkpoints_do_not_retain_the_catalogue_and_legacy_retries_compact_it() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        let directory = local(&s, &e);
        let workspace = s.directory(&format!(
            "local-jobs/{}/workspace",
            e.launch_intent_id.unwrap()
        ));
        for i in 0..600 {
            fs::write(workspace.join(format!("scratch-{i}")), b"payload").unwrap();
        }
        let references: Vec<_> = (0..1024)
            .map(|i| ReleaseReference {
                release_path: format!("/opt/enumeray/releases/model-{i}/r1"),
                files: vec![format!("/opt/enumeray/releases/model-{i}/r1/wrapper")],
            })
            .collect();
        record_release_protection(&s.0, 1, true, &[], &references).unwrap();
        let first = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            1,
            0,
        )
        .unwrap();
        assert_eq!(first["runs"][0]["state"], "pending");
        let id = e.assignment.decoded().assignment_id;
        let path = s.0.join("cleanup").join(format!("{id}.json"));
        let mut old = state(&s.0, id).unwrap().unwrap();
        assert!(old.request.protection.is_none());
        assert!(fs::metadata(&path).unwrap().len() < 16 * 1024);
        old.request.protection = Some(release_protection(&s.0).unwrap());
        replace(&s.0.join("cleanup"), &format!("{id}.json"), &old).unwrap();
        assert!(fs::metadata(&path).unwrap().len() > 100 * 1024);
        for _ in 0..6 {
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                1,
                0,
            )
            .unwrap();
        }
        let complete = state(&s.0, id).unwrap().unwrap();
        assert!(complete.connector_complete && complete.compute_complete);
        assert!(complete.request.protection.is_none());
        assert!(fs::metadata(&path).unwrap().len() < 16 * 1024);
        assert!(
            !s.0.join("cleanup/pending")
                .join(format!("{id}.json"))
                .exists()
        );
        assert!(!workspace.exists());
        assert!(directory.join("completion.json").exists());
    }
    #[test]
    fn pending_scan_at_end_retries_retained_payloads_in_the_same_pass() {
        for (restarted, limit) in [(false, 1), (false, 8), (true, 1), (true, 8)] {
            let s = Scratch::new();
            let e = entry();
            archived(&s, &e);
            local(&s, &e);
            let workspace = s.directory(&format!(
                "local-jobs/{}/workspace",
                e.launch_intent_id.unwrap()
            ));
            fs::write(workspace.join("payload"), b"retained").unwrap();
            let archive_path =
                s.0.join("archive")
                    .join(format!("{}.json", e.assignment.decoded().assignment_id));
            let history = fs::read(&archive_path).unwrap();
            let journal_id = e.assignment.decoded().journal_id;

            // The service stops a full page immediately after its last retained
            // run, leaving the durable cursor at the end of the pending queue.
            let retained = run(&s.0, journal_id, "test-connector", 1, u64::MAX).unwrap();
            assert_eq!(retained["runs"][0]["state"], "blocked");
            assert_eq!(retained["more"], true);
            let scan_path = s.0.join("cleanup/pending-scan.json");
            let cursor = fs::read(&scan_path).unwrap();
            assert!(
                local_process::read_record::<Scan>(&scan_path)
                    .unwrap()
                    .offset
                    != 0
            );
            if restarted {
                #[cfg(not(target_os = "linux"))]
                take_walk(&s.0.join("cleanup/pending"));
            }
            let dry = inspect(&s.0, journal_id, "test-connector", 1, 0).unwrap();
            assert_eq!(dry["runs"][0]["state"], "eligible");
            assert_eq!(fs::read(&scan_path).unwrap(), cursor);

            let applied = run(&s.0, journal_id, "test-connector", limit, 0).unwrap();
            assert_eq!(applied["runs"].as_array().unwrap().len(), 1, "{applied}");
            assert_eq!(applied["runs"][0]["state"], "completed");
            assert_eq!(applied["newly_completed"], 1);
            assert!(!workspace.exists());
            assert_eq!(fs::read(&archive_path).unwrap(), history);

            let idle = run(&s.0, journal_id, "test-connector", 1, 0).unwrap();
            assert!(idle["runs"].as_array().unwrap().is_empty());
            assert_eq!(idle["more"], false);
        }
    }
    #[test]
    fn pending_scan_wrap_does_not_repeat_a_blocked_run_to_fill_the_page() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        let workspace = s.directory(&format!(
            "local-jobs/{}/workspace",
            e.launch_intent_id.unwrap()
        ));
        let payload = workspace.join("payload");
        fs::write(&payload, b"retained").unwrap();
        let journal_id = e.assignment.decoded().journal_id;
        run(&s.0, journal_id, "test-connector", 1, u64::MAX).unwrap();

        let retry = run(&s.0, journal_id, "test-connector", 8, u64::MAX).unwrap();
        assert_eq!(retry["runs"].as_array().unwrap().len(), 1, "{retry}");
        assert_eq!(retry["runs"][0]["state"], "blocked");
        assert_eq!(retry["scanned"], 1);
        assert_eq!(retry["more"], false);
        assert_eq!(fs::read(&payload).unwrap(), b"retained");
        assert!(
            state(&s.0, e.assignment.decoded().assignment_id)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn pending_work_progresses_during_discovery_and_completed_history_is_not_rescanned() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            8,
            0,
        )
        .unwrap();
        let original = state(&s.0, e.assignment.decoded().assignment_id)
            .unwrap()
            .unwrap();
        assert!(original.connector_complete && original.compute_complete);
        // An upgraded journal may contain a large already-cleaned history.
        let mut historical = Vec::new();
        for _ in 0..300 {
            let id = Uuid::new_v4();
            let old: PreparedAssignment =
                serde_json::from_str(&serde_json::to_string(&e).unwrap().replace(
                    &e.assignment.decoded().assignment_id.to_string(),
                    &id.to_string(),
                ))
                .unwrap();
            archived(&s, &old);
            let path = s.0.join("archive").join(format!("{id}.json"));
            let mut checkpoint = original.clone();
            checkpoint.assignment_id = id;
            checkpoint.request = request(&old, 0).unwrap();
            checkpoint.archive_sha256 = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
            persist(&s.0, &checkpoint).unwrap();
            historical.push(path);
        }
        fs::remove_file(s.0.join("cleanup/discovery.json")).unwrap();
        let id = Uuid::new_v4();
        let fresh: PreparedAssignment =
            serde_json::from_str(&serde_json::to_string(&e).unwrap().replace(
                &e.assignment.decoded().assignment_id.to_string(),
                &id.to_string(),
            ))
            .unwrap();
        archived(&s, &fresh);
        enqueue(&s.0, id).unwrap(); // normal archive publication bypasses discovery
        let first = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            1,
            0,
        )
        .unwrap();
        assert_eq!(first["runs"][0]["assignment_id"], json!(id));
        assert_eq!(first["runs"][0]["state"], "completed");
        assert_eq!(first["discovery"]["more"], true);
        for _ in 0..512 {
            if discovery_complete(&s.0).unwrap() {
                break;
            }
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                1,
                0,
            )
            .unwrap();
        }
        assert!(discovery_complete(&s.0).unwrap());
        // Finished archives remain inspectable but are outside routine work.
        fs::write(&historical[0], b"damaged historical record").unwrap();
        let idle = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            1,
            0,
        )
        .unwrap();
        assert_eq!(idle["scanned"], 0);
        assert!(idle["runs"].as_array().unwrap().is_empty());
        assert!(retained_binding_keys(&s.0).unwrap().is_empty());
        // A durable completion with a surviving marker is dequeued on retry.
        enqueue(&s.0, id).unwrap();
        assert_eq!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                1,
                0
            )
            .unwrap()["runs"][0]["state"],
            "completed"
        );
        assert!(
            !s.0.join("cleanup/pending")
                .join(format!("{id}.json"))
                .exists()
        );
    }
    #[test]
    fn discovery_cannot_finish_without_durable_queue_publication() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        let pending = s.directory("cleanup/pending");
        let id = e.assignment.decoded().assignment_id;
        replace(
            &pending,
            &format!("{id}.json"),
            &Pending {
                version: 2,
                assignment_id: id,
            },
        )
        .unwrap();
        assert!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                1,
                0
            )
            .is_err()
        );
        assert!(!discovery_complete(&s.0).unwrap());
        assert!(
            retained_binding_keys(&s.0)
                .unwrap()
                .contains(&("test-context".into(), "scalar-double-r1".into()))
        );
        replace(
            &pending,
            &format!("{id}.json"),
            &Pending {
                version: 1,
                assignment_id: id,
            },
        )
        .unwrap();
        assert_eq!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                1,
                0
            )
            .unwrap()["runs"][0]["state"],
            "completed"
        );
        assert!(discovery_complete(&s.0).unwrap());
        fs::remove_dir(&pending).unwrap();
        assert!(discovery_complete(&s.0).is_err());
        assert!(retained_binding_keys(&s.0).is_err());
        let empty = Scratch::new();
        empty.directory("archive");
        run(&empty.0, Uuid::new_v4(), "test-connector", 1, 0).unwrap();
        assert!(discovery_complete(&empty.0).unwrap());
    }
    #[test]
    fn missing_authority_pending_delivery_and_grace_never_reclaim() {
        let e = entry();
        crate::execution::validate_history(&e).unwrap();
        assert!(request(&e, 0).is_ok());
        assert!(request(&e, u64::MAX).is_err());
        for pointer in [
            "/terminal_result/receipt/provider_cleanup_acknowledged",
            "/logs/streams/stdout/closed",
        ] {
            let mut value = serde_json::to_value(&e).unwrap();
            *value.pointer_mut(pointer).unwrap() = json!(false);
            let changed: PreparedAssignment = serde_json::from_value(value).unwrap();
            assert!(request(&changed, 0).is_err());
        }
        let mut value = serde_json::to_value(&e).unwrap();
        value["terminal_result"]["receipt"]
            .as_object_mut()
            .unwrap()
            .remove("provider_cleanup_acknowledged");
        assert!(request(&serde_json::from_value(value).unwrap(), 0).is_err());
        let mut value = serde_json::to_value(&e).unwrap();
        value["log_archive"] = json!({"finished":false,"intent_registered":true,"finalization_registered":true,"finalized_at":"2026-09-21T12:00:00Z","status":null});
        assert!(request(&serde_json::from_value(value).unwrap(), 0).is_err());
    }
    #[test]
    fn inspection_reports_missing_failed_and_stale_protection_without_writing() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        fs::remove_file(s.0.join("release-protection.json")).unwrap();
        let check = || {
            inspect(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap()
        };
        assert_eq!(check()["runs"][0]["state"], "blocked");
        assert!(!s.0.join("cleanup").exists());
        assert!(record_release_protection(&s.0, 2, false, &[], &[]).is_err());
        assert_eq!(check()["runs"][0]["state"], "blocked");
        record_release_protection(&s.0, 3, true, &[], &[]).unwrap();
        let unsafe_ref = ReleaseReference {
            release_path: "relative/path".into(),
            files: vec![],
        };
        assert!(record_release_protection(&s.0, 4, true, &[], &[unsafe_ref]).is_err());
        assert!(!protection_ready(&s.0));
        let durable: ReleaseProtection =
            local_process::read_record(&s.0.join("release-protection.json")).unwrap();
        assert!(!durable.run_payload_namespace_reserved);
        record_release_protection(&s.0, 5, true, &[], &[]).unwrap();
        local_process::write_record(
            &s.0,
            "compiled-installations.json",
            &json!({"version":1,"installations":[]}),
        )
        .unwrap();
        let stale = check();
        assert_eq!(stale["runs"][0]["state"], "blocked");
        assert!(
            stale["runs"][0]["diagnostic"]
                .as_str()
                .unwrap()
                .contains("stale")
        );
        assert!(!s.0.join("cleanup").exists());
    }
    #[test]
    fn release_equal_to_owned_execution_root_preserves_its_payload() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        let job = local(&s, &e);
        let workspace = s.directory(&format!(
            "local-jobs/{}/workspace",
            e.launch_intent_id.unwrap()
        ));
        fs::write(workspace.join("data"), b"installation-owned").unwrap();
        let reference = ReleaseReference {
            release_path: job.to_string_lossy().into_owned(),
            files: vec![job.join("wrapper").to_string_lossy().into_owned()],
        };
        record_release_protection(&s.0, 2, true, &[], &[reference]).unwrap();
        let report = inspect(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            10,
            0,
        )
        .unwrap();
        assert_eq!(report["runs"][0]["state"], "blocked");
        assert_eq!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        assert!(workspace.join("data").exists());
        assert!(
            state(&s.0, e.assignment.decoded().assignment_id)
                .unwrap()
                .is_none()
        );
    }
    #[cfg(unix)]
    #[test]
    fn relative_journal_preserves_retired_legacy_release_alias_but_broad_roots_are_harmless() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let name = format!("enumeray-relative-protection-{}", Uuid::new_v4());
        let s = Scratch(cwd.join(&name));
        local_process::create_private_directory(&s.0).unwrap();
        let e = entry();
        archived(&s, &e);
        let job = local(&s, &e);
        let release = s.directory(&format!(
            "local-jobs/{}/workspace/release",
            e.launch_intent_id.unwrap()
        ));
        fs::write(release.join("wrapper"), b"retained software").unwrap();
        fs::write(s.0.join("external-wrapper"), b"outside payload").unwrap();
        let broad = ReleaseReference {
            release_path: s.0.to_string_lossy().into_owned(),
            files: vec![s.0.join("external-wrapper").to_string_lossy().into_owned()],
        };
        record_release_protection(&s.0, 2, true, &[], &[broad]).unwrap();
        let relative = Path::new(&name);
        let check = || {
            inspect(
                relative,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap()
        };
        assert_eq!(check()["runs"][0]["state"], "eligible");
        std::os::unix::fs::symlink(&release, s.0.join("release-alias")).unwrap();
        let alias = ReleaseReference {
            release_path: s.0.join("release-alias").to_string_lossy().into_owned(),
            files: vec![
                s.0.join("release-alias/wrapper")
                    .to_string_lossy()
                    .into_owned(),
            ],
        };
        record_release_protection(relative, 3, true, &[], &[alias]).unwrap();
        assert_eq!(check()["runs"][0]["state"], "blocked");
        // Configuration retirement cannot make installation files payload-owned.
        record_release_protection(relative, 4, true, &[], &[]).unwrap();
        assert_eq!(check()["runs"][0]["state"], "blocked");
        assert_eq!(
            run(
                relative,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        assert_eq!(
            fs::read(release.join("wrapper")).unwrap(),
            b"retained software"
        );
        assert!(!job.join("release-pins.json").exists());
        assert!(
            state(&s.0, e.assignment.decoded().assignment_id)
                .unwrap()
                .is_none()
        );
    }
    #[cfg(unix)]
    #[test]
    fn legacy_installation_alias_inside_payload_is_preserved_after_retirement() {
        let s = Scratch::new();
        let outside = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        let workspace = s.directory(&format!(
            "local-jobs/{}/workspace",
            e.launch_intent_id.unwrap()
        ));
        fs::write(outside.0.join("wrapper"), b"external software").unwrap();
        let alias = workspace.join("installed-release");
        std::os::unix::fs::symlink(&outside.0, &alias).unwrap();
        let ancestor = s.0.join("alternate-journal-root");
        std::os::unix::fs::symlink(&s.0, &ancestor).unwrap();
        let alternate = ancestor
            .join("local-jobs")
            .join(e.launch_intent_id.unwrap().to_string())
            .join("workspace/installed-release");
        let reference = ReleaseReference {
            release_path: alternate.to_string_lossy().into_owned(),
            files: vec![alternate.join("wrapper").to_string_lossy().into_owned()],
        };
        record_release_protection(&s.0, 2, true, &[], &[reference]).unwrap();
        record_release_protection(&s.0, 3, true, &[], &[]).unwrap();
        assert_eq!(
            inspect(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        assert_eq!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        assert_eq!(
            fs::read(alias.join("wrapper")).unwrap(),
            b"external software"
        );
    }
    #[test]
    fn dry_run_is_read_only_and_interrupted_cleanup_restarts_with_history_intact() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        let directory = local(&s, &e);
        let intent = e.launch_intent_id.unwrap();
        let workspace = s.directory(&format!("local-jobs/{intent}/workspace"));
        for i in 0..600 {
            fs::write(workspace.join(format!("file-{i}")), vec![7; 32]).unwrap();
        }
        let staged = s.directory(&format!("input-staging/{intent}"));
        fs::write(staged.join("input"), b"data").unwrap();
        let archive_path =
            s.0.join("archive")
                .join(format!("{}.json", e.assignment.decoded().assignment_id));
        let history = fs::read(&archive_path).unwrap();
        let inspection = inspect(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            10,
            0,
        )
        .unwrap();
        assert_eq!(inspection["runs"][0]["state"], "eligible");
        assert!(!s.0.join("cleanup").exists());
        assert!(workspace.exists());
        let first = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            10,
            0,
        )
        .unwrap();
        assert_eq!(first["runs"][0]["state"], "pending");
        record_release_protection(&s.0, 2, true, &[], &[]).unwrap();
        assert!(first["runs"][0]["removed_entries"].as_u64().unwrap() <= ENTRY_BUDGET as u64);
        assert!(
            retained_binding_keys(&s.0)
                .unwrap()
                .contains(&("test-context".into(), "scalar-double-r1".into()))
        );
        for _ in 0..8 {
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap();
        }
        let done = state(&s.0, e.assignment.decoded().assignment_id)
            .unwrap()
            .unwrap();
        assert!(done.compute_complete && done.connector_complete);
        assert!(!workspace.exists());
        assert!(!staged.exists());
        for evidence in ["started", "completion.json", "worker.lock"] {
            assert!(directory.join(evidence).exists());
        }
        assert_eq!(fs::read(&archive_path).unwrap(), history);
        assert!(retained_binding_keys(&s.0).unwrap().is_empty());
        let mut corrupt: Value = serde_json::from_slice(
            &fs::read(
                s.0.join("cleanup")
                    .join(format!("{}.json", done.assignment_id)),
            )
            .unwrap(),
        )
        .unwrap();
        corrupt["archive_sha256"] = json!("wrong");
        replace(
            &s.0.join("cleanup"),
            &format!("{}.json", done.assignment_id),
            &corrupt,
        )
        .unwrap();
        PIN_INVENTORIES.lock().unwrap().remove(&s.0);
        // Explicit history inspection still detects corruption. Finished work
        // is absent from the operational pin inventory until explicitly queued.
        assert_eq!(
            inspect(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        enqueue(&s.0, done.assignment_id).unwrap();
        assert!(retained_binding_keys(&s.0).is_err());
    }
    #[test]
    fn transfer_and_reader_ownership_prevent_cleanup_without_losing_lock_inodes() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        let directory = local(&s, &e);
        let intent = e.launch_intent_id.unwrap();
        let workspace = s.directory(&format!("local-jobs/{intent}/workspace"));
        fs::write(workspace.join("payload"), b"data").unwrap();
        let reader = read_fence(&s.0, intent).unwrap();
        assert_eq!(
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0
            )
            .unwrap()["runs"][0]["state"],
            "blocked"
        );
        assert!(workspace.exists());
        drop(reader);
        let bulk = named_lock(&s.0, "bulk-locks", intent, false).unwrap();
        for _ in 0..2 {
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap();
        }
        assert!(workspace.exists());
        drop(bulk);
        let worker = worker_fence(&directory).unwrap();
        for _ in 0..2 {
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap();
        }
        assert!(workspace.exists());
        drop(worker);
        for _ in 0..3 {
            run(
                &s.0,
                e.assignment.decoded().journal_id,
                "test-connector",
                10,
                0,
            )
            .unwrap();
        }
        assert!(!workspace.exists());
        assert!(
            s.0.join("bulk-locks")
                .join(format!("{intent}.lock"))
                .exists()
        );
        assert!(
            s.0.join("payload-locks")
                .join(format!("{intent}.lock"))
                .exists()
        );
    }
    #[cfg(unix)]
    #[test]
    fn nofollow_cleanup_unlinks_owned_links_and_refuses_symlink_ancestors() {
        let s = Scratch::new();
        let outside = Scratch::new();
        let workspace = s.directory("job/workspace");
        fs::write(outside.0.join("keep"), b"protected").unwrap();
        std::os::unix::fs::symlink(&outside.0, workspace.join("escape")).unwrap();
        let mut budget = Budget::new();
        assert!(payloads(&s.0.join("job"), &mut budget).unwrap());
        assert_eq!(fs::read(outside.0.join("keep")).unwrap(), b"protected");
        std::os::unix::fs::symlink(&outside.0, s.0.join("alias")).unwrap();
        assert!(payloads(&s.0.join("alias"), &mut Budget::new()).is_err());
    }
    #[test]
    fn scoped_and_attributable_legacy_transfers_retire_without_touching_foreign_records() {
        let s = Scratch::new();
        let mut e = entry();
        let mut value = serde_json::to_value(&e).unwrap();
        value["execution"]["files"] = json!({"result":{"path":"result.dat","media_type":"application/octet-stream","scientific_metadata":{},"size_bytes":4,"sha256":"a".repeat(64)}});
        e = serde_json::from_value(value).unwrap();
        let intent = e.launch_intent_id.unwrap();
        let parent = s.directory("artifact-transfers");
        let scoped = s.directory(&format!("artifact-transfers/{intent}"));
        fs::write(scoped.join("scoped.json"), b"scoped").unwrap();
        let original = crate::artifacts::transfer_key(intent, "result", false, 0);
        local_process::write_record(
            &parent,
            &format!("reservation-{original}.json"),
            &json!({"generation":4}),
        )
        .unwrap();
        for generation in 0..=4 {
            local_process::write_record(
                &parent,
                &format!(
                    "{}.json",
                    crate::artifacts::transfer_key(intent, "result", false, generation)
                ),
                &json!({"version_id":"kept"}),
            )
            .unwrap();
        }
        fs::write(parent.join("unowned.json"), b"keep").unwrap();
        assert!(
            legacy_transfers(&s.0, intent, &e, &mut BTreeMap::new(), &mut Budget::new()).unwrap()
        );
        assert!(parent.join("unowned.json").exists());
        archived(&s, &e);
        fs::write(s.0.join("archive/unknown-sibling"), b"keep").unwrap();
        let report = inspect(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            10,
            0,
        )
        .unwrap();
        assert_eq!(report["unscoped_transfer_entries"], json!(["unowned.json"]));
        assert_eq!(
            report["unattributed_archive_entries"],
            json!(["unknown-sibling"])
        );
        assert!(scoped.join("scoped.json").exists());
        assert!(!parent.join(format!("reservation-{original}.json")).exists());
        for generation in 0..=4 {
            assert!(
                !parent
                    .join(format!(
                        "{}.json",
                        crate::artifacts::transfer_key(intent, "result", false, generation)
                    ))
                    .exists()
            );
        }
    }
    #[test]
    fn relative_roots_resolve_without_following_parent_components() {
        let relative = open_directory(Path::new(".")).unwrap();
        let absolute = open_directory(&std::env::current_dir().unwrap()).unwrap();
        assert_eq!(
            rfs::fstat(relative).unwrap().st_ino,
            rfs::fstat(absolute).unwrap().st_ino
        );
        assert!(open_directory(Path::new("..")).is_err());
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn restarted_directory_replay_progresses_across_bounded_passes() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local(&s, &e);
        for i in 0..600 {
            fs::write(s.0.join("archive").join(format!("other-{i}")), b"unknown").unwrap();
        }
        let target = s.directory("cleanup");
        replace(
            &target,
            "discovery.json",
            &Scan {
                offset: 500,
                ..Scan::default()
            },
        )
        .unwrap();
        take_walk(&s.0.join("archive"));
        let first = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            128,
            0,
        )
        .unwrap();
        assert_eq!(first["discovery"]["replayed"], ENTRY_BUDGET);
        assert_eq!(first["discovery"]["scanned"], 0);
        let second = run(
            &s.0,
            e.assignment.decoded().journal_id,
            "test-connector",
            128,
            0,
        )
        .unwrap();
        assert!(second["discovery"]["scanned"].as_u64().unwrap() > 0);
        assert_eq!(second["more"], false);
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn retained_replay_walk_survives_pending_scan_at_full_cache_capacity() {
        let s = Scratch::new();
        let root = s.directory("archive");
        let mut walks = VecDeque::new();
        for index in 0..16 {
            let stale = s.directory(&format!("zz-stale-{index}"));
            let directory = open_directory(&stale).unwrap();
            cache_walk(
                &mut walks,
                &stale,
                Dir::read_from(&directory).unwrap(),
                0,
                0,
            );
        }
        let directory = open_directory(&root).unwrap();
        cache_walk(
            &mut walks,
            &root,
            Dir::read_from(&directory).unwrap(),
            ENTRY_BUDGET as i64,
            500,
        );
        let pending = s.directory("cleanup/pending");
        let directory = open_directory(&pending).unwrap();
        cache_walk(
            &mut walks,
            &pending,
            Dir::read_from(&directory).unwrap(),
            0,
            0,
        );
        assert_eq!(walks.len(), 16);
        let replay = walks
            .iter()
            .find_map(|(path, walk)| (path == &root).then_some(walk))
            .expect("the sibling pending scan must not evict the latest archive replay");
        assert_eq!(replay.position, ENTRY_BUDGET as i64);
        assert_eq!(replay.target, 500);
        assert!(
            !walks
                .iter()
                .any(|(path, _)| path == &s.0.join("zz-stale-0"))
        );
        assert!(
            !walks
                .iter()
                .any(|(path, _)| path == &s.0.join("zz-stale-1"))
        );
    }
    #[test]
    fn retained_configuration_inventory_is_bounded_and_caches_completed_scans() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        for i in 0..400 {
            fs::write(s.0.join("archive").join(format!("other-{i}")), b"unknown").unwrap();
        }
        assert!(retained_binding_keys(&s.0).is_err());
        let keys = retained_binding_keys(&s.0).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(retained_binding_keys(&s.0).unwrap(), keys);
    }
    #[test]
    fn malformed_archive_cannot_be_skipped_by_the_next_inventory_pass() {
        let s = Scratch::new();
        let e = entry();
        archived(&s, &e);
        local_process::write_record(
            &s.0.join("archive"),
            &format!("{}.json", Uuid::new_v4()),
            &json!({"version":1}),
        )
        .unwrap();
        for _ in 0..3 {
            assert!(retained_binding_keys(&s.0).is_err());
        }
    }
    #[test]
    fn archive_publication_proof_requires_ready_files_and_matching_identities() {
        let e = entry();
        let mut value = serde_json::to_value(&e).unwrap();
        value["log_archive"] = json!({"finished":true,"intent_registered":true,"finalization_registered":true,
            "finalized_at":"2026-09-21T12:00:00Z","status":{"state":"ready","artifact_id":Uuid::new_v4(),
                "manifest":{"version":1,"assignment_id":e.assignment.decoded().assignment_id,"attempt_id":e.assignment.decoded().attempt_id,
                    "launch_intent_id":e.launch_intent_id,"sources":{},"complete":false},"expires_at":null,"diagnostic":null}});
        assert!(request(&serde_json::from_value(value.clone()).unwrap(), 0).is_ok());
        for key in ["assignment_id", "attempt_id", "launch_intent_id"] {
            let mut changed = value.clone();
            changed["log_archive"]["status"]["manifest"][key] = json!(Uuid::new_v4());
            assert!(request(&serde_json::from_value(changed).unwrap(), 0).is_err());
        }
        for key in ["manifest", "artifact_id"] {
            let mut changed = value.clone();
            changed["log_archive"]["status"][key] = Value::Null;
            assert!(request(&serde_json::from_value(changed).unwrap(), 0).is_err());
        }
    }
}
