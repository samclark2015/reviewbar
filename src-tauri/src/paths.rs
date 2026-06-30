//! Resolved on-disk locations for config, runtime state, and per-PR logs.
//!
//! Everything lives under the OS-standard app config/data directories provided
//! by Tauri's path API, so the app behaves natively on macOS, Windows, and Linux.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

/// `config.json` holding the user's repos + global settings.
pub fn config_file(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .expect("app config dir")
        .join("config.json")
}

/// Directory for runtime state + logs (created on demand).
pub fn data_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().expect("app data dir")
}

/// `state.json` holding the reconciled PR queue.
pub fn state_file(app: &AppHandle) -> PathBuf {
    data_dir(app).join("state.json")
}

/// Directory holding per-PR and launch build logs.
pub fn log_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("logs")
}

/// Build log for a given PR key (`<repo-id>-<number>`).
pub fn pr_log_path(app: &AppHandle, key: &str) -> PathBuf {
    log_dir(app).join(format!("{key}.log"))
}

/// Launch log for a given PR key.
pub fn launch_log_path(app: &AppHandle, key: &str) -> PathBuf {
    log_dir(app).join(format!("{key}-launch.log"))
}

/// Ensure the config + data + log directories exist.
pub fn ensure_dirs(app: &AppHandle) -> std::io::Result<()> {
    if let Some(parent) = config_file(app).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(log_dir(app))?;
    Ok(())
}
