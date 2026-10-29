//! Bounded immutable gzip/tar diagnostics, independent of scientific outputs.
use crate::{Result, artifacts::OutputFile, capture, failure, local_process};
use enumeray_contracts::logs::{
    ArchiveRange, ArchiveSource, LogArchiveManifest, LogPolicy, LogSource,
};
use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use uuid::Uuid;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Progress {
    pub finalized_at: Option<String>,
    pub intent_registered: bool,
    pub finalization_registered: bool,
    pub finished: bool,
    pub status: Option<enumeray_contracts::logs::LogArchiveStatus>,
}

impl Progress {
    pub(crate) fn finalize(&mut self, at: String) -> Result<()> {
        time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339)
            .map_err(|_| failure("invalid archive finalization time"))?;
        if let Some(prior) = &self.finalized_at {
            if prior != &at {
                return Err(failure(
                    "archive finalization conflicts with retained evidence",
                ));
            }
        } else {
            if self.finished || self.finalization_registered {
                return Err(failure(
                    "settled archive lacks its original finalization evidence",
                ));
            }
            self.finalized_at = Some(at);
        }
        Ok(())
    }

    fn record_registration(
        &mut self,
        status: enumeray_contracts::logs::LogArchiveStatus,
    ) -> Result<()> {
        if self.finished
            && serde_json::to_value(&self.status)
                .map_err(|_| failure("invalid retained archive status"))?
                != serde_json::to_value(&status).map_err(|_| failure("invalid archive status"))?
        {
            return Err(failure(
                "archive registration conflicts with settled delivery",
            ));
        }
        self.intent_registered = true;
        self.finalization_registered = self.finalized_at.is_some();
        self.status = Some(status);
        Ok(())
    }
}

pub(crate) fn validate_completion(
    entry: &crate::journal::PreparedAssignment,
    status: &enumeray_contracts::logs::LogArchiveStatus,
) -> Result<()> {
    use enumeray_contracts::logs::ArchiveState;
    let intent = entry
        .launch_intent_id
        .filter(|id| !id.is_nil())
        .ok_or_else(|| failure("archive work has no owned launch intent"))?;
    let archive = entry
        .log_archive
        .as_ref()
        .ok_or_else(|| failure("archive work has no pinned intent"))?;
    if archive.finalized_at.is_none()
        || !matches!(
            status.state,
            ArchiveState::Ready | ArchiveState::Unavailable | ArchiveState::Expired
        )
        || status.manifest.as_ref().is_some_and(|manifest| {
            manifest.version != 1
                || manifest.assignment_id != entry.assignment.decoded().assignment_id
                || manifest.attempt_id != entry.assignment.decoded().attempt_id
                || manifest.launch_intent_id != intent
        })
        || status.state == ArchiveState::Ready
            && (status.artifact_id.is_none_or(|id| id.is_nil()) || status.manifest.is_none())
        || archive.finished
            && serde_json::to_value(&archive.status)
                .map_err(|_| failure("cannot encode retained archive status"))?
                != serde_json::to_value(status)
                    .map_err(|_| failure("cannot encode archive status"))?
    {
        return Err(failure(
            "archive worker status conflicts with retained publication evidence",
        ));
    }
    Ok(())
}

pub(crate) fn complete(
    entry: &mut crate::journal::PreparedAssignment,
    status: enumeray_contracts::logs::LogArchiveStatus,
) -> Result<()> {
    validate_completion(entry, &status)?;
    let archive = entry.log_archive.as_mut().expect("checked archive");
    if !archive.finished {
        archive.status = Some(status);
        archive.finished = true;
    }
    Ok(())
}

