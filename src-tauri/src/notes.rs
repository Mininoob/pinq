//! Desktop note windows: creation, restore-on-launch, and the note commands.

use crate::store::Store;
use pinq_core::{NoteState, Rect};
use std::sync::Arc;
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, WebviewWindowBuilder,
};

const LABEL_PREFIX: &str = "note-";

/// Pinned notes whose saved position is on a disconnected display. They stay
/// pinned (spec: nothing is ever lost by unplugging a monitor) but have no
/// visible window until their display returns.
#[derive(Default)]
pub struct ParkedState(pub std::sync::Mutex<std::collections::HashSet<String>>);

pub fn window_label(id: &str) -> String {
    format!("{LABEL_PREFIX}{id}")
}

pub fn label_to_id(label: &str) -> Option<&str> {
    label.strip_prefix(LABEL_PREFIX)
}

/// Keep at least a grabbable strip of the note inside the work area.
pub fn clamp_to_area(mut rect: Rect, wa: Rect) -> Rect {
    let min_visible = 60;
    let max_x = wa.x + wa.w as i32 - min_visible;
    let max_y = wa.y + wa.h as i32 - min_visible;
    rect.x = rect.x.clamp(wa.x - rect.w as i32 + min_visible, max_x);
    rect.y = rect.y.clamp(wa.y, max_y);
    rect
}

/// How much of a tucked note peeks out of the edge (logical px).
const TUCK_PEEK: f64 = 16.0;
/// Extra slide-out while hovering, enough to read the title chip.
const TUCK_HOVER: f64 = 132.0;

/// The parked rect for a note thrown at `edge`: almost entirely off-screen,
/// with a TUCK_PEEK sliver showing, aligned to where it was released.
pub fn tucked_rect(edge: &str, rect: Rect, wa: Rect, scale: f64) -> Rect {
    let peek = (TUCK_PEEK * scale).round() as i32;
    let (w, h) = (rect.w as i32, rect.h as i32);
    let mut r = rect;
    match edge {
        "left" => r.x = wa.x - w + peek,
        "right" => r.x = wa.x + wa.w as i32 - peek,
        "top" => r.y = wa.y - h + peek,
        _ => r.y = wa.y + wa.h as i32 - peek,
    }
    // Keep the sliver's cross-axis fully on this monitor.
    match edge {
        "left" | "right" => r.y = r.y.clamp(wa.y, wa.y + wa.h as i32 - h),
        _ => r.x = r.x.clamp(wa.x, wa.x + wa.w as i32 - w),
    }
    r
}

/// Hover slide: the tucked rect shifted inward by TUCK_HOVER so the title
/// strip becomes readable; `back` returns it to the sliver.
fn peeked_rect(edge: &str, tucked: Rect, scale: f64, out: bool) -> Rect {
    let d = if out { (TUCK_HOVER * scale).round() as i32 } else { 0 };
    let mut r = tucked;
    match edge {
        "left" => r.x += d,
        "right" => r.x -= d,
        "top" => r.y += d,
        _ => r.y -= d,
    }
    r
}

pub fn open_note_window(app: &AppHandle, note: &NoteState) -> tauri::Result<()> {
    let label = window_label(&note.id);
    let win = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html?role=note&id={}", note.id).into()),
    )
    .title("Pinq Note")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .min_inner_size(160.0, 120.0)
    .shadow(false)
    .visible(false)
    .build()?;

    win.set_size(PhysicalSize::new(note.rect.w, note.rect.h))?;
    win.set_position(PhysicalPosition::new(note.rect.x, note.rect.y))?;
    win.show()?;
    let _ = win.set_focus();
    // New notes call the current virtual desktop home; restored notes keep
    // the home they already had.
    #[cfg(windows)]
    crate::desktops::assign_home(app, &note.id, false);
    tuck_follows_spaces(&win, note.tucked.is_some());
    Ok(())
}

/// macOS Spaces behavior: a TUCKED note follows the user everywhere (its
/// sliver joins all Spaces, like the pad), while an untucked note belongs to
/// the Space it lives on. Windows has its own per-virtual-desktop system
/// (desktops.rs) — no-op there.
pub fn tuck_follows_spaces(win: &tauri::WebviewWindow, tucked: bool) {
    #[cfg(target_os = "macos")]
    let _ = win.set_visible_on_all_workspaces(tucked);
    #[cfg(not(target_os = "macos"))]
    let _ = (win, tucked);
}

