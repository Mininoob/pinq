//! Reminder scheduler: sticky notes that come find you. A background thread
//! checks for due reminders; a due note is pulled back onto the desktop
//! (pin/untuck/focus — the same ladder links use) plus a Windows notification.
//! The reminder is cleared FIRST via an ordinary synced op, so a crash can't
//! re-fire it and other devices that sync the clear never fire it either
//! (whichever device is awake first wins).

use crate::store::Store;
use std::sync::Arc;
use tauri::AppHandle;

pub fn spawn_scheduler(app: AppHandle, store: Arc<Store>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(20));
        fire_due(&app, &store);
    });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn fire_due(app: &AppHandle, store: &Store) {
    let now = now_ms();
    for n in store.all_notes() {
        if n.trashed {
            continue;
        }
        let Some(at) = n.remind_ms else { continue };
        if at > now {
            continue;
        }
        // Clear before showing anything — see module docs.
        if store.set_reminder(&n.id, None).is_err() {
            continue;
        }
        let _ = crate::notes::open_note_by_id(app, store, &n.id);

        let title = crate::links::title_of(&n);
        let title = if title.is_empty() { "A note wants your attention".to_string() } else { title };
        use tauri_plugin_notification::NotificationExt;
        let _ = app
            .notification()
            .builder()
            .title("Pinq reminder")
            .body(&title)
            .show();

        // Open note windows re-read state (reminder gone) off this event.
        use tauri::Emitter;
        let _ = app.emit("pinq-sync-applied", vec![n.id.clone()]);
    }
}
