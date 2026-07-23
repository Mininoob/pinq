mod attachments;
mod blobs;
#[cfg(windows)]
mod desktops;
mod drag;
mod export;
mod links;
mod monitors;
mod notes;
mod pad;
mod panels;
mod pull;
mod registry;
mod reminders;
mod settings;
mod smart;
mod store;
mod sync;
mod tray;
mod visibility;

use pinq_core::Rect;
use std::sync::Arc;
use std::time::Duration;
use tauri::{Manager, WindowEvent};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(pull::PullState::default())
        .manage(pad::ExpandedPad::default())
        .manage(drag::DragState::default())
        .manage(drag::UndoState::default())
        .manage(visibility::VisibilityState::default())
        .manage(notes::ParkedState::default())
        .manage(smart::SmartState::default())
        // Serves blob bytes to note webviews as pinq-blob://<hash>. Read-only,
        // hash-validated; images sniff fine without a precise content type.
        .register_uri_scheme_protocol("pinq-blob", |ctx, request| {
            let hash = request.uri().path().trim_start_matches('/');
            match blobs::blob_path(&ctx.app_handle().clone(), hash)
                .and_then(|p| std::fs::read(p).ok())
            {
                Some(bytes) => tauri::http::Response::builder()
                    .header("Content-Type", "application/octet-stream")
                    .header("Cache-Control", "max-age=31536000, immutable")
                    .body(bytes)
                    .unwrap(),
                None => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .invoke_handler(tauri::generate_handler![
            pull::pull_start,
            pull::pull_end,
            pull::pull_cancel,
            pull::click_create_note,
            notes::get_note,
            notes::save_note_text,
            notes::file_note,
            drag::note_drag_start,
            drag::note_drag_end,
            drag::note_drag_cancel,
            drag::library_pull_start,
            drag::undo_last,
            drag::toast_dismiss,
            notes::get_all_notes,
            notes::search_notes,
            notes::pin_note,
            notes::trash_note,
            notes::restore_note,
            notes::purge_note,
            notes::empty_trash,
            notes::set_note_color,
            notes::set_note_paper,
            notes::tag_note,
            notes::untag_note,
            notes::set_reminder,
            notes::get_note_history,
            export::export_all,
            export::import_files,
            export::backup_archive,
            attachments::attach_image_base64,
            attachments::attach_files,
            attachments::open_attachment,
            notes::peek_note,
            notes::untuck_note,
            notes::add_text_item,
            notes::open_note,
            links::link_targets,
            links::get_backlinks,
            links::open_link,
            notes::set_item_text,
            notes::move_item,
            notes::remove_item,
            notes::restore_item,
            settings::get_settings,
            settings::set_settings,
            panels::open_settings,
            panels::open_library,
            panels::close_settings,
            panels::close_find,
            pad::expand_pad,
            panels::open_viewer,
            panels::close_viewer,
            sync::get_sync_info,
            sync::sync_now,
            sync::get_registry,
            sync::revoke_device,
            sync::rotate_space,
            sync::pairing_state,
            sync::pairing_host_start,
            sync::pairing_join,
            sync::pairing_confirm,
            sync::pairing_cancel,
            sync::pair_request,
            sync::pair_respond,
            sync::note_is_live,
            smart::smart_status,
            smart::related_notes,
            smart::tag_suggestions,
            smart::accept_tag_suggestion,
            smart::dismiss_tag_suggestion,
            smart::note_suggestions,
            smart::apply_title,
            smart::dismiss_title_suggestion,
        ])
        .setup(|app| {
            use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, ShortcutState};
            app.handle().plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(|app, shortcut, event| {
                        if event.state() != ShortcutState::Pressed {
                            return;
                        }
                        let ca = Modifiers::CONTROL | Modifiers::ALT;
                        if shortcut.matches(ca, Code::KeyH) {
                            visibility::toggle_user_hidden(app);
                        } else if shortcut.matches(ca, Code::KeyF) {
                            let app = app.clone();
                            std::thread::spawn(move || {
                                let _ = panels::open_find(&app);
                            });
                        } else if shortcut.matches(ca, Code::KeyN) {
                            let app = app.clone();
                            std::thread::spawn(move || {
                                let _ = tray::create_note_at_center(&app);
                            });
                        }
                    })
                    .build(),
            )?;
            let _ = app.global_shortcut().register("ctrl+alt+h");
            let _ = app.global_shortcut().register("ctrl+alt+f");
            let _ = app.global_shortcut().register("ctrl+alt+n");

            let data_dir = app.path().app_data_dir()?;
            let store = Arc::new(store::Store::open(&data_dir)?);
            app.manage(store.clone());
            app.manage(settings::SettingsStore::open(data_dir.join("settings.json")));
            {
                let show_in = app.state::<settings::SettingsStore>().current().show_in;
                #[cfg(target_os = "macos")]
                if show_in == "menubar" {
                    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
                // "dock" hides the tray on macOS only; everywhere else the tray
                // is the app's only always-available entry point.
                if !(cfg!(target_os = "macos") && show_in == "dock") {
                    tray::init(app.handle())?;
                }
            }

            // Placement flusher: coalesced move/resize actions hit the log at most
            // once per second per note (crash loses ≤1s of placement, never content).
            {
                let store = store.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(1));
                    store.flush_places();
                });
            }

            // Blob GC: reclaim attachment bytes no note references anymore
            // (purged notes, emptied trash). Day-old orphans only, every 6h.
            {
                let app = app.handle().clone();
                let store = store.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(60));
                    let (n, bytes) = blobs::gc(&app, &store);
                    if n > 0 {
                        eprintln!("pinq: blob gc reclaimed {bytes} bytes across {n} files");
                    }
                    std::thread::sleep(Duration::from_secs(6 * 60 * 60));
                });
            }

            pad::create_pads(app.handle())?;
            pull::create_preview_window(app.handle())?;
            drag::create_toast_window(app.handle())?;
            panels::create_find_window(app.handle())?;
            #[cfg(windows)]
            desktops::init(app.handle());
            notes::restore_pinned(app.handle(), &store)?;
            visibility::spawn_fullscreen_watcher(app.handle().clone());
            pad::spawn_pad_watchdog(app.handle().clone());
            sync::init(app.handle());
            smart::init(app.handle());
            reminders::spawn_scheduler(app.handle().clone(), store.clone());
            // Settings are the source of truth for launch-at-login.
            {
                use tauri_plugin_autostart::ManagerExt;
                let want = app.state::<settings::SettingsStore>().current().launch_at_login;
                let al = app.autolaunch();
                let _ = if want { al.enable() } else { al.disable() };
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Quick-find dismisses itself when it loses focus.
            if window.label() == panels::FIND_LABEL {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
                return;
            }
            let Some(id) = notes::label_to_id(window.label()) else {
                return;
            };
            let store = window.state::<Arc<store::Store>>();
            match event {
                WindowEvent::Moved(pos) => {
                    if let Ok(size) = window.inner_size() {
                        store.queue_place(
                            id,
                            Rect { x: pos.x, y: pos.y, w: size.width, h: size.height },
                        );
                    }
                }
                WindowEvent::Resized(size) => {
                    if let Ok(pos) = window.outer_position() {
                        store.queue_place(
                            id,
                            Rect { x: pos.x, y: pos.y, w: size.width, h: size.height },
                        );
                    }
                }
                // OS-level close (Alt+F4, etc.) files the note — never deletes.
                WindowEvent::CloseRequested { .. } => {
                    let _ = store.file_note(id);
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Pinq")
        .run(|app_handle, event| match event {
            tauri::RunEvent::Exit => {
                if let Some(store) = app_handle.try_state::<Arc<store::Store>>() {
                    store.flush_places();
                }
            }
            // macOS Dock icon click with no visible windows: open the library.
            // Off the main thread — window creation on the event loop deadlocks.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                let app = app_handle.clone();
                std::thread::spawn(move || {
                    let _ = panels::create_or_focus_library(&app);
                });
            }
            _ => {}
        });
}