fn rect_center(r: &pinq_core::Rect) -> (i32, i32) {
    (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
}

/// The point that decides which monitor a note "is on". For a tucked note the
/// center is off-screen by design, so probe just inside its visible sliver.
fn anchor_point(n: &NoteState) -> (i32, i32) {
    let (cx, cy) = rect_center(&n.rect);
    match n.tucked.as_deref() {
        Some("left") => (n.rect.x + n.rect.w as i32 - 8, cy),
        Some("right") => (n.rect.x + 8, cy),
        Some("top") => (cx, n.rect.y + n.rect.h as i32 - 8),
        Some(_) => (cx, n.rect.y + 8),
        None => (cx, cy),
    }
}

/// Restore pinned notes to the monitor they were on; park the ones whose
/// display isn't connected right now. Tucked notes restore at their exact
/// sliver rect — clamping would drag them back on-screen.
pub fn restore_pinned(app: &AppHandle, store: &Arc<Store>) -> tauri::Result<()> {
    for note in store.pinned_notes() {
        let mut note = note;
        let (ax, ay) = anchor_point(&note);
        match crate::monitors::work_area_containing(app, ax, ay) {
            Some(wa) => {
                if note.tucked.is_none() {
                    note.rect = clamp_to_area(note.rect, wa);
                }
                open_note_window(app, &note)?;
            }
            None => {
                app.state::<ParkedState>().0.lock().unwrap().insert(note.id.clone());
            }
        }
    }
    Ok(())
}

/// Reconcile parked notes with the current monitor topology. Called from the
/// watchdog thread (never the main thread — it creates windows).
pub fn sync_parked(app: &AppHandle) {
    let Some(store) = app.try_state::<Arc<Store>>() else { return };
    let parked = app.state::<ParkedState>();
    let mut changed = false;

    for note in store.pinned_notes() {
        let (ax, ay) = anchor_point(&note);
        let containing = crate::monitors::work_area_containing(app, ax, ay);
        let label = window_label(&note.id);

        match containing {
            Some(wa) => {
                let was_parked = parked.0.lock().unwrap().remove(&note.id);
                if was_parked {
                    changed = true;
                    let mut note = note.clone();
                    if note.tucked.is_none() {
                        note.rect = clamp_to_area(note.rect, wa);
                    }
                    match app.get_webview_window(&label) {
                        Some(w) => {
                            let _ = w.set_position(tauri::PhysicalPosition::new(
                                note.rect.x,
                                note.rect.y,
                            ));
                            let _ = w.show();
                        }
                        None => {
                            let _ = open_note_window(app, &note);
                        }
                    }
                }
            }
            None => {
                if parked.0.lock().unwrap().insert(note.id.clone()) {
                    changed = true;
                    if let Some(w) = app.get_webview_window(&label) {
                        let _ = w.hide();
                    }
                }
            }
        }
    }

    // Let the visibility layers (fullscreen / user-hidden) settle the final state.
    if changed {
        crate::visibility::apply(app);
    }
}

/// After a sync applied remote ops: presentation is per-device now, so remote
/// changes never open windows or move notes here — a note created elsewhere
/// lands in the library only. The one lifecycle ripple: a note trashed or
/// purged remotely closes its local window (deleting is data, not layout).
/// Content in open windows refreshes via the pinq-sync-applied event.
/// Called from sync threads only — never the main thread (window teardown).
pub fn reconcile_after_sync(app: &AppHandle, affected: &[String]) {
    let Some(store) = app.try_state::<Arc<Store>>() else { return };
    let parked = app.state::<ParkedState>();
    let mut changed = false;

    for id in affected {
        // Unpin/clean the local overlay for remotely trashed/purged notes.
        store.reconcile_presentation(id);
        let gone = match store.get(id) {
            None => true,
            Some(n) => n.trashed,
        };
        if gone {
            parked.0.lock().unwrap().remove(id);
            if let Some(w) = app.get_webview_window(&window_label(id)) {
                let _ = w.destroy();
                changed = true;
            }
        }
    }

    if changed {
        crate::visibility::apply(app);
    }
}

#[tauri::command]
pub fn get_note(store: State<'_, Arc<Store>>, id: String) -> Option<NoteState> {
    store.get(&id)
}

#[tauri::command]
pub fn get_all_notes(store: State<'_, Arc<Store>>) -> Vec<NoteState> {
    store.all_notes()
}

#[derive(serde::Serialize)]
pub struct HistoryEntry {
    pub at_ms: u64,
    pub text: String,
}

/// Past text versions of a note, newest first.
#[tauri::command]
pub fn get_note_history(
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<Vec<HistoryEntry>, String> {
    Ok(store
        .text_history(&id, 50)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|(at_ms, text)| HistoryEntry { at_ms, text })
        .collect())
}

/// FTS-backed search (token-prefix match, best match first, trash excluded).
#[tauri::command]
pub async fn search_notes(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    query: String,
) -> Result<Vec<NoteState>, String> {
    let mut hits = store.search(&query, 10).map_err(|e| e.to_string())?;
    // Word matches first (they're usually the intent), then semantic hits
    // fill the remaining slots — "dentist" finds "book a checkup".
    if !query.trim().is_empty() && hits.len() < 10 {
        let have: std::collections::HashSet<String> = hits.iter().map(|n| n.id.clone()).collect();
        for id in crate::smart::semantic_search_ids(&app, &query, 10) {
            if hits.len() >= 10 {
                break;
            }
            if have.contains(&id) {
                continue;
            }
            if let Some(n) = store.get(&id).filter(|n| !n.trashed) {
                hits.push(n);
            }
        }
    }
    Ok(hits)
}

/// Move a note to trash: window closes, note is recoverable from the library's
/// Trash section, and an undo toast appears. Async: window teardown.
#[tauri::command]
pub async fn trash_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    undo: State<'_, crate::drag::UndoState>,
    id: String,
) -> Result<(), String> {
    store.trash_note(&id).map_err(|e| e.to_string())?;
    if let Some(win) = app.get_webview_window(&window_label(&id)) {
        let _ = win.destroy();
    }
    *undo.0.lock().unwrap() = Some((id.clone(), crate::drag::UndoKind::Trashed));
    crate::drag::show_toast(&app, &id, crate::drag::UndoKind::Trashed);
    Ok(())
}

