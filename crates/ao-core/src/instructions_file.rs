//! Write agent prompt as an instructions file in the workspace.
//!
//! Agent backends read different files automatically:
//! - claude-code → `CLAUDE.md`
//! - codex, opencode, and others → `AGENTS.md`
//!
//! The prompt is wrapped in HTML comment sentinels so user-authored content
//! in the file is preserved across re-spawns.

use std::path::Path;

const SENTINEL_START: &str = "<!-- AO_PROMPT_START -->";
const SENTINEL_END: &str = "<!-- AO_PROMPT_END -->";

/// Returns the instructions filename the given agent reads automatically.
pub fn instructions_file_name(agent: &str) -> &'static str {
    if agent == "claude-code" {
        "CLAUDE.md"
    } else {
        "AGENTS.md"
    }
}

/// Write `prompt` into the agent-specific instructions file inside `workspace_path`.
///
/// Behaviour:
/// - If the file doesn't exist: created with just the AO sentinel block.
/// - If the file exists but has no prior AO block: prompt is appended after
///   existing content so user-authored sections are preserved.
/// - If a prior AO block (from a previous spawn) already exists: it is
///   replaced in-place so the file stays clean on re-spawns.
pub fn write_instructions_file(
    workspace_path: &Path,
    agent: &str,
    prompt: &str,
) -> std::io::Result<()> {
    let file_path = workspace_path.join(instructions_file_name(agent));
    let ao_block = format!("{SENTINEL_START}\n{prompt}\n{SENTINEL_END}\n");

    let existing = if file_path.exists() {
        std::fs::read_to_string(&file_path)?
    } else {
        String::new()
    };

    let new_content = match (existing.find(SENTINEL_START), existing.find(SENTINEL_END)) {
        (Some(start), Some(end_pos)) if end_pos > start => {
            let after_end = end_pos + SENTINEL_END.len();
            let prefix = existing[..start].trim_end();
            let suffix = existing[after_end..].trim_start();
            match (prefix.is_empty(), suffix.is_empty()) {
                (true, true) => ao_block,
                (true, false) => format!("{ao_block}\n{suffix}"),
                (false, true) => format!("{prefix}\n\n{ao_block}"),
                (false, false) => format!("{prefix}\n\n{ao_block}\n{suffix}"),
            }
        }
        _ if existing.is_empty() => ao_block,
        _ => format!("{}\n\n{ao_block}", existing.trim_end()),
    };

    std::fs::write(&file_path, new_content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ao-instructions-{label}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn claude_code_maps_to_claude_md() {
        assert_eq!(instructions_file_name("claude-code"), "CLAUDE.md");
    }

    #[test]
    fn codex_maps_to_agents_md() {
        assert_eq!(instructions_file_name("codex"), "AGENTS.md");
    }

    #[test]
    fn opencode_maps_to_agents_md() {
        assert_eq!(instructions_file_name("opencode"), "AGENTS.md");
    }

    #[test]
    fn unknown_agent_maps_to_agents_md() {
        assert_eq!(instructions_file_name("aider"), "AGENTS.md");
    }

    #[test]
    fn creates_new_file_with_sentinel_block() {
        let dir = tmpdir("new-file");
        write_instructions_file(&dir, "codex", "do the thing").unwrap();
        let content = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(content.contains(SENTINEL_START));
        assert!(content.contains(SENTINEL_END));
        assert!(content.contains("do the thing"));
    }

    #[test]
    fn appends_to_existing_file_without_sentinel() {
        let dir = tmpdir("append");
        let existing = "# My project rules\nBe careful.";
        fs::write(dir.join("AGENTS.md"), existing).unwrap();
        write_instructions_file(&dir, "codex", "task prompt").unwrap();
        let content = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(content.starts_with("# My project rules"));
        assert!(content.contains(SENTINEL_START));
        assert!(content.contains("task prompt"));
    }

    #[test]
    fn replaces_existing_sentinel_block() {
        let dir = tmpdir("replace");
        let initial = format!("{SENTINEL_START}\nold prompt\n{SENTINEL_END}\n");
        fs::write(dir.join("AGENTS.md"), &initial).unwrap();
        write_instructions_file(&dir, "codex", "new prompt").unwrap();
        let content = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(!content.contains("old prompt"));
        assert!(content.contains("new prompt"));
    }

    #[test]
    fn preserves_content_before_and_after_sentinel() {
        let dir = tmpdir("preserve");
        let initial =
            format!("# Before\n\n{SENTINEL_START}\nold\n{SENTINEL_END}\n\n## After section");
        fs::write(dir.join("AGENTS.md"), &initial).unwrap();
        write_instructions_file(&dir, "codex", "fresh").unwrap();
        let content = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(content.contains("# Before"));
        assert!(content.contains("## After section"));
        assert!(content.contains("fresh"));
        assert!(!content.contains("old"));
    }

    #[test]
    fn claude_code_writes_to_claude_md() {
        let dir = tmpdir("claude-md");
        write_instructions_file(&dir, "claude-code", "claude task").unwrap();
        assert!(dir.join("CLAUDE.md").exists());
        assert!(!dir.join("AGENTS.md").exists());
        let content = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(content.contains("claude task"));
    }
}
