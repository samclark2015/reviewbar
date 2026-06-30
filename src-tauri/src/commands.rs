//! Actions invoked from the tray menu and the web windows.
//!
//! The `pub async fn` helpers are shared by the tray dispatcher; the
//! `#[tauri::command]` wrappers expose what the Settings and Log windows need.

use std::process::Stdio;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::config::{self, Config, RepoConfig};
use crate::state::{self, PrEntry, Status};
use crate::{builder, github, paths, shell, tray, worktree, AppState};

/// Look up a PR entry and its still-configured repo.
async fn lookup(app: &AppHandle, key: &str) -> Option<(PrEntry, RepoConfig)> {
    let st = app.state::<AppState>();
    let entry = st.state.lock().await.prs.get(key).cloned()?;
    let repo = st.config.lock().await.repo(&entry.repo_id).cloned()?;
    Some((entry, repo))
}

// --- Shared helpers (used by the tray) -------------------------------------

pub async fn trigger_rebuild(app: &AppHandle, key: &str) {
    let st = app.state::<AppState>();
    {
        let mut guard = st.state.lock().await;
        if let Some(e) = guard.prs.get_mut(key) {
            e.status = Status::Queued;
            e.built_sha = None;
            e.failed_sha = None;
            e.error = None;
        }
        state::save(app, &mut guard).ok();
    }
    st.build_notify.notify_one();
    tray::rebuild(app).await;
}

pub async fn trigger_remove(app: &AppHandle, key: &str) {
    let st = app.state::<AppState>();
    {
        let mut guard = st.state.lock().await;
        if let Some(e) = guard.prs.get_mut(key) {
            e.status = Status::Cleaning;
        }
        state::save(app, &mut guard).ok();
    }
    st.build_notify.notify_one();
    tray::rebuild(app).await;
}

pub async fn refresh_now(app: &AppHandle) {
    app.state::<AppState>().refresh_notify.notify_one();
}

/// Launch the built app via the repo's configured launch command (detached).
pub async fn launch(app: &AppHandle, key: &str) {
    let Some((entry, repo)) = lookup(app, key).await else {
        return;
    };
    if repo.launch_command.trim().is_empty() {
        return;
    }
    paths::ensure_dirs(app).ok();
    let dir = worktree::worktree_path(&repo.worktree_base, entry.number);
    let mut cmd = shell::shell_command(
        &repo.launch_command,
        &dir,
        &repo.env,
        &repo.path_prepend,
        &repo.shell,
    );
    if let Ok(file) = std::fs::File::create(paths::launch_log_path(app, key)) {
        if let Ok(err_file) = file.try_clone() {
            cmd.stderr(Stdio::from(err_file));
        }
        cmd.stdout(Stdio::from(file));
    }
    if let Ok(mut child) = cmd.spawn() {
        // Detach: reap in the background so the process keeps running.
        tauri::async_runtime::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

/// Open an interactive terminal in the PR's worktree running Claude Code.
pub async fn launch_claude(app: &AppHandle, key: &str) {
    let Some((entry, repo)) = lookup(app, key).await else {
        return;
    };
    let dir = worktree::worktree_path(&repo.worktree_base, entry.number);
    shell::open_terminal(&dir, "claude").ok();
}

/// Open the PR's worktree in the configured editor.
pub async fn open_in_editor(app: &AppHandle, key: &str) {
    let Some((entry, repo)) = lookup(app, key).await else {
        return;
    };
    let dir = worktree::worktree_path(&repo.worktree_base, entry.number);
    let dir_str = dir.to_string_lossy().to_string();
    let editor = app
        .state::<AppState>()
        .config
        .lock()
        .await
        .settings
        .editor_command
        .clone();
    let quoted = shell::quote(&dir_str);
    let script = if editor.contains("{path}") {
        editor.replace("{path}", &quoted)
    } else {
        format!("{editor} {quoted}")
    };
    let mut cmd = shell::shell_command(&script, &dir, &repo.env, &repo.path_prepend, &repo.shell);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        tauri::async_runtime::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

/// Open the PR page on GitHub in the default browser.
pub async fn open_pr(app: &AppHandle, key: &str) {
    let url = {
        let st = app.state::<AppState>();
        let guard = st.state.lock().await;
        guard.prs.get(key).map(|e| {
            if e.url.is_empty() {
                format!("https://github.com/{}/pull/{}", e.repo_github, e.number)
            } else {
                e.url.clone()
            }
        })
    };
    if let Some(url) = url {
        app.opener().open_url(url, None::<&str>).ok();
    }
}

/// Show (or create) the Settings window.
pub fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        w.show().ok();
        w.set_focus().ok();
        return;
    }
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
        .title("Review Bar — Settings")
        .inner_size(820.0, 680.0)
        .build()
        .ok();
}

/// Show (or create) the live log window for a PR.
pub fn open_log(app: &AppHandle, key: &str) {
    let label = format!("log-{key}");
    if let Some(w) = app.get_webview_window(&label) {
        w.show().ok();
        w.set_focus().ok();
        return;
    }
    let url = format!("log.html?key={key}");
    WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        .title(format!("Build log — {key}"))
        .inner_size(860.0, 580.0)
        .build()
        .ok();
}

#[cfg(desktop)]
fn apply_autostart(app: &AppHandle, enabled: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().ok();
    } else {
        manager.disable().ok();
    }
}

#[cfg(not(desktop))]
fn apply_autostart(_app: &AppHandle, _enabled: bool) {}

// --- Commands exposed to the web windows -----------------------------------

#[tauri::command]
pub async fn get_config(app: AppHandle) -> Config {
    app.state::<AppState>().config.lock().await.clone()
}

#[tauri::command]
pub async fn save_config(app: AppHandle, mut config: Config) -> Result<(), String> {
    config::normalize_ids(&mut config);
    {
        *app.state::<AppState>().config.lock().await = config.clone();
    }
    config::save(&app, &config).map_err(|e| e.to_string())?;
    apply_autostart(&app, config.settings.autostart);
    refresh_now(&app).await;
    tray::rebuild(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn list_prs(app: AppHandle) -> Vec<PrEntry> {
    app.state::<AppState>()
        .state
        .lock()
        .await
        .prs
        .values()
        .cloned()
        .collect()
}

#[tauri::command]
pub async fn read_log(app: AppHandle, key: String) -> String {
    std::fs::read_to_string(paths::pr_log_path(&app, &key)).unwrap_or_default()
}

#[tauri::command]
pub async fn log_event_name(key: String) -> String {
    builder::log_event(&key)
}

#[tauri::command]
pub async fn list_github_repos(app: AppHandle) -> Vec<String> {
    // Resolve gh via the first repo's env/PATH if configured, else defaults.
    let (env, pp) = {
        let st = app.state::<AppState>();
        let cfg = st.config.lock().await;
        cfg.repos
            .first()
            .map(|r| (r.env.clone(), r.path_prepend.clone()))
            .unwrap_or_default()
    };
    github::list_repos(&env, &pp).await
}
