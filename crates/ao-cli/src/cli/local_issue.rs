//! Local markdown issues under `docs/issues/`.

use std::path::PathBuf;

pub(crate) fn issues_dir(repo: Option<PathBuf>) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let repo_path = repo.unwrap_or(std::env::current_dir()?);
    Ok(repo_path.join("docs").join("issues"))
}

pub(crate) fn issue_list(repo: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let issues_dir = issues_dir(repo)?;
    if !issues_dir.exists() {
        println!("(no docs/issues/ — run `ao-rs issue new --title \"…\"`)");
        return Ok(());
    }
    let entries = collect_local_issue_entries(&issues_dir)?;
    if entries.is_empty() {
        println!("(no NNNN-*.md files in {})", issues_dir.display());
        return Ok(());
    }
    for (n, path) in entries {
        let title = read_local_issue_title(&path);
        println!("{n:04}  {title}  {}", path.display());
    }
    Ok(())
}

pub(crate) fn issue_show(target: String, repo: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let repo_path = repo.unwrap_or(std::env::current_dir()?);
    let path = resolve_local_issue_for_show(&repo_path, target.trim())
        .map_err(|s| std::io::Error::new(std::io::ErrorKind::InvalidInput, s))?;
    let text = std::fs::read_to_string(&path)?;
    print!("{text}");
    Ok(())
}

/// If `target` is 1–4 decimal digits, match `docs/issues/NNNN-*.md` under `repo_root`.
/// Otherwise treat `target` as a path (relative to `repo_root` when not absolute).
pub(crate) fn resolve_local_issue_for_show(
    repo_root: &std::path::Path,
    target: &str,
) -> Result<PathBuf, String> {
    if let Some(id) = parse_local_issue_id_token(target) {
        let issues_dir = repo_root.join("docs").join("issues");
        if !issues_dir.is_dir() {
            return Err(format!(
                "no directory {} — create issues first (`ao-rs issue new`)",
                issues_dir.display()
            ));
        }
        let prefix = format!("{id:04}-");
        let mut matches: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&issues_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".md") {
                continue;
            }
            if name.starts_with(&prefix) {
                matches.push(entry.path());
            }
        }
        matches.sort();
        match matches.len() {
            0 => Err(format!(
                "no file matching {prefix}*.md in {}",
                issues_dir.display()
            )),
            1 => Ok(matches.into_iter().next().expect("one match")),
            _ => Err(format!(
                "ambiguous id {id:04}: multiple files in {} — use a full path",
                issues_dir.display()
            )),
        }
    } else {
        let p = resolve_path_in_repo(repo_root, std::path::Path::new(target));
        if !p.is_file() {
            return Err(format!("not a file: {}", p.display()));
        }
        Ok(p)
    }
}

/// Accepts `1` … `9999` (and zero-padding). Longer all-digit strings are treated as paths by callers.
pub(crate) fn parse_local_issue_id_token(target: &str) -> Option<u32> {
    if target.is_empty() || target.len() > 4 || !target.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    target.parse().ok()
}

pub(crate) async fn issue_new(
    title: String,
    body: Option<String>,
    repo: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let issues_dir = issues_dir(repo)?;
    std::fs::create_dir_all(&issues_dir)?;

    let n = next_local_issue_number(&issues_dir)?;
    let slug = slugify_filename(&title);
    let filename = format!("{n:04}-{slug}.md");
    let path = issues_dir.join(filename);

    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    if let Some(b) = body {
        let b = b.trim();
        if !b.is_empty() {
            out.push_str(b);
            out.push('\n');
            out.push('\n');
        }
    }
    out.push_str("## Notes\n\n- \n");

    std::fs::write(&path, out)?;
    println!("{}", path.display());
    Ok(())
}

pub(crate) fn local_issue_id_from_filename(name: &str) -> Option<u32> {
    if !name.ends_with(".md") {
        return None;
    }
    let base = name.strip_suffix(".md")?;
    let (prefix, _rest) = base.split_once('-')?;
    if prefix.len() != 4 || !prefix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    prefix.parse().ok()
}

pub(crate) fn collect_local_issue_entries(
    dir: &std::path::Path,
) -> std::io::Result<Vec<(u32, std::path::PathBuf)>> {
    let mut v = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(n) = local_issue_id_from_filename(name) else {
            continue;
        };
        v.push((n, entry.path()));
    }
    v.sort_by_key(|(n, _)| *n);
    Ok(v)
}

pub(crate) fn read_local_issue_title(path: &std::path::Path) -> String {
    let Ok(s) = std::fs::read_to_string(path) else {
        return "?".into();
    };
    for line in s.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('#') {
            let t = rest.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn next_local_issue_number(dir: &std::path::Path) -> std::io::Result<u32> {
    let mut max_n: u32 = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(n) = local_issue_id_from_filename(name) else {
            continue;
        };
        max_n = max_n.max(n);
    }
    Ok(max_n.saturating_add(1).max(1))
}

pub(crate) fn slugify_filename(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
            continue;
        }
        if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "issue".into()
    } else {
        out
    }
}

pub(crate) fn resolve_path_in_repo(repo_path: &std::path::Path, p: &std::path::Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_path.join(p)
    }
}

/// Returns (`local-0001`, `feat/local-0001-slug`) for `0001-slug.md`.
pub(crate) fn local_issue_ids_from_path(path: &std::path::Path) -> Result<(String, String), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "local issue path has no file name".to_string())?;
    let base = name
        .strip_suffix(".md")
        .ok_or_else(|| "local issue file must end with .md".to_string())?;
    let (prefix, rest) = base
        .split_once('-')
        .ok_or_else(|| "expected filename NNNN-slug.md".to_string())?;
    if prefix.len() != 4 || !prefix.chars().all(|c| c.is_ascii_digit()) {
        return Err("expected 4-digit id prefix in filename (e.g. 0001-slug.md)".into());
    }
    if rest.is_empty() {
        return Err("expected slug after id in filename".into());
    }
    Ok((
        format!("local-{prefix}"),
        format!("feat/local-{prefix}-{rest}"),
    ))
}

pub(crate) fn parse_local_issue_markdown(text: &str) -> (String, String) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let title = if i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix('#') {
            let t = rest.trim().trim_start_matches('#').trim();
            if t.is_empty() {
                "Local issue".to_string()
            } else {
                t.to_string()
            }
        } else {
            line.to_string()
        }
    } else {
        "Local issue".to_string()
    };
    i += 1;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let body = lines[i..].join("\n");
    (title, body)
}

pub(crate) fn format_local_issue_context(title: &str, path: &std::path::Path, body: &str) -> String {
    let mut s = format!("## Local issue: {title}\n\n");
    s.push_str(&format!("File: `{}`\n\n", path.display()));
    let b = body.trim();
    if !b.is_empty() {
        s.push_str(b);
        s.push('\n');
    }
    s
}
