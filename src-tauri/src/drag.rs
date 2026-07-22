//! Custom note dragging with throw-to-unpin (spec §10).
//!
//! The native drag region can't observe velocity, so note headers start a Rust-side
//! drag session instead: a thread polls the cursor (~120 Hz), moves the window, and
//! keeps a rolling sample window. On release the throw classifier decides:
//!   throw  — speed ≥ 1500 logical px/s, direction within ±35° of an edge's outward
//!            normal, projected rest ≥ 40% of the note beyond that edge, and total
//!            drag distance ≥ 80 logical px. The note animates off-screen, is FILED
//!            (never deleted), and an undo toast appears.
//!   coast  — fast-but-not-throw releases coast briefly with friction, clamped to
//!            the work area.
//!   move   — everything else just stays where released.

use crate::store::Store;
use pinq_core::Rect;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State};

const THROW_SPEED: f64 = 1500.0; // logical px/s
const THROW_CONE_DEG: f64 = 35.0;
const THROW_MIN_DRAG: f64 = 80.0; // logical px
const THROW_OVERHANG: f64 = 0.40; // fraction of note past the edge at projected rest
const PROJECT_SECS: f64 = 0.18;
const COAST_MIN_SPEED: f64 = 300.0; // logical px/s
const VELOCITY_WINDOW_MS: u64 = 80;

#[derive(Default)]
pub struct DragState(Mutex<Option<ActiveDrag>>);

#[derive(Clone, Copy, PartialEq)]
pub enum UndoKind {
    Filed,
    Trashed,
}

#[derive(Default)]
pub struct UndoState(pub Mutex<Option<(String, UndoKind)>>);

struct Sample {
    t: Instant,
    x: f64,
    y: f64,
}

struct ActiveDrag {
    note_id: String,
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<VecDeque<Sample>>>,
    start_rect: Rect,
}

pub const TOAST_LABEL: &str = "toast";
const TOAST_W: f64 = 300.0;
const TOAST_H: f64 = 48.0;

pub fn create_toast_window(app: &AppHandle) -> tauri::Result<()> {
    let win = tauri::WebviewWindowBuilder::new(
        app,
        TOAST_LABEL,
        tauri::WebviewUrl::App("index.html?role=toast".into()),
    )
    .title("Pinq")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .shadow(false)
    .visible(false)
    .build()?;
    let _ = win;
    Ok(())
}

#[derive(Clone, serde::Serialize)]
struct ToastPayload {
    id: String,
    kind: String,
}

pub fn show_toast(app: &AppHandle, note_id: &str, kind: UndoKind) {
    let wa = crate::monitors::primary_work_area(app);
    let scale = crate::monitors::primary_scale(app);
    let w = (TOAST_W * scale).round() as i32;
    let h = (TOAST_H * scale).round() as i32;
    let x = wa.x + (wa.w as i32 - w) / 2;
    let y = wa.y + wa.h as i32 - h - (24.0 * scale).round() as i32;
    if let Some(t) = app.get_webview_window(TOAST_LABEL) {
        let _ = t.set_size(tauri::PhysicalSize::new(w as u32, h as u32));
        let _ = t.set_position(PhysicalPosition::new(x, y));
        let _ = app.emit_to(
            TOAST_LABEL,
            "pinq-filed",
            ToastPayload {
                id: note_id.to_string(),
                kind: if kind == UndoKind::Trashed { "trashed".into() } else { "filed".into() },
            },
        );
        let _ = t.show();
    }
}

#[tauri::command]
pub fn note_drag_start(
    app: AppHandle,
    drag: State<'_, DragState>,
    id: String,
) -> Result<(), String> {
    start_drag(&app, &drag, &id)
}

