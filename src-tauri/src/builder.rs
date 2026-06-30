//! Background build worker. Ported from `worker_loop`/`do_build`/`do_clean`.
//!
//! A single long-lived task drains the queue one PR at a time: it runs each
//! repo's configured build commands in sequence, streaming combined
//! stdout/stderr to the PR's log file *and* to a Tauri event so the in-app log
//! viewer can tail it live. Replaces the multi-process + flock design with one
//! task coordinated by a `Notify`.

use std::io::Write as _;
use std::process::Stdio;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::mpsc;

use crate::state::{self, PrEntry, Status};
use crate::{now_iso, paths, shell, tray, worktree, AppState};

/// Event name the log viewer subscribes to for a given PR key.
pub fn log_event(key: &str) -> String {
    format!("log-{key}")
}

enum Task {
    Build(String, Box<PrEntry>),
    Clean(String),
    Idle,
}

/// Atomically pick the next actionable PR and mark a build in-progress.
async fn claim_next(app: &AppHandle) -> Task {
    let st = app.state::<AppState>();
    let mut guard = st.state.lock().await;
    let build_key = guard
        .prs
        .iter()
        .find(|(_, e)| e.status == Status::Queued)
        .map(|(k, _)| k.clone());
    if let Some(key) = build_key {
        let entry = guard.prs.get_mut(&key).unwrap();
        entry.status = Status::Building;
        let snapshot = entry.clone();
        state::save(app, &mut guard).ok();
        return Task::Build(key, Box::new(snapshot));
    }
    let clean_key = guard
        .prs
        .iter()
        .find(|(_, e)| e.status == Status::Cleaning)
        .map(|(k, _)| k.clone());
    if let Some(key) = clean_key {
        return Task::Clean(key);
    }
    Task::Idle
}

/// Apply a mutation to one entry and persist.
async fn update_entry(app: &AppHandle, key: &str, f: impl FnOnce(&mut PrEntry)) {
    let st = app.state::<AppState>();
    let mut guard = st.state.lock().await;
    if let Some(e) = guard.prs.get_mut(key) {
        f(e);
    }
    state::save(app, &mut guard).ok();
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .ok();
}

