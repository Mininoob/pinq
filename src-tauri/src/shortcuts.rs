//! Configurable global shortcuts (spec §28/E7). Settings hold the combo
//! strings; this module parses, registers, and dispatches them, and keeps the
//! last apply's failures so the Settings UI can say a combo is invalid or
//! already taken by another app — never a silent `let _ =` again.

use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    HideAll,
    Find,
    NewNote,
}

#[derive(Default)]
pub struct ShortcutMap {
    bound: Mutex<Vec<(Shortcut, Action)>>,
    errors: Mutex<Vec<String>>,
}

/// Install the plugin + dispatcher. Call once in setup, before `apply`.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    app.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(|app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                let Some(map) = app.try_state::<ShortcutMap>() else { return };
                let action = map
                    .bound
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(s, _)| s == shortcut)
                    .map(|(_, a)| *a);
                match action {
                    Some(Action::HideAll) => crate::visibility::toggle_user_hidden(app),
                    Some(Action::Find) => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = crate::panels::open_find(&app);
                        });
                    }
                    Some(Action::NewNote) => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = crate::tray::create_note_at_center(&app);
                        });
                    }
                    None => {}
                }
            })
            .build(),
    )
}

/// (Re)bind the three shortcuts from settings. Bindings that fail are
/// reported (and queryable via `shortcut_errors`); the rest still work.
/// An empty combo string means "deliberately unbound".
pub fn apply(app: &AppHandle, s: &crate::settings::Settings) -> Vec<String> {
    let map = app.state::<ShortcutMap>();
    let _ = app.global_shortcut().unregister_all();
    map.bound.lock().unwrap().clear();
    let mut errors = Vec::new();
    for (label, combo, action) in [
        ("Hide notes", s.shortcut_hide.as_str(), Action::HideAll),
        ("Quick find", s.shortcut_find.as_str(), Action::Find),
        ("New note", s.shortcut_new.as_str(), Action::NewNote),
    ] {
        let combo = combo.trim();
        if combo.is_empty() {
            continue;
        }
        match combo.parse::<Shortcut>() {
            Ok(sc) => match app.global_shortcut().register(sc) {
                Ok(()) => map.bound.lock().unwrap().push((sc, action)),
                Err(e) => errors.push(format!("{label} ({combo}): {e}")),
            },
            Err(e) => errors.push(format!("{label} ({combo}): {e}")),
        }
    }
    *map.errors.lock().unwrap() = errors.clone();
    errors
}

/// What failed on the last apply — the Settings UI shows these after saving.
#[tauri::command]
pub fn shortcut_errors(map: tauri::State<'_, ShortcutMap>) -> Vec<String> {
    map.errors.lock().unwrap().clone()
}
