//! Disk-backed session store with in-memory TTL cache.
//!
//! Each session is one yaml file at:
//!     `<base>/<project_id>/<session_uuid>.yaml`
//!
//! Writes are atomic (write to `.tmp`, then rename). Reads scan all project
//! subdirectories and are cached with a 2-second TTL. Concurrent callers
//! share a single in-flight disk read via the cache mutex — no thundering
//! herd on the hot polling loop.

use crate::{
    error::{AoError, Result},
    paths,
    types::{Session, SessionId},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{fs, sync::Mutex};

const CACHE_TTL: Duration = Duration::from_secs(2);

pub struct SessionManager {
    base_dir: PathBuf,
    /// Guards both the cached value and acts as a coalescing lock: concurrent
    /// callers that arrive on a cold/expired cache block here while the first
    /// caller does the disk scan. When the first caller releases the lock, all
    /// waiters find a warm cache and return immediately.
    cache: Mutex<Option<(Instant, Arc<Vec<Session>>)>>,
}

impl SessionManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            cache: Mutex::new(None),
        }
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
    /// Invalidates the list cache so the next `list()` sees the new state.
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
        self.invalidate_cache().await;
        Ok(())
    }

    /// Read every session across all projects, sorted newest-first.
    ///
    /// Results are cached for [`CACHE_TTL`]. Concurrent callers on a cold
    /// cache coalesce: only the first does disk I/O; the rest wait and then
    /// read the warm cache. Use [`list_uncached`] when you need a guaranteed
    /// fresh read.
    ///
    /// `.archive/` subdirectories inside each project dir are safe because
    /// the inner `read_dir` is non-recursive — only direct children of the
    /// project directory are inspected, and `.archive` (a directory) is
    /// skipped by the `.yaml` extension filter.
    pub async fn list(&self) -> Result<Vec<Session>> {
        let mut guard = self.cache.lock().await;
        if let Some((ts, sessions)) = guard.as_ref() {
            if ts.elapsed() < CACHE_TTL {
                return Ok((**sessions).clone());
            }
        }
        let sessions = Arc::new(self.list_from_disk().await?);
        *guard = Some((Instant::now(), sessions.clone()));
        Ok((*sessions).clone())
    }

    /// Like [`list`] but always reads from disk and refreshes the cache.
    pub async fn list_uncached(&self) -> Result<Vec<Session>> {
        let sessions = Arc::new(self.list_from_disk().await?);
        *self.cache.lock().await = Some((Instant::now(), sessions.clone()));
        Ok((*sessions).clone())
    }

    /// Same as `list` but filtered to one project. Uses the shared cache.
    pub async fn list_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let all = self.list().await?;
        Ok(all
            .into_iter()
            .filter(|s| s.project_id == project_id)
            .collect())
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

    /// Find all non-terminal sessions with a matching `issue_id`.
    ///
    /// Used for duplicate detection before `ao-rs spawn --issue` — if another
    /// active session is already working on the same issue, the user should
    /// either wait or use `--force`.
    pub async fn find_by_issue_id(&self, issue_id: &str) -> Result<Vec<Session>> {
        let all = self.list().await?;
        Ok(all
            .into_iter()
            .filter(|s| !s.is_terminal() && s.issue_id.as_deref() == Some(issue_id))
            .collect())
    }

    /// Remove a session's yaml file. No-op if it doesn't exist.
    /// Invalidates the list cache.
    pub async fn delete(&self, project_id: &str, id: &SessionId) -> Result<()> {
        let path = self.session_path(project_id, id);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        self.invalidate_cache().await;
        Ok(())
    }

    /// Archive a session: move its YAML from the active directory into
    /// `sessions/<project>/.archive/<uuid>.yaml`. Archiving removes the
    /// session from `list()` results while preserving it on disk for
    /// historical reference. No-op if the source file doesn't exist
    /// (already archived or never persisted).
    /// Invalidates the list cache.
    pub async fn archive(&self, session: &Session) -> Result<()> {
        let source = self.session_path(&session.project_id, &session.id);
        let archive_dir = self.project_dir(&session.project_id).join(".archive");
        fs::create_dir_all(&archive_dir).await?;
        let target = archive_dir.join(format!("{}.yaml", session.id.0));
        // Attempt the rename directly — treat NotFound as success (already
        // archived or never persisted) to avoid a TOCTOU race with concurrent
        // callers.
        let result = match fs::rename(&source, &target).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        };
        self.invalidate_cache().await;
        result
    }

    /// List archived sessions for a project, sorted newest-first.
    pub async fn list_archived(&self, project_id: &str) -> Result<Vec<Session>> {
        let archive_dir = self.project_dir(project_id).join(".archive");
        if !archive_dir.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        let mut entries = fs::read_dir(&archive_dir).await?;
        while let Some(file) = entries.next_entry().await? {
            let path = file.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            match load_file(&path).await {
                Ok(session) => result.push(session),
                Err(e) => tracing::warn!("skipping archived {path:?}: {e}"),
            }
        }
        result.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(result)
    }

    async fn invalidate_cache(&self) {
        *self.cache.lock().await = None;
    }

    async fn list_from_disk(&self) -> Result<Vec<Session>> {
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
        result.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(result)
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
            agent: "claude-code".into(),
            agent_config: None,
            branch: format!("ao-{id}"),
            task: task.into(),
            workspace_path: None,
            runtime_handle: None,
            runtime: "tmux".into(),
            activity: None,
            created_at: now_ms(),
            cost: None,
            issue_id: None,
            issue_url: None,
            claimed_pr_number: None,
            claimed_pr_url: None,
            initial_prompt_override: None,
            spawned_by: None,
            last_merge_conflict_dispatched: None,
            last_review_backlog_fingerprint: None,
            last_automated_review_fingerprint: None,
            last_automated_review_dispatch_hash: None,
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
    async fn find_by_issue_id_returns_active_matches_only() {
        let base = unique_temp_dir("find-issue");
        let manager = SessionManager::new(base.clone());

        // Active session on issue 42.
        let mut active = fake_session("uuid-active", "demo", "fix it");
        active.issue_id = Some("42".into());
        active.status = SessionStatus::Working;
        manager.save(&active).await.unwrap();

        // Terminal session on same issue (should not match).
        let mut killed = fake_session("uuid-killed", "demo", "old attempt");
        killed.issue_id = Some("42".into());
        killed.status = SessionStatus::Killed;
        manager.save(&killed).await.unwrap();

        // Active session on different issue (should not match).
        let mut other = fake_session("uuid-other", "demo", "other thing");
        other.issue_id = Some("99".into());
        other.status = SessionStatus::Working;
        manager.save(&other).await.unwrap();

        let matches = manager.find_by_issue_id("42").await.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id.0, "uuid-active");

        // No match for unknown issue.
        let empty = manager.find_by_issue_id("999").await.unwrap();
        assert!(empty.is_empty());

        let _ = std::fs::remove_dir_all(&base);
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
    async fn archive_moves_yaml_to_dot_archive_dir() {
        let base = unique_temp_dir("archive");
        let manager = SessionManager::new(base.clone());
        let s = fake_session("uuid-arc", "demo", "archivable");
        manager.save(&s).await.unwrap();
        assert_eq!(manager.list().await.unwrap().len(), 1);

        manager.archive(&s).await.unwrap();

        // No longer in active list.
        assert_eq!(manager.list().await.unwrap().len(), 0);
        // Present in archived list.
        let archived = manager.list_archived("demo").await.unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id.0, "uuid-arc");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn archive_is_noop_when_source_missing() {
        let base = unique_temp_dir("archive-noop");
        let manager = SessionManager::new(base.clone());
        let s = fake_session("uuid-gone", "demo", "already gone");
        // Don't save — source doesn't exist on disk.
        manager.archive(&s).await.unwrap(); // should not error
        let archived = manager.list_archived("demo").await.unwrap();
        assert!(archived.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn list_archived_returns_empty_when_no_archive() {
        let base = unique_temp_dir("archive-empty");
        let manager = SessionManager::new(base.clone());
        let archived = manager.list_archived("nonexistent").await.unwrap();
        assert!(archived.is_empty());
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

    #[tokio::test]
    async fn list_skips_corrupted_yaml_among_many() {
        let base = unique_temp_dir("corrupt");
        let manager = SessionManager::new(base.clone());

        // One valid session.
        let ok = fake_session("uuid-ok", "demo", "good");
        manager.save(&ok).await.unwrap();

        // One corrupted YAML file in the same project dir.
        let project_dir = base.join("demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        let bad_path = project_dir.join("uuid-bad.yaml");
        std::fs::write(&bad_path, "this: is: not: valid: yaml: [").unwrap();

        let all = manager.list().await.unwrap();
        assert_eq!(all.len(), 1, "expected only the valid session to load");
        assert_eq!(all[0].id.0, "uuid-ok");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Cache serves stale data within TTL when files change behind the manager.
    #[tokio::test]
    async fn cache_serves_stale_within_ttl() {
        let base = unique_temp_dir("cache-stale");
        let manager = SessionManager::new(base.clone());

        let s = fake_session("uuid-a", "demo", "task");
        manager.save(&s).await.unwrap();
        // Warm the cache.
        assert_eq!(manager.list().await.unwrap().len(), 1);

        // Write a second session directly to disk, bypassing the manager so
        // the cache is not invalidated.
        let s2 = fake_session("uuid-b", "demo", "task2");
        let project_dir = base.join("demo");
        let yaml = serde_yaml::to_string(&s2).unwrap();
        std::fs::write(project_dir.join("uuid-b.yaml"), &yaml).unwrap();

        // list() should still return 1 (cache hit within TTL).
        assert_eq!(manager.list().await.unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// list_uncached bypasses the TTL and returns fresh disk state.
    #[tokio::test]
    async fn list_uncached_bypasses_cache() {
        let base = unique_temp_dir("cache-bypass");
        let manager = SessionManager::new(base.clone());

        let s = fake_session("uuid-a", "demo", "task");
        manager.save(&s).await.unwrap();
        // Warm the cache.
        assert_eq!(manager.list().await.unwrap().len(), 1);

        // Write directly to disk without invalidating cache.
        let s2 = fake_session("uuid-b", "demo", "task2");
        let project_dir = base.join("demo");
        let yaml = serde_yaml::to_string(&s2).unwrap();
        std::fs::write(project_dir.join("uuid-b.yaml"), &yaml).unwrap();

        // list_uncached sees the new file.
        assert_eq!(manager.list_uncached().await.unwrap().len(), 2);
        // Cache now refreshed; subsequent list() also returns 2.
        assert_eq!(manager.list().await.unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Mutations (save/delete/archive) must invalidate so the next list() is fresh.
    #[tokio::test]
    async fn mutation_invalidates_cache() {
        let base = unique_temp_dir("cache-invalidate");
        let manager = SessionManager::new(base.clone());

        let s = fake_session("uuid-a", "demo", "task");
        manager.save(&s).await.unwrap();
        // Warm the cache with 1 session.
        assert_eq!(manager.list().await.unwrap().len(), 1);

        // delete goes through the manager → invalidates cache.
        manager.delete("demo", &s.id).await.unwrap();
        // list() must reflect the deletion immediately (cache was invalidated).
        assert_eq!(manager.list().await.unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// save invalidates so a second save on the same id returns updated state.
    #[tokio::test]
    async fn save_invalidates_cache() {
        let base = unique_temp_dir("cache-save-invalidate");
        let manager = SessionManager::new(base.clone());

        let mut s = fake_session("uuid-a", "demo", "original");
        manager.save(&s).await.unwrap();
        let cached = manager.list().await.unwrap();
        assert_eq!(cached[0].task, "original");

        // Mutate and re-save — cache must be invalidated.
        s.task = "updated".into();
        manager.save(&s).await.unwrap();
        let fresh = manager.list().await.unwrap();
        assert_eq!(fresh[0].task, "updated");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Concurrent list() callers on a cold cache all receive consistent results.
    #[tokio::test]
    async fn concurrent_list_callers_return_consistent_results() {
        let base = unique_temp_dir("cache-concurrent");
        let manager = std::sync::Arc::new(SessionManager::new(base.clone()));

        // Pre-populate 5 sessions. Each save invalidates the cache, so after
        // the last save the cache is cold — all subsequent concurrent callers
        // hit the lock simultaneously.
        for i in 0..5 {
            manager
                .save(&fake_session(&format!("uuid-{i}"), "demo", "task"))
                .await
                .unwrap();
        }

        // Spawn 8 concurrent list() calls.
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let m = manager.clone();
            set.spawn(async move { m.list().await.unwrap().len() });
        }

        let mut counts = Vec::new();
        while let Some(res) = set.join_next().await {
            counts.push(res.unwrap());
        }

        // Every caller must see exactly 5 sessions.
        assert!(counts.iter().all(|&c| c == 5), "inconsistent: {counts:?}");

        let _ = std::fs::remove_dir_all(&base);
    }
}
