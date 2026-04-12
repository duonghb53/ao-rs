//! Disk-backed session store.
//!
//! Each session is one yaml file at:
//!     `<base>/<project_id>/<session_uuid>.yaml`
//!
//! Writes are atomic (write to `.tmp`, then rename). Reads scan all project
//! subdirectories — fine for Slice 1 since N is small (tens of sessions).
//!
//! There's intentionally **no in-memory cache**. The disk is the source of
//! truth, and Slice 1's `ao-rs status` is happy to do a full directory walk
//! per invocation. Slice 2+ may add caching for the daemon polling loop.

use crate::{
    error::{AoError, Result},
    paths,
    types::{Session, SessionId},
};
use std::path::{Path, PathBuf};
use tokio::fs;

pub struct SessionManager {
    base_dir: PathBuf,
}

impl SessionManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Use the default `~/.ao-rs/sessions` location.
    pub fn with_default() -> Self {
        Self::new(paths::default_sessions_dir())
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn project_dir(&self, project_id: &str) -> PathBuf {
        self.base_dir.join(project_id)
    }

    fn session_path(&self, project_id: &str, id: &SessionId) -> PathBuf {
        self.project_dir(project_id).join(format!("{}.yaml", id.0))
    }

    /// Atomically persist a session. Creates parent dirs as needed.
    pub async fn save(&self, session: &Session) -> Result<()> {
        let project_dir = self.project_dir(&session.project_id);
        fs::create_dir_all(&project_dir).await?;

        let target = self.session_path(&session.project_id, &session.id);
        // Write to a sibling temp file first, then rename — rename is atomic
        // on the same filesystem so a reader never sees a half-written yaml.
        let temp = target.with_extension("yaml.tmp");

        let yaml =
            serde_yaml::to_string(session).map_err(|e| AoError::Yaml(format!("serialize: {e}")))?;

        fs::write(&temp, yaml).await?;
        fs::rename(&temp, &target).await?;
        Ok(())
    }

    /// Read every session across all projects, sorted newest-first.
    pub async fn list(&self) -> Result<Vec<Session>> {
        let mut result = Vec::new();
        if !self.base_dir.exists() {
            return Ok(result);
        }

        let mut projects = fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = projects.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let mut sessions = fs::read_dir(entry.path()).await?;
            while let Some(file) = sessions.next_entry().await? {
                let path = file.path();
                if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                    continue;
                }
                match load_file(&path).await {
                    Ok(session) => result.push(session),
                    Err(e) => {
                        // Skip unreadable files instead of failing the whole list.
                        // A half-written tmp file (extremely rare given atomic writes)
                        // shouldn't break `ao-rs status`.
                        tracing::warn!("skipping unreadable session {path:?}: {e}");
                    }
                }
            }
        }
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(result)
    }

    /// Same as `list` but filtered to one project.
    pub async fn list_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let project_dir = self.project_dir(project_id);
        if !project_dir.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        let mut sessions = fs::read_dir(&project_dir).await?;
        while let Some(file) = sessions.next_entry().await? {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            match load_file(&path).await {
                Ok(session) => result.push(session),
                Err(e) => tracing::warn!("skipping {path:?}: {e}"),
            }
        }
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(result)
    }

    /// Find a session by full uuid or any unambiguous prefix.
    ///
    /// `starts_with` semantics — the 8-char short id used by the CLI
    /// (`ao-rs status`, `ao-rs send <short>`) is a valid lookup key, as is
    /// the full uuid. Returns `SessionNotFound` on no match and
    /// `AoError::Runtime` on more than one. Shared by `restore_session`,
    /// `ao-rs send`, `ao-rs pr`, so the CLI's "resolve a session" idiom
    /// lives in one place.
    pub async fn find_by_prefix(&self, id_or_prefix: &str) -> Result<Session> {
        if id_or_prefix.is_empty() {
            return Err(AoError::SessionNotFound(String::new()));
        }
        let all = self.list().await?;
        let mut matches = all.into_iter().filter(|s| s.id.0.starts_with(id_or_prefix));
        let first = matches
            .next()
            .ok_or_else(|| AoError::SessionNotFound(id_or_prefix.to_string()))?;
        if matches.next().is_some() {
            // We've consumed two (`first` + the one that made this branch
            // fire); anything still in the iterator is `extra`. Avoids
            // collecting into a Vec in the common (unique-match) path.
            let extra = matches.count();
            return Err(AoError::Runtime(format!(
                "ambiguous session id \"{id_or_prefix}\": {} matches",
                2 + extra
            )));
        }
        Ok(first)
    }

    /// Remove a session's yaml file. No-op if it doesn't exist.
    pub async fn delete(&self, project_id: &str, id: &SessionId) -> Result<()> {
        let path = self.session_path(project_id, id);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

async fn load_file(path: &Path) -> Result<Session> {
    let bytes = fs::read(path).await?;
    serde_yaml::from_slice::<Session>(&bytes)
        .map_err(|e| AoError::Yaml(format!("parse {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{now_ms, SessionStatus};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("ao-rs-sm-{label}-{nanos}"))
    }

    fn fake_session(id: &str, project: &str, task: &str) -> Session {
        Session {
            id: SessionId(id.into()),
            project_id: project.into(),
            status: SessionStatus::Spawning,
            branch: format!("ao-{id}"),
            task: task.into(),
            workspace_path: None,
            runtime_handle: None,
            activity: None,
            created_at: now_ms(),
            cost: None,
        }
    }

    #[tokio::test]
    async fn save_and_list_roundtrip() {
        let base = unique_temp_dir("roundtrip");
        let manager = SessionManager::new(base.clone());

        let s1 = fake_session("uuid-1", "demo", "first task");
        let s2 = fake_session("uuid-2", "demo", "second task");
        let s3 = fake_session("uuid-3", "other", "third task");

        manager.save(&s1).await.unwrap();
        manager.save(&s2).await.unwrap();
        manager.save(&s3).await.unwrap();

        let all = manager.list().await.unwrap();
        assert_eq!(all.len(), 3);

        let demo_only = manager.list_for_project("demo").await.unwrap();
        assert_eq!(demo_only.len(), 2);
        assert!(demo_only.iter().all(|s| s.project_id == "demo"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn list_returns_empty_when_dir_missing() {
        let manager = SessionManager::new(unique_temp_dir("missing"));
        assert!(manager.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let base = unique_temp_dir("delete");
        let manager = SessionManager::new(base.clone());
        let s = fake_session("uuid-x", "demo", "doomed");
        manager.save(&s).await.unwrap();
        assert_eq!(manager.list().await.unwrap().len(), 1);

        manager.delete("demo", &s.id).await.unwrap();
        assert_eq!(manager.list().await.unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn find_by_prefix_resolves_unique_short_id() {
        let base = unique_temp_dir("find-unique");
        let manager = SessionManager::new(base.clone());
        manager
            .save(&fake_session("deadbeef-aaaa-bbbb", "demo", "only one"))
            .await
            .unwrap();

        let hit = manager.find_by_prefix("deadbeef").await.unwrap();
        assert_eq!(hit.id.0, "deadbeef-aaaa-bbbb");

        // Full uuid also works via starts_with.
        let hit_full = manager.find_by_prefix("deadbeef-aaaa-bbbb").await.unwrap();
        assert_eq!(hit_full.id.0, "deadbeef-aaaa-bbbb");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn find_by_prefix_unknown_returns_session_not_found() {
        let base = unique_temp_dir("find-missing");
        let manager = SessionManager::new(base.clone());
        let err = manager.find_by_prefix("no-such-session").await.unwrap_err();
        assert!(
            matches!(err, AoError::SessionNotFound(ref s) if s == "no-such-session"),
            "unexpected error: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn find_by_prefix_empty_string_is_session_not_found() {
        // Empty prefix would otherwise match every session via `starts_with`,
        // so the CLI would surface the *ambiguous* branch and the message
        // would talk about N matches instead of "did you forget the id?".
        // Short-circuit explicitly.
        let base = unique_temp_dir("find-empty");
        let manager = SessionManager::new(base.clone());
        manager
            .save(&fake_session("anything", "demo", "task"))
            .await
            .unwrap();
        let err = manager.find_by_prefix("").await.unwrap_err();
        assert!(matches!(err, AoError::SessionNotFound(_)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn find_by_prefix_ambiguous_lists_match_count() {
        let base = unique_temp_dir("find-ambig");
        let manager = SessionManager::new(base.clone());
        manager
            .save(&fake_session("abc-111", "demo", "one"))
            .await
            .unwrap();
        manager
            .save(&fake_session("abc-222", "demo", "two"))
            .await
            .unwrap();
        manager
            .save(&fake_session("abc-333", "demo", "three"))
            .await
            .unwrap();

        let err = manager.find_by_prefix("abc").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ambiguous"), "got: {msg}");
        assert!(msg.contains("3 matches"), "got: {msg}");
    }

    #[tokio::test]
    async fn list_sorts_newest_first() {
        let base = unique_temp_dir("sort");
        let manager = SessionManager::new(base.clone());

        let mut a = fake_session("a", "demo", "older");
        a.created_at = 1000;
        let mut b = fake_session("b", "demo", "newest");
        b.created_at = 3000;
        let mut c = fake_session("c", "demo", "middle");
        c.created_at = 2000;

        manager.save(&a).await.unwrap();
        manager.save(&b).await.unwrap();
        manager.save(&c).await.unwrap();

        let all = manager.list().await.unwrap();
        assert_eq!(all[0].id.0, "b");
        assert_eq!(all[1].id.0, "c");
        assert_eq!(all[2].id.0, "a");

        let _ = std::fs::remove_dir_all(&base);
    }
}
