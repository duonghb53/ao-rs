//! `ao-rs stop` — terminate the singleton lifecycle service.

use std::path::Path;
use std::time::Duration;

use ao_core::{is_process_alive, paths, read_pidfile};

/// Stop the background lifecycle service (`watch` / `dashboard`) if it is running.
pub async fn stop(all: bool, purge_session: bool) -> Result<(), Box<dyn std::error::Error>> {
    if all {
        eprintln!("(note) --all is reserved for future multi-service support; stopping lifecycle service only.");
    }
    if purge_session {
        eprintln!(
            "(note) --purge-session is reserved for future supervisor state; sessions remain intact (use `cleanup`)."
        );
    }

    let pid_path = paths::lifecycle_pid_file();
    stop_pidfile(&pid_path, StopOptions::default()).await
}

#[derive(Debug, Clone)]
struct StopOptions {
    wait_total: Duration,
    poll_every: Duration,
}

impl Default for StopOptions {
    fn default() -> Self {
        Self {
            wait_total: Duration::from_secs(2),
            poll_every: Duration::from_millis(50),
        }
    }
}

async fn stop_pidfile(path: &Path, opts: StopOptions) -> Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        println!(
            "→ no lifecycle service is running (no pidfile at {}).",
            path.display()
        );
        return Ok(());
    }

    let Some(pid) = read_pidfile(path) else {
        eprintln!(
            "→ pidfile exists but is unreadable; removing stale file at {}.",
            path.display()
        );
        let _ = std::fs::remove_file(path);
        return Ok(());
    };

    if !is_process_alive(pid) {
        eprintln!(
            "→ lifecycle pidfile was stale (pid {pid} is not running); removing {}.",
            path.display()
        );
        let _ = std::fs::remove_file(path);
        return Ok(());
    }

    #[cfg(unix)]
    {
        // SIGTERM the process and wait for it to exit.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!("failed to signal pid {pid}: {e}").into());
        }

        println!("→ sent SIGTERM to lifecycle service (pid {pid}); waiting...");
        wait_for_exit(pid, opts.wait_total, opts.poll_every).await;

        if is_process_alive(pid) {
            return Err(format!(
                "lifecycle service (pid {pid}) did not exit within {}ms",
                opts.wait_total.as_millis()
            )
            .into());
        }

        // Best-effort cleanup: remove the pidfile if it still points at the pid we stopped.
        if read_pidfile(path) == Some(pid) {
            let _ = std::fs::remove_file(path);
        }
        println!("→ stopped.");
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = (pid, opts);
        Err("`ao-rs stop` is currently supported only on Unix-like OSes".into())
    }
}

async fn wait_for_exit(pid: u32, total: Duration, poll_every: Duration) {
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if !is_process_alive(pid) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(poll_every).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_pidfile(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("ao-rs-stop-{label}-{nanos}-{n}.pid"))
    }

    #[tokio::test]
    async fn stop_removes_unparseable_pidfile() {
        let path = unique_pidfile("unparseable");
        std::fs::write(&path, "not-a-pid\n").unwrap();
        assert!(path.exists());

        stop_pidfile(&path, StopOptions::default()).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stop_removes_stale_pidfile() {
        let path = unique_pidfile("stale");
        // 999_999 is extremely unlikely to exist on macOS/Linux.
        std::fs::write(&path, "999999\n").unwrap();
        assert!(path.exists());

        stop_pidfile(&path, StopOptions::default()).await.unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_sends_sigterm_to_live_process() {
        // Spawn a long-lived process that is *not* our child, so when it
        // exits it won't stick around as a zombie until we reap it.
        //
        // Use `nohup` so the background job survives shell exit.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("nohup sleep 60 >/dev/null 2>&1 & echo $!")
            .output()
            .expect("sh should be available on unix");
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        let pid: u32 = stdout.trim().parse().expect("pid");
        assert!(is_process_alive(pid), "expected pid {pid} to be alive");

        let path = unique_pidfile("live");
        std::fs::write(&path, format!("{pid}\n")).unwrap();

        let opts = StopOptions {
            wait_total: Duration::from_secs(2),
            poll_every: Duration::from_millis(25),
        };
        let result = stop_pidfile(&path, opts).await;

        // Ensure we don't leak the process even if the test fails.
        if is_process_alive(pid) {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }

        result.unwrap();
        assert!(!is_process_alive(pid));
        // pidfile should be removed if it still pointed at the pid.
        assert!(!path.exists() || read_pidfile(&path) != Some(pid));
    }
}
