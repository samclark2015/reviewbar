//! The system-tray icon and its dynamic menu. Ported from `render()` in the
//! original plugin: an aggregate status glyph/count in the menu bar, plus a
//! per-PR submenu of actions. Rebuilt whenever state changes.

use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Manager};

use crate::state::{PrEntry, Status};
use crate::{commands, AppState};

pub const TRAY_ID: &str = "main";

fn glyph(status: Status) -> &'static str {
    match status {
        Status::Queued => "⏳",
        Status::Building => "🛠",
        Status::Success => "✅",
        Status::Failed => "⚠️",
        Status::Reviewed => "📝",
        Status::Cleaning => "🧹",
    }
}

fn status_label(entry: &PrEntry) -> String {
    match entry.status {
        Status::Queued => "Queued for build".to_string(),
        Status::Building => "Building…".to_string(),
        Status::Success => "Ready to launch".to_string(),
        Status::Failed => format!(
            "Build failed ({})",
            entry.error.as_deref().unwrap_or("see log")
        ),
        Status::Reviewed => format!(
            "Reviewed ({})",
            entry.my_review.to_lowercase().replace('_', " ")
        ),
        Status::Cleaning => "Removing…".to_string(),
    }
}

fn aggregate_title(prs: &[PrEntry]) -> String {
    let building = prs
        .iter()
        .filter(|e| matches!(e.status, Status::Queued | Status::Building))
        .count();
    let failed = prs.iter().filter(|e| e.status == Status::Failed).count();
    let ready = prs
        .iter()
        .filter(|e| e.status == Status::Success && e.awaiting)
        .count();
    let awaiting = prs.iter().filter(|e| e.awaiting).count();

    if building > 0 {
        format!("🛠 {building}")
    } else if failed > 0 {
        format!("⚠️ {failed}")
    } else if ready > 0 {
        format!("✅ {ready}")
    } else if awaiting > 0 {
        format!("👁 {awaiting}")
    } else {
        "👁".to_string()
    }
}

/// Rebuild the tray menu + title from current state (runs on the main thread).
pub async fn rebuild(app: &AppHandle) {
    let st = app.state::<AppState>();
    let mut prs: Vec<PrEntry> = st.state.lock().await.prs.values().cloned().collect();
    // Awaiting first, then by PR number descending.
    prs.sort_by(|a, b| {
        let ka = (!a.awaiting, std::cmp::Reverse(a.number));
        let kb = (!b.awaiting, std::cmp::Reverse(b.number));
        ka.cmp(&kb)
    });

    let app = app.clone();
    app.clone()
        .run_on_main_thread(move || {
            if let Err(err) = build_and_set(&app, &prs) {
                eprintln!("tray rebuild failed: {err}");
            }
        })
        .ok();
}

fn build_and_set(app: &AppHandle, prs: &[PrEntry]) -> tauri::Result<()> {
    let title = aggregate_title(prs);
    let mut mb = MenuBuilder::new(app);

    if prs.is_empty() {
        let empty = MenuItemBuilder::new("No review requests")
            .id("noop")
            .enabled(false)
            .build(app)?;
        mb = mb.item(&empty);
    }

    for entry in prs {
        let key = crate::state::key(&entry.repo_id, entry.number);
        let draft = if entry.is_draft { " [draft]" } else { "" };
        let header = format!(
            "{} #{} {}{}",
            glyph(entry.status),
            entry.number,
            entry.title,
            draft
        );

        let status_item = MenuItemBuilder::new(status_label(entry))
            .id(format!("noop-status-{key}"))
            .enabled(false)
            .build(app)?;
        let author_item = MenuItemBuilder::new(format!("by {}", entry.author))
            .id(format!("noop-author-{key}"))
            .enabled(false)
            .build(app)?;

        let mut sb = SubmenuBuilder::new(app, &header)
            .item(&status_item)
            .item(&author_item)
            .separator();

        if entry.status == Status::Success {
            sb = sb.text(format!("launch::{key}"), "🚀 Launch");
        }
        // A worktree exists once a build has started, so Claude can run there.
        if matches!(
            entry.status,
            Status::Building | Status::Failed | Status::Success | Status::Reviewed
        ) {
            sb = sb.text(format!("claude::{key}"), "🤖 Claude Code here");
        }
        sb = sb
            .text(format!("rebuild::{key}"), "🔄 Rebuild")
            .text(format!("log::{key}"), "📡 Watch build log")
            .text(format!("editor::{key}"), "🗂 Open in editor")
            .text(format!("openpr::{key}"), "🌐 Open PR on GitHub")
            .separator()
            .text(format!("remove::{key}"), "🗑 Remove worktree");

        let submenu = sb.build()?;
        mb = mb.item(&submenu);
    }

    let menu = mb
        .separator()
        .text("settings", "Settings…")
        .text("refresh", "Refresh now")
        .separator()
        .text("quit", "Quit")
        .build()?;

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu))?;
        #[cfg(target_os = "macos")]
        tray.set_title(Some(&title)).ok();
        tray.set_tooltip(Some(&title)).ok();
    }
    Ok(())
}

/// Handle a click on a tray menu item.
pub fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "settings" => commands::open_settings(app),
        "refresh" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { commands::refresh_now(&app).await });
        }
        "quit" => app.exit(0),
        _ => {
            if let Some((action, key)) = id.split_once("::") {
                let app = app.clone();
                let action = action.to_string();
                let key = key.to_string();
                tauri::async_runtime::spawn(async move {
                    match action.as_str() {
                        "launch" => commands::launch(&app, &key).await,
                        "claude" => commands::launch_claude(&app, &key).await,
                        "rebuild" => commands::trigger_rebuild(&app, &key).await,
                        "log" => commands::open_log(&app, &key),
                        "editor" => commands::open_in_editor(&app, &key).await,
                        "openpr" => commands::open_pr(&app, &key).await,
                        "remove" => commands::trigger_remove(&app, &key).await,
                        _ => {}
                    }
                });
            }
        }
    }
}
