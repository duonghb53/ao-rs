//! Worktree branch refresh — ports `refreshTrackedBranch` from the TS reference.
//!
//! Before each `detect_pr` call the lifecycle loop reads the worktree's
//! `.git/HEAD` to discover whether the agent has switched branches since
//! the session was spawned. If so, `session.branch` is updated in-place
//! and persisted so the next `detect_pr` looks up the correct PR.
//!
//! Reference: `packages/core/src/lifecycle-manager.ts:688`

use super::*;
use std::path::{Path, PathBuf};

/// Markers indicating a transient detached-HEAD state (rebase / cherry-pick
/// in progress). When any of these exist in the git dir we return
/// `Unavailable` instead of `Detached` so we don't clear the session branch
/// while the agent is mid-operation.
///
/// Mirrors `TRANSIENT_DETACHED_GIT_MARKERS` in the TS reference.
const TRANSIENT_MARKERS: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "CHERRY_PICK_HEAD",
    "BISECT_LOG",
];

/// Result of reading `HEAD` from a worktree.
#[derive(Debug, PartialEq)]
enum WorkspaceBranchProbe {
    /// HEAD points to a named branch.
    Branch(String),
    /// HEAD is detached and no in-progress git operation is underway.
    Detached,
    /// Branch could not be determined (missing worktree, ongoing rebase, I/O error).
    Unavailable,
}

/// Resolve the git object directory for `workspace_path`.
///
/// Clone layout: `.git` is a directory — return it.
/// Worktree layout: `.git` is a file (`gitdir: <path>`) — resolve and return that path.
async fn resolve_git_dir(workspace_path: &Path) -> Option<PathBuf> {
    let dot_git = workspace_path.join(".git");
    let meta = tokio::fs::metadata(&dot_git).await.ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    // `.git` is a file (git worktree pointer).
    let content = tokio::fs::read_to_string(&dot_git).await.ok()?;
    let raw = content.trim().strip_prefix("gitdir:")?.trim();
    let p = Path::new(raw);
    let resolved = if p.is_absolute() {
        p.to_owned()
    } else {
        workspace_path.join(p)
    };
    Some(resolved)
}

/// Return `true` if a transient in-progress git operation is underway in `git_dir`.
async fn has_transient_detached_state(git_dir: &Path) -> bool {
    for marker in TRANSIENT_MARKERS {
        if tokio::fs::metadata(git_dir.join(marker)).await.is_ok() {
            return true;
        }
    }
    false
}

/// Read the current branch from `workspace_path/.git/HEAD` without shelling out.
async fn read_workspace_branch(workspace_path: &Path) -> WorkspaceBranchProbe {
    let git_dir = match resolve_git_dir(workspace_path).await {
        Some(d) => d,
        None => return WorkspaceBranchProbe::Unavailable,
    };

    let head = match tokio::fs::read_to_string(git_dir.join("HEAD")).await {
        Ok(s) => s.trim().to_owned(),
        Err(_) => return WorkspaceBranchProbe::Unavailable,
    };

    const REF_PREFIX: &str = "ref: refs/heads/";
    if let Some(rest) = head.strip_prefix(REF_PREFIX) {
        let branch = rest.trim().to_owned();
        if !branch.is_empty() {
            return WorkspaceBranchProbe::Branch(branch);
        }
    }

    // HEAD is detached or has an empty branch name after the prefix.
    if has_transient_detached_state(&git_dir).await {
        WorkspaceBranchProbe::Unavailable
    } else {
        WorkspaceBranchProbe::Detached
    }
}

