//! Device-local settings (theme, library appearance) — JSON file, distinct from
//! the content action log: this is configuration, not knowledge, and does not
//! sync as content.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

fn default_theme() -> String {
    "pinq".into()
}
fn default_mode() -> String {
    "auto".into()
}
fn default_paper() -> String {
    "plain".into()
}
fn default_note_paper() -> String {
    "plain".into()
}
fn default_edge() -> String {
    "right".into()
}
fn default_pad_position() -> f64 {
    0.5
}
fn default_pad_monitors() -> String {
    "primary".into()
}
fn default_show_in() -> String {
    "both".into()
}
fn default_shortcut_hide() -> String {
    "ctrl+alt+h".into()
}
fn default_shortcut_find() -> String {
    "ctrl+alt+f".into()
}
fn default_shortcut_new() -> String {
    "ctrl+alt+n".into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Sticky palette: drives pad, notes, preview, and library accents.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Library light/dark: "auto" follows the OS, or "light" / "dark".
    #[serde(default = "default_mode")]
    pub library_mode: String,
    /// Library paper texture: "plain" | "ruled" | "dotted" | "grid".
    #[serde(default = "default_paper")]
    pub library_paper: String,
    /// Default paper for sticky notes (per-note override wins).
    #[serde(default = "default_note_paper")]
    pub note_paper: String,
    /// Screen edge the pad docks to: "left" | "right" | "top" | "bottom".
    #[serde(default = "default_edge")]
    pub pad_edge: String,
    /// Fraction 0.0–1.0 along the edge (resolution-independent).
    #[serde(default = "default_pad_position")]
    pub pad_position: f64,
    /// Which monitors get a pad: "primary" | "all".
    #[serde(default = "default_pad_monitors")]
    pub pad_monitors: String,
    /// Accessibility: a plain click on the pad creates a note (no pull needed).
    #[serde(default)]
    pub click_creates: bool,
    /// Sync relay "host:port" for off-LAN sync; empty = LAN only. The relay
    /// stores only end-to-end-encrypted envelopes.
    #[serde(default)]
    pub relay_addr: String,
    /// Host a relay inside this app (no separate program). Announced over
    /// mDNS so other devices in the space adopt it automatically.
    #[serde(default)]
    pub relay_host: bool,
    /// Smart suggestions: a small embedding model runs ON THIS DEVICE to
    /// suggest tags and related notes. First enable downloads the model
    /// (~25 MB) into app data; nothing about notes ever leaves the machine.
    #[serde(default)]
    pub smart_suggestions: bool,
    /// Start Pinq when the user signs in (Windows registry / macOS LaunchAgent).
    #[serde(default)]
    pub launch_at_login: bool,
    /// Where Pinq surfaces on macOS: "both" | "menubar" | "dock". Ignored on
    /// Windows — the tray icon is the only handle to the app there.
    #[serde(default = "default_show_in")]
    pub show_in: String,
    /// Global shortcut combos ("ctrl+alt+h" style); empty = unbound.
    #[serde(default = "default_shortcut_hide")]
    pub shortcut_hide: String,
    #[serde(default = "default_shortcut_find")]
    pub shortcut_find: String,
    #[serde(default = "default_shortcut_new")]
    pub shortcut_new: String,
    /// First-run tour shown and dismissed.
    #[serde(default)]
    pub onboarding_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            library_mode: default_mode(),
            library_paper: default_paper(),
            note_paper: default_note_paper(),
            pad_edge: default_edge(),
            pad_position: default_pad_position(),
            pad_monitors: default_pad_monitors(),
            click_creates: false,
            relay_addr: String::new(),
            relay_host: false,
            smart_suggestions: false,
            launch_at_login: false,
            show_in: default_show_in(),
            shortcut_hide: default_shortcut_hide(),
            shortcut_find: default_shortcut_find(),
            shortcut_new: default_shortcut_new(),
            onboarding_done: false,
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    inner: Mutex<Settings>,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, inner: Mutex::new(inner) }
    }

    fn save(&self, settings: &Settings) {
        if let Ok(json) = serde_json::to_string_pretty(settings) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    pub fn current(&self) -> Settings {
        self.inner.lock().unwrap().clone()
    }

    /// Mutate one or two fields in place and persist (no change broadcast —
    /// for flags the UI doesn't restyle on, like onboarding_done).
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.inner.lock().unwrap();
        f(&mut s);
        self.save(&s);
    }
}

#[tauri::command]
pub fn get_settings(store: State<'_, SettingsStore>) -> Settings {
    store.inner.lock().unwrap().clone()
}


/// Persists the full settings and broadcasts them so every window restyles live.
/// Pad geometry changes rebuild the pad windows (off the main thread — creation
/// on the event-loop thread deadlocks, see pull_end).
#[tauri::command]
pub fn set_settings(app: AppHandle, store: State<'_, SettingsStore>, settings: Settings) {
    let (pad_changed, relay_changed, smart_changed, launch_changed, presence_changed, keys_changed) = {
        let mut s = store.inner.lock().unwrap();
        let pad = s.pad_edge != settings.pad_edge
            || s.pad_position != settings.pad_position
            || s.pad_monitors != settings.pad_monitors;
        let relay = s.relay_host != settings.relay_host;
        let smart = s.smart_suggestions != settings.smart_suggestions;
        let launch = s.launch_at_login != settings.launch_at_login;
        let presence = s.show_in != settings.show_in;
        let keys = s.shortcut_hide != settings.shortcut_hide
            || s.shortcut_find != settings.shortcut_find
            || s.shortcut_new != settings.shortcut_new;
        *s = settings.clone();
        store.save(&s);
        (pad, relay, smart, launch, presence, keys)
    };
    let smart_on = settings.smart_suggestions;
    let launch_on = settings.launch_at_login;
    let show_in = settings.show_in.clone();
    let applied = settings.clone();
    let _ = app.emit("pinq-settings", settings);
    if relay_changed {
        let app = app.clone();
        std::thread::spawn(move || crate::sync::update_relay_host(&app));
    }
    if smart_changed {
        crate::smart::set_enabled(&app, smart_on);
    }
    if launch_changed {
        use tauri_plugin_autostart::ManagerExt;
        let al = app.autolaunch();
        let _ = if launch_on { al.enable() } else { al.disable() };
    }
    if presence_changed {
        crate::tray::apply_presence(&app, &show_in);
    }
    if keys_changed {
        let _ = crate::shortcuts::apply(&app, &applied);
    }
    if pad_changed {
        std::thread::spawn(move || {
            let _ = crate::pad::rebuild_pads(&app);
        });
    }
}
