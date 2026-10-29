//! Private, crash-safe retained payloads. A checkpoint proves only a retained
//! prefix; publication still requires the pinned full-object digest.
use crate::Error;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Instant,
};

pub const CHECKPOINT_BYTES: u64 = 8 * 1024 * 1024;
pub const RANGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 64 * 1024;

pub fn checkpoint_directory(destination: &Path) -> Result<PathBuf, Error> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let destination = parent.join(
        destination
            .file_name()
            .ok_or_else(|| invalid("Missing download filename."))?,
    );
    let key: String = Sha256::digest(destination.as_os_str().as_encoded_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(parent.join(format!(".enumeray-download-{key}")))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadEvidence {
    /// API/account or connector/assignment binding, without credentials.
    pub scope: String,
    pub source: String,
    pub version_id: Option<String>,
    pub size_bytes: u64,
    pub checksum_sha256: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferProgress {
    pub resumed_bytes: u64,
    pub transferred_bytes: u64,
    pub verified_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    evidence: DownloadEvidence,
    destination: PathBuf,
    offset: u64,
    prefix_sha256: String,
    published: bool,
}

pub struct RetainedTransfer {
    directory: PathBuf,
    record: Record,
    file: File,
    _lock: File,
    digest: Sha256,
    offset: u64,
    resumed: u64,
    deadline: Instant,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(format!(
        "{message} Retain the download checkpoint for inspection; move it aside explicitly to restart."
    ))
}

fn private(path: &Path, directory: bool) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(invalid("Transfer state is not a regular private path."));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid("Transfer state permits group or other access."));
        }
    }
    Ok(())
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

