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

    let icon = app.default_window_icon().cloned().ok_or("no default window icon")?;

    TrayIconBuilder::with_id("pinq-tray")
        .icon(icon)
        .tooltip("Pinq — sticky notes")
        .menu(&menu)
        .show_menu_on_left_click(false)
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
