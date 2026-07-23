//! Auxiliary app windows: the settings panel (opened by hold-then-pull on the pad)
//! and the library window (opened from the system tray).

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

pub const SETTINGS_LABEL: &str = "settings";
pub const LIBRARY_LABEL: &str = "library";
pub const FIND_LABEL: &str = "find";

const FIND_W: f64 = 560.0;
const FIND_H: f64 = 420.0;

/// Pre-created hidden at startup so Ctrl+Alt+F opens instantly.
pub fn create_find_window(app: &AppHandle) -> tauri::Result<()> {
    let builder = WebviewWindowBuilder::new(
        app,
        FIND_LABEL,
        WebviewUrl::App("index.html?role=find".into()),
    )
    .title("Pinq Find")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .visible(false);
    // Quick-find is summoned by a global shortcut from any Space (macOS).
    #[cfg(target_os = "macos")]
    let builder = builder.visible_on_all_workspaces(true);
    let win = builder.build()?;
    let _ = win;
    Ok(())
}

/// Show quick-find centered on the monitor the cursor is on.
pub fn open_find(app: &AppHandle) -> tauri::Result<()> {
    use tauri::Emitter;
    let Some(win) = app.get_webview_window(FIND_LABEL) else { return Ok(()) };

    let cursor = app.cursor_position().unwrap_or(tauri::PhysicalPosition { x: 0.0, y: 0.0 });
    let wa = crate::monitors::work_area_at(app, cursor.x as i32, cursor.y as i32);
    let scale = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| crate::monitors::primary_scale(app));

    let w = (FIND_W * scale).round() as u32;
    let h = (FIND_H * scale).round() as u32;
    win.set_size(PhysicalSize::new(w, h))?;
    win.set_position(PhysicalPosition::new(
        wa.x + (wa.w as i32 - w as i32) / 2,
        wa.y + (wa.h as i32) / 5,
    ))?;
    let _ = app.emit_to(FIND_LABEL, "pinq-find-open", ());
    win.show()?;
    let _ = win.set_focus();
    Ok(())
}

#[tauri::command]
pub fn close_find(app: AppHandle) {
    if let Some(w) = app.get_webview_window(FIND_LABEL) {
        let _ = w.hide();
    }
}

pub const VIEWER_LABEL: &str = "viewer";

/// Open (or refocus) the image viewer on a note's attachment. Async: may
/// create a window (main-thread deadlock rule, see pull_end).
#[tauri::command]
pub async fn open_viewer(app: AppHandle, note_id: String, hash: String) -> Result<(), String> {
    use tauri::Emitter;
    if let Some(w) = app.get_webview_window(VIEWER_LABEL) {
        let _ = app.emit_to(
            VIEWER_LABEL,
            "pinq-view",
            serde_json::json!({ "noteId": note_id, "hash": hash }),
        );
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    let cursor = app.cursor_position().unwrap_or(tauri::PhysicalPosition { x: 0.0, y: 0.0 });
    let wa = crate::monitors::work_area_at(&app, cursor.x as i32, cursor.y as i32);
    let scale = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| crate::monitors::primary_scale(&app));
    let w = ((900.0 * scale).round() as u32).min(wa.w.saturating_sub(80));
    let h = ((640.0 * scale).round() as u32).min(wa.h.saturating_sub(80));

    let win = WebviewWindowBuilder::new(
        &app,
        VIEWER_LABEL,
        WebviewUrl::App(format!("index.html?role=viewer&note={note_id}&hash={hash}").into()),
    )
    .title("Pinq Viewer")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(true)
    .min_inner_size(420.0, 320.0)
    .shadow(false)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())?;
    win.set_size(PhysicalSize::new(w, h)).map_err(|e| e.to_string())?;
    win.set_position(PhysicalPosition::new(
        wa.x + (wa.w as i32 - w as i32) / 2,
        wa.y + (wa.h as i32 - h as i32) / 2,
    ))
    .map_err(|e| e.to_string())?;
    win.show().map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    Ok(())
}

#[tauri::command]
pub fn close_viewer(app: AppHandle) {
    if let Some(w) = app.get_webview_window(VIEWER_LABEL) {
        let _ = w.hide();
    }
}

pub fn create_or_focus_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    let wa = crate::monitors::primary_work_area(app);
    let scale = crate::monitors::primary_scale(app);
    let w = (420.0 * scale).round() as u32;
    let h = (660.0 * scale).round() as u32;

    let win = WebviewWindowBuilder::new(
        app,
        SETTINGS_LABEL,
        WebviewUrl::App("index.html?role=settings".into()),
    )
    .title("Pinq Settings")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(true)
    .min_inner_size(340.0, 420.0)
    .shadow(false)
    .visible(false)
    .build()?;

    win.set_size(PhysicalSize::new(w, h))?;
    win.set_position(PhysicalPosition::new(
        wa.x + (wa.w as i32 - w as i32) / 2,
        wa.y + (wa.h as i32 - h as i32) / 2,
    ))?;
    win.show()?;
    let _ = win.set_focus();
    Ok(())
}

pub fn create_or_focus_library(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(LIBRARY_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    let wa = crate::monitors::primary_work_area(app);
    let scale = crate::monitors::primary_scale(app);
    let w = (760.0 * scale).round() as u32;
    let h = (540.0 * scale).round() as u32;

    let win = WebviewWindowBuilder::new(
        app,
        LIBRARY_LABEL,
        WebviewUrl::App("index.html?role=library".into()),
    )
    .title("Pinq — Library")
    .visible(false)
    .build()?;

    win.set_size(PhysicalSize::new(w, h))?;
    win.set_position(PhysicalPosition::new(
        wa.x + (wa.w as i32 - w as i32) / 2,
        wa.y + (wa.h as i32 - h as i32) / 2,
    ))?;
    win.show()?;
    let _ = win.set_focus();
    Ok(())
}

/// Async: creates a window (see pull_end for the main-thread deadlock rule).
#[tauri::command]
pub async fn open_settings(app: AppHandle) -> Result<(), String> {
    create_or_focus_settings(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_library(app: AppHandle) -> Result<(), String> {
    create_or_focus_library(&app).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn close_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = w.hide();
    }
}
