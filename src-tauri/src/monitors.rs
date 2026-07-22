//! Monitor topology helpers. Prototype scope: primary monitor only.
//! The work area (screen minus taskbar/docked bars) comes from Win32 on Windows;
//! elsewhere we fall back to full monitor bounds.

use pinq_core::Rect;
use tauri::AppHandle;

pub fn primary_work_area(app: &AppHandle) -> Rect {
    #[cfg(windows)]
    if let Some(r) = windows_primary_work_area() {
        return r;
    }
    if let Ok(Some(m)) = app.primary_monitor() {
        let p = m.position();
        let s = m.size();
        return Rect { x: p.x, y: p.y, w: s.width, h: s.height };
    }
    Rect { x: 0, y: 0, w: 1920, h: 1080 }
}

/// Work area of a specific monitor (falls back to its full bounds).
pub fn work_area_of(monitor: &tauri::Monitor) -> Rect {
    let p = monitor.position();
    let s = monitor.size();
    #[cfg(windows)]
    if let Some(r) = windows_work_area_at(p.x + 1, p.y + 1) {
        return r;
    }
    Rect { x: p.x, y: p.y, w: s.width, h: s.height }
}

/// Work area of the monitor strictly containing a point — None if the point is
/// on no connected display (unlike `work_area_at`, which snaps to the nearest).
pub fn work_area_containing(app: &AppHandle, x: i32, y: i32) -> Option<Rect> {
    for m in app.available_monitors().unwrap_or_default() {
        let p = m.position();
        let s = m.size();
        if x >= p.x && x < p.x + s.width as i32 && y >= p.y && y < p.y + s.height as i32 {
            return Some(work_area_of(&m));
        }
    }
    None
}

/// Work area of the monitor containing a point (falls back to primary).
pub fn work_area_at(app: &AppHandle, x: i32, y: i32) -> Rect {
    #[cfg(windows)]
    if let Some(r) = windows_work_area_at(x, y) {
        return r;
    }
    primary_work_area(app)
}

pub fn primary_scale(app: &AppHandle) -> f64 {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0)
}

#[cfg(windows)]
fn windows_primary_work_area() -> Option<Rect> {
    use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY;
    windows_work_area_from(0, 0, MONITOR_DEFAULTTOPRIMARY)
}

#[cfg(windows)]
fn windows_work_area_at(x: i32, y: i32) -> Option<Rect> {
    use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST;
    windows_work_area_from(x, y, MONITOR_DEFAULTTONEAREST)
}

#[cfg(windows)]
fn windows_work_area_from(
    x: i32,
    y: i32,
    flags: windows::Win32::Graphics::Gdi::MONITOR_FROM_FLAGS,
) -> Option<Rect> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO};

    unsafe {
        let hmon = MonitorFromPoint(POINT { x, y }, flags);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut info).as_bool() {
            return None;
        }
        let r = info.rcWork;
        Some(Rect {
            x: r.left,
            y: r.top,
            w: (r.right - r.left).max(0) as u32,
            h: (r.bottom - r.top).max(0) as u32,
        })
    }
}