impl RetainedTransfer {
    pub fn open(
        destination: &Path,
        evidence: DownloadEvidence,
        deadline: Instant,
        mut progress: impl FnMut(u64),
    ) -> Result<Self, Error> {
        if evidence.scope.is_empty()
            || evidence.scope.len() > 4096
            || evidence.source.is_empty()
            || evidence.source.len() > 4096
            || evidence
                .version_id
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 1024 || v == "null")
            || !STANDARD
                .decode(&evidence.checksum_sha256)
                .is_ok_and(|bytes| bytes.len() == 32)
            || evidence.size_bytes > enumeray_contracts::models::MAX_ARTIFACT_BYTES
        {
            return Err(invalid("Invalid pinned transfer evidence."));
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = parent.canonicalize()?;
        let destination = parent.join(
            destination
                .file_name()
                .ok_or_else(|| invalid("Missing download filename."))?,
        );
        let directory = checkpoint_directory(&destination)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => {
                File::open(&parent)?.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        private(&directory, true)?;
        let lock_path = directory.join("lock");
        let lock = match options().create_new(true).open(&lock_path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                private(&lock_path, false)?;
                options().open(&lock_path)?
            }
            Err(e) => return Err(e.into()),
        };
        lock.try_lock()
            .map_err(|_| invalid("Another command owns this transfer."))?;
        let state = directory.join("state.json");
        let payload = directory.join("payload.part");
        let mut record = if exists(&state)? {
            private(&state, false)?;
            let mut bytes = Vec::new();
            File::open(&state)?
                .take(MAX_STATE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_STATE_BYTES {
                return Err(invalid("Transfer checkpoint exceeds its bound."));
            }
            let record: Record = serde_json::from_slice(&bytes)?;
            if record.schema_version != 1
                || record.evidence != evidence
                || record.destination != destination
                || record.offset > evidence.size_bytes
                || record.published && record.offset != evidence.size_bytes
            {
                return Err(invalid("Transfer identity conflicts with its checkpoint."));
            }
            record
        } else {
            if exists(&payload)? || exists(&destination)? {
                return Err(invalid(
                    "Download destination or unowned partial already exists.",
                ));
            }
            let record = Record {
                schema_version: 1,
                evidence,
                destination,
                offset: 0,
                prefix_sha256: STANDARD.encode(Sha256::digest([])),
                published: false,
            };
            save(&directory, &record)?;
            record
        };
        let payload_exists = exists(&payload)?;
        // A caller may have moved a verified staging file to its final confined
        // location, or explicitly removed a downloaded file. Reacquiring the
        // same pinned source is safe when no destination remains to overwrite.
        if record.published && !exists(&record.destination)? {
            record.published = false;
            if !payload_exists {
                record.offset = 0;
                record.prefix_sha256 = STANDARD.encode(Sha256::digest([]));
            }
            save(&directory, &record)?;
        }
        let file = if record.published {
            private(&record.destination, false)?;
            options().open(&record.destination)?
        } else if payload_exists {
            private(&payload, false)?;
            options().open(&payload)?
        } else if record.offset == 0 {
            options().create_new(true).open(&payload)?
        } else {
            return Err(invalid("Retained payload is missing."));
        };
        #[cfg(unix)]
        if !record.published {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            if metadata.nlink() != 1 {
                // The only extra name we own is an interrupted publication of
                // this complete payload. Never truncate an unrelated hard link.
                let owned_publication = metadata.nlink() == 2
                    && record.offset == record.evidence.size_bytes
                    && fs::symlink_metadata(&record.destination).is_ok_and(|destination| {
                        destination.is_file()
                            && (metadata.dev(), metadata.ino())
                                == (destination.dev(), destination.ino())
                    });
                if !owned_publication {
                    return Err(invalid("Retained payload has an unowned hard link."));
                }
            }
        }
        let mut transfer = Self {
            directory,
            offset: record.offset,
            resumed: record.offset,
            record,
            file,
            _lock: lock,
            digest: Sha256::new(),
            deadline,
        };
        if transfer.file.metadata()?.len() < transfer.offset {
            return Err(invalid("Retained payload is shorter than its checkpoint."));
        }
        let mut remaining = transfer.offset;
        let mut buffer = [0; 64 * 1024];
        while remaining > 0 {
            transfer.check_deadline()?;
            let size = transfer
                .file
                .read(&mut buffer[..remaining.min(65536) as usize])?;
            if size == 0 {
                return Err(invalid("Retained prefix is truncated."));
            }
            transfer.digest.update(&buffer[..size]);
            remaining -= size as u64;
            progress(size as u64);
        }
        if STANDARD.encode(transfer.digest.clone().finalize()) != transfer.record.prefix_sha256 {
            return Err(invalid(
                "Retained prefix checksum differs from its checkpoint.",
            ));
        }
        if transfer.record.published && transfer.file.metadata()?.len() != transfer.offset {
            return Err(invalid("Published download changed."));
        }
        Ok(transfer)
    }

    pub fn check_deadline(&self) -> Result<(), Error> {
        if Instant::now() >= self.deadline {
            return Err(Error::Invalid(
                "Transfer reached its overall deadline; repeat the same command to resume.".into(),
            ));
        }
        Ok(())
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn mark_resumed(&mut self) {
        self.resumed = self.offset;
    }
    pub fn evidence(&self) -> &DownloadEvidence {
        &self.record.evidence
    }
    pub fn payload_path(&self) -> PathBuf {
        self.directory.join("payload.part")
    }
    pub fn is_published(&self) -> bool {
        self.record.published
    }
    pub fn progress(&self) -> TransferProgress {
        TransferProgress {
            resumed_bytes: self.resumed,
            transferred_bytes: self.offset - self.resumed,
            verified_bytes: if self.record.published {
                self.offset
            } else {
                0
            },
            total_bytes: self.record.evidence.size_bytes,
        }
    }
    /// HTTP/direct readers cannot trust a tail whose checkpoint did not commit.
    pub fn discard_uncommitted(&mut self) -> Result<(), Error> {
        if !self.record.published {
            self.file.set_len(self.offset)?;
            self.file.seek(SeekFrom::Start(self.offset))?;
        }
        Ok(())
    }
    pub fn append(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        self.check_deadline()?;
        if self.record.published
            || bytes.len() as u64 > self.record.evidence.size_bytes - self.offset
        {
            return Err(invalid("Transfer exceeds its pinned size."));
        }
        while !bytes.is_empty() {
            self.check_deadline()?;
            let length = (CHECKPOINT_BYTES - (self.offset - self.record.offset))
                .min(bytes.len() as u64) as usize;
            let (chunk, remaining) = bytes.split_at(length);
            self.file.write_all(chunk)?;
            self.digest.update(chunk);
            self.offset += length as u64;
            if self.offset - self.record.offset == CHECKPOINT_BYTES {
                self.checkpoint()?;
            }
            bytes = remaining;
        }
        Ok(())
    }
    pub fn checkpoint(&mut self) -> Result<(), Error> {
        self.file.sync_all()?;
        self.record.offset = self.offset;
        self.record.prefix_sha256 = STANDARD.encode(self.digest.clone().finalize());
        save(&self.directory, &self.record)
    }
    /// SFTP may have written beyond the last checkpoint. Compare each bounded
    /// range with the pinned remote source before adopting those bytes.
    pub fn reconcile_external(
        &mut self,
        mut expected: impl FnMut(u64, u64) -> Result<String, Error>,
        mut progress: impl FnMut(u64),
    ) -> Result<(), Error> {
        let length = self.file.metadata()?.len();
        if length > self.record.evidence.size_bytes {
            return Err(invalid("Partial exceeds pinned size."));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut offset = 0;
        let mut prefix = Sha256::new();
        let mut buffer = [0; 65536];
        while offset < length {
            self.check_deadline()?;
            let count = (length - offset).min(RANGE_BYTES);
            let mut range = Sha256::new();
            let mut remaining = count;
            while remaining > 0 {
                let n = self
                    .file
                    .read(&mut buffer[..remaining.min(65536) as usize])?;
                if n == 0 {
                    return Err(invalid("Partial changed during verification."));
                }
                range.update(&buffer[..n]);
                prefix.update(&buffer[..n]);
                remaining -= n as u64;
                progress(n as u64);
            }
            if STANDARD.encode(range.finalize()) != expected(offset, count)? {
                return Err(invalid("Partial content differs from the pinned source."));
            }
            offset += count;
        }
        self.digest = prefix;
        self.offset = length;
        self.checkpoint()
    }
    pub fn finish(self) -> Result<TransferProgress, Error> {
        self.finish_with_progress(|_| {})
    }
    pub fn finish_with_progress(
        mut self,
        mut progress: impl FnMut(u64),
    ) -> Result<TransferProgress, Error> {
        self.check_deadline()?;
        if self.offset != self.record.evidence.size_bytes
            || self.file.metadata()?.len() != self.offset
        {
            return Err(invalid(
                "Complete payload differs from its pinned size or checksum.",
            ));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut remaining = self.offset;
        let mut buffer = [0; 65536];
        while remaining > 0 {
            self.check_deadline()?;
            let n = self
                .file
                .read(&mut buffer[..remaining.min(65536) as usize])?;
            if n == 0 {
                return Err(invalid("Complete payload was truncated."));
            }
            digest.update(&buffer[..n]);
            remaining -= n as u64;
            progress(n as u64);
        }
        if STANDARD.encode(digest.clone().finalize()) != self.record.evidence.checksum_sha256 {
            return Err(invalid(
                "Complete payload differs from its pinned size or checksum.",
            ));
        }
        self.digest = digest;
        self.checkpoint()?;
        if !self.record.published {
            let payload = self.payload_path();
            match fs::hard_link(&payload, &self.record.destination) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    private(&self.record.destination, false)?;
                    // Only our original publication inode proves a lost acknowledgement.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        let a = self.file.metadata()?;
                        let b = fs::metadata(&self.record.destination)?;
                        if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                            return Err(invalid("Download destination already exists."));
                        }
                    }
                    #[cfg(not(unix))]
                    return Err(invalid("Download destination already exists."));
                }
                Err(e) => return Err(e.into()),
            }
            File::open(self.record.destination.parent().expect("canonical parent"))?.sync_all()?;
            self.record.published = true;
            save(&self.directory, &self.record)?;
            fs::remove_file(payload)?;
            File::open(&self.directory)?.sync_all()?;
        }
        Ok(self.progress())
    }
}

fn save(directory: &Path, record: &Record) -> Result<(), Error> {
    let temporary = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = options().create_new(true).open(&temporary)?;
        let bytes = serde_json::to_vec(record)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(invalid("Transfer checkpoint exceeds its bound."));
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("state.json"))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