pub(crate) fn register(
    client: &crate::client::Client,
    journal: &mut crate::journal::Journal,
    index: usize,
    session: Uuid,
) -> Result<()> {
    let entry = &journal.data.prepared_assignments[index];
    let Some(progress) = &entry.log_archive else {
        return Ok(());
    };
    if progress.intent_registered
        && (progress.finalized_at.is_none() || progress.finalization_registered)
    {
        return Ok(());
    }
    let directory =
        local_process::job_directory(&journal.directory, entry.launch_intent_id.expect("intent"));
    let intent = enumeray_contracts::logs::LogArchiveIntent {
        attempt_id: entry.assignment.decoded().attempt_id,
        policy: capture::policy(&directory)?,
        finalized_at: progress.finalized_at.clone(),
        diagnostic: None,
    };
    let status = send_intent(
        client,
        entry.assignment.decoded().assignment_id,
        session,
        &intent,
    )?;
    let progress = journal.data.prepared_assignments[index]
        .log_archive
        .as_mut()
        .expect("archive");
    progress.record_registration(status)?;
    journal.save()
}
fn send_intent(
    client: &crate::client::Client,
    assignment: Uuid,
    session: Uuid,
    intent: &enumeray_contracts::logs::LogArchiveIntent,
) -> Result<enumeray_contracts::logs::LogArchiveStatus> {
    let value =
        serde_json::to_value(intent).map_err(|_| failure("cannot encode archive intent"))?;
    let (status, bytes) = client.request(
        reqwest::Method::PUT,
        &format!("/connector/v1/assignments/{assignment}/log-archive"),
        Some(session),
        None,
        Some(&value),
    )?;
    if status != reqwest::StatusCode::OK {
        return Err(crate::client::http_error("archive intent", status));
    }
    serde_json::from_slice(&bytes).map_err(|_| failure("invalid archive status"))
}

pub(crate) fn publish(
    client: &crate::client::Client,
    journal: &Path,
    intent: Uuid,
    assignment: &Value,
    session: Uuid,
    finalized_at: &str,
) -> Result<Value> {
    use enumeray_contracts::logs::ArchiveState;
    let directory = local_process::job_directory(journal, intent);
    let policy = capture::policy(&directory)?;
    let mut request = enumeray_contracts::logs::LogArchiveIntent {
        attempt_id: serde_json::from_value(assignment["attempt_id"].clone())
            .map_err(|_| failure("invalid archive attempt"))?,
        policy: policy.clone(),
        finalized_at: Some(finalized_at.into()),
        diagnostic: None,
    };
    let id = serde_json::from_value(assignment["assignment_id"].clone())
        .map_err(|_| failure("invalid archive assignment"))?;
    let mut status = send_intent(client, id, session, &request)?;
    if !matches!(
        status.state,
        ArchiveState::Ready | ArchiveState::Expired | ArchiveState::Unavailable
    ) {
        let finalized = time::OffsetDateTime::parse(
            finalized_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| failure("invalid archive finalization time"))?;
        if time::OffsetDateTime::now_utc().unix_timestamp()
            >= finalized
                .unix_timestamp()
                .saturating_add(policy.archive_publication_seconds as i64)
        {
            request.diagnostic =
                Some("Archive publication exceeded its independent retention deadline.".into());
            status = send_intent(client, id, session, &request)?;
        } else {
            let (root, built) = if crate::compute::has_capture_job(journal, intent) {
                crate::compute::collect_archive(journal, intent)?
            } else {
                (
                    directory.clone(),
                    build(&directory, intent, assignment, &policy)?,
                )
            };
            let files = BTreeMap::from([("log-archive.tar.gz".into(), built.file)]);
            crate::artifacts::publish_from(
                client,
                journal,
                intent,
                assignment,
                session,
                &files,
                &json!({"outputs":{},"files":{}}),
                &root,
                true,
            )?;
            status = send_intent(client, id, session, &request)?;
            if status.state != ArchiveState::Ready {
                return Err(failure("archive publication receipt is unavailable")
                    .with_kind(crate::ErrorKind::Retry));
            }
        }
    }
    // Server publication/abandonment evidence precedes deleting any owned copy.
    // Lost child replies recover the status first, so no deleted archive is rebuilt.
    if crate::compute::has_capture_job(journal, intent) {
        crate::compute::cleanup_archive(journal, intent)?;
    }
    cleanup(&directory)?;
    serde_json::to_value(status).map_err(|_| failure("cannot encode archive publication status"))
}

