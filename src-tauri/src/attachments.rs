//! Attachment commands: paste (base64 from the clipboard), drag-drop (paths),
//! remove, and open-in-default-app (via a named temp copy, so the canonical
//! blob is never edited in place — spec §22).

use crate::store::Store;
use base64::Engine;
use pinq_core::{AttachmentRef, CanvasItem};
use std::sync::Arc;
use tauri::{AppHandle, State};

const MAX_PASTE_BYTES: usize = 20 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_FILES_PER_DROP: usize = 20;

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

/// Drag-drop: files arrive as native paths and cascade from (x, y).
#[tauri::command]
pub fn attach_files(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    paths: Vec<String>,
    x: f64,
    y: f64,
) -> Result<Vec<CanvasItem>, String> {
    let mut added = Vec::new();
    for (k, p) in paths.iter().take(MAX_FILES_PER_DROP).enumerate() {
        let path = std::path::Path::new(p);
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let hash = crate::blobs::store_blob(&app, &bytes).map_err(|e| e.to_string())?;
        let attachment = AttachmentRef {
            hash,
            name,
            mime: mime_for_ext(&ext).to_string(),
            size: bytes.len() as u64,
        };
        let k = k as f64;
        let w = if attachment.mime.starts_with("video/") { 220.0 } else { 140.0 };
        let item = CanvasItem::attachment_item(x + k * 18.0, y + k * 18.0, w, attachment);
        store.add_item(&id, item.clone()).map_err(|e| e.to_string())?;
        added.push(item);
    }
    Ok(added)
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
