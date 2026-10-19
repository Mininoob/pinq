//! Pull-to-create gesture, Rust side.
//!
//! The pad's webview detects the 24px tear threshold and calls `pull_start`; from
//! then on a background thread polls the global cursor (~120 Hz) and moves a
//! pre-created, click-through preview window under it. The note hangs from its
//! top-center — the cursor holds the middle of the header, like gripping a real
//! sticky. `pull_end` classifies the release: ≥48 logical px from the pad's edge
//! creates a real note at the preview rect, closer releases cancel (spec §9).

use crate::store::Store;
use pinq_core::Rect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, WebviewWindowBuilder,
};

pub const PREVIEW_LABEL: &str = "pull-preview";

/// Default note size in logical px (spec §9).
const NOTE_W: f64 = 280.0;
const NOTE_H: f64 = 220.0;
/// Release-distance threshold from the pad edge, logical px.
const RELEASE_THRESHOLD: f64 = 48.0;
/// The grab point sits this far below the note's top edge (mid-header), logical px.
const GRAB_Y: f64 = 13.0;

#[derive(Default)]
pub struct PullState(Mutex<Option<ActivePull>>);

struct ActivePull {
    stop: Arc<AtomicBool>,
    /// Work area of the monitor the initiating pad sits on.
    wa: Rect,
    scale: f64,
    edge: String,
}

impl PullState {
    fn stop_current(&self) -> Option<ActivePull> {
        let active = self.0.lock().unwrap().take();
        if let Some(a) = &active {
            a.stop.store(true, Ordering::Relaxed);
        }
        active
    }
}

/// Created hidden at startup and reused for every pull, so the gesture never waits
/// on webview construction.
pub fn create_preview_window(app: &AppHandle) -> tauri::Result<()> {
    let builder = WebviewWindowBuilder::new(
        app,
        PREVIEW_LABEL,
        WebviewUrl::App("index.html?role=preview".into()),
    )
    .title("Pinq Preview")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .shadow(false)
    .visible(false);
    // Pads live on every Space (macOS); the preview they spawn must too.
    #[cfg(target_os = "macos")]
    let builder = builder.visible_on_all_workspaces(true);
    let win = builder.build()?;
    win.set_ignore_cursor_events(true)?;
    Ok(())
}

/// Note rect for a cursor position: held by the top-center of the header.
fn rect_for_cursor(cx: f64, cy: f64, w: u32, h: u32, scale: f64) -> Rect {
    Rect {
        x: (cx - w as f64 / 2.0).round() as i32,
        y: (cy - GRAB_Y * scale).round() as i32,
        w,
        h,
    }
}

fn note_size_physical(scale: f64) -> (u32, u32) {
    ((NOTE_W * scale).round() as u32, (NOTE_H * scale).round() as u32)
}

/// Distance from the cursor to the pad's docked edge of its work area.
fn distance_from_edge(edge: &str, wa: Rect, cx: f64, cy: f64) -> f64 {
    match edge {
        "left" => cx - wa.x as f64,
        "top" => cy - wa.y as f64,
        "bottom" => (wa.y + wa.h as i32) as f64 - cy,
        _ => (wa.x + wa.w as i32) as f64 - cx,
    }
}

