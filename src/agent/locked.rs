//! Changing a config file others write too, an agent's or roster's own:
//! under a lock beside it, `<file>.lock`, a directory made to take it and
//! removed to let it go, taken over once it's 10 s old (its holder gone:
//! Claude, holding it, touches it every few seconds, and roster holds it no
//! longer than a read and a write, never across an editor). It's Claude's
//! own lock for `.claude.json`; codex has none, so for its `config.toml`
//! it's roster's alone, which keeps roster's helpers (two sessions starting
//! at once) from undoing each other, as it keeps two rosters changing
//! roster's settings at once from undoing each other's. Held from reading
//! the file to replacing it, whole, in one rename; one that changed
//! meanwhile all the same (a write that took no lock: an editor's, codex's,
//! a few of Claude's) is read again.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};

/// How old a lock is let be before it's taken for its holder's, gone.
const STALE: Duration = Duration::from_secs(10);

/// How long a lock held is waited for.
const WAIT: Duration = Duration::from_secs(5);

/// Changes the config at `file` as `change` says, given its text (empty if
/// there's none yet): what to write, or None to leave it. Whether it wrote;
/// if it did, what `change` last made is what's written.
pub(crate) fn update(
    file: &Path,
    mut change: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<bool> {
    let _lock = Lock::take(file, WAIT)?;
    for _ in 0..3 {
        let read = stamp(file);
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("can't read {}", file.display())),
        };
        let Some(changed) = change(&text)? else {
            return Ok(false);
        };
        if stamp(file) != read {
            continue;
        }
        crate::settings::write_file(file, &changed)?;
        return Ok(true);
    }
    anyhow::bail!("{} kept changing as it was read", file.display())
}

/// When the file at `path` last changed, and its size: what it was read as.
fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// The lock beside a config file, held while it's kept.
struct Lock(PathBuf);

impl Lock {
    /// Takes it, waiting at most `wait` for one held.
    fn take(file: &Path, wait: Duration) -> Result<Lock> {
        let mut path = file.as_os_str().to_owned();
        path.push(".lock");
        let path = PathBuf::from(path);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
        }
        let since = Instant::now();
        loop {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e).with_context(|| format!("can't lock {}", file.display())),
            }
            let age = (fs::metadata(&path).and_then(|m| m.modified()).ok())
                .and_then(|t| t.elapsed().ok());
            // One that can't be removed (a file, say) is waited for as one
            // held, and given up on.
            if age.is_some_and(|age| age > STALE) && fs::remove_dir(&path).is_ok() {
                continue;
            }
            if since.elapsed() > wait {
                anyhow::bail!(
                    "{} stays locked by {} (another program saving it, or one that ended \
                     left it): try again in a few seconds",
                    file.display(),
                    path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_taken_waited_for_and_taken_over_when_stale() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        let lock = dir.path().join("config.lock");
        // Held while it's kept, let go after.
        let held = Lock::take(&file, WAIT).unwrap();
        assert!(lock.is_dir());
        drop(held);
        assert!(!lock.exists());
        // Held a moment by another: waited for, then changed.
        fs::create_dir(&lock).unwrap();
        let other = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                fs::remove_dir(lock).unwrap();
            })
        };
        assert!(update(&file, |text| Ok(Some(format!("{text}a")))).unwrap());
        other.join().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "a");
        assert!(!lock.exists());
        // Left as it was: nothing written.
        assert!(!update(&file, |_| Ok(None)).unwrap());
        // Held on: given up on.
        fs::create_dir(&lock).unwrap();
        let short = Duration::from_millis(100);
        assert!(Lock::take(&file, short).is_err());
        // Left by a holder gone (old): taken over.
        let old = SystemTime::now() - STALE - Duration::from_secs(5);
        fs::File::open(&lock).unwrap().set_modified(old).unwrap();
        assert!(Lock::take(&file, short).is_ok());
        assert!(!lock.exists());
        // Old, but not one to remove (a file): given up on all the same.
        fs::write(&lock, "").unwrap();
        fs::File::open(&lock).unwrap().set_modified(old).unwrap();
        assert!(Lock::take(&file, short).is_err());
        assert!(lock.is_file());
    }

    #[test]
    fn changes_made_at_once_are_all_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        // Each slow to change it, all at once: none undoes another's.
        let changes: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    update(&file, |text| {
                        std::thread::sleep(Duration::from_millis(20));
                        Ok(Some(format!("{text}{i}\n")))
                    })
                    .unwrap()
                })
            })
            .collect();
        for change in changes {
            assert!(change.join().unwrap());
        }
        let text = fs::read_to_string(&file).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        assert_eq!(lines, ["0", "1", "2", "3", "4", "5", "6", "7"]);
    }
}
