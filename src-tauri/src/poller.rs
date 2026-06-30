//! Periodic GitHub poll + reconcile. Ported from `reconcile()` in the original
//! plugin, generalized across all configured repos.

use std::collections::{BTreeSet, HashMap};

use tauri::{AppHandle, Manager};

use crate::state::{self, PrEntry, Status};
use crate::{github, now_iso, tray, worktree, AppState};

/// How many PRs that dropped out of the review queue to re-check per cycle.
const MAX_DROPPED_CHECKS: usize = 10;

/// Should a build be (re)queued for an entry at `head`? True when we have not
/// already attempted this exact head (success or failure) and no build is
/// already underway. SHA-keying stops a broken build re-queueing every refresh.
pub fn should_queue(status: Status, head: &str, built: Option<&str>, failed: Option<&str>) -> bool {
    !matches!(status, Status::Queued | Status::Building)
        && !head.is_empty()
        && built != Some(head)
        && failed != Some(head)
}

/// Status for a PR that dropped out of the review queue, given its PR state and
/// my latest review. Approved/merged/closed/un-requested -> clean up;
/// changes-requested/commented -> keep the worktree around.
pub fn dropped_status(pr_state: Option<&str>, my_review: &str) -> Status {
    if matches!(pr_state, Some("MERGED") | Some("CLOSED")) {
        Status::Cleaning
    } else if my_review == "APPROVED" {
        Status::Cleaning
    } else if my_review == "CHANGES_REQUESTED" || my_review == "COMMENTED" {
        Status::Reviewed
    } else {
        Status::Cleaning
    }
}

struct Awaited {
    repo_id: String,
    github: String,
    worktree_base: String,
    info: github::PrInfo,
}

/// Run one poll cycle: fetch review-requested PRs across all repos, reconcile
/// state (enqueue builds / clean up dropped PRs), and refresh the tray.
pub async fn poll_once(app: &AppHandle) {
    let st = app.state::<AppState>();
    let config = st.config.lock().await.clone();

    // Resolve the current user's login (cached in state once known).
    let me = {
        let cur = st.state.lock().await.me.clone();
        match cur {
            Some(m) => m,
            None => {
                let env = config
                    .repos
                    .first()
                    .map(|r| r.env.clone())
                    .unwrap_or_default();
                let pp = config
                    .repos
                    .first()
                    .map(|r| r.path_prepend.clone())
                    .unwrap_or_default();
                let login = github::get_my_login(&env, &pp).await.unwrap_or_default();
                if !login.is_empty() {
                    st.state.lock().await.me = Some(login.clone());
                }
                login
            }
        }
    };

    // --- Phase A: fetch the awaiting PRs for every repo (no state lock held). ---
    let mut awaiting: Vec<Awaited> = Vec::new();
    for repo in &config.repos {
        worktree::prune(&repo.local_repo, &repo.env, &repo.path_prepend)
            .await
            .ok();
        let prs =
            github::fetch_awaiting(&repo.github, &repo.search, &repo.env, &repo.path_prepend).await;
        for info in prs {
            awaiting.push(Awaited {
                repo_id: repo.id.clone(),
                github: repo.github.clone(),
                worktree_base: repo.worktree_base.clone(),
                info,
            });
        }
    }
    let awaiting_keys: BTreeSet<String> = awaiting
        .iter()
        .map(|a| state::key(&a.repo_id, a.info.number))
        .collect();

    // --- Phase B: figure out which existing PRs dropped out, and re-check them. ---
    let dropped_candidates: Vec<(String, String, u64)> = {
        let state_guard = st.state.lock().await;
        state_guard
            .prs
            .iter()
            .filter(|(k, e)| {
                !awaiting_keys.contains(*k)
                    && !matches!(e.status, Status::Queued | Status::Building | Status::Cleaning)
            })
            .take(MAX_DROPPED_CHECKS)
            .map(|(k, e)| (k.clone(), e.repo_github.clone(), e.number))
            .collect()
    };

    let mut dropped_reviews: HashMap<String, (Option<String>, String)> = HashMap::new();
    for (key, github_repo, number) in &dropped_candidates {
        // Use the owning repo's env if it still exists in config.
        let repo = config
            .repos
            .iter()
            .find(|r| r.github == *github_repo);
        let (env, pp) = repo
            .map(|r| (r.env.clone(), r.path_prepend.clone()))
            .unwrap_or_default();
        let result = github::fetch_my_latest_review(github_repo, *number, &me, &env, &pp).await;
        dropped_reviews.insert(key.clone(), result);
    }

    // --- Phase C: apply everything under the lock, then persist. ---
    let has_work = {
        let mut state_guard = st.state.lock().await;

        // 1) Upsert awaiting PRs; queue builds when the head moved.
        for a in &awaiting {
            let key = state::key(&a.repo_id, a.info.number);
            let worktree = worktree::worktree_path(&a.worktree_base, a.info.number)
                .to_string_lossy()
                .to_string();
            let log_path = crate::paths::pr_log_path(app, &key)
                .to_string_lossy()
                .to_string();

            let entry = state_guard.prs.entry(key.clone()).or_insert_with(|| PrEntry {
                repo_id: a.repo_id.clone(),
                repo_github: a.github.clone(),
                number: a.info.number,
                title: String::new(),
                url: String::new(),
                author: String::new(),
                branch: worktree::branch_name(a.info.number),
                worktree: worktree.clone(),
                head_sha: String::new(),
                built_sha: None,
                failed_sha: None,
                is_draft: false,
                awaiting: true,
                my_review: "NONE".to_string(),
                status: Status::Queued,
                error: None,
                log_path: log_path.clone(),
            });

            entry.repo_github = a.github.clone();
            entry.title = a.info.title.clone();
            entry.url = a.info.url.clone();
            entry.author = a.info.author_login().to_string();
            entry.branch = worktree::branch_name(a.info.number);
            entry.worktree = worktree;
            entry.head_sha = a.info.head_ref_oid.clone();
            entry.is_draft = a.info.is_draft;
            entry.awaiting = true;
            entry.log_path = log_path;

            if should_queue(
                entry.status,
                &entry.head_sha,
                entry.built_sha.as_deref(),
                entry.failed_sha.as_deref(),
            ) {
                entry.status = Status::Queued;
                entry.error = None;
            }
        }

        // 2) Handle PRs that dropped out of the review queue.
        let dropped_keys: Vec<String> = state_guard
            .prs
            .keys()
            .filter(|k| !awaiting_keys.contains(*k))
            .cloned()
            .collect();
        let known_repo_ids: BTreeSet<String> =
            config.repos.iter().map(|r| r.id.clone()).collect();

        for key in dropped_keys {
            let Some(entry) = state_guard.prs.get_mut(&key) else {
                continue;
            };
            entry.awaiting = false;

            // Repo removed from config -> clean it up.
            if !known_repo_ids.contains(&entry.repo_id) {
                if !matches!(entry.status, Status::Building) {
                    entry.status = Status::Cleaning;
                }
                continue;
            }

            // Leave in-flight work alone.
            if matches!(entry.status, Status::Queued | Status::Building | Status::Cleaning) {
                continue;
            }

            if let Some((pr_state, my_review)) = dropped_reviews.get(&key) {
                entry.my_review = my_review.clone();
                entry.status = dropped_status(pr_state.as_deref(), my_review);
            }
            // Beyond MAX_DROPPED_CHECKS this cycle: leave as-is, re-check next time.
        }

        state_guard.last_poll = Some(now_iso());
        let has_work = state_guard
            .prs
            .values()
            .any(|e| matches!(e.status, Status::Queued | Status::Cleaning));
        state::save(app, &mut state_guard).ok();
        has_work
    };

    if has_work {
        st.build_notify.notify_one();
    }
    tray::rebuild(app).await;
}

