//! Private persistent reservations for scientific transfer copies. Unknown or
//! oversized evidence blocks further staging; restart does not forget occupied space.
use crate::{ErrorKind, Result, failure, local_process};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
const DEFAULT_LIMIT: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_LEDGER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
thread_local! {static LIMIT:Cell<u64>=const {Cell::new(DEFAULT_LIMIT)};}
pub(crate) fn set_limit(limit: u64) {
    LIMIT.set(limit);
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    #[serde(default)]
    collection: bool,
    intent: Uuid,
    bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partial: Option<PathBuf>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    #[serde(default)]
    entries: BTreeMap<PathBuf, Reservation>,
}
fn unavailable() -> crate::ConnectorError {
    failure("scientific-file spool capacity is unavailable; retain payloads and retry after safe cleanup").with_kind(ErrorKind::Retry)
}
fn lock(journal: &Path) -> Result<(PathBuf, File, Ledger)> {
    let directory = journal.join("spool");
    local_process::create_private_directory(&directory)?;
    let path = directory.join("lock");
    if path.try_exists().map_err(|_| unavailable())?
        && !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    {
        return Err(unavailable());
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| unavailable())?;
    file.lock().map_err(|_| unavailable())?;
    let path = directory.join("reservations.json");
    let ledger = if path.try_exists().map_err(|_| unavailable())? {
        let metadata = fs::symlink_metadata(&path).map_err(|_| unavailable())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_LEDGER_BYTES
        {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| unavailable())?
            .take(MAX_LEDGER_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        serde_json::from_slice(&bytes).map_err(|_| unavailable())?
    } else {
        inventory(journal)?
    };
    Ok((directory, file, ledger))
}
fn inventory(journal: &Path) -> Result<Ledger> {
    let mut ledger = Ledger::default();
    let markers = journal.join("spool/collections");
    if markers.exists() {
        for entry in fs::read_dir(markers).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let marker: CollectionReservation =
                crate::collections::read(&entry.path()).map_err(|_| unavailable())?;
            if marker.root.is_absolute()
                || marker.partial.is_absolute()
                || marker
                    .root
                    .components()
                    .chain(marker.partial.components())
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(unavailable());
            }
            ledger.entries.insert(
                marker.root,
                Reservation {
                    intent: marker.intent,
                    bytes: marker.bytes,
                    partial: Some(marker.partial),
                    collection: true,
                },
            );
        }
    }
    let mut stack = vec![
        journal.join("input-staging"),
        journal.join("compute-jobs"),
        journal.join("artifact-transfers"),
    ];
    let mut visited = 0;
    while let Some(path) = stack.pop() {
        if ledger.entries.iter().any(|(root, r)| {
            r.collection
                && (path.starts_with(journal.join(root))
                    || r.partial
                        .as_ref()
                        .is_some_and(|p| path.starts_with(journal.join(p))))
        }) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(unavailable()),
        };
        visited += 1;
        if visited > 100000 {
            return Err(unavailable());
        }
        if metadata.file_type().is_symlink() {
            return Err(unavailable());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).map_err(|_| unavailable())? {
                stack.push(entry.map_err(|_| unavailable())?.path());
            }
            continue;
        }
        let relative = path.strip_prefix(journal).map_err(|_| unavailable())?;
        if relative
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == "input-staging")
            && relative
                .components()
                .nth(2)
                .is_some_and(|c| c.as_os_str() == "manifests")
        {
            // Immutable metadata has its own 16 MiB bound; payload reservations
            // account for complete collection copies rather than sidecars.
            continue;
        }
        if !relative.components().any(|p| {
            matches!(
                p.as_os_str().to_str(),
                Some("input-staging" | "outputs" | "output-staging")
            )
        }) {
            continue;
        }
        let checkpoint = relative.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with(".enumeray-download-"))
        });
        if checkpoint && path.file_name().is_none_or(|name| name != "payload.part") {
            continue;
        }
        if !checkpoint {
            // A crash between hard-link publication and unlink can expose two
            // names for one copy. Inventory the retained payload only once.
            if let Ok(directory) = enumeray_client::bulk::checkpoint_directory(&path)
                && let Ok(other) = fs::symlink_metadata(directory.join("payload.part"))
            {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if (metadata.dev(), metadata.ino()) == (other.dev(), other.ino()) {
                        continue;
                    }
                }
            }
        }
        let intent = relative
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .find_map(|s| Uuid::parse_str(s).ok())
            .ok_or_else(unavailable)?;
        if ledger.entries.len() >= MAX_ENTRIES {
            return Err(unavailable());
        }
        let bytes = if checkpoint {
            let state = path.parent().ok_or_else(unavailable)?.join("state.json");
            crate::journal::check_private_path(&state, false)?;
            let mut bytes = Vec::new();
            File::open(state)
                .map_err(|_| unavailable())?
                .take(65537)
                .read_to_end(&mut bytes)
                .map_err(|_| unavailable())?;
            if bytes.len() > 65536 {
                return Err(unavailable());
            }
            let state: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
            let size = state["evidence"]["size_bytes"]
                .as_u64()
                .filter(|size| {
                    *size <= enumeray_contracts::models::MAX_ARTIFACT_BYTES
                        && *size >= metadata.len()
                })
                .ok_or_else(unavailable)?;
            if state["schema_version"] != 1 {
                return Err(unavailable());
            }
            size
        } else {
            metadata.len()
        };
        ledger.entries.insert(
            relative.to_path_buf(),
            Reservation {
                collection: false,
                intent,
                bytes,
                partial: None,
            },
        );
    }
    Ok(ledger)
}
fn save(directory: &Path, ledger: &Ledger) -> Result<()> {
    let bytes = serde_json::to_vec(ledger).map_err(|_| unavailable())?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err(unavailable());
    }
    let temporary = directory.join(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = crate::capture::private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("reservations.json"))?;
        File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| unavailable())
}
#[cfg(test)]
pub(crate) fn reserve(journal: &Path, intent: Uuid, target: &Path, size: u64) -> Result<()> {
    reserve_inner(journal, intent, target, size, None, false)
}
pub(crate) fn reserve_copy(
    journal: &Path,
    intent: Uuid,
    target: &Path,
    size: u64,
    partial: &Path,
) -> Result<()> {
    if GROUP.with(|g| {
        g.borrow().as_ref().is_some_and(|(root, temporary)| {
            target.starts_with(root) && partial.starts_with(temporary)
        })
    }) {
        return Ok(());
    }
    reserve_inner(journal, intent, target, size, Some(partial), false)
}
fn reserve_inner(
    journal: &Path,
    intent: Uuid,
    target: &Path,
    size: u64,
    partial: Option<&Path>,
    collection: bool,
) -> Result<()> {
    let relative = target.strip_prefix(journal).map_err(|_| unavailable())?;
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(unavailable());
    }
    let (directory, _lock, mut ledger) = lock(journal)?;
    let absolute_journal = journal.canonicalize().map_err(|_| unavailable())?;
    let partial = partial
        .map(|path| {
            path.strip_prefix(journal)
                .or_else(|_| path.strip_prefix(&absolute_journal))
                .map(Path::to_path_buf)
                .map_err(|_| unavailable())
        })
        .transpose()?;
    if partial.as_ref().is_some_and(|path| {
        path.components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    }) {
        return Err(unavailable());
    }
    if let Some(partial) = &partial
        && partial != relative
        && let Some(recovered) = ledger.entries.remove(partial)
    {
        if recovered.intent != intent {
            return Err(unavailable());
        }
        let existing = ledger
            .entries
            .entry(relative.into())
            .or_insert(Reservation {
                collection: false,
                intent,
                bytes: 0,
                partial: None,
            });
        existing.bytes = existing.bytes.max(recovered.bytes);
    }
    let old = ledger.entries.get(relative).map(|r| r.bytes).unwrap_or(0);
    if ledger
        .entries
        .get(relative)
        .is_some_and(|r| r.intent != intent)
    {
        return Err(unavailable());
    }
    let total = ledger
        .entries
        .values()
        .try_fold(0_u64, |sum, r| sum.checked_add(r.bytes))
        .ok_or_else(unavailable)?;
    let charge = size.max(old);
    let needed = charge - old;
    if (needed > 0 && total.checked_add(needed).is_none_or(|n| n > LIMIT.get()))
        || (ledger.entries.len() >= MAX_ENTRIES && !ledger.entries.contains_key(relative))
    {
        return Err(unavailable());
    }
    let free = rustix::fs::statvfs(journal).map_err(|_| unavailable())?;
    if needed > 0 {
        // Other workers may have reserved their copies before writing them.
        // Count those future writes as well as the bytes already on disk. A
        // temporary copy whose final name is absent is conservatively charged
        // in full until its atomic rename, so it cannot oversubscribe the disk.
        let pending = ledger
            .entries
            .iter()
            .try_fold(0_u64, |sum, (path, entry)| {
                let mut actual = journal.join(path);
                if !actual.try_exists().map_err(|_| unavailable())?
                    && let Some(partial) = &entry.partial
                {
                    actual = journal.join(partial);
                }
                let present = if entry.collection {
                    collection_present(journal, path, entry.partial.as_deref())?
                } else {
                    match fs::symlink_metadata(actual) {
                        Ok(metadata)
                            if metadata.is_file() && !metadata.file_type().is_symlink() =>
                        {
                            metadata.len()
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                        _ => return Err(unavailable()),
                    }
                };
                sum.checked_add(entry.bytes.saturating_sub(present))
                    .ok_or_else(unavailable)
            })?;
        if pending
            .checked_add(needed)
            .is_none_or(|required| required > free.f_bavail.saturating_mul(free.f_frsize))
        {
            return Err(unavailable());
        }
    }
    ledger.entries.insert(
        relative.to_path_buf(),
        Reservation {
            collection,
            intent,
            bytes: charge,
            partial,
        },
    );
    save(&directory, &ledger)
}
pub(crate) fn release(journal: &Path, intent: Uuid) -> Result<()> {
    if !journal.join("spool").exists() {
        return Ok(());
    }
    let (directory, _lock, mut ledger) = lock(journal)?;
    ledger
        .entries
        .retain(|_, reservation| reservation.intent != intent);
    save(&directory, &ledger)?;
    let markers = journal.join("spool/collections");
    if markers.exists() {
        for entry in fs::read_dir(markers).map_err(|_| unavailable())? {
            let path = entry.map_err(|_| unavailable())?.path();
            let marker: CollectionReservation =
                crate::collections::read(&path).map_err(|_| unavailable())?;
            if marker.intent == intent {
                fs::remove_file(path).map_err(|_| unavailable())?;
            }
        }
    }
    Ok(())
}
thread_local! {static GROUP:std::cell::RefCell<Option<(PathBuf,PathBuf)>>=const {std::cell::RefCell::new(None)};}
#[derive(Serialize, Deserialize)]
struct CollectionReservation {
    root: PathBuf,
    partial: PathBuf,
    intent: Uuid,
    bytes: u64,
}
pub(crate) struct CollectionGuard;
impl Drop for CollectionGuard {
    fn drop(&mut self) {
        GROUP.with(|g| *g.borrow_mut() = None);
    }
}
pub(crate) fn collection(
    journal: &Path,
    intent: Uuid,
    root: &Path,
    partial: &Path,
    bytes: u64,
) -> Result<CollectionGuard> {
    reserve_inner(journal, intent, root, bytes, Some(partial), true)?;
    let marker = CollectionReservation {
        root: root
            .strip_prefix(journal)
            .map_err(|_| unavailable())?
            .into(),
        partial: partial
            .strip_prefix(journal)
            .map_err(|_| unavailable())?
            .into(),
        intent,
        bytes,
    };
    let name = crate::artifacts::transfer_key(intent, &root.to_string_lossy(), false, 0);
    crate::collections::write(
        &journal
            .join("spool/collections")
            .join(format!("{name}.json")),
        &marker,
    )?;
    GROUP.with(|g| *g.borrow_mut() = Some((root.into(), partial.into())));
    Ok(CollectionGuard)
}
fn collection_present(journal: &Path, root: &Path, partial: Option<&Path>) -> Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![(journal.join(root), partial == Some(root))];
    if let Some(other) = partial
        && other != root
    {
        stack.push((journal.join(other), true));
    }
    let mut visited = 0;
    #[cfg(unix)]
    let mut seen = std::collections::BTreeSet::new();
    while let Some((path, temporary)) = stack.pop() {
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(unavailable()),
        };
        visited += 1;
        if visited > 100000 || meta.file_type().is_symlink() {
            return Err(unavailable());
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path).map_err(|_| unavailable())? {
                stack.push((entry.map_err(|_| unavailable())?.path(), temporary));
            }
        } else if meta.is_file() {
            if temporary
                && path.components().any(|c| {
                    c.as_os_str()
                        .to_str()
                        .is_some_and(|s| s.starts_with(".enumeray-download-"))
                })
                && path.file_name().is_none_or(|s| s != "payload.part")
            {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if !seen.insert((meta.dev(), meta.ino())) {
                    continue;
                }
            }
            total = total.checked_add(meta.len()).ok_or_else(unavailable)?;
        } else {
            return Err(unavailable());
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_prefix_inventory_recovers_one_full_reservation_without_double_charging() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use enumeray_client::bulk::{DownloadEvidence, RetainedTransfer};
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!("enumeray-spool-copy-{}", Uuid::new_v4()));
        let intent = Uuid::new_v4();
        let staging = root.join("input-staging").join(intent.to_string());
        crate::local_process::create_private_directory(&root).unwrap();
        crate::local_process::create_private_directory(&root.join("input-staging")).unwrap();
        crate::local_process::create_private_directory(&staging).unwrap();
        let target = staging.join("artifact");
        let partial = enumeray_client::bulk::checkpoint_directory(&target)
            .unwrap()
            .join("payload.part");
        set_limit(6);
        reserve_copy(&root, intent, &target, 6, &partial).unwrap();
        let evidence = DownloadEvidence {
            scope: "assignment".into(),
            source: "artifact".into(),
            version_id: None,
            size_bytes: 6,
            checksum_sha256: STANDARD.encode(Sha256::digest(b"abcdef")),
        };
        let mut retained = RetainedTransfer::open(
            &target,
            evidence,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            |_| {},
        )
        .unwrap();
        retained.append(b"abc").unwrap();
        retained.checkpoint().unwrap();
        drop(retained);
        fs::remove_file(root.join("spool/reservations.json")).unwrap();
        reserve_copy(&root, intent, &target, 6, &partial).unwrap();
        assert!(reserve(&root, Uuid::new_v4(), &root.join("other"), 1).is_err());
        let ledger: Ledger =
            serde_json::from_slice(&fs::read(root.join("spool/reservations.json")).unwrap())
                .unwrap();
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries.values().next().unwrap().bytes, 6);
        set_limit(DEFAULT_LIMIT);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn restart_retains_charges_until_confirmed_cleanup() {
        let root = std::env::temp_dir().join(format!("enumeray-spool-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let intent = Uuid::new_v4();
        set_limit(10);
        reserve(&root, intent, &root.join("one"), 6).unwrap();
        reserve(&root, intent, &root.join("one"), 6).unwrap();
        set_limit(1);
        reserve(&root, intent, &root.join("one"), 6).unwrap();
        assert!(reserve(&root, intent, &root.join("two"), 1).is_err());
        set_limit(10);
        assert!(reserve(&root, intent, &root.join("two"), 5).is_err());
        release(&root, intent).unwrap();
        reserve(&root, Uuid::new_v4(), &root.join("two"), 10).unwrap();
        assert!(reserve(&root, intent, &root.join("../escape"), 1).is_err());
        set_limit(DEFAULT_LIMIT);
        fs::remove_dir_all(root).unwrap();
    }
}
