pub(crate) fn render_config_help() -> String {
    // Keep this output stable for copy/paste and snapshot testing.
    // No colors, no dynamic paths, no timestamps.
    let s = r#"ao-rs config help

Config file name
  ao-rs.yaml

Discovery
  ao-rs searches for `ao-rs.yaml` by walking up from the current working directory.
  The first match wins.

Getting started
  1) Copy the example config:
       cp ao-rs.yaml.example ao-rs.yaml
  2) Edit `ao-rs.yaml` for your repos, defaults, reactions, and notifier routing.
  3) Validate your setup:
       ao-rs doctor

Example config file
  ao-rs.yaml.example

Common keys (high level)
  port: dashboard port
  defaults: runtime/agent/workspace/tracker defaults
  projects: per-repo settings (repo, path, default branch, worker overrides)
  reactions: automation rules (thresholds, actions, escalation)
  notificationRouting / notification_routing: route notifications by priority

Docs
  docs/config.md
  docs/reactions.md
"#;
    s.to_string()
}

pub async fn config_help() -> Result<(), Box<dyn std::error::Error>> {
    print!("{}", render_config_help());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_help_output_snapshot() {
        insta::assert_snapshot!(render_config_help());
    }
}