/// Begin a cursor-following drag session on a note's window, deriving the grab
/// offset from where the window currently sits (a header grab).
pub fn start_drag(app: &AppHandle, drag: &DragState, id: &str) -> Result<(), String> {
    let label = crate::notes::window_label(id);
    let win = app.get_webview_window(&label).ok_or("note window missing")?;
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let size = win.inner_size().map_err(|e| e.to_string())?;
    let cursor = app.cursor_position().map_err(|e| e.to_string())?;

    let grab = (cursor.x - pos.x as f64, cursor.y - pos.y as f64);
    let rect = Rect { x: pos.x, y: pos.y, w: size.width, h: size.height };
    start_drag_at(app, drag, id, grab, rect);
    Ok(())
}

/// True while no mouse button is physically held. Polled by the drag thread as
/// a safety net: if the webview that started the drag loses pointer capture
/// (e.g. the freshly opened note window stole focus), its pointerup never
/// arrives and nothing would ever stop the follow loop.
#[cfg(windows)]
fn mouse_buttons_up() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    unsafe {
        (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) == 0
            && (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) == 0
    }
}

/// The drag session itself: registers the ActiveDrag and spawns the follow
/// thread. `grab` is where the cursor sits inside the window — passed
/// explicitly because a just-moved window can still report its old position.
fn start_drag_at(app: &AppHandle, drag: &DragState, id: &str, grab: (f64, f64), start_rect: Rect) {
    let label = crate::notes::window_label(id);
    let stop = Arc::new(AtomicBool::new(false));
    let samples: Arc<Mutex<VecDeque<Sample>>> = Arc::new(Mutex::new(VecDeque::new()));

    *drag.0.lock().unwrap() = Some(ActiveDrag {
        note_id: id.to_string(),
        stop: stop.clone(),
        samples: samples.clone(),
        start_rect,
    });

    let app2 = app.clone();
    std::thread::spawn(move || {
        #[cfg(windows)]
        let mut released_at: Option<Instant> = None;
        while !stop.load(Ordering::Relaxed) {
            if let Ok(c) = app2.cursor_position() {
                {
                    let mut s = samples.lock().unwrap();
                    s.push_back(Sample { t: Instant::now(), x: c.x, y: c.y });
                    let cutoff = Instant::now() - Duration::from_millis(150);
                    while s.front().map(|f| f.t < cutoff).unwrap_or(false) {
                        s.pop_front();
                    }
                }
                if let Some(w) = app2.get_webview_window(&label) {
                    let _ = w.set_position(PhysicalPosition::new(
                        (c.x - grab.0).round() as i32,
                        (c.y - grab.1).round() as i32,
                    ));
                }
            }
            // Safety net: physical button released but no end command came
            // (lost capture). Give the normal path a grace window, then
            // finish the drag ourselves — whoever takes the session wins.
            #[cfg(windows)]
            if mouse_buttons_up() {
                match released_at {
                    None => released_at = Some(Instant::now()),
                    Some(t) if t.elapsed() > Duration::from_millis(250) => {
                        let _ = finish_drag(&app2);
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
}

/// Grab a card in the library and carry the real note out to the desktop: the
/// note is untucked/repinned as needed, its window parks under the cursor, and
/// the standard drag session takes over — releasing drops it where it is, and
/// a fling at a screen edge tucks it, exactly like a header drag.
/// Async: may create a window (never on the main thread in a sync command).
#[tauri::command]
pub async fn library_pull_start(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    drag: State<'_, DragState>,
    id: String,
) -> Result<(), String> {
    let Some(note) = store.get(&id) else { return Err("note not found".into()) };
    if note.trashed {
        return Err("note is in the trash".into());
    }

    // Park the window with the cursor on the header, like it was just grabbed.
    let cursor = app.cursor_position().map_err(|e| e.to_string())?;
    let (w, h) = (note.rect.w, note.rect.h);
    let rect = Rect {
        x: (cursor.x - w as f64 / 2.0).round() as i32,
        y: (cursor.y - 14.0).round() as i32,
        w,
        h,
    };

    if note.tucked.is_some() {
        store.untuck_note(&id, rect).map_err(|e| e.to_string())?;
    } else {
        store.repin(&id).map_err(|e| e.to_string())?; // no-op if already pinned
    }

    let label = crate::notes::window_label(&id);
    match app.get_webview_window(&label) {
        Some(win) => {
            let _ = win.set_position(PhysicalPosition::new(rect.x, rect.y));
            let _ = win.show();
        }
        None => {
            let mut note = note;
            note.rect = rect;
            crate::notes::open_note_window(&app, &note).map_err(|e| e.to_string())?;
        }
    }
    // Grab offset is where we PUT the cursor relative to the window (the same
    // offsets `rect` was built from) — never read the window position back
    // here: a just-moved window can still report its old spot, which would
    // park the note beside the cursor for the whole drag.
    let grab = (rect.w as f64 / 2.0, 14.0);
    start_drag_at(&app, &drag, &id, grab, rect);
    Ok(())
}

/// Velocity in physical px/s over the trailing sample window.
fn end_velocity(samples: &VecDeque<Sample>) -> (f64, f64) {
    let Some(last) = samples.back() else { return (0.0, 0.0) };
    let cutoff = last.t - Duration::from_millis(VELOCITY_WINDOW_MS);
    let first = samples.iter().find(|s| s.t >= cutoff).unwrap_or(last);
    let dt = last.t.duration_since(first.t).as_secs_f64();
    if dt < 0.005 {
        return (0.0, 0.0);
    }
    ((last.x - first.x) / dt, (last.y - first.y) / dt)
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
    }
}

/// Spec §10 classifier. Returns the edge being thrown at, if any.
fn classify_throw(
    wa: Rect,
    scale: f64,
    rect: Rect,
    v: (f64, f64),
    drag_dist: f64,
) -> Option<Edge> {
    let speed = (v.0 * v.0 + v.1 * v.1).sqrt();
    if speed < THROW_SPEED * scale || drag_dist < THROW_MIN_DRAG * scale {
        return None;
    }
    let cone = (THROW_CONE_DEG.to_radians()).cos();
    let (vx, vy) = (v.0 / speed, v.1 / speed);
    // Projected rest position of the note after PROJECT_SECS of coasting.
    let px = rect.x as f64 + v.0 * PROJECT_SECS;
    let py = rect.y as f64 + v.1 * PROJECT_SECS;
    let (w, h) = (rect.w as f64, rect.h as f64);
    let (wa_l, wa_t) = (wa.x as f64, wa.y as f64);
    let (wa_r, wa_b) = (wa.x as f64 + wa.w as f64, wa.y as f64 + wa.h as f64);

    // (edge, alignment with outward normal, overhang fraction beyond the edge)
    let candidates = [
        (Edge::Right, vx, (px + w - wa_r) / w),
        (Edge::Left, -vx, (wa_l - px) / w),
        (Edge::Bottom, vy, (py + h - wa_b) / h),
        (Edge::Top, -vy, (wa_t - py) / h),
    ];
    candidates
        .into_iter()
        .filter(|(_, align, overhang)| *align >= cone && *overhang >= THROW_OVERHANG)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(edge, _, _)| edge)
}

#[tauri::command]
pub fn note_drag_end(app: AppHandle) -> Result<(), String> {
    finish_drag(&app)
}

/// End the active drag session (classify throw/coast/move and settle the
/// note). Called by the command on release, or by the follow thread's safety
/// net; taking the session out of DragState makes the two race-free.
pub fn finish_drag(app: &AppHandle) -> Result<(), String> {
    let active = app.state::<DragState>().0.lock().unwrap().take();
    let Some(active) = active else { return Ok(()) };
    active.stop.store(true, Ordering::Relaxed);
    let store = app.state::<Arc<Store>>().inner().clone();
    let app = app.clone();

    let id = active.note_id.clone();
    // Wherever a drag happens is the note's home virtual desktop from now on.
    #[cfg(windows)]
    crate::desktops::assign_home(&app, &id, true);
    let label = crate::notes::window_label(&id);
    let win = app.get_webview_window(&label).ok_or("note window missing")?;
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let size = win.inner_size().map_err(|e| e.to_string())?;
    let rect = Rect { x: pos.x, y: pos.y, w: size.width, h: size.height };

    let samples = active.samples.lock().unwrap();
    let v = end_velocity(&samples);
    drop(samples);
    // Cursor travel equals window-origin travel while the grab offset is held.
    let drag_dist = {
        let (ox, oy) = (
            rect.x as f64 - active.start_rect.x as f64,
            rect.y as f64 - active.start_rect.y as f64,
        );
        (ox * ox + oy * oy).sqrt()
    };

    // Classify against the monitor the note is actually on, not the primary.
    let wa = crate::monitors::work_area_at(
        &app,
        rect.x + rect.w as i32 / 2,
        rect.y + rect.h as i32 / 2,
    );
    let scale = win
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| crate::monitors::primary_scale(&app));

    if let Some(edge) = classify_throw(wa, scale, rect, v, drag_dist) {
        // Throw: the note sticks to that edge as a sliver (spec change: thrown
        // notes stay out, tucked, instead of filing to the library).
        let store = store.clone();
        let app2 = app.clone();
        std::thread::spawn(move || {
            let _ = app2.emit_to(&label, "pinq-flush", ());
            let target = crate::notes::tucked_rect(edge.name(), rect, wa, scale);
            // Quick slide to the pocket.
            let (sx, sy) = (rect.x as f64, rect.y as f64);
            let (dx, dy) = (target.x as f64 - sx, target.y as f64 - sy);
            for i in 1..=12 {
                let t = i as f64 / 12.0;
                let ease = 1.0 - (1.0 - t) * (1.0 - t);
                if let Some(w) = app2.get_webview_window(&label) {
                    let _ = w.set_position(PhysicalPosition::new(
                        (sx + dx * ease).round() as i32,
                        (sy + dy * ease).round() as i32,
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = store.tuck_note(&id, edge.name(), target);
            let _ = app2.emit_to(&label, "pinq-tucked", edge.name());
        });
        return Ok(());
    }

    let speed = (v.0 * v.0 + v.1 * v.1).sqrt();
    if speed >= COAST_MIN_SPEED * scale {
        // Coast with friction, clamped inside the work area.
        let app2 = app.clone();
        let store = store.clone();
        std::thread::spawn(move || {
            let (mut x, mut y) = (rect.x as f64, rect.y as f64);
            let (mut vx, mut vy) = v;
            for _ in 0..20 {
                x += vx * 0.016;
                y += vy * 0.016;
                vx *= 0.8;
                vy *= 0.8;
                let clamped = crate::notes::clamp_to_area(
                    Rect { x: x.round() as i32, y: y.round() as i32, w: rect.w, h: rect.h },
                    wa,
                );
                if let Some(w) = app2.get_webview_window(&label) {
                    let _ = w.set_position(PhysicalPosition::new(clamped.x, clamped.y));
                }
                if (vx * vx + vy * vy).sqrt() < 60.0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            if let Some(w) = app2.get_webview_window(&label) {
                if let (Ok(p), Ok(s)) = (w.outer_position(), w.inner_size()) {
                    store.queue_place(&id, Rect { x: p.x, y: p.y, w: s.width, h: s.height });
                }
            }
        });
        return Ok(());
    }

    store.queue_place(&id, rect);
    Ok(())
}

#[tauri::command]
pub fn note_drag_cancel(app: AppHandle, drag: State<'_, DragState>) -> Result<(), String> {
    let Some(active) = drag.0.lock().unwrap().take() else { return Ok(()) };
    active.stop.store(true, Ordering::Relaxed);
    let label = crate::notes::window_label(&active.note_id);
    if let Some(w) = app.get_webview_window(&label) {
        let r = active.start_rect;
        let _ = w.set_position(PhysicalPosition::new(r.x, r.y));
    }
    Ok(())
}

/// Undo the last file/trash. Async: re-opens a note window; see pull_end for the
/// main-thread deadlock rule.
#[tauri::command]
pub async fn undo_last(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    undo: State<'_, UndoState>,
) -> Result<(), String> {
    let Some((id, kind)) = undo.0.lock().unwrap().take() else { return Ok(()) };
    if kind == UndoKind::Trashed {
        store.restore_note(&id).map_err(|e| e.to_string())?;
    }
    if let Some(note) = store.repin(&id).map_err(|e| e.to_string())? {
        let wa = crate::monitors::primary_work_area(&app);
        let mut note = note;
        note.rect = crate::notes::clamp_to_area(note.rect, wa);
        crate::notes::open_note_window(&app, &note).map_err(|e| e.to_string())?;
    }
    if let Some(t) = app.get_webview_window(TOAST_LABEL) {
        let _ = t.hide();
    }
    Ok(())
}

#[tauri::command]
pub fn toast_dismiss(app: AppHandle) -> Result<(), String> {
    if let Some(t) = app.get_webview_window(TOAST_LABEL) {
        let _ = t.hide();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WA: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    fn note(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 280, h: 220 }
    }

    #[test]
    fn fast_outward_release_near_edge_throws() {
        let edge = classify_throw(WA, 1.0, note(1500, 400), (2000.0, 0.0), 200.0);
        assert!(matches!(edge, Some(Edge::Right)));
    }

    #[test]
    fn slow_release_never_throws() {
        assert!(classify_throw(WA, 1.0, note(1500, 400), (800.0, 0.0), 200.0).is_none());
    }

    #[test]
    fn short_twitch_never_throws() {
        assert!(classify_throw(WA, 1.0, note(1500, 400), (2000.0, 0.0), 40.0).is_none());
    }

    #[test]
    fn fast_but_far_from_edge_does_not_throw() {
        // Projected rest lands well inside the work area.
        assert!(classify_throw(WA, 1.0, note(800, 400), (2000.0, 0.0), 200.0).is_none());
    }

    #[test]
    fn direction_outside_cone_does_not_throw() {
        // Moving fast downward next to the right edge: aligned with no throwable edge.
        assert!(classify_throw(WA, 1.0, note(1500, 100), (0.0, 2000.0), 200.0).is_none());
    }

    #[test]
    fn diagonal_within_cone_throws() {
        // ~21° off the right edge's outward normal, inside the ±35° cone.
        let edge = classify_throw(WA, 1.0, note(1500, 400), (1800.0, 700.0), 200.0);
        assert!(matches!(edge, Some(Edge::Right)));
    }

    #[test]
    fn left_edge_throw_detected() {
        let edge = classify_throw(WA, 1.0, note(100, 400), (-2000.0, 0.0), 200.0);
        assert!(matches!(edge, Some(Edge::Left)));
    }

    #[test]
    fn scale_raises_thresholds() {
        // 2000 px/s is a throw at 1.0 scale but not at 2.0 scale (needs 3000 physical).
        assert!(classify_throw(WA, 2.0, note(1500, 400), (2000.0, 0.0), 400.0).is_none());
    }

    #[test]
    fn velocity_from_trailing_window() {
        let t0 = Instant::now();
        let mut samples = VecDeque::new();
        for i in 0..10u32 {
            samples.push_back(Sample {
                t: t0 + Duration::from_millis(i as u64 * 10),
                x: (i * 20) as f64, // 20px per 10ms = 2000 px/s
                y: 0.0,
            });
        }
        let (vx, vy) = end_velocity(&samples);
        assert!((vx - 2000.0).abs() < 100.0, "vx was {vx}");
        assert!(vy.abs() < 1.0);
    }
}
