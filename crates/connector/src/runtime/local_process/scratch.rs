//! Sampled workspace accounting. Traversal never follows directory links and
//! yields to process observation/cancellation between bounded chunks.
use enumeray_contracts::execution::{ProcessScratchPolicy, ScratchSymlinks};
use rustix::{
    fd::OwnedFd,
    fs::{self as rfs, AtFlags, Dir, FileType, Mode, OFlags},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const CHUNK_ENTRIES: usize = 256;
const CHUNK_TIME: Duration = Duration::from_millis(5);

#[derive(Debug)]
pub(super) struct Violation {
    pub code: &'static str,
    pub message: String,
}
fn violation(code: &'static str, message: impl Into<String>) -> Violation {
    Violation {
        code,
        message: message.into(),
    }
}
fn unavailable() -> Violation {
    violation(
        "scratch_scan_failed",
        "Cannot safely inspect the process workspace.",
    )
}

struct Frame {
    fd: OwnedFd,
    entries: Dir,
    path: PathBuf,
    depth: u8,
}
impl Frame {
    fn new(fd: OwnedFd, path: PathBuf, depth: u8) -> Result<Self, Violation> {
        let entries = Dir::read_from(&fd).map_err(|_| unavailable())?;
        Ok(Self {
            fd,
            entries,
            path,
            depth,
        })
    }
}

pub(super) struct Scan {
    root: PathBuf,
    frames: Vec<Frame>,
    bytes: u64,
    entries: u64,
    policy: ProcessScratchPolicy,
}
impl Scan {
    pub fn new(root: &Path, policy: &ProcessScratchPolicy) -> Result<Self, Violation> {
        let fd = rfs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?;
        let root = root.canonicalize().map_err(|_| unavailable())?;
        Ok(Self {
            frames: vec![Frame::new(fd, root.clone(), 0)?],
            root,
            bytes: 0,
            entries: 0,
            policy: policy.clone(),
        })
    }

    /// True means a complete pass. No materialized tree or recursive walk.
    pub fn step(&mut self) -> Result<bool, Violation> {
        let started = Instant::now();
        let mut inspected = 0;
        while inspected < CHUNK_ENTRIES && started.elapsed() < CHUNK_TIME {
            let Some(frame) = self.frames.last_mut() else {
                return Ok(true);
            };
            let Some(entry) = frame.entries.read() else {
                self.frames.pop();
                continue;
            };
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            inspected += 1;
            self.entries += 1;
            if self.entries > u64::from(self.policy.max_entries) {
                return Err(violation(
                    "scratch_entries",
                    format!(
                        "Process workspace entries {} exceed configured maximum {}.",
                        self.entries, self.policy.max_entries
                    ),
                ));
            }
            let stat = match rfs::statat(&frame.fd, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(_) => return Err(unavailable()),
            };
            let path = frame
                .path
                .join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    let depth = frame.depth + 1;
                    if depth > self.policy.max_depth {
                        return Err(violation(
                            "scratch_depth",
                            format!(
                                "Process workspace directory depth {depth} exceeds configured maximum {}.",
                                self.policy.max_depth
                            ),
                        ));
                    }
                    let fd = match rfs::openat(
                        &frame.fd,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    ) {
                        Ok(fd) => fd,
                        Err(rustix::io::Errno::NOENT) => continue,
                        Err(_) => return Err(unavailable()),
                    };
                    self.frames.push(Frame::new(fd, path, depth)?);
                }
                FileType::RegularFile => {
                    let bytes = u128::from(self.bytes) + u128::from(stat.st_size.max(0) as u64);
                    if bytes > u128::from(self.policy.max_bytes) {
                        return Err(violation(
                            "scratch_bytes",
                            format!(
                                "Process workspace bytes {} exceed configured maximum {}.",
                                bytes, self.policy.max_bytes
                            ),
                        ));
                    }
                    self.bytes = bytes as u64;
                }
                FileType::Symlink => {
                    if self.policy.symlinks == ScratchSymlinks::Forbid {
                        return Err(violation(
                            "scratch_symlink",
                            "Process workspace symlinks are forbidden by infrastructure policy.",
                        ));
                    }
                    // Resolution validates links only; accounting always uses the
                    // physical tree. Targets are never scanned through aliases.
                    let target = path.canonicalize().map_err(|_| {
                        violation(
                            "scratch_symlink",
                            "Process workspace link is dangling, cyclic or unavailable.",
                        )
                    })?;
                    if !target.starts_with(&self.root)
                        || !std::fs::metadata(&target).is_ok_and(|m| m.is_file() || m.is_dir())
                    {
                        return Err(violation(
                            "scratch_symlink",
                            "Process workspace link target is outside its workspace or is not a regular file/directory.",
                        ));
                    }
                }
                _ => {
                    return Err(violation(
                        "scratch_entry_type",
                        "Process workspace contains an unsupported filesystem entry.",
                    ));
                }
            }
        }
        Ok(self.frames.is_empty())
    }
}
use std::os::unix::ffi::OsStrExt;

pub(super) fn check(root: &Path, policy: &ProcessScratchPolicy) -> Result<(), Violation> {
    let mut scan = Scan::new(root, policy)?;
    while !scan.step()? {}
    Ok(())
}