pub(crate) fn cleanup(directory: &Path) -> Result<()> {
    let captures = directory.join("log-captures");
    if captures.exists() {
        for (index, entry) in fs::read_dir(&captures)
            .map_err(|_| failure("cannot inspect archive prefixes"))?
            .enumerate()
        {
            if index >= enumeray_contracts::logs::MAX_MODEL_LOG_FILES + 3 {
                return Err(failure("too many archive sources"));
            }
            let entry = entry.map_err(|_| failure("cannot inspect archive prefix"))?;
            let source: LogSource = serde_json::from_value(serde_json::Value::String(
                entry.file_name().to_string_lossy().into_owned(),
            ))
            .map_err(|_| failure("invalid archive source"))?;
            let path = capture::source_directory(directory, source).join("prefix.raw");
            if path.exists() {
                crate::journal::check_private_path(&path, false)?;
                fs::remove_file(path).map_err(|_| failure("cannot retire archive prefix"))?;
            }
        }
    }
    for name in ["logs.tar.gz", "logs.tar.gz.partial", "logs.tar.partial"] {
        let path = directory.join("log-archives").join(name);
        if path.exists() {
            crate::journal::check_private_path(&path, false)?;
            fs::remove_file(path).map_err(|_| failure("cannot retire archive copy"))?;
        }
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Built {
    pub file: OutputFile,
    pub manifest: LogArchiveManifest,
}
struct Member {
    range: ArchiveRange,
    path: std::path::PathBuf,
    file_offset: u64,
    parts: Vec<(std::path::PathBuf, u64, u64)>,
}

pub(crate) fn build(
    directory: &Path,
    intent: Uuid,
    assignment: &Value,
    policy: &LogPolicy,
) -> Result<Built> {
    policy.validate().map_err(failure)?;
    if !policy.archive_enabled {
        return Err(failure("log archives are disabled"));
    }
    let target = directory.join("log-archives");
    local_process::create_private_directory(&target)?;
    let lock_path = target.join("owner.lock");
    if !lock_path.exists() {
        let _ = capture::private_file(&lock_path);
    }
    crate::journal::check_private_path(&lock_path, false)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|_| failure("cannot open archive ownership"))?;
    lock.try_lock().map_err(|_| {
        failure("prior archive worker owns finalization").with_kind(crate::ErrorKind::Retry)
    })?;
    if target.join("archive.json").exists() {
        let built: Built = local_process::read_record(&target.join("archive.json"))?;
        crate::artifacts::verify_file(
            &directory.join(&built.file.path),
            built.file.size_bytes,
            &built.file.sha256,
        )?;
        return Ok(built);
    }
    // A rename can commit before the descriptor. Rebuild from retained raw
    // fragments without holding both a final gzip and a replacement gzip.
    let orphan = target.join("logs.tar.gz");
    if orphan.exists() {
        crate::journal::check_private_path(&orphan, false)?;
        fs::remove_file(orphan).map_err(|_| failure("cannot retire interrupted archive"))?;
    }
    let mut manifest = LogArchiveManifest {
        version: 1,
        assignment_id: serde_json::from_value(assignment["assignment_id"].clone())
            .map_err(|_| failure("invalid archive assignment"))?,
        attempt_id: serde_json::from_value(assignment["attempt_id"].clone())
            .map_err(|_| failure("invalid archive attempt"))?,
        launch_intent_id: intent,
        sources: BTreeMap::new(),
        complete: true,
    };
    let sources = capture::sources(assignment)?;
    // A Slurm final read and archive worker can finalize named files at the
    // same time. Retry while a capturer owns any source; never freeze a transient
    // partial manifest or read segments while that capturer can rotate them.
    let _source_owners: Vec<File> = sources
        .iter()
        .filter(|source| matches!(source, LogSource::Model(_)))
        .map(|source| capture::source_lock(directory, *source))
        .collect::<Result<_>>()?;

    let effective = capture::allowance(policy, assignment)?;
    let policy = &effective;
    let mut members = Vec::new();
    for source in sources.into_iter().filter(|s| *s != LogSource::SlurmBatch) {
        let root = capture::source_directory(directory, source);
        let Some(m) = capture::manifest(&root)? else {
            manifest.sources.insert(
                source,
                ArchiveSource {
                    ranges: vec![],
                    total_bytes: None,
                    omitted_bytes: None,
                    unknown_loss: true,
                    complete: false,
                },
            );
            continue;
        };
        // Callers fence allocation release. An unclosed manifest then means the
        // capture owner was lost; preserve committed fragments as partial evidence.
        let unknown_loss = m.unavailable || !m.closed;
        let prefix = m
            .prefix_bytes
            .min(policy.archive_source_bytes - policy.live_bytes());
        let mut ranges = Vec::new();
        if prefix > 0 {
            let range = ArchiveRange {
                epoch: m.epoch,
                offset: 0,
                byte_length: prefix,
                member: format!("{}/prefix.log", source.as_str()),
            };
            members.push(Member {
                range: range.clone(),
                path: root.join("prefix.raw"),
                file_offset: 0,
                parts: vec![],
            });
            ranges.push(range);
        }
        let mut parts = Vec::new();
        let mut tail_start = None;
        let mut tail_bytes = 0;
        for segment in &m.segments {
            let start = segment.offset.max(prefix);
            let length = (segment.offset + segment.bytes).saturating_sub(start);
            if length == 0 {
                continue;
            }
            tail_start.get_or_insert(start);
            tail_bytes += length;
            parts.push((
                root.join(format!("{}-{}.segment", m.epoch, segment.offset)),
                start - segment.offset,
                length,
            ));
        }
        if let Some(offset) = tail_start {
            let range = ArchiveRange {
                epoch: m.epoch,
                offset,
                byte_length: tail_bytes,
                member: format!("{}/tail.log", source.as_str()),
            };
            members.push(Member {
                range: range.clone(),
                path: root.clone(),
                file_offset: 0,
                parts,
            });
            ranges.push(range);
        }
        let retained: u64 = ranges.iter().map(|r| r.byte_length).sum();
        if retained > policy.archive_source_bytes {
            return Err(failure("archive source exceeds its budget"));
        }
        manifest.sources.insert(
            source,
            ArchiveSource {
                ranges,
                total_bytes: (!unknown_loss).then_some(m.end),
                omitted_bytes: (!unknown_loss).then(|| m.end.saturating_sub(retained)),
                unknown_loss: unknown_loss || m.epoch != 1,
                complete: !unknown_loss && m.epoch == 1 && retained == m.end,
            },
        );
    }
    if assignment["requirements"]["driver"] == "slurm" {
        let path = directory.join("slurm.log");
        let source = LogSource::SlurmBatch;
        if path.exists() {
            let file =
                File::open(&path).map_err(|_| failure("cannot open final batch diagnostics"))?;
            let size = file
                .metadata()
                .map_err(|_| failure("cannot inspect final batch diagnostics"))?
                .len();
            let prefix = size.min(policy.archive_source_bytes - policy.live_bytes());
            let tail = size.saturating_sub(policy.live_bytes()).max(prefix);
            let epoch = capture::manifest(&capture::source_directory(directory, source))?
                .map_or(1, |m| m.epoch);
            let mut ranges = Vec::new();
            for (offset, length, name) in [(0, prefix, "prefix"), (tail, size - tail, "tail")] {
                if length == 0 {
                    continue;
                }
                let range = ArchiveRange {
                    epoch,
                    offset,
                    byte_length: length,
                    member: format!("slurm_batch/{name}.log"),
                };
                members.push(Member {
                    range: range.clone(),
                    path: path.clone(),
                    file_offset: offset,
                    parts: vec![],
                });
                ranges.push(range);
            }
            let retained: u64 = ranges.iter().map(|r| r.byte_length).sum();
            manifest.sources.insert(
                source,
                ArchiveSource {
                    ranges,
                    total_bytes: Some(size),
                    omitted_bytes: Some(size - retained),
                    unknown_loss: epoch != 1,
                    complete: retained == size && epoch == 1,
                },
            );
        } else {
            manifest.sources.insert(
                source,
                ArchiveSource {
                    ranges: vec![],
                    total_bytes: None,
                    omitted_bytes: None,
                    unknown_loss: true,
                    complete: false,
                },
            );
        }
    }
    manifest.complete = manifest.sources.values().all(|s| s.complete);
    let metadata =
        serde_json::to_vec(&manifest).map_err(|_| failure("cannot encode archive manifest"))?;
    if metadata.len() > 64 * 1024 {
        return Err(failure("archive manifest exceeds bound"));
    }
    let partial = target.join("logs.tar.gz.partial");
    if partial.exists() {
        crate::journal::check_private_path(&partial, false)?;
        fs::remove_file(&partial).map_err(|_| failure("cannot replace interrupted archive"))?;
    }
    let file = capture::private_file(&partial).map_err(|_| failure("cannot create archive"))?;
    let mut gzip = GzEncoder::new(
        Limited {
            file,
            bytes: 0,
            limit: policy.archive_compressed_bytes,
        },
        Compression::fast(),
    );
    write_member(
        &mut gzip,
        "manifest.json",
        metadata.len() as u64,
        &mut metadata.as_slice(),
    )?;
    for member in members {
        let parts = if member.parts.is_empty() {
            vec![(member.path, member.file_offset, member.range.byte_length)]
        } else {
            member.parts
        };
        let mut readers = std::collections::VecDeque::new();
        for (path, offset, length) in parts {
            let confined = crate::artifacts::confined_file(
                directory,
                path.strip_prefix(directory)
                    .map_err(|_| failure("archive path escapes job"))?
                    .to_str()
                    .ok_or_else(|| failure("invalid archive path"))?,
            )?;
            let mut file =
                File::open(confined).map_err(|_| failure("archive source disappeared"))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| failure("cannot seek archive source"))?;
            readers.push_back(file.take(length));
        }
        write_member(
            &mut gzip,
            &member.range.member,
            member.range.byte_length,
            &mut Fragments(readers),
        )?;
    }
    gzip.write_all(&[0; 1024])
        .map_err(|_| failure("cannot finish archive tar"))?;
    let writer = gzip
        .finish()
        .map_err(|_| failure("cannot finish archive gzip"))?;
    writer
        .file
        .sync_all()
        .map_err(|_| failure("cannot sync archive"))?;
    let final_path = target.join("logs.tar.gz");
    fs::rename(&partial, &final_path).map_err(|_| failure("cannot commit archive"))?;
    File::open(&target)
        .and_then(|f| f.sync_all())
        .map_err(|_| failure("cannot sync archive directory"))?;
    let (size_bytes, sha256) = crate::artifacts::file_digest(&final_path)?;
    let built = Built {
        file: OutputFile {
            path: "log-archives/logs.tar.gz".into(),
            media_type: "application/gzip".into(),
            scientific_metadata: serde_json::json!({"log_archive_manifest":manifest}),
            size_bytes,
            sha256,
        },
        manifest,
    };
    local_process::write_record(&target, "archive.json", &built)?;
    Ok(built)
}

