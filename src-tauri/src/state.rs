//! Runtime state: the reconciled queue of PRs and their build status.
//!
//! Persisted as `state.json` so the queue survives restarts. Ported from the
//! `state.json` schema of the original SwiftBar plugin, keyed by
//! `<repo-id>-<number>` so multiple repos can coexist.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::paths;

/// Lifecycle status of a watched PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Head changed; a build is queued.
    Queued,
    /// Build commands are running.
    Building,
    /// Build succeeded; ready to launch.
    Success,
    /// Build failed (see `error` + log).
    Failed,
    /// Dropped out of the review queue with changes-requested/commented; kept.
    Reviewed,
    /// Marked for worktree removal + drop from state.
    Cleaning,
}

/// One watched PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrEntry {
    pub repo_id: String,
    pub repo_github: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub branch: String,
    pub worktree: String,
    pub head_sha: String,
    #[serde(default)]
    pub built_sha: Option<String>,
    #[serde(default)]
    pub failed_sha: Option<String>,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub awaiting: bool,
    #[serde(default)]
    pub my_review: String,
    pub status: Status,
    #[serde(default)]
    pub error: Option<String>,
    pub log_path: String,
}

/// Persisted runtime state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeState {
    /// PR entries keyed by `<repo-id>-<number>`.
    #[serde(default)]
    pub prs: BTreeMap<String, PrEntry>,
    /// Cached GitHub login of the current user.
    #[serde(default)]
    pub me: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub last_poll: Option<String>,
}

/// Build the state key for a repo id + PR number.
pub fn key(repo_id: &str, number: u64) -> String {
    format!("{repo_id}-{number}")
}

/// Load runtime state from disk, defaulting to empty.
pub fn load(app: &AppHandle) -> RuntimeState {
    let path = paths::state_file(app);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => RuntimeState::default(),
    }
}

/// Persist runtime state to disk atomically.
pub fn save(app: &AppHandle, state: &mut RuntimeState) -> anyhow::Result<()> {
    paths::ensure_dirs(app)?;
    state.updated = Some(crate::now_iso());
    let path = paths::state_file(app);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