#[tauri::command]
pub fn pull_start(
    window: tauri::WebviewWindow,
    app: AppHandle,
    pull: State<'_, PullState>,
) -> Result<(), String> {
    pull.stop_current();

    // The initiating pad's monitor defines the work area, scale, and edge.
    let monitor = window.current_monitor().ok().flatten();
    let (wa, scale) = match &monitor {
        Some(m) => (crate::monitors::work_area_of(m), m.scale_factor()),
        None => (
            crate::monitors::primary_work_area(&app),
            crate::monitors::primary_scale(&app),
        ),
    };
    let edge = app.state::<crate::settings::SettingsStore>().current().pad_edge;

    let (w, h) = note_size_physical(scale);
    let cursor = app.cursor_position().map_err(|e| e.to_string())?;
    let rect = rect_for_cursor(cursor.x, cursor.y, w, h, scale);

    let preview = app
        .get_webview_window(PREVIEW_LABEL)
        .ok_or("preview window missing")?;
    preview.set_size(PhysicalSize::new(w, h)).map_err(|e| e.to_string())?;
    preview
        .set_position(PhysicalPosition::new(rect.x, rect.y))
        .map_err(|e| e.to_string())?;
    preview.show().map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    *pull.0.lock().unwrap() = Some(ActivePull { stop: stop.clone(), wa, scale, edge });

    let app2 = app.clone();
    std::thread::spawn(move || {
        #[cfg(any(windows, target_os = "macos"))]
        let mut released_at: Option<std::time::Instant> = None;
        while !stop.load(Ordering::Relaxed) {
            if let (Ok(c), Some(p)) = (app2.cursor_position(), app2.get_webview_window(PREVIEW_LABEL)) {
                let r = rect_for_cursor(c.x, c.y, w, h, scale);
                let _ = p.set_position(PhysicalPosition::new(r.x, r.y));
            }
            // Safety net (same rule as the note drag loop): button released but
            // no pull_end arrived — lost capture. Cancel and hide the preview.
            #[cfg(any(windows, target_os = "macos"))]
            if crate::drag::mouse_buttons_up() {
                match released_at {
                    None => released_at = Some(std::time::Instant::now()),
                    Some(t) if t.elapsed() > Duration::from_millis(250) => {
                        app2.state::<PullState>().stop_current();
                        if let Some(p) = app2.get_webview_window(PREVIEW_LABEL) {
                            let _ = p.hide();
                        }
                        break;
                    }
                    Some(_) => {}
                }
            } else {
                released_at = None;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    });

    Ok(())
}

/// Returns the created note id, or null when the release was too close to the pad.
/// Async so it runs off the main thread: window creation from a sync command
/// deadlocks the Windows event loop (it can't pump while the command blocks it).
#[tauri::command]
pub async fn pull_end(
    app: AppHandle,
    pull: State<'_, PullState>,
    store: State<'_, Arc<Store>>,
) -> Result<Option<String>, String> {
    let Some(active) = pull.stop_current() else { return Ok(None) };
    let preview = app.get_webview_window(PREVIEW_LABEL);

    let cursor = app.cursor_position().map_err(|e| e.to_string())?;
    let distance = distance_from_edge(&active.edge, active.wa, cursor.x, cursor.y);

    if distance < RELEASE_THRESHOLD * active.scale {
        if let Some(p) = preview {
            let _ = p.hide();
        }
        return Ok(None);
    }

    let (w, h) = note_size_physical(active.scale);
    let rect = crate::notes::clamp_to_area(
        rect_for_cursor(cursor.x, cursor.y, w, h, active.scale),
        active.wa,
    );
    let note = store.create_note(rect).map_err(|e| e.to_string())?;
    crate::notes::open_note_window(&app, &note).map_err(|e| e.to_string())?;
    if let Some(p) = preview {
        let _ = p.hide();
    }
    Ok(Some(note.id))
}

/// Create (and open) a note just inward of the pad, aligned with the cursor,
/// optionally seeded with template text boxes. Shared by click-to-create and
/// the pad card's template rows.
fn create_note_near_pad(
    window: &tauri::WebviewWindow,
    app: &AppHandle,
    store: &Arc<Store>,
    seeds: &[&str],
) -> Result<String, String> {
    let monitor = window.current_monitor().ok().flatten();
    let (wa, scale) = match &monitor {
        Some(m) => (crate::monitors::work_area_of(m), m.scale_factor()),
        None => (
            crate::monitors::primary_work_area(app),
            crate::monitors::primary_scale(app),
        ),
    };
    let edge = app.state::<crate::settings::SettingsStore>().current().pad_edge;
    let cursor = app.cursor_position().map_err(|e| e.to_string())?;

    let (w, h) = note_size_physical(scale);
    let gap = (24.0 * scale).round() as i32;
    let (x, y) = match edge.as_str() {
        "left" => (wa.x + gap, cursor.y as i32 - h as i32 / 2),
        "top" => (cursor.x as i32 - w as i32 / 2, wa.y + gap),
        "bottom" => (
            cursor.x as i32 - w as i32 / 2,
            wa.y + wa.h as i32 - gap - h as i32,
        ),
        _ => (wa.x + wa.w as i32 - gap - w as i32, cursor.y as i32 - h as i32 / 2),
    };
    let rect = crate::notes::clamp_to_area(Rect { x, y, w, h }, wa);
    let note = store.create_note(rect).map_err(|e| e.to_string())?;
    let mut sy = 8.0;
    for seed in seeds {
        let item = pinq_core::CanvasItem::text_item(8.0, sy, 160.0, seed.to_string());
        sy += 20.0 * (seed.lines().count().max(1) as f64) + 12.0;
        store.add_item(&note.id, item).map_err(|e| e.to_string())?;
    }
    crate::notes::open_note_window(app, store.get(&note.id).as_ref().unwrap_or(&note))
        .map_err(|e| e.to_string())?;
    Ok(note.id)
}

/// Accessibility path (spec §9): a plain click on the pad creates a note just
/// inward of the pad, aligned with the cursor. Async: creates a window.
#[tauri::command]
pub async fn click_create_note(
    window: tauri::WebviewWindow,
    app: AppHandle,
    store: State<'_, Arc<Store>>,
) -> Result<String, String> {
    create_note_near_pad(&window, &app, &store, &[])
}

/// Quick capture from the pad card (spec S7/S9): a note pre-seeded with a
/// built-in template's text boxes, caret-ready.
#[tauri::command]
pub async fn create_note_from_template(
    window: tauri::WebviewWindow,
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    kind: String,
) -> Result<String, String> {
    let seeds: &[&str] = match kind.as_str() {
        "todo" => &["Todo", "\u{2610} "],
        "meeting" => &["Meeting \u{2014} ", "Attendees: ", "Notes\n\u{2022} ", "Actions\n\u{2610} "],
        _ => &[],
    };
    let id = create_note_near_pad(&window, &app, &store, seeds)?;
    if kind == "todo" {
        let _ = store.set_status(&id, Some("todo".into()));
    }
    crate::sync::note_edited(&app, &id);
    Ok(id)
}

#[tauri::command]
pub fn pull_cancel(app: AppHandle, pull: State<'_, PullState>) -> Result<(), String> {
    pull.stop_current();
    if let Some(p) = app.get_webview_window(PREVIEW_LABEL) {
        let _ = p.hide();
    }
    Ok(())
}
