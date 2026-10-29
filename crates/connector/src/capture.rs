//! Private segmented captures. Readers consume only manifest-committed bytes.
use crate::{Result, failure, journal, local_process};
use base64::{Engine, engine::general_purpose::STANDARD};
use enumeray_contracts::logs::{
    LogPolicy, LogSource, READ_WINDOW_BYTES, ReadLogsRequest, ReadLogsResponse,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub source: LogSource,
    pub epoch: i64,
    pub earliest: u64,
    pub end: u64,
    pub closed: bool,
    pub unavailable: bool,
    pub segments: Vec<Segment>,
    pub prefix_bytes: u64,
    pub file_identity: Option<String>,
    /// Last observed raw size can exceed the committed, bounded read position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Segment {
    pub offset: u64,
    pub bytes: u64,
}

pub(crate) fn sources(assignment: &serde_json::Value) -> Result<Vec<LogSource>> {
    let definition: enumeray_contracts::models::ScientificDefinition =
        serde_json::from_value(assignment["definition"].clone())
            .map_err(|_| failure("invalid log definition"))?;
    enumeray_contracts::logs::validate_log_files(&definition.log_files).map_err(failure)?;
    Ok(enumeray_contracts::logs::expected_sources(
        &definition,
        assignment["requirements"]["driver"] == "slurm",
    ))
}
pub(crate) fn allowance(policy: &LogPolicy, assignment: &serde_json::Value) -> Result<LogPolicy> {
    Ok(policy.for_sources(
        sources(assignment)?.len(),
        assignment["requirements"]["driver"] == "slurm",
    ))
}

/// Descriptor-relative traversal never follows a model-controlled symlink. NONBLOCK
/// also prevents a replacement FIFO from blocking before its type can be checked.
fn open_model_file(workspace: &Path, relative: &str) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !enumeray_contracts::logs::valid_log_path(relative) {
        return Err(std::io::Error::other("invalid model log path"));
    }
    let mut directory = open(
        workspace,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = openat(
            &directory,
            part,
            if parts.peek().is_some() {
                flags | OFlags::DIRECTORY
            } else {
                flags
            },
            Mode::empty(),
        )?;
        if parts.peek().is_none() {
            let file = File::from(fd);
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::other("log is not a regular file"));
            }
            return Ok(file);
        }
        directory = fd;
    }
    Err(std::io::Error::other("empty log path"))
}