impl LifecycleManager {
    /// Refresh `session.branch` from the worktree's `.git/HEAD` before SCM polling.
    ///
    /// Ports `refreshTrackedBranch` from `lifecycle-manager.ts:688`.
    ///
    /// * `has_open_pr` — pass `true` when the session has a live open PR in the
    ///   current tick (the branch is locked to that PR; skip refresh to avoid
    ///   orphaning the PR lookup).
    /// * `tick_sessions` — full session list for the current tick; used to detect
    ///   branch ownership conflicts across sibling sessions.
    ///
    /// Returns `true` if HEAD is detached and the caller should skip `detect_pr`
    /// for this tick (there is no meaningful branch to look up).
    pub(super) async fn refresh_tracked_branch(
        &self,
        session: &mut Session,
        has_open_pr: bool,
        tick_sessions: &[Session],
    ) -> bool {
        let Some(ref workspace_path) = session.workspace_path else {
            return false;
        };
        // Branch is locked when a live PR exists — skip to avoid orphaning the PR lookup.
        if has_open_pr {
            return false;
        }

        let workspace_path = workspace_path.clone();
        match read_workspace_branch(&workspace_path).await {
            WorkspaceBranchProbe::Unavailable => false,

            WorkspaceBranchProbe::Detached => {
                if !session.branch.is_empty() {
                    tracing::info!(
                        session = %session.id,
                        old_branch = %session.branch,
                        "worktree HEAD detached — clearing session.branch"
                    );
                    session.branch = String::new();
                    if let Err(e) = self.sessions.save(session).await {
                        tracing::warn!(
                            session = %session.id,
                            error = %e,
                            "refresh_tracked_branch: save failed after clearing detached branch"
                        );
                    }
                }
                true
            }

            WorkspaceBranchProbe::Branch(new_branch) => {
                if new_branch == session.branch {
                    return false;
                }

                let reservation_key = format!("{}:{}", session.project_id, new_branch);

                // Acquire per-tick adoption reservation so two concurrent sessions
                // in the same tick can't both adopt the same branch name.
                let acquired = {
                    let mut map = self
                        .branch_adoption_reservations
                        .lock()
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "branch_adoption_reservations poisoned; recovering: {e}"
                            );
                            e.into_inner()
                        });
                    match map.get(&reservation_key).cloned() {
                        None => {
                            map.insert(reservation_key.clone(), session.id.clone());
                            true
                        }
                        Some(ref owner) if *owner == session.id => true,
                        _ => false,
                    }
                };

                if !acquired {
                    return false;
                }

                // Check that no other active session in the same project already
                // claims this branch name.
                let owned_by_other = tick_sessions.iter().any(|other| {
                    other.id != session.id
                        && other.project_id == session.project_id
                        && !other.is_terminal()
                        && other.branch == new_branch
                });

                if !owned_by_other {
                    tracing::info!(
                        session = %session.id,
                        old_branch = %session.branch,
                        new_branch = %new_branch,
                        "worktree branch changed — updating session.branch"
                    );
                    session.branch = new_branch;
                    if let Err(e) = self.sessions.save(session).await {
                        tracing::warn!(
                            session = %session.id,
                            error = %e,
                            "refresh_tracked_branch: save failed after branch update"
                        );
                    }
                }

