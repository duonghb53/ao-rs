//! `ao-rs open` — open dashboard/session targets.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use ao_core::{Scm, SessionManager};

use crate::cli::args::OpenTarget;
use crate::cli::auto_scm::AutoScm;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenRequest {
    Url(String),
    Path(PathBuf),
}

pub(crate) trait Opener {
    fn open(&self, req: &OpenRequest, new_window: bool) -> Result<(), Box<dyn std::error::Error>>;
}

pub(crate) struct OsOpener;

impl Opener for OsOpener {
    fn open(&self, req: &OpenRequest, new_window: bool) -> Result<(), Box<dyn std::error::Error>> {
        match req {
            OpenRequest::Url(url) => open_str(url, new_window),
            OpenRequest::Path(path) => open_str(&path.display().to_string(), new_window),
        }
    }
}

fn open_str(target: &str, new_window: bool) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if new_window {
            cmd.arg("-n");
        }
        cmd.arg(target);
        cmd.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        let _ = new_window;
        cmd.arg(target);
        cmd.spawn()?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("cmd");
        let _ = new_window;
        cmd.args(["/C", "start", "", target]);
        cmd.spawn()?;
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (target, new_window);
        Err("open is not supported on this platform".into())
    }
}

pub async fn open(
    port: u16,
    new_window: bool,
    target: OpenTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    let opener = OsOpener;
    open_with(&opener, port, new_window, target).await
}

pub(crate) async fn open_with(
    opener: &dyn Opener,
    port: u16,
    new_window: bool,
    target: OpenTarget,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = resolve_open_request(port, target).await?;
    opener.open(&req, new_window)?;
    Ok(())
}

pub(crate) async fn resolve_open_request(
    port: u16,
    target: OpenTarget,
) -> Result<OpenRequest, Box<dyn std::error::Error>> {
    match target {
        OpenTarget::Dashboard => Ok(OpenRequest::Url(dashboard_root_url(port))),
        OpenTarget::Session { id } => {
            let sessions = SessionManager::with_default();
            let session = sessions.find_by_prefix(&id).await?;
            let alive = probe_local_tcp(port);
            let pr_url = if alive {
                None
            } else {
                let scm = AutoScm::new();
                scm.detect_pr(&session)
                    .await
                    .ok()
                    .flatten()
                    .map(|pr| pr.url)
            };
            choose_session_open_request(alive, port, &id, pr_url.as_deref(), session.workspace_path)
        }
    }
}

pub(crate) fn dashboard_root_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

pub(crate) fn dashboard_session_url(port: u16, session_id_or_prefix: &str) -> String {
    // The dashboard crate is API-only; the best "detail view" we can open
    // is the single-session JSON endpoint.
    format!("http://127.0.0.1:{port}/api/sessions/{session_id_or_prefix}")
}

pub(crate) fn choose_session_open_request(
    dashboard_alive: bool,
    port: u16,
    session_id_or_prefix: &str,
    pr_url: Option<&str>,
    workspace_path: Option<PathBuf>,
) -> Result<OpenRequest, Box<dyn std::error::Error>> {
    if dashboard_alive {
        return Ok(OpenRequest::Url(dashboard_session_url(
            port,
            session_id_or_prefix,
        )));
    }
    if let Some(url) = pr_url {
        return Ok(OpenRequest::Url(url.to_string()));
    }
    let ws = workspace_path.ok_or_else(|| "session has no workspace path".to_string())?;
    Ok(OpenRequest::Path(ws))
}

pub(crate) fn probe_local_tcp(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok()
}
