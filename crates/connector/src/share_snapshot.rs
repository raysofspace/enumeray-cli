//! Content-addressed installations for foreground shares.

use crate::{Result, failure};
use rustix::fs::{self as rfs, Dir, FileType, Mode, OFlags, RenameFlags};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub struct ShareSnapshot {
    pub path: PathBuf,
    pub sha256: String,
}

pub fn stage_share_snapshot(
    project: &Path,
    releases: &Path,
) -> std::result::Result<ShareSnapshot, String> {
    stage(project, releases).map_err(|error| error.to_string())
}

pub fn verify_share_snapshot(path: &Path, expected: &str) -> std::result::Result<(), String> {
    verify(path, expected).map_err(|error| error.to_string())
}

fn stage(project: &Path, releases: &Path) -> Result<ShareSnapshot> {
    crate::journal::check_private_path(releases, true)?;
    let project = fs::canonicalize(project).map_err(|_| failure("cannot resolve share project"))?;
    let releases =
        fs::canonicalize(releases).map_err(|_| failure("cannot resolve share release storage"))?;
    if releases.starts_with(&project) || project.starts_with(&releases) {
        return Err(failure("share release storage must be outside the project"));
    }

    let root = open_directory(&releases)?;
    let temporary_name = format!(".staging-{}", Uuid::new_v4());
    let temporary = releases.join(&temporary_name);
    private_directory(&temporary)?;
    let staged = (|| {
        let sha256 = tree_digest(&project, Some(&temporary))?;
        let path = releases.join(&sha256);
        match rfs::renameat_with(
            &root,
            &temporary_name,
            &root,
            &sha256,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => (),
            Err(rustix::io::Errno::EXIST) => verify(&path, &sha256)?,
            Err(_) => return Err(failure("cannot atomically install staged share release")),
        }
        root.sync_all()
            .map_err(|_| failure("cannot persist staged share release"))?;
        Ok(ShareSnapshot { path, sha256 })
    })();
    if temporary.exists() {
        discard_temporary(&temporary);
    }
    staged
}

pub(crate) fn verify(path: &Path, expected: &str) -> Result<()> {
    if tree_digest(path, None)? != expected {
        return Err(failure(
            "staged share release no longer matches its SHA-256 digest",
        ));
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<File> {
    rfs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| failure("cannot open share snapshot directory"))
}

fn private_directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| failure("cannot create private share staging directory"))
}

fn tree_digest(root: &Path, destination: Option<&Path>) -> Result<String> {
    let root = open_directory(root)?;
    let mut tree = Sha256::new();
    tree.update(b"enumeray-share-snapshot-v1\0");
    visit(&root, destination, &mut tree, 0)?;
    Ok(format!("{:x}", tree.finalize()))
}

fn visit(source: &File, destination: Option<&Path>, tree: &mut Sha256, depth: usize) -> Result<()> {
    if depth > 128 {
        return Err(failure(
            "share snapshot directory nesting exceeds 128 levels",
        ));
    }
    if destination.is_none()
        && source
            .metadata()
            .map_err(|_| failure("cannot inspect snapshot directory"))?
            .permissions()
            .mode()
            & 0o777
            != 0o500
    {
        return Err(failure("staged share directory has unexpected permissions"));
    }
    let mut entries =
        Dir::read_from(source).map_err(|_| failure("cannot read share snapshot directory"))?;
    let mut names: Vec<CString> = Vec::new();
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(|_| failure("cannot read share snapshot entry"))?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." || destination.is_some() && excluded(bytes) {
            continue;
        }
        names.push(name.to_owned());
    }
    names.sort_by(|left, right| left.to_bytes().cmp(right.to_bytes()));

    for name in names {
        let bytes = name.to_bytes();
        tree.update((bytes.len() as u64).to_be_bytes());
        tree.update(bytes);
        let stat = rfs::statat(source, name.as_c_str(), rfs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| failure("cannot inspect share snapshot entry"))?;
        let target = destination.map(|root| root.join(OsStr::from_bytes(bytes)));
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => {
                tree.update(b"d");
                let child = rfs::openat(
                    source,
                    name.as_c_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(|_| failure("share snapshot directory changed while being read"))?;
                if let Some(target) = &target {
                    private_directory(target)?;
                }
                visit(&child, target.as_deref(), tree, depth + 1)?;
            }
            FileType::RegularFile => {
                tree.update(b"f");
                let file = rfs::openat(
                    source,
                    name.as_c_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(|_| failure("share snapshot file changed while being read"))?;
                file_digest(file, target.as_deref(), tree)?;
            }
            _ => {
                return Err(failure(
                    "share snapshots require regular files and directories without symlinks",
                ));
            }
        }
    }
    tree.update(0_u64.to_be_bytes());
    if let Some(path) = destination {
        seal(&open_directory(path)?, 0o500)?;
    }
    Ok(())
}