                // Release reservation regardless of whether we adopted the branch.
                {
                    let mut map = self
                        .branch_adoption_reservations
                        .lock()
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "branch_adoption_reservations poisoned; recovering: {e}"
                            );
                            e.into_inner()
                        });
                    if map
                        .get(&reservation_key)
                        .is_some_and(|id| id == &session.id)
                    {
                        map.remove(&reservation_key);
                    }
                }

                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::tests::{fake_session, setup, unique_temp_dir};
    use crate::types::ActivityState;

    fn create_git_dir_repo(dir: &Path, branch: Option<&str>) {
        let git_dir = dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let head = match branch {
            Some(b) => format!("ref: refs/heads/{b}\n"),
            None => "abc1234abc1234abc1234abc1234abc1234abc1234\n".to_owned(),
        };
        std::fs::write(git_dir.join("HEAD"), head).unwrap();
    }

    fn create_worktree_style(workspace: &Path, git_dir: &Path, branch: &str) {
        std::fs::create_dir_all(git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn branch_changed_updates_session_and_persists() {
        let ws = unique_temp_dir("refresh-ws-changed");
        std::fs::create_dir_all(&ws).unwrap();
        create_git_dir_repo(&ws, Some("new-branch"));

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-changed", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "old-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "new-branch");
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].branch, "new-branch");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn branch_unchanged_is_noop() {
        let ws = unique_temp_dir("refresh-ws-noop");
        std::fs::create_dir_all(&ws).unwrap();
        create_git_dir_repo(&ws, Some("same-branch"));

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-noop", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "same-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "same-branch");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn detached_head_clears_branch_and_signals_skip() {
        let ws = unique_temp_dir("refresh-ws-detached");
        std::fs::create_dir_all(&ws).unwrap();
        create_git_dir_repo(&ws, None); // bare SHA → detached

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-detached", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "some-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(detached);
        assert_eq!(session.branch, "");
        let persisted = sessions.list().await.unwrap();
        assert_eq!(persisted[0].branch, "");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn missing_worktree_is_noop() {
        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-missing", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(PathBuf::from("/nonexistent/worktree/does-not-exist"));
        session.branch = "my-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "my-branch");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn open_pr_guard_skips_refresh() {
        let ws = unique_temp_dir("refresh-ws-pr-guard");
        std::fs::create_dir_all(&ws).unwrap();
        create_git_dir_repo(&ws, Some("new-branch"));

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-pr-guard", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "old-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        // has_open_pr = true → skip
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, true, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "old-branch");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn transient_detached_state_returns_unavailable() {
        let ws = unique_temp_dir("refresh-ws-transient");
        std::fs::create_dir_all(&ws).unwrap();
        let git_dir = ws.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        // Detached HEAD + rebase-merge marker → Unavailable
        std::fs::write(
            git_dir.join("HEAD"),
            "abc1234abc1234abc1234abc1234abc1234abc1234\n",
        )
        .unwrap();
        std::fs::create_dir_all(git_dir.join("rebase-merge")).unwrap();

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-transient", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "my-branch".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "my-branch"); // unchanged — transient state

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn worktree_style_dot_git_file_works() {
        let ws = unique_temp_dir("refresh-ws-wt");
        let git_d = unique_temp_dir("refresh-git-wt");
        std::fs::create_dir_all(&ws).unwrap();
        create_worktree_style(&ws, &git_d, "feature-branch");

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-worktree-style", ActivityState::Ready).await;
        let mut session = fake_session("s1", "demo");
        session.workspace_path = Some(ws.clone());
        session.branch = "old".to_owned();
        sessions.save(&session).await.unwrap();

        let snap = vec![session.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session.branch, "feature-branch");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&git_d);
    }

    #[tokio::test]
    async fn sibling_owns_branch_blocks_adoption() {
        let ws1 = unique_temp_dir("refresh-ws-sib1");
        std::fs::create_dir_all(&ws1).unwrap();
        create_git_dir_repo(&ws1, Some("contested-branch"));

        let (lifecycle, sessions, _rt, _agent, base) =
            setup("refresh-sibling", ActivityState::Ready).await;

        let mut session1 = fake_session("s1", "demo");
        session1.workspace_path = Some(ws1.clone());
        session1.branch = "old-branch".to_owned();
        sessions.save(&session1).await.unwrap();

        // Sibling already owns the target branch.
        let mut sibling = fake_session("s2", "demo");
        sibling.branch = "contested-branch".to_owned();
        sessions.save(&sibling).await.unwrap();

        let snap = vec![session1.clone(), sibling.clone()];
        let detached = lifecycle
            .refresh_tracked_branch(&mut session1, false, &snap)
            .await;

        assert!(!detached);
        assert_eq!(session1.branch, "old-branch"); // blocked by sibling

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&ws1);
    }
}