/// Stream a tokio reader's lines into the log file + a Tauri event.
fn spawn_reader(reader: ChildStdout, tx: mpsc::UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// Run one shell command, streaming combined output. Returns its exit code.
async fn run_step(
    app: &AppHandle,
    key: &str,
    number: u64,
    script: &str,
    repo: &crate::config::RepoConfig,
    file: &mut std::fs::File,
) -> std::io::Result<i32> {
    writeln!(file, "\n$ {script}").ok();
    let event = log_event(key);
    app.emit(&event, format!("$ {script}")).ok();

    let worktree_dir = worktree::worktree_path(&repo.worktree_base, number);
    let mut cmd = shell::shell_command(
        script,
        &worktree_dir,
        &repo.env,
        &repo.path_prepend,
        &repo.shell,
    );
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    if let Some(out) = stdout {
        spawn_reader(out, tx.clone());
    }
    if let Some(err) = stderr {
        // ChildStderr -> reuse the same reader path via dynamic dispatch.
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    while let Some(line) = rx.recv().await {
        writeln!(file, "{line}").ok();
        app.emit(&event, line).ok();
    }
    let status = child.wait().await?;
    Ok(status.code().unwrap_or(-1))
}

async fn do_build(app: &AppHandle, key: &str, entry: &PrEntry) {
    paths::ensure_dirs(app).ok();
    let log_path = paths::pr_log_path(app, key);
    let trigger_sha = entry.head_sha.clone();
    let url = entry.url.clone();
    let title = entry.title.clone();

    // Resolve repo config snapshot.
    let repo = {
        let st = app.state::<AppState>();
        let cfg = st.config.lock().await;
        cfg.repo(&entry.repo_id).cloned()
    };
    let Some(repo) = repo else {
        update_entry(app, key, |e| {
            e.status = Status::Failed;
            e.error = Some("repo no longer configured".into());
            e.failed_sha = Some(trigger_sha.clone());
        })
        .await;
        return;
    };

    let mut file = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(err) => {
            update_entry(app, key, |e| {
                e.status = Status::Failed;
                e.error = Some(format!("cannot open log: {err}"));
                e.failed_sha = Some(trigger_sha.clone());
            })
            .await;
            return;
        }
    };
    writeln!(file, "=== Build {} #{} ===", repo.github, entry.number).ok();
    writeln!(file, "{}", now_iso()).ok();
    file.flush().ok();

    // Create/update the worktree.
    match worktree::ensure_worktree(
        &repo.local_repo,
        &repo.worktree_base,
        entry.number,
        &repo.env,
        &repo.path_prepend,
    )
    .await
    {
        Ok((_head, log)) => {
            write!(file, "{log}").ok();
            app.emit(&log_event(key), log).ok();
        }
        Err(err) => {
            writeln!(file, "{err}").ok();
            update_entry(app, key, |e| {
                e.status = Status::Failed;
                e.error = Some(format!("worktree: {err}"));
                e.failed_sha = Some(trigger_sha.clone());
            })
            .await;
            notify(app, "Build failed", &format!("{} #{}: worktree error", repo.github, entry.number));
            return;
        }
    }

    // Run each build command in order.
    for script in &repo.build_commands {
        let rc = run_step(app, key, entry.number, script, &repo, &mut file).await;
        let rc = rc.unwrap_or(-1);
        if rc != 0 {
            writeln!(file, "\n=== FAILED (exit {rc}) {} ===", now_iso()).ok();
            update_entry(app, key, |e| {
                e.status = Status::Failed;
                e.error = Some(format!("`{script}` exited {rc}"));
                e.failed_sha = Some(trigger_sha.clone());
            })
            .await;
            notify(app, "Build failed", &format!("{} #{}: {script}", repo.github, entry.number));
            return;
        }
    }

    writeln!(file, "\n=== SUCCESS {} ===", now_iso()).ok();
    update_entry(app, key, |e| {
        e.status = Status::Success;
        e.built_sha = Some(trigger_sha.clone());
        e.error = None;
    })
    .await;
    let body = if url.is_empty() {
        format!("{} #{}: {title}", repo.github, entry.number)
    } else {
        format!("{} #{}: {title}\nReady to launch", repo.github, entry.number)
    };
    notify(app, "Build ready", &body);
}

async fn do_clean(app: &AppHandle, key: &str) {
    let entry = {
        let st = app.state::<AppState>();
        let guard = st.state.lock().await;
        guard.prs.get(key).cloned()
    };
    if let Some(entry) = entry {
        let repo = {
            let st = app.state::<AppState>();
            let guard = st.config.lock().await;
            guard.repo(&entry.repo_id).cloned()
        };
        if let Some(repo) = repo {
            worktree::remove_worktree(
                &repo.local_repo,
                &repo.worktree_base,
                entry.number,
                &repo.env,
                &repo.path_prepend,
            )
            .await
            .ok();
        }
    }
    std::fs::remove_file(paths::pr_log_path(app, key)).ok();
    let st = app.state::<AppState>();
    let mut guard = st.state.lock().await;
    guard.prs.remove(key);
    state::save(app, &mut guard).ok();
}

/// Drain the queue until empty, then sleep on the build notifier.
pub async fn worker_loop(app: AppHandle) {
    let st = app.state::<AppState>();
    loop {
        match claim_next(&app).await {
            Task::Build(key, entry) => {
                do_build(&app, &key, &entry).await;
                tray::rebuild(&app).await;
            }
            Task::Clean(key) => {
                do_clean(&app, &key).await;
                tray::rebuild(&app).await;
            }
            Task::Idle => {
                st.build_notify.notified().await;
            }
        }
    }
}
