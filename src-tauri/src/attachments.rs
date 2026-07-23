//! Attachment commands: paste (base64 from the clipboard), drag-drop (paths),
//! remove, and open-in-default-app (via a named temp copy, so the canonical
//! blob is never edited in place — spec §22).

use crate::store::Store;
use base64::Engine;
use pinq_core::{AttachmentRef, CanvasItem};
use std::sync::Arc;
use tauri::{AppHandle, State};

const MAX_PASTE_BYTES: usize = 20 * 1024 * 1024;
/// Drag-dropped files stream to disk (never fully in memory), so the cap is
/// about sync practicality, not RAM.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_FILES_PER_DROP: usize = 20;

fn fmt_size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

/// Clipboard paste: image bytes arrive base64-encoded from the webview and
/// land as a canvas item at (x, y).
#[tauri::command]
pub fn attach_image_base64(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    data: String,
    mime: String,
    x: f64,
    y: f64,
) -> Result<CanvasItem, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_PASTE_BYTES {
        return Err("image too large (20 MB max)".into());
    }
    let hash = crate::blobs::store_blob(&app, &bytes).map_err(|e| e.to_string())?;
    let attachment = AttachmentRef {
        name: format!("pasted-{}.{}", &hash[..8], ext_for_mime(&mime)),
        hash,
        mime,
        size: bytes.len() as u64,
    };
    let item = CanvasItem::attachment_item(x, y, 140.0, attachment);
    store.add_item(&id, item.clone()).map_err(|e| e.to_string())?;
    Ok(item)
}

/// What a drop produced: the items that landed plus a human-readable line for
/// every file that didn't, so the note can say WHY instead of silently
/// swallowing it.
#[derive(serde::Serialize)]
pub struct AttachResult {
    pub added: Vec<CanvasItem>,
    pub skipped: Vec<String>,
}

/// Drag-drop: files arrive as native paths and cascade from (x, y).
#[tauri::command]
pub fn attach_files(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    paths: Vec<String>,
    x: f64,
    y: f64,
) -> Result<AttachResult, String> {
    let mut added = Vec::new();
    let mut skipped = Vec::new();
    if paths.len() > MAX_FILES_PER_DROP {
        skipped.push(format!(
            "{} files ({MAX_FILES_PER_DROP} max per drop)",
            paths.len() - MAX_FILES_PER_DROP
        ));
    }
    for (k, p) in paths.iter().take(MAX_FILES_PER_DROP).enumerate() {
        let path = std::path::Path::new(p);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let Ok(meta) = std::fs::metadata(path) else {
            skipped.push(format!("{name} (couldn't read it)"));
            continue;
        };
        if meta.is_dir() {
            skipped.push(format!("{name} (folders can't be attached)"));
            continue;
        }
        if meta.len() > MAX_FILE_BYTES {
            skipped.push(format!(
                "{name} ({} — over the {} limit)",
                fmt_size(meta.len()),
                fmt_size(MAX_FILE_BYTES)
            ));
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let Ok((hash, size)) = crate::blobs::store_blob_from_file(&app, path) else {
            skipped.push(format!("{name} (couldn't read it)"));
            continue;
        };
        let attachment = AttachmentRef {
            hash,
            name,
            mime: mime_for_ext(&ext).to_string(),
            size,
        };
        let k = k as f64;
        let w = if attachment.mime.starts_with("video/") { 220.0 } else { 140.0 };
        let item = CanvasItem::attachment_item(x + k * 18.0, y + k * 18.0, w, attachment);
        store.add_item(&id, item.clone()).map_err(|e| e.to_string())?;
        added.push(item);
    }
    Ok(AttachResult { added, skipped })
}

/// Open in the default app via a temp copy with a real filename/extension.
#[tauri::command]
pub fn open_attachment(app: AppHandle, hash: String, name: String) -> Result<(), String> {
    let Some(blob) = crate::blobs::blob_path(&app, &hash) else {
        return Err("attachment data missing".into());
    };
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let dir = std::env::temp_dir().join("pinq-open");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(format!("{}-{safe}", &hash[..8]));
    if !target.exists() {
        std::fs::copy(&blob, &target).map_err(|e| e.to_string())?;
    }
    crate::export::open_with_default(&target).map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether the blob bytes for a hash are on THIS device yet. Chips on a peer
/// can show "syncing…" until the transfer lands.
#[tauri::command]
pub fn attachment_ready(app: AppHandle, hash: String) -> bool {
    crate::blobs::blob_path(&app, &hash).is_some()
}

/// The chip's ⬇ button: copy the blob into the user's Downloads folder under
/// its real filename (deduped with " (n)" suffixes) and reveal it in the file
/// manager. Returns the saved path.
#[tauri::command]
pub fn save_attachment(app: AppHandle, hash: String, name: String) -> Result<String, String> {
    use tauri::Manager;
    let Some(blob) = crate::blobs::blob_path(&app, &hash) else {
        return Err("Still syncing from your other device — try again in a moment".into());
    };
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    let safe: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' { '_' } else { c })
        .collect();
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    let mut target = dir.join(&safe);
    let mut n = 1;
    while target.exists() {
        target = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    std::fs::copy(&blob, &target).map_err(|e| e.to_string())?;
    let _ = crate::export::reveal_in_folder(&target);
    Ok(target.display().to_string())
}