/// Only adjacent committed segments are combined; loss boundaries remain ranges.
struct Fragments(std::collections::VecDeque<std::io::Take<File>>);
impl Read for Fragments {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while let Some(file) = self.0.front_mut() {
            let count = file.read(buffer)?;
            if count > 0 {
                return Ok(count);
            }
            if file.limit() != 0 {
                return Err(std::io::Error::other("archive segment truncated"));
            }
            self.0.pop_front();
        }
        Ok(0)
    }
}
struct Limited {
    file: File,
    bytes: u64,
    limit: u64,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("archive quota exhausted"));
        }
        let count = self.file.write(bytes)?;
        self.bytes += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
// A fixed USTAR writer: only our bounded member names, regular files and measured
// lengths enter it. No executable modes, links, ownership or source timestamps.
fn write_member(
    target: &mut impl Write,
    name: &str,
    size: u64,
    source: &mut impl Read,
) -> Result<()> {
    if name.len() > 100 || size > enumeray_contracts::logs::MAX_ARCHIVE_BYTES {
        return Err(failure("invalid archive member"));
    }
    let mut header = [0_u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    for (start, width, value) in [
        (100, 8, 0o600),
        (108, 8, 0),
        (116, 8, 0),
        (124, 12, size),
        (136, 12, 0),
    ] {
        let value = format!("{:0width$o}\0", value, width = width - 1);
        header[start..start + width].copy_from_slice(value.as_bytes());
    }
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..265].copy_from_slice(b"ustar\x0000");
    let sum: u64 = header.iter().map(|b| u64::from(*b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    target
        .write_all(&header)
        .map_err(|_| failure("cannot write archive header"))?;
    let copied = std::io::copy(&mut source.take(size), target)
        .map_err(|_| failure("cannot write archive content"))?;
    if copied != size {
        return Err(failure("archive source changed during finalization"));
    }
    let padding = (512 - size % 512) % 512;
    target
        .write_all(&[0; 512][..padding as usize])
        .map_err(|_| failure("cannot write archive padding"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_replay_keeps_the_original_archive_time_and_rejects_conflicts() {
        let mut progress = Progress::default();
        progress.finalize("2026-09-21T12:00:00Z".into()).unwrap();
        let before = serde_json::to_value(&progress).unwrap();
        progress.finalize("2026-09-21T12:00:00Z".into()).unwrap();
        for time in ["2026-09-21T12:01:00Z", "invalid"] {
            assert!(progress.finalize(time.into()).is_err());
            assert_eq!(serde_json::to_value(&progress).unwrap(), before);
        }
    }
    fn unpack(path: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut tar = Vec::new();
        flate2::read::GzDecoder::new(File::open(path).unwrap())
            .read_to_end(&mut tar)
            .unwrap();
        let mut members = BTreeMap::new();
        let mut offset = 0;
        while tar[offset] != 0 {
            let header = &tar[offset..offset + 512];
            assert_eq!(&header[257..263], b"ustar\0");
            let name = String::from_utf8(
                header[..100]
                    .iter()
                    .copied()
                    .take_while(|b| *b != 0)
                    .collect(),
            )
            .unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&header[124..135]).unwrap(), 8).unwrap();
            let sum =
                usize::from_str_radix(std::str::from_utf8(&header[148..154]).unwrap(), 8).unwrap();
            assert_eq!(
                sum,
                header
                    .iter()
                    .enumerate()
                    .map(|(n, b)| if (148..156).contains(&n) {
                        32
                    } else {
                        usize::from(*b)
                    })
                    .sum::<usize>()
            );
            offset += 512;
            assert!(
                members
                    .insert(name, tar[offset..offset + size].to_vec())
                    .is_none()
            );
            offset += size.div_ceil(512) * 512;
        }
        assert!(tar[offset..].iter().all(|b| *b == 0));
        members
    }
    #[test]
    fn archive_waits_for_the_named_capture_owner_before_freezing_evidence() {
        let root = std::env::temp_dir().join(format!("enumeray-archive-owner-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        local_process::create_private_directory(&root.join("workspace")).unwrap();
        fs::write(root.join("workspace/solver.log"), b"final solver bytes").unwrap();
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/scalar-double.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut assignment = json!({"assignment_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),"definition":model["definition"],"requirements":{"driver":"process"}});
        assignment["definition"]["log_files"] = json!({"solver":{"path":"solver.log"}});
        let policy = LogPolicy {
            archive_enabled: true,
            ..Default::default()
        };
        let effective = capture::allowance(&policy, &assignment).unwrap();
        for source in [LogSource::Stdout, LogSource::Stderr] {
            capture::Writer::new(&root, source, effective.clone())
                .unwrap()
                .finish(false)
                .unwrap();
        }
        capture::collect_named(&root, &assignment, &policy, false).unwrap();
        let source = LogSource::model("solver").unwrap();
        let owner = capture::source_lock(&root, source).unwrap();
        let intent = Uuid::new_v4();
        let error = build(&root, intent, &assignment, &policy)
            .err()
            .expect("capture owns the snapshot");
        assert_eq!(error.kind, crate::ErrorKind::Retry);
        assert!(!root.join("log-archives/archive.json").exists());
        drop(owner);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let built = loop {
            capture::collect_named(&root, &assignment, &policy, true).unwrap();
            if !capture::manifest(&capture::source_directory(&root, source))
                .unwrap()
                .unwrap()
                .closed
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "capture ownership did not settle"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            match build(&root, intent, &assignment, &policy) {
                Ok(built) => break built,
                Err(error)
                    if error.kind == crate::ErrorKind::Retry
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                Err(error) => panic!("archive did not recover ownership: {error}"),
            }
        };
        assert!(built.manifest.complete);
        let members = unpack(&root.join(&built.file.path));
        let bytes: Vec<u8> = built.manifest.sources[&source]
            .ranges
            .iter()
            .flat_map(|range| members[&range.member].clone())
            .collect();
        assert_eq!(bytes, b"final solver bytes");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn maximum_named_sources_keep_archive_metadata_and_aggregate_bytes_bounded() {
        let root = std::env::temp_dir().join(format!("enumeray-named-archive-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        local_process::create_private_directory(&root.join("workspace")).unwrap();
        let model: Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/scalar-double.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut assignment = json!({"assignment_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),"definition":model["definition"],"requirements":{"driver":"process"}});
        for n in 0..8 {
            assignment["definition"]["log_files"][format!("{}{}", "a".repeat(63), n)] =
                json!({"path":format!("solver-{n}.log")});
            fs::write(
                root.join("workspace").join(format!("solver-{n}.log")),
                vec![n as u8; 150_000],
            )
            .unwrap();
        }
        let policy = LogPolicy {
            segment_bytes: 4096,
            retained_segments: 128,
            archive_enabled: true,
            archive_source_bytes: 1024 * 1024,
            ..Default::default()
        };
        let effective = capture::allowance(&policy, &assignment).unwrap();
        for source in [LogSource::Stdout, LogSource::Stderr] {
            let mut writer = capture::Writer::new(&root, source, effective.clone()).unwrap();
            writer.append(&vec![42; 150_000]).unwrap();
            writer.finish(false).unwrap();
        }
        capture::collect_named(&root, &assignment, &policy, true).unwrap();
        let built = build(&root, Uuid::new_v4(), &assignment, &policy).unwrap();
        assert_eq!(built.manifest.sources.len(), 10);
        assert!(built.manifest.complete);
        assert!(serde_json::to_vec(&built.manifest).unwrap().len() < 16 * 1024);
        let members = unpack(&root.join(&built.file.path));
        let mut retained = 0;
        for (source, value) in &built.manifest.sources {
            assert!(value.ranges.len() <= 2);
            let raw: Vec<u8> = value
                .ranges
                .iter()
                .flat_map(|range| members[&range.member].clone())
                .collect();
            assert_eq!(raw.len(), 150_000);
            let byte = if let LogSource::Model(_) = source {
                source.as_str().as_bytes().last().unwrap() - b'0'
            } else {
                42
            };
            assert!(raw.iter().all(|b| *b == byte));
            retained += raw.len() as u64;
        }
        assert!(retained <= policy.archive_source_bytes * 2);
        cleanup(&root).unwrap();
        for source in capture::sources(&assignment).unwrap() {
            assert!(
                !capture::source_directory(&root, source)
                    .join("prefix.raw")
                    .exists()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lost_capture_owner_keeps_committed_bytes_as_partial_evidence() {
        let root = std::env::temp_dir().join(format!("enumeray-log-archive-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        let policy = LogPolicy {
            archive_enabled: true,
            ..Default::default()
        };
        let mut writer = capture::Writer::new(&root, LogSource::Stdout, policy.clone()).unwrap();
        writer.append(b"committed diagnostic\0\xff").unwrap();
        drop(writer);
        let assignment = json!({"definition": serde_json::to_value(enumeray_contracts::models::parse_publication(include_bytes!("../../../examples/scalar-double.json")).unwrap()).unwrap()["definition"], "assignment_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4()});
        let built = build(&root, Uuid::new_v4(), &assignment, &policy).unwrap();
        assert!(!built.manifest.complete);
        let stdout = &built.manifest.sources[&LogSource::Stdout];
        assert!(stdout.unknown_loss);
        assert_eq!(stdout.total_bytes, None);
        let members = unpack(&root.join(built.file.path));
        assert_eq!(
            members[&stdout.ranges[0].member],
            b"committed diagnostic\0\xff"
        );
        assert!(built.manifest.sources[&LogSource::Stderr].unknown_loss);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn complete_and_partial_archives_preserve_raw_prefix_tail_and_restart() {
        for produced in [4000, 100_019] {
            let root =
                std::env::temp_dir().join(format!("enumeray-log-archive-{}", Uuid::new_v4()));
            local_process::create_private_directory(&root).unwrap();
            let policy = LogPolicy {
                segment_bytes: 4096,
                retained_segments: 2,
                archive_enabled: true,
                archive_source_bytes: 32768,
                archive_compressed_bytes: 2 * 1024 * 1024 + 3 * 32768,
                ..Default::default()
            };
            policy.validate().unwrap();
            let mut seed = 42_u64;
            let raw: Vec<u8> = (0..produced)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (seed >> 32) as u8
                })
                .collect();
            for source in [LogSource::Stdout, LogSource::Stderr] {
                let mut writer = capture::Writer::new(&root, source, policy.clone()).unwrap();
                for bytes in raw.chunks(8192) {
                    writer.append(bytes).unwrap();
                }
                writer.finish(false).unwrap();
            }
            fs::write(root.join("slurm.log"), &raw).unwrap();
            capture::collect_batch(&root, &policy, true).unwrap();
            let intent = Uuid::new_v4();
            let assignment = json!({"definition": serde_json::to_value(enumeray_contracts::models::parse_publication(include_bytes!("../../../examples/scalar-double.json")).unwrap()).unwrap()["definition"], "assignment_id":Uuid::new_v4(),"attempt_id":Uuid::new_v4(),"requirements":{"driver":"slurm"}});
            let built = build(&root, intent, &assignment, &policy).unwrap();
            assert!(built.file.size_bytes <= policy.archive_compressed_bytes);
            assert_eq!(
                built.manifest.complete,
                produced <= policy.archive_source_bytes as usize
            );
            let members = unpack(&root.join(&built.file.path));
            assert_eq!(
                serde_json::from_slice::<LogArchiveManifest>(&members["manifest.json"]).unwrap(),
                built.manifest
            );
            for source in built.manifest.sources.values() {
                let retained: u64 = source.ranges.iter().map(|r| r.byte_length).sum();
                assert!(retained <= policy.archive_source_bytes);
                assert_eq!(source.omitted_bytes, Some(raw.len() as u64 - retained));
                assert!(!source.unknown_loss);
                for range in &source.ranges {
                    assert_eq!(
                        members[&range.member],
                        raw[range.offset as usize..(range.offset + range.byte_length) as usize]
                    );
                }
                assert_eq!(source.ranges.first().unwrap().offset, 0);
                assert_eq!(
                    source.ranges.last().unwrap().offset
                        + source.ranges.last().unwrap().byte_length,
                    produced as u64
                );
            }
            let replay = build(&root, intent, &assignment, &policy).unwrap();
            assert_eq!(replay.file.sha256, built.file.sha256);
            cleanup(&root).unwrap();
            cleanup(&root).unwrap();
            assert!(!root.join(&built.file.path).exists());
            assert!(
                capture::manifest(&capture::source_directory(&root, LogSource::Stdout))
                    .unwrap()
                    .is_some()
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}
