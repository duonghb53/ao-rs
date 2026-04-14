use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn validate_session_id(session_id: &str) -> Result<(), String> {
    let ok = !session_id.is_empty()
        && session_id
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("Invalid session ID: {session_id}"))
    }
}

fn metadata_path(data_dir: &Path, session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(session_id)?;
    Ok(data_dir.join(session_id))
}

pub fn parse_key_value_content(content: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in content.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let key = trimmed[..eq].trim();
        let val = trimmed[eq + 1..].trim();
        if !key.is_empty() {
            out.insert(key.to_string(), val.to_string());
        }
    }
    out
}

fn serialize_metadata(map: &HashMap<String, String>) -> String {
    let mut lines: Vec<String> = map
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| {
            let v = v.replace(['\r', '\n'], " ");
            format!("{k}={v}")
        })
        .collect();
    lines.sort(); // stable content for tests
    lines.join("\n") + "\n"
}

pub fn atomic_write_file(path: &Path, content: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsSessionMetadata {
    pub worktree: String,
    pub branch: String,
    pub status: String,
    pub issue: Option<String>,
    pub pr: Option<String>,
    pub pr_auto_detect: Option<String>,
    pub summary: Option<String>,
    pub project: Option<String>,
    pub created_at: Option<String>,
    pub runtime_handle: Option<String>,
    pub pinned_summary: Option<String>,
}

pub fn write_metadata(
    data_dir: &Path,
    session_id: &str,
    meta: &TsSessionMetadata,
) -> Result<(), String> {
    let path = metadata_path(data_dir, session_id)?;
    let mut data: HashMap<String, String> = HashMap::new();
    data.insert("worktree".into(), meta.worktree.clone());
    data.insert("branch".into(), meta.branch.clone());
    data.insert("status".into(), meta.status.clone());
    if let Some(v) = &meta.issue {
        data.insert("issue".into(), v.clone());
    }
    if let Some(v) = &meta.pr {
        data.insert("pr".into(), v.clone());
    }
    if let Some(v) = &meta.pr_auto_detect {
        data.insert("prAutoDetect".into(), v.clone());
    }
    if let Some(v) = &meta.summary {
        data.insert("summary".into(), v.clone());
    }
    if let Some(v) = &meta.project {
        data.insert("project".into(), v.clone());
    }
    if let Some(v) = &meta.created_at {
        data.insert("createdAt".into(), v.clone());
    }
    if let Some(v) = &meta.runtime_handle {
        data.insert("runtimeHandle".into(), v.clone());
    }
    if let Some(v) = &meta.pinned_summary {
        data.insert("pinnedSummary".into(), v.clone());
    }
    atomic_write_file(&path, &serialize_metadata(&data)).map_err(|e| e.to_string())
}

pub fn read_metadata_raw(
    data_dir: &Path,
    session_id: &str,
) -> Result<Option<HashMap<String, String>>, String> {
    let path = metadata_path(data_dir, session_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(Some(parse_key_value_content(&content)))
}

pub fn read_metadata(
    data_dir: &Path,
    session_id: &str,
) -> Result<Option<TsSessionMetadata>, String> {
    let Some(raw) = read_metadata_raw(data_dir, session_id)? else {
        return Ok(None);
    };
    Ok(Some(TsSessionMetadata {
        worktree: raw.get("worktree").cloned().unwrap_or_default(),
        branch: raw.get("branch").cloned().unwrap_or_default(),
        status: raw
            .get("status")
            .cloned()
            .unwrap_or_else(|| "unknown".into()),
        issue: raw.get("issue").cloned(),
        pr: raw.get("pr").cloned(),
        pr_auto_detect: raw.get("prAutoDetect").cloned(),
        summary: raw.get("summary").cloned(),
        project: raw.get("project").cloned(),
        created_at: raw.get("createdAt").cloned(),
        runtime_handle: raw.get("runtimeHandle").cloned(),
        pinned_summary: raw.get("pinnedSummary").cloned(),
    }))
}

pub fn update_metadata(
    data_dir: &Path,
    session_id: &str,
    updates: &HashMap<String, String>,
) -> Result<(), String> {
    let path = metadata_path(data_dir, session_id)?;
    let mut existing = if path.exists() {
        parse_key_value_content(&fs::read_to_string(&path).map_err(|e| e.to_string())?)
    } else {
        HashMap::new()
    };
    for (k, v) in updates {
        if v.is_empty() {
            existing.remove(k);
        } else {
            existing.insert(k.clone(), v.clone());
        }
    }
    atomic_write_file(&path, &serialize_metadata(&existing)).map_err(|e| e.to_string())
}

pub fn delete_metadata(data_dir: &Path, session_id: &str, archive: bool) -> Result<(), String> {
    let path = metadata_path(data_dir, session_id)?;
    if !path.exists() {
        return Ok(());
    }
    if archive {
        let archive_dir = data_dir.join("archive");
        fs::create_dir_all(&archive_dir).map_err(|e| e.to_string())?;
        let ts = chrono_like_ts();
        let archive_path = archive_dir.join(format!("{session_id}_{ts}"));
        fs::write(
            &archive_path,
            fs::read_to_string(&path).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    fs::remove_file(&path).map_err(|e| e.to_string())
}

fn chrono_like_ts() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ms}")
}

pub fn read_archived_metadata_raw(
    data_dir: &Path,
    session_id: &str,
) -> Result<Option<HashMap<String, String>>, String> {
    validate_session_id(session_id)?;
    let archive_dir = data_dir.join("archive");
    if !archive_dir.exists() {
        return Ok(None);
    }
    let prefix = format!("{session_id}_");
    let mut latest: Option<PathBuf> = None;
    for ent in fs::read_dir(&archive_dir).map_err(|e| e.to_string())? {
        let ent = ent.map_err(|e| e.to_string())?;
        let name = ent.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        let replace = match &latest {
            None => true,
            Some(p) => {
                let latest_name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                name > latest_name
            }
        };
        if replace {
            latest = Some(ent.path());
        }
    }
    let Some(path) = latest else { return Ok(None) };
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(Some(parse_key_value_content(&content)))
}

pub fn list_metadata(data_dir: &Path) -> Result<Vec<String>, String> {
    if !data_dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for ent in fs::read_dir(data_dir).map_err(|e| e.to_string())? {
        let ent = ent.map_err(|e| e.to_string())?;
        if !ent.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if name == "archive" {
            continue;
        }
        if validate_session_id(&name).is_ok() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}
