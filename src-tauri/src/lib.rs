//! Review Bar: a cross-platform menu bar app that watches GitHub review
//! requests, builds each PR in a git worktree, and launches it.
//!
//! This is a port of the macOS-only SwiftBar plugin `positron-reviews.2m.py`
//! into a single long-lived Tauri process. The multi-process + flock design is
//! replaced by in-memory state behind async mutexes, a periodic poller task,
//! and a single build worker task coordinated by a `Notify`.

mod builder;
mod commands;
mod config;
mod github;
mod paths;
mod poller;
mod shell;
mod state;
mod tray;
mod worktree;

use tauri::async_runtime::Mutex;
use tauri::{Manager, RunEvent};
use tokio::sync::Notify;

use config::Config;
use state::RuntimeState;

/// Shared application state, managed by Tauri.
pub struct AppState {
    pub config: Mutex<Config>,
    pub state: Mutex<RuntimeState>,
    /// Wakes the build worker when work is enqueued.
    pub build_notify: Notify,
    /// Wakes the poller for an immediate refresh.
    pub refresh_notify: Notify,
}

/// Current time as an RFC3339 string (used for state timestamps + log headers).
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        builder = builder
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                // Second launch: surface the settings window.
                commands::open_settings(app);
            }))
            .plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::save_config,
            commands::list_prs,
            commands::read_log,
            commands::log_event_name,
            commands::list_github_repos,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // Load persisted config + state.
            let mut cfg = config::load(&handle);
            config::normalize_ids(&mut cfg);
            let runtime = state::load(&handle);

            app.manage(AppState {
                config: Mutex::new(cfg),
                state: Mutex::new(runtime),
                build_notify: Notify::new(),
                refresh_notify: Notify::new(),
            });

            // Menu-bar-only app (no dock icon) on macOS.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Tray icon; the menu is filled in by the first rebuild.
            let icon = app
                .default_window_icon()
                .cloned()
                .expect("bundle icon configured");
            tauri::tray::TrayIconBuilder::with_id(tray::TRAY_ID)
                .icon(icon)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| tray::on_menu(app, event.id().as_ref()))
                .build(app)?;

            // Background tasks: poll GitHub and process the build queue.
            let poll_handle = handle.clone();
            tauri::async_runtime::spawn(async move { poller::run_loop(poll_handle).await });
            let work_handle = handle.clone();
            tauri::async_runtime::spawn(async move { builder::worker_loop(work_handle).await });
            tauri::async_runtime::spawn(async move { tray::rebuild(&handle).await });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Keep running in the tray when all windows are closed.
            if let RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
