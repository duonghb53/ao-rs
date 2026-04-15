use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

fn usage() -> ! {
    eprintln!(
        "usage: terminal_load --base <http://127.0.0.1:3000> --session <SESSION_PREFIX> [--slow-ms <N>] [--seconds <N>]\n\
         \n\
         Connects to the dashboard terminal WebSocket and reads output.\n\
         Use --slow-ms to intentionally read slowly (exercise server backpressure/drop policy)."
    );
    std::process::exit(2);
}

fn ws_url(base: &str, session: &str) -> Url {
    let base = base.trim_end_matches('/');
    let mut u = Url::parse(base).unwrap_or_else(|_| usage());
    match u.scheme() {
        "http" => {
            u.set_scheme("ws").ok();
        }
        "https" => {
            u.set_scheme("wss").ok();
        }
        "ws" | "wss" => {}
        _ => usage(),
    }
    u.set_path(&format!(
        "/api/sessions/{}/terminal",
        urlencoding::encode(session)
    ));
    u
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut base: Option<String> = None;
    let mut session: Option<String> = None;
    let mut slow_ms: u64 = 0;
    let mut seconds: u64 = 15;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--base" => base = args.next(),
            "--session" => session = args.next(),
            "--slow-ms" => slow_ms = args.next().unwrap_or_default().parse().unwrap_or(0),
            "--seconds" => seconds = args.next().unwrap_or_default().parse().unwrap_or(15),
            "--help" | "-h" => usage(),
            _ => usage(),
        }
    }
    let base = base.unwrap_or_else(|| usage());
    let session = session.unwrap_or_else(|| usage());

    let url = ws_url(&base, &session);
    let url_s = url.to_string();
    eprintln!("connecting to {url_s}");

    let (mut ws, _) = tokio_tungstenite::connect_async(url_s).await?;

    // Trigger an initial resize to keep tmux layouts sane.
    let _ = ws
        .send(Message::Text(
            r#"{"type":"resize","cols":120,"rows":40}"#.into(),
        ))
        .await;

    let start = Instant::now();
    let stop_at = start + Duration::from_secs(seconds);
    let mut bytes: u64 = 0;
    let mut dropped_notices: u64 = 0;

    while let Some(msg) = ws.next().await {
        let msg = msg?;
        match msg {
            Message::Binary(b) => {
                bytes += b.len() as u64;
                if slow_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(slow_ms)).await;
                }
            }
            Message::Text(t) => {
                if t.contains(r#""type":"dropped""#) {
                    dropped_notices += 1;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }

        if Instant::now() >= stop_at {
            break;
        }
    }

    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    eprintln!(
        "done: {:.2}s, {:.2} MiB read, {:.2} MiB/s, dropped_notices={}",
        elapsed,
        (bytes as f64) / (1024.0 * 1024.0),
        (bytes as f64) / (1024.0 * 1024.0) / elapsed,
        dropped_notices
    );

    Ok(())
}