/// The supervisor collects independently of connector/API availability. Runner
/// recovery uses the same stable lock and only closes after allocation release.
pub(crate) fn collect_named(
    directory: &Path,
    assignment: &serde_json::Value,
    policy: &LogPolicy,
    complete: bool,
) -> Result<()> {
    let definition: enumeray_contracts::models::ScientificDefinition =
        serde_json::from_value(assignment["definition"].clone())
            .map_err(|_| failure("invalid log definition"))?;
    enumeray_contracts::logs::validate_log_files(&definition.log_files).map_err(failure)?;
    let policy = policy.for_sources(
        enumeray_contracts::logs::expected_sources(
            &definition,
            assignment["requirements"]["driver"] == "slurm",
        )
        .len(),
        assignment["requirements"]["driver"] == "slurm",
    );
    let mut first_error = None;
    for (name, declaration) in &definition.log_files {
        let result = (|| {
            let source = LogSource::model(name).expect("validated source");
            let root = source_directory(directory, source);
            let _owner = match source_lock(directory, source) {
                Ok(owner) => owner,
                Err(error) if error.kind == crate::ErrorKind::Retry => return Ok(()),
                Err(error) => return Err(error),
            };
            let previous = manifest(&root)?;
            if previous.as_ref().is_some_and(|m| m.closed) {
                return Ok(());
            }
            let result = open_model_file(&directory.join("workspace"), &declaration.path)
                .map_err(|_| failure("model log unavailable"))
                .and_then(|file| collect_file(directory, source, file, &policy, complete, true));
            if result.is_err() && complete {
                // Retain committed bytes, then emit a terminal unavailable marker.
                let mut m = manifest(&root)?.unwrap_or(Manifest {
                    source,
                    epoch: 1,
                    earliest: 0,
                    end: 0,
                    closed: false,
                    unavailable: false,
                    segments: vec![],
                    prefix_bytes: 0,
                    file_identity: None,
                    file_size: None,
                });
                m.closed = true;
                m.unavailable = true;
                replace(&root, "manifest.json", &m)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn source_directory(directory: &Path, source: LogSource) -> PathBuf {
    directory.join("log-captures").join(source.as_str())
}
/// Capturers and archive readers share the same stable ownership inode.
/// Holding this descriptor prevents an archive snapshot racing final capture.
pub(crate) fn source_lock(directory: &Path, source: LogSource) -> Result<File> {
    local_process::create_private_directory(&directory.join("log-captures"))?;
    let root = source_directory(directory, source);
    local_process::create_private_directory(&root)?;
    let path = root.join("owner.lock");
    if !path.exists() {
        let _ = private_file(&path);
    }
    journal::check_private_path(&path, false)?;
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|_| failure("cannot open log ownership"))?;
    owner.try_lock().map_err(|_| {
        failure("log capture still owns its source").with_kind(crate::ErrorKind::Retry)
    })?;
    Ok(owner)
}

fn segment_path(directory: &Path, epoch: i64, offset: u64) -> PathBuf {
    directory.join(format!("{epoch}-{offset}.segment"))
}
pub(crate) fn policy(directory: &Path) -> Result<LogPolicy> {
    let path = directory.join("log-policy.json");
    let policy: LogPolicy = if path
        .try_exists()
        .map_err(|_| failure("cannot inspect log policy"))?
    {
        local_process::read_record(&path)?
    } else {
        LogPolicy::default()
    };
    policy.validate().map_err(failure)?;
    Ok(policy)
}
pub(crate) fn load_policy(path: Option<&Path>) -> Result<LogPolicy> {
    let policy = match path {
        Some(path) => {
            if !fs::symlink_metadata(path)
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() <= 65536)
            {
                return Err(failure("log policy must be a bounded regular JSON file"));
            }
            let mut bytes = Vec::new();
            File::open(path)
                .and_then(|file| file.take(65537).read_to_end(&mut bytes))
                .map_err(|_| failure("cannot read log policy"))?;
            if bytes.len() > 65536 {
                return Err(failure("log policy must be a bounded regular JSON file"));
            }
            if bytes.is_empty()
                || !serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|v| v.is_object())
            {
                return Err(failure("log policy must be a JSON object"));
            }
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid log policy"))?
        }
        None => LogPolicy::default(),
    };
    policy.validate().map_err(failure)?;
    Ok(policy)
}
pub(crate) fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
pub(crate) fn replace(directory: &Path, name: &str, value: &impl Serialize) -> Result<()> {
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(value).map_err(|_| failure("cannot encode capture manifest"))?;
    if bytes.len() > 64 * 1024 {
        return Err(failure("capture metadata exceeds bound"));
    }
    let result = (|| {
        let mut file = private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join(name))?;
        File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| failure("cannot persist capture manifest"))
}
pub(crate) fn manifest(directory: &Path) -> Result<Option<Manifest>> {
    let path = directory.join("manifest.json");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect capture manifest"))?
    {
        return Ok(None);
    }
    let value: Manifest = serde_json::from_slice(&local_process::read_private(&path, 64 * 1024)?)
        .map_err(|_| failure("invalid capture manifest"))?;
    if value.epoch < 1
        || directory.file_name().and_then(|s| s.to_str()) != Some(value.source.as_str())
        || value
            .file_size
            .is_some_and(|n| n < value.end || n > i64::MAX as u64)
        || value.earliest > value.end
        || value.segments.len() > 128
        || value.segments.iter().any(|s| {
            s.bytes == 0
                || s.offset
                    .checked_add(s.bytes)
                    .is_none_or(|end| end > value.end)
        })
        || value
            .segments
            .windows(2)
            .any(|w| w[0].offset + w[0].bytes != w[1].offset)
        || value
            .segments
            .first()
            .is_some_and(|s| s.offset != value.earliest)
        || value
            .segments
            .last()
            .is_some_and(|s| s.offset + s.bytes != value.end)
    {
        return Err(failure("capture ranges conflict with committed metadata"));
    }
    Ok(Some(value))
}

pub(crate) struct Writer {
    directory: PathBuf,
    policy: LogPolicy,
    pub manifest: Manifest,
}
impl Writer {
    pub fn new(directory: &Path, source: LogSource, policy: LogPolicy) -> Result<Self> {
        local_process::create_private_directory(&directory.join("log-captures"))?;
        let directory = source_directory(directory, source);
        local_process::create_private_directory(&directory)?;
        if manifest(&directory)?.is_some() {
            return Err(failure("capture already has an owner"));
        }
        let writer = Self {
            directory,
            policy,
            manifest: Manifest {
                source,
                epoch: 1,
                earliest: 0,
                end: 0,
                closed: false,
                unavailable: false,
                segments: Vec::new(),
                prefix_bytes: 0,
                file_identity: None,
                file_size: None,
            },
        };
        writer.commit()?;
        Ok(writer)
    }
    fn commit(&self) -> Result<()> {
        replace(&self.directory, "manifest.json", &self.manifest)
    }
    pub fn append(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.manifest.closed {
            return Err(failure("closed capture cannot grow"));
        }
        if self.policy.archive_enabled {
            let limit = self.policy.archive_source_bytes - self.policy.live_bytes();
            let count = bytes
                .len()
                .min(limit.saturating_sub(self.manifest.prefix_bytes) as usize);
            if count > 0 {
                let path = self.directory.join("prefix.raw");
                let mut file = if path.exists() {
                    journal::check_private_path(&path, false)?;
                    OpenOptions::new().append(true).open(path)
                } else {
                    private_file(&path)
                }
                .map_err(|_| failure("cannot write archive prefix"))?;
                file.write_all(&bytes[..count])
                    .and_then(|()| file.sync_all())
                    .map_err(|_| failure("cannot sync archive prefix"))?;
                self.manifest.prefix_bytes += count as u64;
            }
        }
        while !bytes.is_empty() {
            if self
                .manifest
                .segments
                .last()
                .is_none_or(|s| s.bytes == self.policy.segment_bytes)
            {
                private_file(&segment_path(
                    &self.directory,
                    self.manifest.epoch,
                    self.manifest.end,
                ))
                .map_err(|_| failure("cannot create capture segment"))?;
                self.manifest.segments.push(Segment {
                    offset: self.manifest.end,
                    bytes: 0,
                });
            }
            let segment = self.manifest.segments.last_mut().expect("segment");
            let count = bytes
                .len()
                .min((self.policy.segment_bytes - segment.bytes) as usize);
            let path = segment_path(&self.directory, self.manifest.epoch, segment.offset);
            journal::check_private_path(&path, false)?;
            let mut file = OpenOptions::new()
                .append(true)
                .open(path)
                .map_err(|_| failure("cannot append capture segment"))?;
            file.write_all(&bytes[..count])
                .and_then(|()| file.sync_all())
                .map_err(|_| failure("cannot sync capture segment"))?;
            segment.bytes += count as u64;
            self.manifest.end = self
                .manifest
                .end
                .checked_add(count as u64)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| failure("capture offset exhausted"))?;
            let discarded = if self.manifest.segments.len() > self.policy.retained_segments as usize
            {
                Some(self.manifest.segments.remove(0))
            } else {
                None
            };
            self.manifest.earliest = self.manifest.segments.first().expect("segment").offset;
            self.commit()?;
            if let Some(discarded) = discarded {
                fs::remove_file(segment_path(
                    &self.directory,
                    self.manifest.epoch,
                    discarded.offset,
                ))
                .map_err(|_| failure("cannot discard retired capture segment"))?;
                File::open(&self.directory)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| failure("cannot sync capture eviction"))?;
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
    pub fn finish(&mut self, unavailable: bool) -> Result<()> {
        self.manifest.closed = true;
        self.manifest.unavailable |= unavailable;
        self.commit()
    }
}

/// Open a stable segment descriptor before rotation can unlink it. A reader
/// losing that race retries from the newer manifest and reports the exact gap.
pub(crate) fn read(directory: &Path, request: &ReadLogsRequest) -> Result<ReadLogsResponse> {
    match read_committed(directory, request) {
        // Callers establish execution/allocation release before requesting final
        // reads. Lost optional capture state becomes explicit unknown loss,
        // rather than blocking scientific delivery settlement forever.
        Err(_)
            if request.complete
                && request.epoch >= 1
                && request.offset <= i64::MAX as u64
                && matches!(request.source, LogSource::Model(_)) =>
        {
            Ok(ReadLogsResponse {
                epoch: request.epoch,
                earliest: request.offset,
                end: request.offset,
                offset: request.offset,
                data: String::new(),
                closed: true,
                unavailable: true,
            })
        }
        result => result,
    }
}
fn read_committed(directory: &Path, request: &ReadLogsRequest) -> Result<ReadLogsResponse> {
    if request.epoch < 1
        || request.offset > i64::MAX as u64
        || request.source == LogSource::Connector
    {
        return Err(failure("invalid capture read position"));
    }
    let source = source_directory(directory, request.source);
    for _ in 0..3 {
        let Some(m) = manifest(&source)? else {
            return Ok(ReadLogsResponse {
                epoch: 1,
                earliest: 0,
                end: 0,
                offset: 0,
                data: String::new(),
                closed: request.complete,
                unavailable: request.complete,
            });
        };
        let offset = if request.epoch == m.epoch {
            request.offset.max(m.earliest)
        } else {
            m.earliest
        };
        if offset > m.end {
            return Err(failure("capture position exceeds source end"));
        }
        let mut bytes = Vec::with_capacity(READ_WINDOW_BYTES);
        let mut raced = false;
        for segment in &m.segments {
            let start = (offset + bytes.len() as u64).max(segment.offset);
            if start >= segment.offset + segment.bytes || bytes.len() == READ_WINDOW_BYTES {
                continue;
            }
            let path = segment_path(&source, m.epoch, segment.offset);
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    raced = true;
                    break;
                }
                Err(_) => return Err(failure("cannot open capture segment")),
            };
            journal::check_private_path(&path, false)
                .or_else(|error| if !path.exists() { Ok(()) } else { Err(error) })?;
            let count = (READ_WINDOW_BYTES - bytes.len())
                .min((segment.offset + segment.bytes - start) as usize);
            file.seek(SeekFrom::Start(start - segment.offset))
                .and_then(|_| file.take(count as u64).read_to_end(&mut bytes))
                .map_err(|_| failure("cannot read capture segment"))?;
        }
        if raced {
            continue;
        }
        return Ok(ReadLogsResponse {
            epoch: m.epoch,
            earliest: m.earliest,
            end: m.end,
            offset,
            data: STANDARD.encode(bytes),
            closed: m.closed || request.complete,
            unavailable: m.unavailable || request.complete && !m.closed,
        });
    }
    Err(failure("capture rotated during read").with_kind(crate::ErrorKind::Waiting))
}

/// The scheduler owns its raw file. Copy only the current bounded tail, using
/// original file offsets; never truncate or impose a scheduler file-size limit.
pub(crate) fn collect_batch(directory: &Path, policy: &LogPolicy, complete: bool) -> Result<()> {
    let path = directory.join("slurm.log");
    if !path
        .try_exists()
        .map_err(|_| failure("cannot inspect batch diagnostics"))?
    {
        return Ok(());
    }
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink()) {
        return Err(failure("batch diagnostics are not a managed regular file"));
    }
    let file = File::open(&path).map_err(|_| failure("cannot read batch diagnostics"))?;
    collect_file(
        directory,
        LogSource::SlurmBatch,
        file,
        policy,
        complete,
        false,
    )
}

