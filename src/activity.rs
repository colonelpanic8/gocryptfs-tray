//! Detects use of an unlocked vault, for auto-locking.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

pub type LastActivity = Arc<Mutex<Instant>>;

/// Records file activity under `root` into `last` until the vault is unmounted.
///
/// Directory opens and reads are ignored: listing directories, including this
/// watcher's own scan, does not count as using the vault.
pub fn watch(root: &Path, last: LastActivity) -> Result<()> {
    let mut inotify = Inotify::init()?;
    let mut dirs = HashMap::new();
    add_tree(&mut inotify, &mut dirs, root);

    let mut buffer = [0u8; 8192];
    loop {
        for event in inotify.read_events_blocking(&mut buffer)? {
            if event.mask.contains(EventMask::UNMOUNT) {
                return Ok(());
            }
            if event.mask.contains(EventMask::IGNORED) {
                dirs.remove(&event.wd);
                continue;
            }
            let is_dir = event.mask.contains(EventMask::ISDIR);
            if is_dir
                && event
                    .mask
                    .intersects(EventMask::CREATE | EventMask::MOVED_TO)
                && let (Some(parent), Some(name)) = (dirs.get(&event.wd), event.name)
            {
                let path = parent.join(name);
                add_tree(&mut inotify, &mut dirs, &path);
            }
            let listing = is_dir
                && event
                    .mask
                    .intersects(EventMask::OPEN | EventMask::ACCESS | EventMask::CLOSE_NOWRITE);
            if !listing {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    let dir = dirs.get(&event.wd).map(PathBuf::as_path).unwrap_or(root);
                    let path = event
                        .name
                        .map_or_else(|| dir.to_path_buf(), |name| dir.join(name));
                    tracing::debug!("activity: {:?} {}", event.mask, path.display());
                }
                *last.lock().unwrap() = Instant::now();
            }
        }
        if dirs.is_empty() {
            return Ok(());
        }
    }
}

fn add_tree(inotify: &mut Inotify, dirs: &mut HashMap<WatchDescriptor, PathBuf>, root: &Path) {
    let mask = WatchMask::OPEN
        | WatchMask::ACCESS
        | WatchMask::MODIFY
        | WatchMask::CLOSE_WRITE
        | WatchMask::CREATE
        | WatchMask::DELETE
        | WatchMask::MOVE
        | WatchMask::ONLYDIR
        | WatchMask::DONT_FOLLOW;
    let device = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata.dev(),
        Err(_) => return,
    };
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        match inotify.watches().add(&dir, mask) {
            Ok(wd) => {
                dirs.insert(wd, dir.clone());
            }
            Err(error) => {
                tracing::warn!("not watching {}: {error}", dir.display());
                continue;
            }
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata()
                && metadata.is_dir()
                && metadata.dev() == device
            {
                pending.push(entry.path());
            }
        }
    }
}

/// Names of this user's processes with a file open, or their working
/// directory, inside `root`.
pub fn users(root: &Path) -> Vec<String> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let me = std::process::id().to_string();
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut names: Vec<String> = processes
        .flatten()
        .filter(|process| {
            let name = process.file_name();
            let Some(pid) = name.to_str() else {
                return false;
            };
            if !pid.bytes().all(|byte| byte.is_ascii_digit()) || pid == me {
                return false;
            }
            let path = process.path();
            if !std::fs::metadata(&path).is_ok_and(|m| m.uid() == uid) {
                return false;
            }
            let inside =
                |link: &Path| std::fs::read_link(link).is_ok_and(|target| target.starts_with(root));
            inside(&path.join("cwd"))
                || inside(&path.join("root"))
                || std::fs::read_dir(path.join("fd"))
                    .is_ok_and(|mut fds| fds.any(|fd| fd.is_ok_and(|fd| inside(&fd.path()))))
        })
        .map(|process| {
            std::fs::read_to_string(process.path().join("comm"))
                .map(|comm| comm.trim().to_owned())
                .unwrap_or_else(|_| process.file_name().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_process_working_inside_the_tree_is_in_use() {
        let dir = std::env::temp_dir().join(format!("gocryptfs-tray-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        assert!(users(&dir).is_empty());
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        // Give the child time to exec so /proc reflects its cwd.
        std::thread::sleep(Duration::from_millis(100));
        let names = users(&dir);
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["sleep"]);
    }

    #[test]
    fn file_writes_count_but_directory_listings_do_not() {
        let dir = std::env::temp_dir().join(format!("gocryptfs-tray-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let start = Instant::now();
        let last = Arc::new(Mutex::new(start));
        let watcher_last = last.clone();
        let watched = dir.clone();
        std::thread::spawn(move || watch(&watched, watcher_last));
        std::thread::sleep(Duration::from_millis(200));

        std::fs::read_dir(dir.join("sub")).unwrap().for_each(drop);
        std::thread::sleep(Duration::from_millis(200));
        let after_listing = *last.lock().unwrap();

        std::fs::write(dir.join("sub/file"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let after_write = *last.lock().unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(after_listing, start);
        assert!(after_write > start);
    }
}
