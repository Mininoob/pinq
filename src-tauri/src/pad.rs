//! The sticky pad: slim, frameless, always-on-top windows docked to a configurable
//! screen edge. One pad instance per selected monitor (primary or all); edge and
//! position-along-edge come from settings and rebuild live when changed.

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

const PAD_LABEL_PREFIX: &str = "pad";

/// Logical size of the pad window: thickness is the pointer activation area
/// (the visible bar is thinner, drawn by CSS); length runs along the edge.
const PAD_THICKNESS: f64 = 32.0;
const PAD_LENGTH: f64 = 240.0;
/// Keep clear of screen corners (snap zones, hot corners) — spec §9.
const END_MARGIN: f64 = 48.0;
/// Extra thickness while the press-and-hold menu is out (a slim label strip).
const MENU_EXTRA: f64 = 26.0;

/// Label of the pad currently expanded for its menu, if any. The watchdog
/// leaves pad geometry alone while set — an expanded pad is off-spec on purpose.
#[derive(Default)]
pub struct ExpandedPad(pub std::sync::Mutex<Option<String>>);

/// Pull the pad out of the edge (menu open) or slide it back flush. The
/// window grows inward so the icons render on the pad surface itself.
/// Async: window sizing off the main thread.
#[tauri::command]
pub async fn expand_pad(
    app: AppHandle,
    window: tauri::WebviewWindow,
    expand: bool,
) -> Result<(), String> {
    let cfg = app.state::<crate::settings::SettingsStore>().current();
    *app.state::<ExpandedPad>().0.lock().unwrap() =
        expand.then(|| window.label().to_string());

    let Some(monitor) = window.current_monitor().ok().flatten() else { return Ok(()) };
    let (w, h, x, y) = pad_geometry(&monitor, &cfg.pad_edge, cfg.pad_position);
    let extra = if expand {
        (MENU_EXTRA * monitor.scale_factor()).round() as i32
    } else {
        0
    };
    let (w, h, x, y) = match cfg.pad_edge.as_str() {
        "left" => (w as i32 + extra, h as i32, x, y),
        "right" => (w as i32 + extra, h as i32, x - extra, y),
        "top" => (w as i32, h as i32 + extra, x, y),
        _ => (w as i32, h as i32 + extra, x, y - extra),
    };
    window.set_size(PhysicalSize::new(w as u32, h as u32)).map_err(|e| e.to_string())?;
    window.set_position(PhysicalPosition::new(x, y)).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn is_pad_label(label: &str) -> bool {
    label == PAD_LABEL_PREFIX || label.starts_with("pad-")
}

fn selected_monitors(app: &AppHandle, cfg: &crate::settings::Settings) -> Vec<tauri::Monitor> {
    if cfg.pad_monitors == "all" {
        let all = app.available_monitors().unwrap_or_default();
        if !all.is_empty() {
            return all;
        }
    }
    app.primary_monitor().ok().flatten().into_iter().collect()
}

pub fn create_pads(app: &AppHandle) -> tauri::Result<()> {
    let cfg = app.state::<crate::settings::SettingsStore>().current();
    for (i, monitor) in selected_monitors(app, &cfg).iter().enumerate() {
        create_pad_on(app, &format!("pad-{i}"), monitor, &cfg.pad_edge, cfg.pad_position)?;
    }
    Ok(())
}

/// Destroy all pads and recreate them from current settings.
pub fn rebuild_pads(app: &AppHandle) -> tauri::Result<()> {
    for (label, win) in app.webview_windows() {
        if is_pad_label(&label) {
            let _ = win.destroy();
        }
    }
    create_pads(app)
}

/// Expected pad geometry (w, h, x, y) on a monitor for the given edge/position.
fn pad_geometry(monitor: &tauri::Monitor, edge: &str, position: f64) -> (u32, u32, i32, i32) {
    let wa = crate::monitors::work_area_of(monitor);
    let scale = monitor.scale_factor();
    let thick = (PAD_THICKNESS * scale).round() as u32;
    let len = (PAD_LENGTH * scale).round() as u32;
    let margin = (END_MARGIN * scale).round() as i32;
    let frac = position.clamp(0.0, 1.0);

    // Position the pad's long axis along the edge at `frac` of the usable run.
    let run = |total: u32, len: u32| -> i32 {
        let usable = total as i32 - len as i32 - 2 * margin;
        if usable > 0 {
            margin + (frac * usable as f64).round() as i32
        } else {
            (total as i32 - len as i32) / 2
        }
    };

    match edge {
        "left" => (thick, len, wa.x, wa.y + run(wa.h, len)),
        "top" => (len, thick, wa.x + run(wa.w, len), wa.y),
        "bottom" => (len, thick, wa.x + run(wa.w, len), wa.y + wa.h as i32 - thick as i32),
        _ => (thick, len, wa.x + wa.w as i32 - thick as i32, wa.y + run(wa.h, len)),
    }
}

fn create_pad_on(
    app: &AppHandle,
    label: &str,
    monitor: &tauri::Monitor,
    edge: &str,
    position: f64,
) -> tauri::Result<()> {
    let (w, h, x, y) = pad_geometry(monitor, edge, position);

    let builder = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html?role=pad&edge={edge}").into()),
    )
    .title("Pinq Pad")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .shadow(false)
    .visible(false);
    // The pad must follow the user across Spaces and over full-screen apps.
    #[cfg(target_os = "macos")]
    let builder = builder.visible_on_all_workspaces(true);
    let win = builder.build()?;

    win.set_size(PhysicalSize::new(w, h))?;
    win.set_position(PhysicalPosition::new(x, y))?;
    win.show()?;
    Ok(())
}

/// Self-healing: every few seconds verify each pad exists at its expected spot,
/// rebuild if missing or drifted (failed rebuild, monitor/resolution change), and
/// re-assert always-on-top so other topmost windows can't bury the pad.
pub fn spawn_pad_watchdog(app: AppHandle) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3));

        // Notes still reconcile with monitors even while a menu is out.
        crate::notes::sync_parked(&app);

        // A pad with its menu out is deliberately off its docked geometry —
        // don't "fix" it out from under the user.
        if app.state::<ExpandedPad>().0.lock().unwrap().is_some() {
            continue;
        }

        let cfg = app.state::<crate::settings::SettingsStore>().current();
        let fs_hidden = app
            .state::<crate::visibility::VisibilityState>()
            .fs_hidden
            .load(Ordering::Relaxed);

        let monitors = selected_monitors(&app, &cfg);
        let pad_count = app
            .webview_windows()
            .keys()
            .filter(|l| is_pad_label(l))
            .count();

        let mut needs_rebuild = pad_count != monitors.len();
        if !needs_rebuild {
            for (i, monitor) in monitors.iter().enumerate() {
                let (_, _, x, y) = pad_geometry(monitor, &cfg.pad_edge, cfg.pad_position);
                match app.get_webview_window(&format!("pad-{i}")) {
                    None => {
                        needs_rebuild = true;
                        break;
                    }
                    Some(win) => {
                        if let Ok(p) = win.outer_position() {
                            if (p.x - x).abs() > 2 || (p.y - y).abs() > 2 {
                                needs_rebuild = true;
                                break;
                            }
                        }
                        if !fs_hidden {
                            let _ = win.show();
                            let _ = win.set_always_on_top(true);
                        }
                    }
                }
            }
        }

        if needs_rebuild {
            let _ = rebuild_pads(&app);
            crate::visibility::apply(&app);
        }
    });
}