fn excluded(name: &[u8]) -> bool {
    matches!(
        name,
        b".git"
            | b".hg"
            | b".svn"
            | b".cache"
            | b".mypy_cache"
            | b".pytest_cache"
            | b".ruff_cache"
            | b"__pycache__"
            | b".venv"
            | b"venv"
            | b".env"
    ) || name.starts_with(b".env.")
        || name.ends_with(b".pyc")
        || name.ends_with(b".pyo")
}

fn file_digest(mut file: File, target: Option<&Path>, tree: &mut Sha256) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|_| failure("cannot inspect snapshot file"))?;
    if !metadata.is_file() {
        return Err(failure("share snapshot file is no longer a regular file"));
    }
    let executable = metadata.permissions().mode() & 0o111 != 0;
    let release_mode = if executable { 0o500 } else { 0o400 };
    if target.is_none() && metadata.permissions().mode() & 0o777 != release_mode {
        return Err(failure("staged share file has unexpected permissions"));
    }
    let mut copy = target
        .map(|path| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        })
        .transpose()
        .map_err(|_| failure("cannot create staged share file"))?;
    let mut contents = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    let mut length = 0_u64;
    loop {
        let read = file
            .read(&mut bytes)
            .map_err(|_| failure("cannot read share snapshot file"))?;
        if read == 0 {
            break;
        }
        length += read as u64;
        contents.update(&bytes[..read]);
        if let Some(copy) = &mut copy {
            copy.write_all(&bytes[..read])
                .map_err(|_| failure("cannot write staged share file"))?;
        }
    }
    if length != metadata.len() {
        return Err(failure("share snapshot file changed while being read"));
    }
    tree.update([u8::from(executable)]);
    tree.update(length.to_be_bytes());
    tree.update(contents.finalize());
    if let Some(copy) = copy {
        seal(&copy, release_mode)?;
    }
    Ok(())
}

fn seal(file: &File, mode: u32) -> Result<()> {
    file.set_permissions(fs::Permissions::from_mode(mode))
        .and_then(|()| file.sync_all())
        .map_err(|_| failure("cannot seal staged share release"))
}

fn discard_temporary(path: &Path) {
    if let Ok(directory) = open_directory(path) {
        let _ = directory.set_permissions(fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    discard_temporary(&entry.path());
                }
            }
        }
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("enumeray-share-snapshot-{}", Uuid::new_v4()));
            private_directory(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            discard_temporary(&self.0);
        }
    }

    #[test]
    fn stages_reuses_and_pins_the_complete_tree() {
        let workspace = Workspace::new();
        let project = workspace.0.join("project");
        let releases = workspace.0.join("releases");
        private_directory(&project).unwrap();
        private_directory(&releases).unwrap();
        fs::write(project.join("wrapper"), b"wrapper").unwrap();
        fs::set_permissions(project.join("wrapper"), fs::Permissions::from_mode(0o755)).unwrap();
        private_directory(&project.join("inputs")).unwrap();
        fs::write(project.join("inputs/expected.csv"), b"42").unwrap();
        fs::write(project.join(".git"), b"worktree metadata").unwrap();
        private_directory(&project.join(".venv")).unwrap();
        std::os::unix::fs::symlink("../private-runtime", project.join(".venv/link")).unwrap();
        fs::write(project.join(".env.local"), b"private configuration").unwrap();
        fs::write(project.join("inputs/cache.pyc"), b"ignored bytecode").unwrap();

        let first = stage(&project, &releases).unwrap();
        verify(&first.path, &first.sha256).unwrap();
        assert_eq!(first.path, stage(&project, &releases).unwrap().path);
        assert!(!first.path.join(".git").exists());
        assert!(!first.path.join(".venv").exists());
        assert!(!first.path.join(".env.local").exists());
        assert!(!first.path.join("inputs/cache.pyc").exists());
        assert_eq!(
            fs::metadata(first.path.join("wrapper"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        fs::write(project.join("inputs/expected.csv"), b"84").unwrap();
        let second = stage(&project, &releases).unwrap();
        assert_ne!(first.sha256, second.sha256);
        assert_eq!(
            fs::read(first.path.join("inputs/expected.csv")).unwrap(),
            b"42"
        );

        let staged_input = second.path.join("inputs/expected.csv");
        fs::set_permissions(&staged_input, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&staged_input, b"43").unwrap();
        fs::set_permissions(&staged_input, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(verify(&second.path, &second.sha256).is_err());
        verify(&first.path, &first.sha256).unwrap();
    }

    #[test]
    fn a_symlink_never_becomes_an_installed_release() {
        let workspace = Workspace::new();
        let project = workspace.0.join("project");
        let releases = workspace.0.join("releases");
        private_directory(&project).unwrap();
        private_directory(&releases).unwrap();
        std::os::unix::fs::symlink(&workspace.0, project.join("outside")).unwrap();
        assert!(stage(&project, &releases).is_err());
        assert!(fs::read_dir(releases).unwrap().next().is_none());
    }
}
