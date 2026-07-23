//! System tray: left-click opens the library; the menu offers library, new note,
//! settings, and quit. Window-creating actions run off the main thread (menu and
//! tray handlers run on it — same deadlock rule as commands).

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "open-library", "Open Library", true, None::<&str>)?;
    let new_note = MenuItem::with_id(app, "new-note", "New Sticky Note", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Pinq", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &new_note, &settings, &quit])?;

    // macOS menu-bar items are template images (alpha-only, system-colored);
    // the full-color app icon would render as a solid blob there.
    #[cfg(target_os = "macos")]
    let icon = tray_template_icon();
    #[cfg(not(target_os = "macos"))]
    let icon = app.default_window_icon().cloned().ok_or("no default window icon")?;

    let builder = TrayIconBuilder::with_id("pinq-tray")
        .icon(icon)
        .tooltip("Pinq — sticky notes")
        .menu(&menu)
        .show_menu_on_left_click(false);
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);
    builder
        .on_menu_event(|app, event| {
            let app = app.clone();
            match event.id.as_ref() {
                "open-library" => {
                    std::thread::spawn(move || {
                        let _ = crate::panels::create_or_focus_library(&app);
                    });
                }
                "new-note" => {
                    std::thread::spawn(move || {
                        let _ = create_note_at_center(&app);
                    });
                }
                "settings" => {
                    std::thread::spawn(move || {
                        let _ = crate::panels::create_or_focus_settings(&app);
                    });
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle().clone();
                std::thread::spawn(move || {
                    let _ = crate::panels::create_or_focus_library(&app);
                });
            }
        })
        .build(app)?;
    Ok(())
}

/// Reconcile the tray icon and (macOS) Dock icon with the "show_in" setting:
/// "both" | "menubar" | "dock". On Windows the setting is ignored — the tray
/// is the only handle to the app there.
pub fn apply_presence(app: &AppHandle, show_in: &str) {
    #[cfg(target_os = "macos")]
    {
        let policy = if show_in == "menubar" {
            tauri::ActivationPolicy::Accessory
        } else {
            tauri::ActivationPolicy::Regular
        };
        let _ = app.set_activation_policy(policy);

        let want_tray = show_in != "dock";
        let have_tray = app.tray_by_id("pinq-tray").is_some();
        if want_tray && !have_tray {
            // Tray/menu construction belongs on the main thread.
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Err(e) = init(&app2) {
                    eprintln!("pinq: tray init failed: {e}");
                }
            });
        } else if !want_tray && have_tray {
            let _ = app.remove_tray_by_id("pinq-tray");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, show_in);
}

/// Menu-bar glyph: a sticky-note silhouette with a folded bottom-right corner,
/// drawn into alpha (macOS colors template images itself). 36px = 18pt @2x.
#[cfg(target_os = "macos")]
fn tray_template_icon() -> tauri::image::Image<'static> {
    const S: usize = 36;
    const SUB: usize = 4; // supersamples per axis, for smooth edges
    let (lo, hi) = (3.0_f64, S as f64 - 3.0);
    let (radius, fold) = (4.5_f64, 11.0_f64);

    let inside = |x: f64, y: f64| -> bool {
        if x < lo || x > hi || y < lo || y > hi {
            return false;
        }
        // Rounded corners, except bottom-right where the fold cuts the corner off.
        for (cx, cy) in [(lo + radius, lo + radius), (hi - radius, lo + radius), (lo + radius, hi - radius)] {
            if (x < cx) != (cx > S as f64 / 2.0)
                && (y < cy) != (cy > S as f64 / 2.0)
                && (x - cx).powi(2) + (y - cy).powi(2) > radius * radius
            {
                return false;
            }
        }
        x + y <= 2.0 * hi - fold
    };

    let mut rgba = vec![0u8; S * S * 4];
    for py in 0..S {
        for px in 0..S {
            let hits = (0..SUB * SUB)
                .filter(|i| {
                    let x = px as f64 + ((i % SUB) as f64 + 0.5) / SUB as f64;
                    let y = py as f64 + ((i / SUB) as f64 + 0.5) / SUB as f64;
                    inside(x, y)
                })
                .count();
            rgba[(py * S + px) * 4 + 3] = (hits * 255 / (SUB * SUB)) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, S as u32, S as u32)
}

pub fn create_note_at_center(app: &AppHandle) -> tauri::Result<()> {
    let wa = crate::monitors::primary_work_area(app);
    let scale = crate::monitors::primary_scale(app);
    let (w, h) = ((280.0 * scale).round() as u32, (220.0 * scale).round() as u32);
    let rect = pinq_core::Rect {
        x: wa.x + (wa.w as i32 - w as i32) / 2,
        y: wa.y + (wa.h as i32 - h as i32) / 2,
        w,
        h,
    };
    let store = app.state::<std::sync::Arc<crate::store::Store>>();
    let note = store.create_note(rect).map_err(tauri::Error::Io)?;
    crate::notes::open_note_window(app, &note)
}
