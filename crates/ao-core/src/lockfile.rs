//! PID-file based advisory locking for singleton daemons.
//!
//! Mirrors `packages/cli/src/lib/lifecycle-service.ts` in the reference repo:
//! a would-be daemon reads a well-known pidfile, checks whether that pid is
//! still running (via `kill(pid, 0)`), and takes over iff the previous owner
//! is gone. The file is removed on clean shutdown.
//!
//! This is **advisory**, not enforced — two racing processes that both pass
//! the "not running" check before either writes can still stomp on each
//! other. The TS reference has the same limitation and shrugs it off for a
//! single-user CLI. Slice 1 Phase D does the same.
//!
//! Why not `fs2::flock`? Flock survives across restarts on Linux but not
//! macOS (BSD flock is tied to the fd), and we want "process-that-owns-pid
//! is alive" semantics anyway, which flock doesn't give us. A PID probe is
//! the behaviour the user actually wants.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum LockError {
    /// Another live process currently holds the lock.
    HeldBy {
        pid: u32,
        path: PathBuf,
    },
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeldBy { pid, path } => {
                write!(f, "pidfile {} held by live process {pid}", path.display())
            }
            Self::Io(e) => write!(f, "pidfile io: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// RAII handle for a pidfile we currently own. Releases on `Drop`.
///
/// Dropping removes the file **only** if its contents still match our pid —
/// so a second daemon that stole the lock (e.g. after we crashed) doesn't
/// get its pidfile deleted out from under it when we eventually unwind.
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
    pid: u32,
    // Once released (drop already removed the file), flip this so the Drop
    // impl doesn't try a second time.
    released: bool,
}

impl PidFile {
    /// Try to take the pidfile at `path`. On success, our pid is written
    /// to disk and held until drop.
    ///
    /// If a previous pidfile exists and that pid is still alive,
    /// `Err(LockError::HeldBy)` is returned and the file is left untouched.
    /// A stale pidfile (dead pid) is silently replaced.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self, LockError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        if let Some(existing) = read_pidfile(&path) {
            if is_process_alive(existing) && existing != std::process::id() {
                return Err(LockError::HeldBy {
                    pid: existing,
                    path,
                });
            }
            // Stale: fall through and overwrite.
        }

        let pid = std::process::id();
        // Write via a sibling temp + rename so a concurrent reader never
        // sees a half-written file (matches `SessionManager::save`).
        let temp = path.with_extension("pid.tmp");
        {
            let mut f = fs::File::create(&temp)?;
            writeln!(f, "{pid}")?;
            f.sync_all()?;
        }
        fs::rename(&temp, &path)?;

        Ok(Self {
            path,
            pid,
            released: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Explicitly release the pidfile now. Equivalent to letting it drop,
    /// except the caller can observe the io error instead of swallowing it.
    pub fn release(mut self) -> io::Result<()> {
        self.released = true;
        remove_if_ours(&self.path, self.pid)
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Best effort — a failed cleanup just leaves a stale file, which
        // the next acquire() will replace.
        let _ = remove_if_ours(&self.path, self.pid);
    }
}

/// Read the pid stored in a pidfile, if the file exists and parses.
pub fn read_pidfile(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

/// Is the given pid currently a running process on this machine?
///
/// Uses `kill(pid, 0)` — the POSIX way to test for a pid's existence
/// without actually signalling it. `EPERM` also counts as "alive" (the
/// process exists but we don't own it, e.g. running as another user).
pub fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` is thread-safe and a signal of 0 performs only the
    // permission/existence check without delivering anything. The only
    // preconditions are a valid pid (we reject 0 above) and a defined
    // signal (0 is always defined), both of which we satisfy.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => true, // exists, not ours
        _ => false,
    }
}

fn remove_if_ours(path: &Path, our_pid: u32) -> io::Result<()> {
    // Re-read the file before deleting; if it no longer says our pid,
    // someone else owns it and we leave it alone.
    match read_pidfile(path) {
        Some(pid) if pid == our_pid => fs::remove_file(path),
        // Either the file is gone already or a different owner took over —
        // both are "nothing to clean up" as far as we're concerned.
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-lock-{label}-{nanos}-{n}.pid"))
    }

    #[test]
    fn acquire_when_no_file_writes_our_pid() {
        let path = unique_tmp("fresh");
        let lock = PidFile::acquire(&path).unwrap();
        assert!(path.exists());
        assert_eq!(read_pidfile(&path), Some(std::process::id()));
        assert_eq!(lock.pid(), std::process::id());
        drop(lock);
        assert!(!path.exists(), "drop should remove the pidfile");
    }

    #[test]
    fn acquire_replaces_stale_pidfile() {
        // Pick a pid that is extremely unlikely to be alive. 999_999 is
        // above the default Linux pid_max (32768) and also not a valid
        // macOS pid. kill(pid, 0) returns ESRCH → dead.
        let stale_pid: u32 = 999_999;
        assert!(!is_process_alive(stale_pid), "sanity: {stale_pid} is dead");

        let path = unique_tmp("stale");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{stale_pid}\n")).unwrap();

        let lock = PidFile::acquire(&path).unwrap();
        assert_eq!(read_pidfile(&path), Some(std::process::id()));
        drop(lock);
    }

    #[test]
    fn acquire_rejects_live_other_pid() {
        // Fake a pidfile holding pid 1. On every Unix box pid 1 is alive
        // (init / launchd), and it's not us, so this should fail.
        let path = unique_tmp("held");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "1\n").unwrap();

        match PidFile::acquire(&path) {
            Err(LockError::HeldBy { pid, .. }) => assert_eq!(pid, 1),
            other => panic!("expected HeldBy(1), got {other:?}"),
        }
        // File must not be rewritten by a failed acquire.
        assert_eq!(read_pidfile(&path), Some(1));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn drop_does_not_remove_file_if_stolen() {
        let path = unique_tmp("stolen");
        let lock = PidFile::acquire(&path).unwrap();

        // Simulate a racing daemon overwriting our pidfile with its own pid.
        fs::write(&path, "1\n").unwrap();

        drop(lock);

        // The hijacked contents must survive — we only clean up our own pid.
        assert_eq!(read_pidfile(&path), Some(1));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn is_process_alive_returns_true_for_self() {
        assert!(is_process_alive(std::process::id()));
    }

    #[test]
    fn is_process_alive_returns_false_for_zero() {
        assert!(!is_process_alive(0));
    }
}
