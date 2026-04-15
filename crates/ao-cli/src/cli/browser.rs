//! Best-effort open URLs in the system browser.

/// Best-effort open `http://127.0.0.1:<port>/` in the default browser after the server has time to bind.
pub(crate) fn spawn_open_dashboard_browser(port: u16) {
    let url = format!("http://127.0.0.1:{port}/");
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(750));
        open_url_in_browser(&url);
    });
}

pub(crate) fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
    }
}