fn collect_file(
    directory: &Path,
    source_name: LogSource,
    mut file: File,
    policy: &LogPolicy,
    complete: bool,
    model: bool,
) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|_| failure("cannot inspect batch diagnostics"))?;
    if !metadata.is_file() || metadata.len() > i64::MAX as u64 {
        return Err(failure("invalid batch log file"));
    }
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let identity = format!("{:?}", metadata.created().ok());
    let source = source_directory(directory, source_name);
    let previous = manifest(&source)?;
    let reset = previous.as_ref().is_some_and(|m| {
        m.file_identity.as_ref() != Some(&identity) || metadata.len() < m.file_size.unwrap_or(m.end)
    });
    if previous.as_ref().is_some_and(|m| {
        !reset
            && metadata.len() == m.end
            && (!complete || m.closed)
            && (!model
                || !policy.archive_enabled
                || m.prefix_bytes
                    >= metadata
                        .len()
                        .min(policy.archive_source_bytes - policy.live_bytes()))
    }) {
        return Ok(());
    }
    local_process::create_private_directory(&directory.join("log-captures"))?;
    local_process::create_private_directory(&source)?;
    let epoch = match &previous {
        Some(m) => m
            .epoch
            .checked_add(i64::from(reset))
            .ok_or_else(|| failure("capture epoch exhausted"))?,
        None => 1,
    };
    let end = metadata.len();
    let earliest = if end == 0 {
        0
    } else {
        ((end - 1) / policy.segment_bytes).saturating_sub(u64::from(policy.retained_segments) - 1)
            * policy.segment_bytes
    };
    let mut current = previous.clone().filter(|_| !reset).unwrap_or(Manifest {
        source: source_name,
        epoch,
        earliest: 0,
        end: 0,
        closed: false,
        unavailable: false,
        segments: Vec::new(),
        prefix_bytes: 0,
        file_identity: Some(identity.clone()),
        file_size: Some(metadata.len()),
    });
    current.file_identity = Some(identity);
    current.file_size = Some(metadata.len());
    if current.end < earliest || reset {
        current.segments.clear();
        current.earliest = earliest;
        current.end = earliest;
        replace(&source, "manifest.json", &current)?;
    }
    // A previous crash can leave one segment awaiting deletion. Reconcile only
    // files excluded by the committed manifest, before allocating another.
    for entry in fs::read_dir(&source).map_err(|_| failure("cannot inspect batch segments"))? {
        let entry = entry.map_err(|_| failure("cannot inspect batch segment"))?;
        if entry.file_name().to_string_lossy().ends_with(".segment")
            && !current
                .segments
                .iter()
                .any(|s| segment_path(&source, epoch, s.offset) == entry.path())
        {
            fs::remove_file(entry.path()).map_err(|_| failure("cannot retire batch segment"))?;
        }
    }
    current.closed = false;
    for segment in &current.segments {
        let path = segment_path(&source, epoch, segment.offset);
        journal::check_private_path(&path, false)?;
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|_| failure("cannot reconcile batch segment"))?;
        let size = file
            .metadata()
            .map_err(|_| failure("cannot inspect log segment"))?
            .len();
        if size < segment.bytes {
            return Err(failure("committed log segment is truncated"));
        }
        if size > segment.bytes {
            file.set_len(segment.bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| failure("cannot reconcile interrupted log write"))?;
        }
    }
    // A replaced file starts a new prefix; a crash before manifest commit is
    // reconciled by truncating to its committed length before appending.
    if model && policy.archive_enabled {
        let prefix_path = source.join("prefix.raw");
        let limit = (policy.archive_source_bytes - policy.live_bytes()).min(end);
        let prefix_end = if complete {
            limit
        } else {
            limit.min(current.prefix_bytes + READ_WINDOW_BYTES as u64)
        };
        if prefix_path.exists() {
            journal::check_private_path(&prefix_path, false)?;
        }
        let mut prefix = if prefix_path.exists() {
            OpenOptions::new().write(true).open(&prefix_path)
        } else {
            private_file(&prefix_path)
        }
        .map_err(|_| failure("cannot open log prefix"))?;
        if prefix
            .metadata()
            .map_err(|_| failure("cannot inspect log prefix"))?
            .len()
            < current.prefix_bytes
        {
            return Err(failure("committed log prefix is truncated"));
        }
        prefix
            .set_len(current.prefix_bytes)
            .map_err(|_| failure("cannot reconcile log prefix"))?;
        prefix
            .seek(SeekFrom::Start(current.prefix_bytes))
            .map_err(|_| failure("cannot seek log prefix"))?;
        file.seek(SeekFrom::Start(current.prefix_bytes))
            .map_err(|_| failure("cannot seek model log"))?;
        let mut buffer = [0; 8192];
        while current.prefix_bytes < prefix_end {
            let count = buffer
                .len()
                .min((prefix_end - current.prefix_bytes) as usize);
            file.read_exact(&mut buffer[..count])
                .map_err(|_| failure("model log changed during prefix capture"))?;
            prefix
                .write_all(&buffer[..count])
                .map_err(|_| failure("cannot capture log prefix"))?;
            current.prefix_bytes += count as u64;
        }
        prefix
            .sync_all()
            .map_err(|_| failure("cannot sync log prefix"))?;
        replace(&source, "manifest.json", &current)?;
    }
    let mut live_policy = policy.clone();
    live_policy.archive_enabled = false; // batch archive prefixes come from the scheduler file
    let mut writer = Writer {
        directory: source,
        policy: live_policy,
        manifest: current,
    };
    let mut buffer = [0; 8192];
    file.seek(SeekFrom::Start(writer.manifest.end))
        .map_err(|_| failure("cannot seek batch log"))?;
    let read_end = if model && !complete {
        end.min(writer.manifest.end + READ_WINDOW_BYTES as u64)
    } else {
        end
    };
    while writer.manifest.end < read_end {
        let count = buffer.len().min((read_end - writer.manifest.end) as usize);
        file.read_exact(&mut buffer[..count])
            .map_err(|_| failure("batch log changed during bounded read"))?;
        writer.append(&buffer[..count])?;
    }
    if complete {
        writer.finish(false)?;
    } else {
        writer.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn named_fixture() -> (PathBuf, serde_json::Value, LogPolicy) {
        let root = std::env::temp_dir().join(format!("enumeray-named-capture-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        local_process::create_private_directory(&root.join("workspace")).unwrap();
        fs::create_dir(root.join("workspace/logs")).unwrap();
        let model: serde_json::Value = serde_json::to_value(
            enumeray_contracts::models::parse_publication(include_bytes!(
                "../../../examples/scalar-double.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let mut assignment = serde_json::json!({"definition":model["definition"],"requirements":{"driver":"process"}});
        assignment["definition"]["log_files"] = serde_json::json!({"solver":{"path":"logs/solver.log"},"missing":{"path":"logs/missing.log"}});
        let policy = LogPolicy {
            segment_bytes: 4096,
            retained_segments: 2,
            archive_enabled: true,
            archive_source_bytes: 32768,
            ..Default::default()
        };
        (root, assignment, policy)
    }
    #[test]
    fn named_files_survive_restart_replacement_truncation_and_missing_finalization() {
        let (root, assignment, policy) = named_fixture();
        let source = LogSource::model("solver").unwrap();
        let path = root.join("workspace/logs/solver.log");
        collect_named(&root, &assignment, &policy, false).unwrap();
        assert!(
            manifest(&source_directory(&root, source))
                .unwrap()
                .is_none()
        );
        fs::write(&path, b"first\0\xff").unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let before = manifest(&source_directory(&root, source)).unwrap().unwrap();
        assert_eq!(before.end, 7);
        // Uncommitted bytes after a simulated crash must never be delivered.
        let segment = &before.segments[0];
        OpenOptions::new()
            .append(true)
            .open(segment_path(
                &source_directory(&root, source),
                1,
                segment.offset,
            ))
            .unwrap()
            .write_all(b"uncommitted")
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b" next")
            .unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let read_at = |epoch| {
            read(
                &root,
                &ReadLogsRequest {
                    source,
                    epoch,
                    offset: 0,
                    complete: false,
                },
            )
            .unwrap()
        };
        assert_eq!(
            STANDARD.decode(read_at(1).data).unwrap(),
            b"first\0\xff next"
        );
        fs::write(&path, b"tiny").unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        assert_eq!(read_at(1).epoch, 2);
        fs::write(root.join("workspace/logs/new"), b"replaced and longer").unwrap();
        fs::rename(root.join("workspace/logs/new"), &path).unwrap();
        collect_named(&root, &assignment, &policy, true).unwrap();
        let final_read = read_at(2);
        assert_eq!(final_read.epoch, 3);
        assert!(final_read.closed);
        assert_eq!(
            STANDARD.decode(final_read.data).unwrap(),
            b"replaced and longer"
        );
        assert_eq!(
            fs::read(source_directory(&root, source).join("prefix.raw")).unwrap(),
            b"replaced and longer"
        );
        let missing = manifest(&source_directory(
            &root,
            LogSource::model("missing").unwrap(),
        ))
        .unwrap()
        .unwrap();
        assert!(missing.closed && missing.unavailable);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_reads_still_detect_truncation_above_the_committed_offset() {
        let (root, assignment, _) = named_fixture();
        let policy = LogPolicy::default();
        let path = root.join("workspace/logs/solver.log");
        fs::write(&path, vec![b'a'; 200_000]).unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let source = source_directory(&root, LogSource::model("solver").unwrap());
        let before = manifest(&source).unwrap().unwrap();
        assert_eq!(before.file_size, Some(200_000));
        assert_eq!(before.end, READ_WINDOW_BYTES as u64);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(100_000)
            .unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let after = manifest(&source).unwrap().unwrap();
        assert_eq!(after.epoch, 2);
        assert_eq!(after.file_size, Some(100_000));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lost_named_capture_state_reports_terminal_loss_and_does_not_block_other_sources() {
        let (root, mut assignment, policy) = named_fixture();
        fs::write(root.join("workspace/logs/solver.log"), b"solver bytes").unwrap();
        assignment["definition"]["log_files"]["missing"] =
            serde_json::json!({"path":"logs/broken.log"});
        fs::write(root.join("workspace/logs/broken.log"), b"other bytes").unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let broken = LogSource::model("missing").unwrap();
        fs::write(
            source_directory(&root, broken).join("manifest.json"),
            b"broken",
        )
        .unwrap();
        assert!(collect_named(&root, &assignment, &policy, true).is_err());
        assert!(
            manifest(&source_directory(
                &root,
                LogSource::model("solver").unwrap()
            ))
            .unwrap()
            .unwrap()
            .closed
        );
        let mut request = ReadLogsRequest {
            source: broken,
            epoch: 2,
            offset: 17,
            complete: false,
        };
        assert!(read(&root, &request).is_err());
        request.complete = true;
        let lost = read(&root, &request).unwrap();
        assert!(lost.closed && lost.unavailable && lost.data.is_empty());
        assert_eq!((lost.epoch, lost.offset, lost.end), (2, 17, 17));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn named_capture_rejects_links_special_files_and_serializes_owners() {
        use std::os::unix::fs::symlink;
        let (root, assignment, policy) = named_fixture();
        let path = root.join("workspace/logs/solver.log");
        fs::write(root.join("private"), b"must not escape").unwrap();
        symlink(root.join("private"), &path).unwrap();
        assert!(open_model_file(&root.join("workspace"), "logs/solver.log").is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(root.join("workspace/logs")).unwrap();
        symlink(&root, root.join("workspace/logs")).unwrap();
        assert!(open_model_file(&root.join("workspace"), "logs/private").is_err());
        fs::remove_file(root.join("workspace/logs")).unwrap();
        fs::create_dir(root.join("workspace/logs")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(open_model_file(&root.join("workspace"), "logs/solver.log").is_err());
        collect_named(&root, &assignment, &policy, true).unwrap();
        let m = manifest(&source_directory(
            &root,
            LogSource::model("solver").unwrap(),
        ))
        .unwrap()
        .unwrap();
        assert!(m.unavailable && m.end == 0);
        fs::remove_dir_all(root).unwrap();

        let (root, assignment, policy) = named_fixture();
        fs::write(root.join("workspace/logs/solver.log"), b"safe").unwrap();
        collect_named(&root, &assignment, &policy, false).unwrap();
        let source = source_directory(&root, LogSource::model("solver").unwrap());
        let owner = File::options()
            .read(true)
            .write(true)
            .open(source.join("owner.lock"))
            .unwrap();
        owner.lock().unwrap();
        collect_named(&root, &assignment, &policy, true).unwrap();
        assert!(!manifest(&source).unwrap().unwrap().closed);
        drop(owner);
        collect_named(&root, &assignment, &policy, true).unwrap();
        assert!(manifest(&source).unwrap().unwrap().closed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_preserves_recent_raw_bytes_and_batch_epochs() {
        let root = std::env::temp_dir().join(format!("enumeray-capture-{}", Uuid::new_v4()));
        local_process::create_private_directory(&root).unwrap();
        let policy = LogPolicy {
            segment_bytes: 4096,
            retained_segments: 2,
            ..Default::default()
        };
        let bytes: Vec<_> = (0..30_019).map(|n| (n % 251) as u8).collect();
        let mut writer = Writer::new(&root, LogSource::Stdout, policy.clone()).unwrap();
        writer.append(&bytes[..3000]).unwrap();
        let request = ReadLogsRequest {
            source: LogSource::Stdout,
            epoch: 1,
            offset: 0,
            complete: false,
        };
        let retained = read(&root, &request).unwrap();
        writer.append(&bytes[3000..]).unwrap();
        writer.finish(false).unwrap();
        assert_eq!(STANDARD.decode(retained.data).unwrap(), bytes[..3000]);
        let m = manifest(&source_directory(&root, LogSource::Stdout))
            .unwrap()
            .unwrap();
        assert_eq!(m.end, bytes.len() as u64);
        assert_eq!(m.segments.len(), 2);
        assert!(m.end - m.earliest <= policy.live_bytes());
        let mut request = request;
        let mut actual = Vec::new();
        while request.offset < m.end {
            let chunk = read(&root, &request).unwrap();
            let data = STANDARD.decode(chunk.data).unwrap();
            assert!(data.len() <= READ_WINDOW_BYTES);
            request.offset = chunk.offset + data.len() as u64;
            actual.extend(data);
        }
        assert_eq!(actual, bytes[m.earliest as usize..]);
        assert_eq!(
            fs::read_dir(source_directory(&root, LogSource::Stdout))
                .unwrap()
                .count(),
            3
        );

        // Scheduler logs commonly have 0644 modes under our private job directory.
        fs::write(root.join("slurm.log"), &bytes).unwrap();
        collect_batch(&root, &policy, false).unwrap();
        let before = manifest(&source_directory(&root, LogSource::SlurmBatch))
            .unwrap()
            .unwrap();
        assert_eq!(before.earliest, m.earliest);
        assert!(!before.closed);
        // Recover a segment write which reached disk before its manifest commit.
        let last = before.segments.last().unwrap();
        OpenOptions::new()
            .append(true)
            .open(segment_path(
                &source_directory(&root, LogSource::SlurmBatch),
                1,
                last.offset,
            ))
            .unwrap()
            .write_all(b"uncommitted")
            .unwrap();
        collect_batch(&root, &policy, true).unwrap();
        assert_eq!(
            fs::metadata(segment_path(
                &source_directory(&root, LogSource::SlurmBatch),
                1,
                last.offset
            ))
            .unwrap()
            .len(),
            last.bytes
        );
        fs::write(root.join("replacement"), b"new batch diagnostics").unwrap();
        fs::rename(root.join("replacement"), root.join("slurm.log")).unwrap();
        collect_batch(&root, &policy, true).unwrap();
        let after = read(
            &root,
            &ReadLogsRequest {
                source: LogSource::SlurmBatch,
                epoch: 1,
                offset: before.end,
                complete: true,
            },
        )
        .unwrap();
        assert_eq!(after.epoch, 2);
        assert_eq!(after.offset, 0);
        assert!(after.closed);
        assert!(!after.unavailable);
        assert_eq!(
            STANDARD.decode(after.data).unwrap(),
            b"new batch diagnostics"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
