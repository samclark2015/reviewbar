//! Thin wrappers over the `gh` CLI. `gh` is cross-platform and owns GitHub auth,
//! so it stays a runtime dependency (as in the original plugin).

use std::collections::HashMap;

use serde::Deserialize;

use crate::shell::{self, Output};

#[derive(Debug, Deserialize, Default)]
struct Author {
    #[serde(default)]
    login: String,
}

/// A PR as returned by `gh pr list --json`.
#[derive(Debug, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "headRefOid", default)]
    pub head_ref_oid: String,
    #[serde(default)]
    author: Author,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
}

impl PrInfo {
    pub fn author_login(&self) -> &str {
        &self.author.login
    }
}

async fn gh(
    args: &[&str],
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> std::io::Result<Output> {
    shell::run_program("gh", args, None, env, path_prepend).await
}

/// Repositories the authenticated user can access (owner, collaborator, org
/// member), most-recently-pushed first. Used to populate the repo picker.
pub async fn list_repos(env: &HashMap<String, String>, path_prepend: &[String]) -> Vec<String> {
    let out = gh(
        &[
            "api",
            "user/repos?per_page=100&sort=pushed&affiliation=owner,collaborator,organization_member",
            "-q",
            ".[].full_name",
        ],
        env,
        path_prepend,
    )
    .await;
    match out {
        Ok(o) if o.ok() => o
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// The authenticated user's login (`gh api user -q .login`).
pub async fn get_my_login(env: &HashMap<String, String>, path_prepend: &[String]) -> Option<String> {
    let out = gh(&["api", "user", "-q", ".login"], env, path_prepend)
        .await
        .ok()?;
    let login = out.stdout.trim().to_string();
    if out.ok() && !login.is_empty() {
        Some(login)
    } else {
        None
    }
}

/// PRs on `github` matching `search` (defaults to review-requested).
pub async fn fetch_awaiting(
    github: &str,
    search: &str,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> Vec<PrInfo> {
    let out = gh(
        &[
            "pr",
            "list",
            "-R",
            github,
            "--search",
            search,
            "--state",
            "open",
            "--limit",
            "30",
            "--json",
            "number,title,url,headRefOid,author,isDraft",
        ],
        env,
        path_prepend,
    )
    .await;
    match out {
        Ok(o) if o.ok() => serde_json::from_str(&o.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `(pr_state, my_latest_review_state)` for a PR no longer requesting my review.
/// `pr_state` is `OPEN`/`MERGED`/`CLOSED`; review state defaults to `NONE`.
pub async fn fetch_my_latest_review(
    github: &str,
    number: u64,
    me: &str,
    env: &HashMap<String, String>,
    path_prepend: &[String],
) -> (Option<String>, String) {
    let num = number.to_string();
    let out = gh(
        &["pr", "view", &num, "-R", github, "--json", "state,reviews"],
        env,
        path_prepend,
    )
    .await;
    let value: serde_json::Value = match out {
        Ok(o) if o.ok() => serde_json::from_str(&o.stdout).unwrap_or(serde_json::Value::Null),
        _ => return (None, "NONE".to_string()),
    };

    let pr_state = value
        .get("state")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    let relevant = ["APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED"];
    let mut mine: Vec<(&str, &str)> = Vec::new(); // (submittedAt, state)
    if let Some(reviews) = value.get("reviews").and_then(|r| r.as_array()) {
        for r in reviews {
            let login = r
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("");
            let state = r.get("state").and_then(|s| s.as_str()).unwrap_or("");
            if login == me && relevant.contains(&state) {
                let at = r.get("submittedAt").and_then(|s| s.as_str()).unwrap_or("");
                mine.push((at, state));
            }
        }
    }
    mine.sort_by(|a, b| a.0.cmp(b.0));
    let latest = mine.last().map(|(_, s)| s.to_string()).unwrap_or_else(|| "NONE".to_string());
    (pr_state, latest)
}
