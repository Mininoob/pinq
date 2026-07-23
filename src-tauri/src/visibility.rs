//! Visibility layers for the pad and notes.
//!
//! Two independent reasons to hide exist: the user pressed Ctrl+Alt+H
//! (`user_hidden`, notes only) and a fullscreen app is active (`fs_hidden`,
//! notes AND pad). A window is visible only when no active layer hides it, so
//! leaving fullscreen never reveals notes the user had hidden themselves.
//!
//! Fullscreen detection (spec §11, Windows): SHQueryUserNotificationState catches
//! D3D-exclusive fullscreen and presentation mode; a foreground-window geometry
//! heuristic catches borderless-fullscreen (window covers its whole monitor and
//! has no caption). The desktop itself (Progman/WorkerW) is caption-less and
//! monitor-covering, so it is explicitly excluded. Detection is polled at 500ms —
//! documented limitation: borderless games are indistinguishable from borderless
//! maximized apps; a per-app setting refines this later.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager};

#[derive(Default)]
pub struct VisibilityState {
    pub user_hidden: AtomicBool,
    pub fs_hidden: AtomicBool,
}

pub fn apply(app: &AppHandle) {
    let vis = app.state::<VisibilityState>();
    let user_hidden = vis.user_hidden.load(Ordering::Relaxed);
    let fs_hidden = vis.fs_hidden.load(Ordering::Relaxed);
    let parked = app.state::<crate::notes::ParkedState>();

    for (label, win) in app.webview_windows() {
        if let Some(id) = crate::notes::label_to_id(&label) {
            // Parked notes live on a disconnected display — never show them here.
            let is_parked = parked.0.lock().unwrap().contains(id);
            // Notes belong to one virtual desktop; elsewhere they stay hidden.
            #[cfg(windows)]
            let off_desktop = app
                .try_state::<crate::desktops::DesktopState>()
                .map(|d| d.is_off_desktop(id))
                .unwrap_or(false);
            #[cfg(not(windows))]
            let off_desktop = false;
            // show() only when actually hidden: on macOS show() is
            // makeKeyAndOrderFront:, which would yank keyboard focus from the
            // window being typed in on every apply() pass.
            let _ = if is_parked || off_desktop || user_hidden || fs_hidden {
                win.hide()
            } else if matches!(win.is_visible(), Ok(false)) {
                win.show()
            } else {
                Ok(())
            };
        } else if crate::pad::is_pad_label(&label) {
            let _ = if fs_hidden {
                win.hide()
            } else if matches!(win.is_visible(), Ok(false)) {
                win.show()
            } else {
                Ok(())
            };
        }
    }
}

/// Ctrl+Alt+H handler.
pub fn toggle_user_hidden(app: &AppHandle) {
    let vis = app.state::<VisibilityState>();
    vis.user_hidden.fetch_xor(true, Ordering::Relaxed);
    apply(app);
}

pub fn spawn_fullscreen_watcher(app: AppHandle) {
    std::thread::spawn(move || loop {
        let fs = fullscreen_active();
        let vis = app.state::<VisibilityState>();
        if vis.fs_hidden.swap(fs, Ordering::Relaxed) != fs {
            apply(&app);
        }
        std::thread::sleep(Duration::from_millis(500));
    });
}

#[cfg(not(windows))]
fn fullscreen_active() -> bool {
    false
}

#[cfg(windows)]
fn fullscreen_active() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect,
        GetWindowThreadProcessId, GWL_STYLE, WS_CAPTION,
    };

    unsafe {
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_BUSY
                || state == QUNS_RUNNING_D3D_FULL_SCREEN
                || state == QUNS_PRESENTATION_MODE
            {
                return true;
            }
        }

        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }

        // Never react to our own windows.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }

        // The desktop and the shell's own overlays (Start menu, search, Task View,
        // Alt-Tab, tray) are caption-less, monitor-covering windows — never treat
        // them as fullscreen apps or the pad blinks out on every Start/Alt-Tab.
        const SHELL_CLASSES: &[&str] = &[
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "Windows.UI.Core.CoreWindow",
            "XamlExplorerHostIslandWindow",
            "MultitaskingViewFrame",
            "ForegroundStaging",
            "NotifyIconOverflowWindow",
        ];
        let mut class = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if SHELL_CLASSES.contains(&class.as_str()) {
            return false;
        }

        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return false;
        }
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        let mr = mi.rcMonitor;

        let covers = wr.left <= mr.left && wr.top <= mr.top && wr.right >= mr.right && wr.bottom >= mr.bottom;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let has_caption = style & WS_CAPTION.0 == WS_CAPTION.0;

        covers && !has_caption
    }
}
