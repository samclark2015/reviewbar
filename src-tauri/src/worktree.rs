//! Git worktree management, one worktree per watched PR.
//!
//! Ported from `ensure_worktree`/`remove_worktree`/`prune_worktrees` in the
//! original plugin. `pull/<n>/head` resolves for same-repo branches and forks
//! alike. All output is accumulated into a returned log string so the builder
//! can stream it to the PR's log.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::shell::{self, expand_tilde, Output};

/// Worktree directory for a PR under the repo's configured base.
pub fn worktree_path(worktree_base: &str, number: u64) -> PathBuf {
    Path::new(&expand_tilde(worktree_base)).join(format!("pr-{number}"))
}

pub fn branch_name(number: u64) -> String {
    format!("review-pr-{number}")
}

async fn git(
    dir: &str,
    args: &[&str],
    env: &HashMap<String, String>,
    path_prepend: &[String],
    log: &mut String,
) -> Result<Output> {
    let mut full: Vec<&str> = vec!["-C", dir];
    full.extend_from_slice(args);
    let out = shell::run_program("git", &full, None, env, path_prepend).await?;
    log.push_str(&out.stdout);
    log.push_str(&out.stderr);
    Ok(out)
}

pub async fn prune(
    local_repo: &str,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> Result<()> {
    let mut log = String::new();
    git(local_repo, &["worktree", "prune"], env, path_prepend, &mut log).await?;
    Ok(())
}

/// Create or fast-forward the worktree for `number` to its current PR head.
/// Returns `(head_sha, log_text)`.
pub async fn ensure_worktree(
    local_repo: &str,
    worktree_base: &str,
    number: u64,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> Result<(String, String)> {
    let local = expand_tilde(local_repo);
    let path = worktree_path(worktree_base, number);
    let path_str = path.to_string_lossy().to_string();
    let branch = branch_name(number);
    let mut log = String::new();

    let fetch = git(
        &local,
        &["fetch", "origin", &format!("pull/{number}/head")],
        env,
        path_prepend,
        &mut log,
    )
    .await?;
    if !fetch.ok() {
        return Err(anyhow!("git fetch pull/{number}/head failed"));
    }

    let head_out = git(&local, &["rev-parse", "FETCH_HEAD"], env, path_prepend, &mut log).await?;
    let head = head_out.stdout.trim().to_string();
    if head.is_empty() {
        return Err(anyhow!("could not resolve FETCH_HEAD"));
    }

    let git_marker = path.join(".git");
    if git_marker.exists() {
        // Existing worktree: move it to the new head, keeping node_modules etc.
        git(&path_str, &["checkout", "-B", &branch, &head], env, path_prepend, &mut log).await?;
        git(&path_str, &["reset", "--hard", &head], env, path_prepend, &mut log).await?;
    } else {
        prune(&local, env, path_prepend).await.ok();
        let add = git(
            &local,
            &["worktree", "add", "-f", "-B", &branch, &path_str, &head],
            env,
            path_prepend,
            &mut log,
        )
        .await?;
        if !add.ok() {
            return Err(anyhow!("git worktree add failed"));
        }
    }
    Ok((head, log))
}

/// Remove the worktree and its review branch.
pub async fn remove_worktree(
    local_repo: &str,
    worktree_base: &str,
    number: u64,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> Result<()> {
    let local = expand_tilde(local_repo);
    let path = worktree_path(worktree_base, number);
    let path_str = path.to_string_lossy().to_string();
    let branch = branch_name(number);
    let mut log = String::new();

    git(&local, &["worktree", "remove", "--force", &path_str], env, path_prepend, &mut log)
        .await
        .ok();
    git(&local, &["branch", "-D", &branch], env, path_prepend, &mut log)
        .await
        .ok();
    prune(&local, env, path_prepend).await.ok();
    // Belt and suspenders if the dir lingered.
    if path.exists() {
        std::fs::remove_dir_all(&path).ok();
    }
    Ok(())
}
