//! Resolve and inline agent rules for sessions.

use ao_core::{default_agent_rules, AgentConfig, Session};

pub(crate) fn resolve_agent_config(
    base: Option<&AgentConfig>,
    repo_path: &std::path::Path,
) -> Option<AgentConfig> {
    let cfg = base.cloned()?;

    let Some(rules_file) = cfg.rules_file.as_deref() else {
        return Some(cfg);
    };

    let path = std::path::Path::new(rules_file);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_path.join(path)
    };

    let mut out = cfg;
    match std::fs::read_to_string(&full) {
        Ok(contents) => {
            out.rules = Some(contents);
            out.rules_file = None;
        }
        Err(e) => {
            if out.rules.is_some() {
                eprintln!(
                    "warning: could not read rules file {}: {e}; using existing inline rules",
                    full.display()
                );
            } else {
                eprintln!(
                    "warning: could not read rules file {}: {e}; no inline rules set",
                    full.display()
                );
                out.rules = Some(default_agent_rules().to_string());
            }
            // Avoid persisting a path that likely won't resolve during restore.
            out.rules_file = None;
        }
    }
    Some(out)
}

/// Ensure `Session::agent_config` is self-contained for restore.
///
/// Older sessions (or sessions created by other tools) may persist `rules_file`
/// instead of inlining the resolved `rules`. On restore we best-effort inline
/// rules using the session's worktree path as the base dir.
pub(crate) fn resolve_agent_config_for_restore(session: &mut Session) {
    let Some(cfg) = session.agent_config.as_ref() else {
        return;
    };
    if cfg.rules_file.is_none() {
        return;
    }
    let Some(ws) = session.workspace_path.as_deref() else {
        return;
    };
    session.agent_config = resolve_agent_config(Some(cfg), ws);
}