#[tauri::command]
pub fn restore_note(store: State<'_, Arc<Store>>, id: String) -> Result<(), String> {
    store.restore_note(&id).map_err(|e| e.to_string())
}

/// Widen a note window so a no-wrap text line stays visible. Grows only
/// (shrinking is always the user's call), clamped to the monitor work area.
/// Async: window sizing off the main thread. The resize flows through the
/// normal move/resize pipeline, so the new size persists and syncs.
#[tauri::command]
pub async fn grow_note_window(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    width_css: f64,
) -> Result<(), String> {
    match store.get(&id) {
        Some(n) if n.tucked.is_none() => {}
        _ => return Ok(()), // tucked slivers and unknown notes never grow
    }
    let Some(win) = app.get_webview_window(&window_label(&id)) else { return Ok(()) };
    let scale = win.scale_factor().map_err(|e| e.to_string())?;
    let size = win.outer_size().map_err(|e| e.to_string())?;
    let desired = (width_css * scale).round() as u32;
    if desired <= size.width {
        return Ok(());
    }
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let (cx, cy) = (pos.x + size.width as i32 / 2, pos.y + size.height as i32 / 2);
    let max_w = crate::monitors::work_area_containing(&app, cx, cy)
        .map(|wa| (wa.x + wa.w as i32 - pos.x).max(0) as u32)
        .unwrap_or(desired);
    let new_w = desired.min(max_w);
    if new_w > size.width {
        win.set_size(PhysicalSize::new(new_w, size.height)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Create a text box on the note's canvas at (x, y).
#[tauri::command]
pub fn add_text_item(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    x: f64,
    y: f64,
    w: f64,
    text: String,
) -> Result<pinq_core::CanvasItem, String> {
    let item = pinq_core::CanvasItem::text_item(x, y, w, text);
    store.add_item(&id, item.clone()).map_err(|e| e.to_string())?;
    crate::sync::note_edited(&app, &id);
    Ok(item)
}

#[tauri::command]
pub fn set_item_text(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    item_id: String,
    text: String,
) -> Result<(), String> {
    let old_title = store.get(&id).map(|n| crate::links::title_of(&n)).unwrap_or_default();
    store.set_item_text(&id, &item_id, text).map_err(|e| e.to_string())?;
    crate::links::sync_links_after_edit(&app, &store, &id, &old_title);
    crate::sync::note_edited(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn move_item(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    item_id: String,
    x: f64,
    y: f64,
    w: f64,
) -> Result<(), String> {
    store.move_item(&id, &item_id, x, y, w).map_err(|e| e.to_string())?;
    crate::sync::note_edited(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn remove_item(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    item_id: String,
) -> Result<(), String> {
    // Removing an attachment item leaves its blob unreferenced, and GC deletes
    // unreferenced blobs whose file mtime is >24h old — which an old image's
    // blob already is. Touch it so undo has a full protection window.
    if let Some(note) = store.get(&id) {
        let hash = note.items.iter().find(|i| i.item_id == item_id).and_then(|i| {
            match &i.content {
                pinq_core::ItemContent::Attachment { attachment } => Some(attachment.hash.clone()),
                pinq_core::ItemContent::Text { .. } => None,
            }
        });
        if let Some(path) = hash.and_then(|h| crate::blobs::blob_path(&app, &h)) {
            let _ = std::fs::File::options()
                .append(true)
                .open(&path)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        }
    }
    // Removing the title text box changes the note's title — retarget links.
    let old_title = store.get(&id).map(|n| crate::links::title_of(&n)).unwrap_or_default();
    store.remove_item(&id, &item_id).map_err(|e| e.to_string())?;
    crate::links::sync_links_after_edit(&app, &store, &id, &old_title);
    crate::sync::note_edited(&app, &id);
    Ok(())
}

/// Undo of a canvas-item delete: re-adds the exact item, keeping its original
/// item_id. Replay-safe: ItemAdded dedupes by item_id and canonical op order
/// (at_ms, device, ctr) puts this after the removal on every device.
#[tauri::command]
pub fn restore_item(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    item: pinq_core::CanvasItem,
) -> Result<(), String> {
    // Undoing a title-box delete brings the old title back — retarget links.
    let old_title = store.get(&id).map(|n| crate::links::title_of(&n)).unwrap_or_default();
    store.add_item(&id, item).map_err(|e| e.to_string())?;
    crate::links::sync_links_after_edit(&app, &store, &id, &old_title);
    crate::sync::note_edited(&app, &id);
    Ok(())
}

/// Set (ms epoch) or clear (null) a note's reminder. When it fires, the
/// scheduler brings the note back to the desktop and clears it.
#[tauri::command]
pub fn set_reminder(
    store: State<'_, Arc<Store>>,
    id: String,
    remind_ms: Option<u64>,
) -> Result<(), String> {
    store.set_reminder(&id, remind_ms).map_err(|e| e.to_string())
}

/// Set (ms epoch) or clear (null) a task due date.
#[tauri::command]
pub fn set_note_due(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    due_ms: Option<u64>,
) -> Result<(), String> {
    store.set_due(&id, due_ms).map_err(|e| e.to_string())?;
    crate::sync::note_edited(&app, &id);
    Ok(())
}

/// Set ("todo" | "doing" | "done") or clear (null) a task status.
#[tauri::command]
pub fn set_note_status(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    status: Option<String>,
) -> Result<(), String> {
    if let Some(s) = &status {
        if !matches!(s.as_str(), "todo" | "doing" | "done") {
            return Err(format!("unknown status {s:?}"));
        }
    }
    store.set_status(&id, status).map_err(|e| e.to_string())?;
    crate::sync::note_edited(&app, &id);
    Ok(())
}

#[tauri::command]
pub fn tag_note(store: State<'_, Arc<Store>>, id: String, tag: String) -> Result<(), String> {
    store.add_tag(&id, &tag).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn untag_note(store: State<'_, Arc<Store>>, id: String, tag: String) -> Result<(), String> {
    store.remove_tag(&id, &tag).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_note_paper(
    store: State<'_, Arc<Store>>,
    id: String,
    paper: Option<String>,
) -> Result<(), String> {
    store.set_paper(&id, paper).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_note_color(
    store: State<'_, Arc<Store>>,
    id: String,
    color: Option<String>,
) -> Result<(), String> {
    store.set_color(&id, color).map_err(|e| e.to_string())
}

/// Permanent delete — the library confirms with the user before calling this.
#[tauri::command]
pub async fn purge_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<(), String> {
    store.purge_note(&id).map_err(|e| e.to_string())?;
    if let Some(win) = app.get_webview_window(&window_label(&id)) {
        let _ = win.destroy();
    }
    #[cfg(windows)]
    crate::desktops::forget(&app, &id);
    Ok(())
}

/// Empty the trash — the library confirms with the user before calling this.
#[tauri::command]
pub fn empty_trash(store: State<'_, Arc<Store>>) -> Result<usize, String> {
    store.purge_trashed().map_err(|e| e.to_string())
}

/// Hover on a tucked note's sliver: slide it out enough to read the title
/// (`out = true`), or back into the pocket. Presentation only — no op logged.
#[tauri::command]
pub async fn peek_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    out: bool,
) -> Result<(), String> {
    let Some(note) = store.get(&id) else { return Ok(()) };
    let Some(edge) = note.tucked.clone() else { return Ok(()) };
    let Some(win) = app.get_webview_window(&window_label(&id)) else { return Ok(()) };
    let scale = win.scale_factor().unwrap_or(1.0);
    let r = peeked_rect(&edge, note.rect, scale, out);
    let _ = win.set_position(PhysicalPosition::new(r.x, r.y));
    Ok(())
}

/// Click on a tucked note: pull it fully back onto the screen.
#[tauri::command]
pub async fn untuck_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<(), String> {
    untuck_and_focus(&app, &store, &id)
}

/// Slide a tucked note out of its edge pocket and focus it. Blocks briefly for
/// the slide animation — call from async commands or worker threads only.
pub fn untuck_and_focus(app: &AppHandle, store: &Store, id: &str) -> Result<(), String> {
    let Some(note) = store.get(id) else { return Ok(()) };
    let Some(edge) = note.tucked.clone() else { return Ok(()) };
    let Some(win) = app.get_webview_window(&window_label(id)) else { return Ok(()) };

    let (ax, ay) = anchor_point(&note);
    let wa = crate::monitors::work_area_at(app, ax, ay);
    let margin = 12;
    let mut r = note.rect;
    match edge.as_str() {
        "left" => r.x = wa.x + margin,
        "right" => r.x = wa.x + wa.w as i32 - r.w as i32 - margin,
        "top" => r.y = wa.y + margin,
        _ => r.y = wa.y + wa.h as i32 - r.h as i32 - margin,
    }
    r = clamp_to_area(r, wa);
    store.untuck_note(id, r).map_err(|e| e.to_string())?;
    // Untucked: the note belongs to this Space again (macOS).
    tuck_follows_spaces(&win, false);

    // Slide out, then hand focus back to the note.
    let start = win.outer_position().map(|p| (p.x, p.y)).unwrap_or((r.x, r.y));
    for i in 1..=12 {
        let t = i as f64 / 12.0;
        let ease = 1.0 - (1.0 - t) * (1.0 - t);
        let x = start.0 as f64 + (r.x - start.0) as f64 * ease;
        let y = start.1 as f64 + (r.y - start.1) as f64 * ease;
        let _ = win.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = win.set_focus();
    Ok(())
}

/// Pin a filed note back to the desktop (library action). Async: creates a window.
#[tauri::command]
pub async fn pin_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<(), String> {
    let Some(mut note) = store.repin(&id).map_err(|e| e.to_string())? else {
        return Err("note not found".into());
    };
    if let Some(w) = app.get_webview_window(&window_label(&id)) {
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        let wa = crate::monitors::primary_work_area(&app);
        note.rect = clamp_to_area(note.rect, wa);
        open_note_window(&app, &note).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Bring any non-trashed note to the desktop by id: untuck it if it's parked
/// at an edge, repin it if it was filed, focus it if it's already up.
/// May create windows or animate — async commands / worker threads only.
pub fn open_note_by_id(app: &AppHandle, store: &Store, id: &str) -> Result<(), String> {
    let Some(note) = store.get(id) else { return Err("note not found".into()) };
    if note.trashed {
        return Err("note is in the trash".into());
    }
    if note.tucked.is_some() {
        return untuck_and_focus(app, store, id);
    }
    let Some(mut note) = store.repin(id).map_err(|e| e.to_string())? else {
        return Err("note not found".into());
    };
    if let Some(w) = app.get_webview_window(&window_label(id)) {
        let _ = w.show();
        let _ = w.set_focus();
    } else {
        let wa = crate::monitors::primary_work_area(app);
        note.rect = clamp_to_area(note.rect, wa);
        open_note_window(app, &note).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Open/focus a note by id (backlink chips, future views). Async: windows.
#[tauri::command]
pub async fn open_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<(), String> {
    open_note_by_id(&app, &store, &id)
}

#[tauri::command]
pub fn save_note_text(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    text: String,
) -> Result<(), String> {
    let old_title = store.get(&id).map(|n| crate::links::title_of(&n)).unwrap_or_default();
    store.set_text(&id, text).map_err(|e| e.to_string())?;
    crate::links::sync_links_after_edit(&app, &store, &id, &old_title);
    crate::sync::note_edited(&app, &id);
    Ok(())
}

/// Unpin the note (file it to the library) and close its window. The note's data
/// is kept; this is never a delete. Async: window teardown must not run on the
/// main thread inside a command (see pull_end).
#[tauri::command]
pub async fn file_note(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<(), String> {
    store.file_note(&id).map_err(|e| e.to_string())?;
    if let Some(win) = app.get_webview_window(&window_label(&id)) {
        win.destroy().map_err(|e| e.to_string())?;
    }
    Ok(())
}
