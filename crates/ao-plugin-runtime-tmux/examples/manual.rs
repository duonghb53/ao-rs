//! Manual smoke test for the tmux runtime plugin.
//!
//! Run:
//!     cargo run --example manual -p ao-plugin-runtime-tmux
//!
//! It spins up a real tmux session, sends a couple of commands into it,
//! prints the `tmux attach` invocation so you can poke at it, and then
//! cleans up after you press Enter.

use ao_core::Runtime;
use ao_plugin_runtime_tmux::TmuxRuntime;
use std::io::Write;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = TmuxRuntime::new();

    // Unique-ish so concurrent runs don't collide.
    let session_id = format!("ao-rs-manual-{}", std::process::id());
    let cwd = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));

    println!("→ creating tmux session: {session_id}");
    println!("  cwd: {}", cwd.display());
    runtime.create(&session_id, &cwd, "bash", &[]).await?;

    println!("→ is_alive: {}", runtime.is_alive(&session_id).await?);

    println!("→ sending: echo 'hello from ao-rs'");
    runtime
        .send_message(&session_id, "echo 'hello from ao-rs'")
        .await?;

    println!("→ sending: pwd && ls -la | head");
    runtime
        .send_message(&session_id, "pwd && ls -la | head")
        .await?;

    // Multiline / long message — exercises the load-buffer / paste-buffer path.
    let long = "echo line1\necho line2\necho line3";
    println!("→ sending multiline message (paste-buffer path)");
    runtime.send_message(&session_id, long).await?;

    println!();
    println!("───────────────────────────────────────────────");
    println!("  attach in another terminal with:");
    println!("    tmux attach -t {session_id}");
    println!("  (Ctrl-b d to detach)");
    println!("───────────────────────────────────────────────");
    print!("press Enter to destroy and exit... ");
    std::io::stdout().flush()?;
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;

    println!("→ destroying session");
    runtime.destroy(&session_id).await?;
    println!("→ is_alive: {}", runtime.is_alive(&session_id).await?);
    println!("done.");
    Ok(())
}
