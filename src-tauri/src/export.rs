//! Markdown export and import (MVP data-ownership slice).
//!
//! Export writes one `.md` per non-trashed note plus a `notes.json` full backup
//! (all notes, all metadata, trash included) into Documents\Pinq Exports\, then
//! opens the folder. Import accepts `.md`/`.txt` paths (from drag-drop onto the
//! library) and files each as an unpinned note. Checklist glyphs round-trip to
//! Markdown task syntax (`☐` ↔ `- [ ]`, `☑` ↔ `- [x]`).

use crate::store::Store;
use pinq_core::Rect;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

const BOX: &str = "\u{2610} "; // "☐ "
const CHECKED: &str = "\u{2611} "; // "☑ "
const ARROW: &str = "\u{2192} "; // "→ "
const DOT: &str = "\u{2022} "; // "• "

fn md_from_note(text: &str) -> String {
    text.lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix(BOX) {
                format!("- [ ] {rest}")
            } else if let Some(rest) = l.strip_prefix(CHECKED) {
                format!("- [x] {rest}")
            } else if let Some(rest) = l.strip_prefix(ARROW).or_else(|| l.strip_prefix(DOT)) {
                format!("- {rest}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn note_from_md(text: &str) -> String {
    text.lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("- [ ] ") {
                format!("{BOX}{rest}")
            } else if let Some(rest) = l.strip_prefix("- [x] ").or_else(|| l.strip_prefix("- [X] ")) {
                format!("{CHECKED}{rest}")
            } else if let Some(rest) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                format!("{DOT}{rest}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pull completed `#tag` tokens out of text (same rule as the editor: word
/// boundary before, whitespace/end after). Returns cleaned text + tags.
fn strip_hashtags(text: &str) -> (String, Vec<String>) {
    let is_tag_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut tags: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || chars[i - 1].is_whitespace();
        if boundary && chars[i] == '#' {
            let mut j = i + 1;
            while j < chars.len() && is_tag_char(chars[j]) {
                j += 1;
            }
            let terminated = j == chars.len() || chars[j].is_whitespace();
            if j > i + 1 && terminated {
                let tag: String = chars[i + 1..j].iter().collect::<String>().to_lowercase();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
                if j < chars.len() && chars[j] == ' ' {
                    j += 1; // eat the separating space too
                }
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    // A tag at line end leaves its preceding space behind — tidy line ends.
    let out = out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n");
    (out, tags)
}

fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(32)
        .collect();
    if out.is_empty() { "note".into() } else { out }
}

/// Write a complete backup: manifest, every note (full state), every referenced
/// blob. One file, restorable anywhere by dropping it onto the library.
fn write_backup(
    zip_path: &std::path::Path,
    notes: &[pinq_core::NoteState],
    blobs_dir: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::Write;
    let file = std::fs::File::create(zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = Default::default();

    zip.start_file("manifest.json", opts)?;
    zip.write_all(
        serde_json::json!({
            "format": "pinq-backup",
            "version": 1,
            "exported_at_ms": pinq_core::now_ms(),
            "notes": notes.len(),
        })
        .to_string()
        .as_bytes(),
    )?;

    zip.start_file("notes.json", opts)?;
    zip.write_all(serde_json::to_string_pretty(notes)?.as_bytes())?;

    let mut seen = std::collections::HashSet::new();
    for n in notes {
        for a in &n.attachments {
            if !seen.insert(a.hash.clone()) {
                continue;
            }
            let Some(path) = crate::blobs::blob_path_in(blobs_dir, &a.hash) else { continue };
            let bytes = std::fs::read(path)?;
            zip.start_file(format!("blobs/{}", a.hash), opts)?;
            zip.write_all(&bytes)?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// Restore a backup archive: blobs re-stored content-addressed (hash verified),
/// notes recreated as ops; existing note ids are skipped. Returns notes restored.
fn restore_backup(
    zip_path: &std::path::Path,
    store: &Store,
    blobs_dir: &std::path::Path,
) -> std::io::Result<usize> {
    use std::io::Read;
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;

    let mut notes_json = String::new();
    zip.by_name("notes.json")
        .map_err(std::io::Error::other)?
        .read_to_string(&mut notes_json)?;
    let notes: Vec<pinq_core::NoteState> =
        serde_json::from_str(&notes_json).map_err(std::io::Error::other)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(std::io::Error::other)?;
        let name = entry.name().to_string();
        let Some(claimed) = name.strip_prefix("blobs/") else { continue };
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        // store_blob_in re-hashes: a tampered/corrupt entry lands under its
        // real hash and simply won't be referenced.
        let actual = crate::blobs::store_blob_in(blobs_dir, &bytes)?;
        if actual != claimed {
            eprintln!("pinq: backup blob {claimed} hash mismatch; skipped");
        }
    }

    let mut restored = 0;
    for n in &notes {
        if store.import_note(n)? {
            restored += 1;
        }
    }
    Ok(restored)
}

/// Encrypted backup container: magic + nonce + ChaCha20Poly1305 ciphertext of
/// the backup zip, keyed off the space key (spec §21 — a backup that leaks
/// from a drive or cloud folder stays sealed; the recovery phrase IS the key).
const BACKUP_MAGIC: &[u8; 8] = b"PINQBAK1";

fn backup_seal(space_key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    let key = blake3::keyed_hash(space_key, b"pinq-backup-v1");
    let cipher = ChaCha20Poly1305::new(key.as_bytes().into());
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(&nonce.into(), plain)
        .map_err(|_| "backup encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(20 + ct.len());
    out.extend_from_slice(BACKUP_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn backup_open(space_key: &[u8; 32], bytes: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    if bytes.len() < 21 || &bytes[..8] != BACKUP_MAGIC {
        return Err("not a Pinq encrypted backup".into());
    }
    let nonce: [u8; 12] = bytes[8..20].try_into().unwrap();
    let key = blake3::keyed_hash(space_key, b"pinq-backup-v1");
    let cipher = ChaCha20Poly1305::new(key.as_bytes().into());
    cipher.decrypt(&nonce.into(), &bytes[20..]).map_err(|_| {
        "This backup belongs to a different space — restore its recovery phrase first, then drop the backup again".to_string()
    })
}

/// Returns the archive path. The backup is sealed with the space key; the
/// recovery phrase (Settings) is what unlocks it on a fresh machine.
#[tauri::command]
pub fn backup_archive(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    sync_state: State<'_, crate::sync::SyncState>,
) -> Result<String, String> {
    use tauri::Manager;
    let docs = app.path().document_dir().map_err(|e| e.to_string())?;
    let dir = docs.join("Pinq Exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = pinq_core::now_ms();
    let tmp = dir.join(format!(".pinq-backup-{stamp}.tmp"));
    let blobs = crate::blobs::blobs_dir(&app).map_err(|e| e.to_string())?;
    write_backup(&tmp, &store.all_notes(), &blobs).map_err(|e| e.to_string())?;
    let plain = std::fs::read(&tmp).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    let space_key = *sync_state.space_key.lock().unwrap();
    let sealed = backup_seal(&space_key, &plain)?;
    let path = dir.join(format!("pinq-backup-{stamp}.pinqbak"));
    std::fs::write(&path, sealed).map_err(|e| e.to_string())?;
    let _ = open_with_default(&dir);
    Ok(path.display().to_string())
}

/// Open a file or folder with the platform's default handler — Explorer,
/// Finder (`open`), or xdg-open. Files launch their associated app; folders
/// open in the file manager.
pub fn open_with_default(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd).arg(path).spawn().map(|_| ())
}

/// Show a file selected in its folder — Explorer `/select,`, Finder `open -R`,
/// or the parent directory where selection isn't supported.
pub fn reveal_in_folder(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    return std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .spawn()
        .map(|_| ());
    #[cfg(target_os = "macos")]
    return std::process::Command::new("open").arg("-R").arg(path).spawn().map(|_| ());
    #[cfg(all(unix, not(target_os = "macos")))]
    open_with_default(path.parent().unwrap_or(path))
}

/// Returns the export folder path.
#[tauri::command]
pub fn export_all(app: AppHandle, store: State<'_, Arc<Store>>) -> Result<String, String> {
    let docs = app.path().document_dir().map_err(|e| e.to_string())?;
    let dir = docs
        .join("Pinq Exports")
        .join(format!("export-{}", pinq_core::now_ms()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let notes = store.all_notes();
    for n in &notes {
        if n.trashed {
            continue;
        }
        let first = n.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        let tail = &n.id[n.id.len().saturating_sub(6)..];
        let name = format!("{}-{}.md", slug(first), tail.to_ascii_lowercase());
        let mut body = md_from_note(&n.text);
        if !n.tags.is_empty() {
            let line = n.tags.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ");
            body = format!("{}\n\n{line}", body.trim_end());
        }
        std::fs::write(dir.join(name), body).map_err(|e| e.to_string())?;
    }
    // Full-fidelity backup: every note with all metadata, trash included.
    let json = serde_json::to_string_pretty(&notes).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("notes.json"), json).map_err(|e| e.to_string())?;

    let _ = open_with_default(&dir);

    Ok(dir.display().to_string())
}

/// Import dropped `.md`/`.txt` files as filed (unpinned) notes; `.zip` and
/// `.pinqbak` backups restore in full. Returns the count.
#[tauri::command]
pub fn import_files(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    sync_state: State<'_, crate::sync::SyncState>,
    paths: Vec<String>,
) -> Result<usize, String> {
    let wa = crate::monitors::primary_work_area(&app);
    let scale = crate::monitors::primary_scale(&app);
    let (w, h) = ((280.0 * scale).round() as u32, (220.0 * scale).round() as u32);
    let rect = Rect {
        x: wa.x + (wa.w as i32 - w as i32) / 2,
        y: wa.y + (wa.h as i32 - h as i32) / 2,
        w,
        h,
    };

    let mut count = 0;
    for p in paths.iter().take(200) {
        let path = std::path::Path::new(p);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        // Backup archives restore in full (notes + blobs).
        if ext.as_deref() == Some("zip") {
            let blobs = crate::blobs::blobs_dir(&app).map_err(|e| e.to_string())?;
            count += restore_backup(path, &store, &blobs).map_err(|e| e.to_string())?;
            continue;
        }
        // Encrypted backups: unseal with the space key, then restore normally.
        if ext.as_deref() == Some("pinqbak") {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let space_key = *sync_state.space_key.lock().unwrap();
            let plain = backup_open(&space_key, &bytes)?;
            let tmp = std::env::temp_dir().join(format!("pinq-restore-{}.zip", pinq_core::now_ms()));
            std::fs::write(&tmp, plain).map_err(|e| e.to_string())?;
            let blobs = crate::blobs::blobs_dir(&app).map_err(|e| e.to_string())?;
            let restored = restore_backup(&tmp, &store, &blobs).map_err(|e| e.to_string());
            let _ = std::fs::remove_file(&tmp);
            count += restored?;
            continue;
        }
        if !matches!(ext.as_deref(), Some("md") | Some("txt")) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else { continue };
        if meta.len() > 1_000_000 {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(path) else { continue };

        let (text, tags) = strip_hashtags(&note_from_md(&raw));
        let note = store.create_note(rect).map_err(|e| e.to_string())?;
        store.set_text(&note.id, text.trim_end().to_string()).map_err(|e| e.to_string())?;
        for tag in &tags {
            store.add_tag(&note.id, tag).map_err(|e| e.to_string())?;
        }
        store.file_note(&note.id).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_backup_roundtrips_and_rejects_wrong_key() {
        let key = [7u8; 32];
        let sealed = backup_seal(&key, b"zip bytes here").unwrap();
        assert_eq!(backup_open(&key, &sealed).unwrap(), b"zip bytes here");
        assert!(backup_open(&[8u8; 32], &sealed).is_err(), "wrong space key must fail");
        assert!(backup_open(&key, b"garbage").is_err());
    }

    #[test]
    fn checkbox_glyphs_roundtrip_markdown() {
        let note = "shopping\n\u{2610} milk\n\u{2611} bread\nplain line";
        let md = md_from_note(note);
        assert_eq!(md, "shopping\n- [ ] milk\n- [x] bread\nplain line");
        assert_eq!(note_from_md(&md), note);
    }

    #[test]
    fn bullets_export_as_md_lists() {
        let note = "ideas\n\u{2192} arrow item\n\u{2022} dot item";
        let md = md_from_note(note);
        assert_eq!(md, "ideas\n- arrow item\n- dot item");
        // Plain md lists import as dot bullets (arrowness isn't representable in md).
        assert_eq!(note_from_md(&md), "ideas\n\u{2022} arrow item\n\u{2022} dot item");
    }

    #[test]
    fn hashtags_strip_into_tags() {
        let (text, tags) = strip_hashtags("plan the offsite #work #q3\nbudget notes");
        assert_eq!(text, "plan the offsite\nbudget notes");
        assert_eq!(tags, vec!["work", "q3"]);
        // Punctuation-adjacent and mid-word hashes are not tags — they stay put.
        let (text, tags) = strip_hashtags("#done issue #42, code#comment");
        assert_eq!(tags, vec!["done"]);
        assert_eq!(text, "issue #42, code#comment");
    }

    #[test]
    fn backup_archive_roundtrips_notes_and_blobs() {
        let src = std::env::temp_dir().join(format!("pinq-bk-src-{}", pinq_core::new_note_id()));
        let dst = std::env::temp_dir().join(format!("pinq-bk-dst-{}", pinq_core::new_note_id()));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let src_blobs = src.join("blobs");
        let dst_blobs = dst.join("blobs");

        // Source store: a tagged, colored note with a text box and an image blob.
        let store = crate::store::Store::open(&src).unwrap();
        let note = store.create_note(pinq_core::Rect { x: 5, y: 6, w: 280, h: 220 }).unwrap();
        let hash = crate::blobs::store_blob_in(&src_blobs, b"fake image bytes").unwrap();
        store
            .add_item(
                &note.id,
                pinq_core::CanvasItem::text_item(8.0, 8.0, 200.0, "backed up".into()),
            )
            .unwrap();
        store
            .add_item(
                &note.id,
                pinq_core::CanvasItem::attachment_item(
                    10.0,
                    60.0,
                    120.0,
                    pinq_core::AttachmentRef {
                        hash: hash.clone(),
                        name: "pic.png".into(),
                        mime: "image/png".into(),
                        size: 16,
                    },
                ),
            )
            .unwrap();
        store.add_tag(&note.id, "vault").unwrap();

        let zip_path = src.join("backup.zip");
        write_backup(&zip_path, &store.all_notes(), &src_blobs).unwrap();

        // Restore into a fresh store + blob dir.
        let store2 = crate::store::Store::open(&dst).unwrap();
        let restored = restore_backup(&zip_path, &store2, &dst_blobs).unwrap();
        assert_eq!(restored, 1);
        let n = store2.get(&note.id).expect("note restored with original id");
        assert_eq!(n.text, "backed up");
        assert_eq!(n.tags, vec!["vault"]);
        assert_eq!(n.attachments.len(), 1);
        assert!(crate::blobs::blob_path_in(&dst_blobs, &hash).is_some(), "blob restored");

        // Restoring again is a no-op (idempotent).
        assert_eq!(restore_backup(&zip_path, &store2, &dst_blobs).unwrap(), 0);

        std::fs::remove_dir_all(src).ok();
        std::fs::remove_dir_all(dst).ok();
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("Demo env: reset @ 3pm!"), "demo-env-reset-3pm");
        assert_eq!(slug("   "), "note");
    }
}
