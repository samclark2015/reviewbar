//! User configuration: the repositories to watch and global settings.
//!
//! Persisted as `config.json`. The build/launch commands and the PATH/env
//! injection are all per-repo, which is what makes this generic across projects
//! (replacing the hardcoded npm/mise/code.sh logic of the original SwiftBar
//! plugin).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::paths;

fn default_search() -> String {
    "review-requested:@me".to_string()
}

/// A single repository the user wants to watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    /// Stable slug used to key state, log files, menu ids, and window labels.
    /// Generated from `github`/`name` if the frontend leaves it blank.
    #[serde(default)]
    pub id: String,
    /// Display name shown in the tray.
    #[serde(default)]
    pub name: String,
    /// `owner/repo` on GitHub.
    pub github: String,
    /// Path to the local clone used as the source for `git worktree add`.
    pub local_repo: String,
    /// Directory in which per-PR worktrees are created.
    pub worktree_base: String,
    /// `gh pr list --search` query (defaults to PRs requesting my review).
    #[serde(default = "default_search")]
    pub search: String,
    /// Build commands, run in order; a non-zero exit fails the build.
    #[serde(default)]
    pub build_commands: Vec<String>,
    /// Command to launch the built app (run detached from the worktree).
    #[serde(default)]
    pub launch_command: String,
    /// Extra environment variables for build/launch commands.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Directories prepended to PATH for build/launch (replaces the mise-shims
    /// hack: e.g. `~/.local/share/mise/shims`, `/opt/homebrew/bin`).
    #[serde(default)]
    pub path_prepend: Vec<String>,
    /// Optional shell override, e.g. `zsh -lc` or `pwsh -Command`. Defaults to
    /// `sh -c` on Unix and `cmd /C` on Windows.
    #[serde(default)]
    pub shell: Option<String>,
}

/// Global, non-repo settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Seconds between GitHub polls.
    #[serde(default = "default_poll")]
    pub poll_interval_secs: u64,
    /// Command used to open a worktree in an editor; `{path}` is substituted.
    #[serde(default = "default_editor")]
    pub editor_command: String,
    /// Launch the app at login.
    #[serde(default)]
    pub autostart: bool,
}

fn default_poll() -> u64 {
    60
}

fn default_editor() -> String {
    "code {path}".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            poll_interval_secs: default_poll(),
            editor_command: default_editor(),
            autostart: false,
        }
    }
}

/// Top-level config document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub settings: Settings,
}

impl Config {
    pub fn repo(&self, id: &str) -> Option<&RepoConfig> {
        self.repos.iter().find(|r| r.id == id)
    }
}

/// Turn an arbitrary string into a filesystem/label-safe slug (`[a-z0-9-]`).
pub fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in input.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("repo");
    }
    out
}

/// Ensure every repo has a unique, slug-safe `id`. Mutates in place.
pub fn normalize_ids(config: &mut Config) {
    let mut seen: Vec<String> = Vec::new();
    for repo in &mut config.repos {
        let base = if repo.id.trim().is_empty() {
            slugify(if repo.name.trim().is_empty() {
                &repo.github
            } else {
                &repo.name
            })
        } else {
            slugify(&repo.id)
        };
        let mut id = base.clone();
        let mut n = 2;
        while seen.contains(&id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        seen.push(id.clone());
        repo.id = id;
    }
}

/// Load config from disk, returning defaults if missing or unreadable.
pub fn load(app: &AppHandle) -> Config {
    let path = paths::config_file(app);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// Persist config to disk atomically.
pub fn save(app: &AppHandle, config: &Config) -> anyhow::Result<()> {
    paths::ensure_dirs(app)?;
    let path = paths::config_file(app);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(config)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
