//! Per-virtual-desktop notes (Windows).
//!
//! Pinq note windows are tool windows (skip_taskbar), which Windows shows on
//! EVERY virtual desktop — that's why notes "followed" desktop switches. The
//! fix is ours, not the shell's: each note remembers a *home desktop* GUID,
//! a watcher derives the *current* desktop from the foreground window via
//! `IVirtualDesktopManager`, and notes that aren't home are hidden through
//! the visibility layers. The map persists (note-desktops.json, device-local
//! — desktop layout never syncs), and homes are validated against the shell's
//! registry list of live desktops so a closed desktop can't strand a note.
//!
//! Home assignment: where the note was created, re-assigned wherever the user
//! last finished dragging it. Notes with no known home show everywhere.

#![cfg(windows)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

#[derive(Default)]
pub struct DesktopState {
    /// note id -> home desktop GUID (as a string).
    home: Mutex<HashMap<String, String>>,
    /// Notes hidden because the current desktop isn't their home.
    pub off_desktop: Mutex<HashSet<String>>,
    /// Notes waiting for a home (re)assignment at the next watcher tick.
    pending: Mutex<Vec<(String, bool)>>, // (id, force)
    path: Mutex<PathBuf>,
}

impl DesktopState {
    pub fn is_off_desktop(&self, id: &str) -> bool {
        self.off_desktop.lock().unwrap().contains(id)
    }
}

pub fn init(app: &AppHandle) {
    let state = DesktopState::default();
    if let Ok(dir) = app.path().app_data_dir() {
        let path = dir.join("note-desktops.json");
        if let Ok(json) = std::fs::read_to_string(&path) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&json) {
                *state.home.lock().unwrap() = map;
            }
        }
        *state.path.lock().unwrap() = path;
    }
    app.manage(state);
    let app = app.clone();
    std::thread::spawn(move || watcher(app));
}

/// Give this note a home on the current desktop. `force` re-assigns (drag
/// finished here); otherwise only notes without a home get one (window
/// creation — restored notes keep their saved home).
pub fn assign_home(app: &AppHandle, id: &str, force: bool) {
    if let Some(state) = app.try_state::<DesktopState>() {
        state.pending.lock().unwrap().push((id.to_string(), force));
    }
}

/// Forget a note (purged).
pub fn forget(app: &AppHandle, id: &str) {
    let Some(state) = app.try_state::<DesktopState>() else { return };
    state.home.lock().unwrap().remove(id);
    state.off_desktop.lock().unwrap().remove(id);
    save(&state);
}

fn save(state: &DesktopState) {
    let path = state.path.lock().unwrap().clone();
    if path.as_os_str().is_empty() {
        return;
    }
    let map = state.home.lock().unwrap().clone();
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, json);
    }
}

fn watcher(app: AppHandle) {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mgr: IVirtualDesktopManager = match unsafe {
        CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)
    } {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pinq: virtual desktop manager unavailable ({e}); notes show everywhere");
            return;
        }
    };

    loop {
        std::thread::sleep(Duration::from_millis(700));
        let Some(state) = app.try_state::<DesktopState>() else { continue };
        let Some(cur) = current_desktop(&mgr) else { continue };

        // Assign pending homes to the current desktop.
        let pending: Vec<(String, bool)> = state.pending.lock().unwrap().drain(..).collect();
        if !pending.is_empty() {
            let mut homes = state.home.lock().unwrap();
            let mut dirty = false;
            for (id, force) in pending {
                if force || !homes.contains_key(&id) {
                    dirty |= homes.insert(id, cur.clone()).as_deref() != Some(cur.as_str());
                }
            }
            drop(homes);
            if dirty {
                save(&state);
            }
        }

        // Homes pointing at desktops that no longer exist go back to "everywhere".
        if let Some(live) = live_desktops() {
            let mut homes = state.home.lock().unwrap();
            let before = homes.len();
            homes.retain(|_, guid| live.contains(guid));
            let dirty = homes.len() != before;
            drop(homes);
            if dirty {
                save(&state);
            }
        }

        // Which notes are away from home right now?
        let homes = state.home.lock().unwrap().clone();
        let off: HashSet<String> = homes
            .into_iter()
            .filter(|(_, home)| home != &cur)
            .map(|(id, _)| id)
            .collect();
        let changed = {
            let mut guard = state.off_desktop.lock().unwrap();
            if *guard != off {
                *guard = off;
                true
            } else {
                false
            }
        };
        if changed {
            crate::visibility::apply(&app);
        }
    }
}

/// The current desktop's GUID, derived from the foreground window (which is on
/// the current desktop by definition). Our own tool windows and shell surfaces
/// are skipped — their desktop assignment is meaningless.
fn current_desktop(mgr: &windows::Win32::UI::Shell::IVirtualDesktopManager) -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongW, GetWindowThreadProcessId, GWL_EXSTYLE,
        WS_EX_TOOLWINDOW,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return None; // our windows float across desktops; not a signal
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let guid = mgr.GetWindowDesktopId(hwnd).ok()?;
        if guid == windows::core::GUID::zeroed() {
            return None;
        }
        Some(format!("{guid:?}"))
    }
}

/// The shell's list of live desktop GUIDs, from the registry (16 bytes each).
fn live_desktops() -> Option<HashSet<String>> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY,
    };
    unsafe {
        let mut size = 0u32;
        let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops");
        let val = w!("VirtualDesktopIDs");
        if RegGetValueW(HKEY_CURRENT_USER, key, val, RRF_RT_REG_BINARY, None, None, Some(&mut size))
            .is_err()
            || size == 0
            || size % 16 != 0
        {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            val,
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .is_err()
        {
            return None;
        }
        Some(
            buf.chunks_exact(16)
                .map(|c| {
                    let guid = windows::core::GUID::from_values(
                        u32::from_le_bytes(c[0..4].try_into().unwrap()),
                        u16::from_le_bytes(c[4..6].try_into().unwrap()),
                        u16::from_le_bytes(c[6..8].try_into().unwrap()),
                        c[8..16].try_into().unwrap(),
                    );
                    format!("{guid:?}")
                })
                .collect(),
        )
    }
}