pub(super) struct Monitor {
    scan: Option<Scan>,
    next: Instant,
    policy: ProcessScratchPolicy,
}
impl Monitor {
    pub fn new(policy: &ProcessScratchPolicy) -> Self {
        Self {
            scan: None,
            next: Instant::now(),
            policy: policy.clone(),
        }
    }
    pub fn tick(&mut self, root: &Path) -> Result<(), Violation> {
        if self.scan.is_none() && Instant::now() >= self.next {
            self.scan = Some(Scan::new(root, &self.policy)?);
        }
        if let Some(scan) = &mut self.scan
            && scan.step()?
        {
            self.scan = None;
            self.next =
                Instant::now() + Duration::from_millis(u64::from(self.policy.scan_interval_ms));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enumeray_contracts::execution::ProcessCache;
    use std::{fs, os::unix::fs::symlink};
    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("enumeray-scratch-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn policy() -> ProcessScratchPolicy {
        ProcessScratchPolicy {
            max_bytes: 100,
            max_entries: 1000,
            max_depth: 32,
            scan_interval_ms: 1000,
            cache: ProcessCache::PerRun,
            symlinks: ScratchSymlinks::WorkspaceOnly,
        }
    }
    #[test]
    fn quotas_report_the_exhausted_dimension_and_allow_exact_boundaries() {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("data"), [0; 100]).unwrap();
        check(&workspace.0, &policy()).unwrap();
        fs::write(workspace.0.join("extra"), [0]).unwrap();
        assert_eq!(
            check(&workspace.0, &policy()).unwrap_err().code,
            "scratch_bytes"
        );
        let mut limits = policy();
        limits.max_bytes = 1000;
        limits.max_entries = 1;
        assert_eq!(
            check(&workspace.0, &limits).unwrap_err().code,
            "scratch_entries"
        );
        limits.max_entries = 1000;
        limits.max_depth = 1;
        fs::create_dir_all(workspace.0.join("one/two")).unwrap();
        assert_eq!(
            check(&workspace.0, &limits).unwrap_err().code,
            "scratch_depth"
        );
    }
    #[test]
    fn maximum_byte_limits_cannot_hide_accounting_overflow() {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("next"), [0]).unwrap();
        let mut limits = policy();
        limits.max_bytes = u64::MAX;
        let mut scan = Scan::new(&workspace.0, &limits).unwrap();
        // A previously accumulated total at the limit must reject another byte.
        scan.bytes = u64::MAX;
        let error = scan.step().unwrap_err();
        assert_eq!(error.code, "scratch_bytes");
        assert!(error.message.contains("18446744073709551616"));
    }
    #[test]
    fn internal_aliases_are_not_traversed_or_double_counted() {
        let workspace = Workspace::new();
        fs::create_dir(workspace.0.join("data")).unwrap();
        fs::write(workspace.0.join("data/file"), [0; 100]).unwrap();
        symlink("data", workspace.0.join("alias")).unwrap();
        symlink("data/file", workspace.0.join("file-alias")).unwrap();
        check(&workspace.0, &policy()).unwrap();
        let mut limits = policy();
        limits.symlinks = ScratchSymlinks::Forbid;
        assert_eq!(
            check(&workspace.0, &limits).unwrap_err().code,
            "scratch_symlink"
        );
    }
    #[test]
    fn escaping_dangling_cyclic_and_replaced_directory_links_fail() {
        for target in ["/etc", "absent", "link"] {
            let workspace = Workspace::new();
            symlink(target, workspace.0.join("link")).unwrap();
            assert_eq!(
                check(&workspace.0, &policy()).unwrap_err().code,
                "scratch_symlink"
            );
        }
        let workspace = Workspace::new();
        fs::create_dir(workspace.0.join("data")).unwrap();
        let mut scan = Scan::new(&workspace.0, &policy()).unwrap();
        fs::remove_dir(workspace.0.join("data")).unwrap();
        symlink("/etc", workspace.0.join("data")).unwrap();
        assert_eq!(scan.step().unwrap_err().code, "scratch_symlink");
    }
    #[test]
    fn scans_yield_after_bounded_chunks_and_wait_between_completed_passes() {
        let workspace = Workspace::new();
        for i in 0..600 {
            fs::write(workspace.0.join(i.to_string()), []).unwrap();
        }
        let mut scan = Scan::new(&workspace.0, &policy()).unwrap();
        assert!(!scan.step().unwrap());
        assert!(scan.entries <= CHUNK_ENTRIES as u64);
        while !scan.step().unwrap() {}
        assert_eq!(scan.entries, 600);
        let mut monitor = Monitor::new(&policy());
        monitor.tick(&workspace.0).unwrap();
        while monitor.scan.is_some() {
            monitor.tick(&workspace.0).unwrap();
        }
        fs::write(workspace.0.join("oversized"), [0; 101]).unwrap();
        monitor.tick(&workspace.0).unwrap();
        assert!(monitor.scan.is_none());
        monitor.next = Instant::now();
        loop {
            if let Err(error) = monitor.tick(&workspace.0) {
                assert_eq!(error.code, "scratch_bytes");
                break;
            }
        }
    }
}