/// Background loop: poll on an interval, or whenever a manual refresh is asked.
pub async fn run_loop(app: AppHandle) {
    loop {
        poll_once(&app).await;
        let interval = {
            let st = app.state::<AppState>();
            let secs = st.config.lock().await.settings.poll_interval_secs.max(10);
            std::time::Duration::from_secs(secs)
        };
        let st = app.state::<AppState>();
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = st.refresh_notify.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_a_fresh_head() {
        // New PR (no prior build attempt) at a real head should queue.
        assert!(should_queue(Status::Success, "abc123", None, None));
    }

    #[test]
    fn does_not_requeue_same_built_head() {
        // Already built this exact head -> no rebuild.
        assert!(!should_queue(Status::Success, "abc", Some("abc"), None));
    }

    #[test]
    fn does_not_requeue_same_failed_head() {
        // A failed build stays failed for that head (broken build doesn't loop).
        assert!(!should_queue(Status::Failed, "abc", None, Some("abc")));
    }

    #[test]
    fn requeues_when_head_moves_past_failure() {
        // A new push (new head) retries even after a failure.
        assert!(should_queue(Status::Failed, "def", None, Some("abc")));
    }

    #[test]
    fn does_not_queue_while_in_flight() {
        assert!(!should_queue(Status::Queued, "abc", None, None));
        assert!(!should_queue(Status::Building, "abc", None, None));
    }

    #[test]
    fn does_not_queue_empty_head() {
        assert!(!should_queue(Status::Success, "", None, None));
    }

    #[test]
    fn dropped_merged_or_closed_cleans() {
        assert_eq!(dropped_status(Some("MERGED"), "NONE"), Status::Cleaning);
        assert_eq!(dropped_status(Some("CLOSED"), "COMMENTED"), Status::Cleaning);
    }

    #[test]
    fn dropped_approved_cleans() {
        assert_eq!(dropped_status(Some("OPEN"), "APPROVED"), Status::Cleaning);
    }

    #[test]
    fn dropped_changes_or_comment_kept() {
        assert_eq!(
            dropped_status(Some("OPEN"), "CHANGES_REQUESTED"),
            Status::Reviewed
        );
        assert_eq!(dropped_status(Some("OPEN"), "COMMENTED"), Status::Reviewed);
    }

    #[test]
    fn dropped_unrequested_cleans() {
        assert_eq!(dropped_status(Some("OPEN"), "NONE"), Status::Cleaning);
        assert_eq!(dropped_status(Some("OPEN"), "DISMISSED"), Status::Cleaning);
    }
}
