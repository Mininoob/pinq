//! [[Note linking]]: wiki-style links between notes, stored as plain
//! `[[Title]]` text inside text items. A link resolves to the note whose title
//! (first non-empty line) matches, case-insensitively — so links survive sync,
//! export as Obsidian-style Markdown, and need no schema change. Backlinks are
//! derived by scanning note text, never stored.

use crate::store::Store;
use pinq_core::NoteState;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Titles are capped so links stay short; matching uses the same cap, so a
/// link inserted from a capped title always resolves.
const TITLE_MAX: usize = 80;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkTarget {
    pub id: String,
    pub title: String,
    pub pinned: bool,
}

/// A note's title: its first non-empty line, trimmed and capped.
pub fn title_of(n: &NoteState) -> String {
    n.text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(TITLE_MAX)
        .collect()
}

/// The note a `[[title]]` link points at: exact title match, case-insensitive,
/// trash excluded. Pinned notes win ties, then most recently modified.
pub fn resolve<'a>(notes: &'a [NoteState], title: &str) -> Option<&'a NoteState> {
    let want = title.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    notes
        .iter()
        .filter(|n| !n.trashed && title_of(n).to_lowercase() == want)
        .max_by_key(|n| (n.pinned, n.modified_ms))
}

/// Autocomplete candidates for `[[query`: titled, non-trashed notes whose
/// title contains the query (all notes when empty). Prefix matches first,
/// then newest; duplicate titles collapse to their newest note.
pub fn targets(notes: &[NoteState], query: &str, limit: usize) -> Vec<LinkTarget> {
    let q = query.trim().to_lowercase();
    let mut sorted: Vec<&NoteState> = notes.iter().filter(|n| !n.trashed).collect();
    sorted.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));

    let mut seen = std::collections::HashSet::new();
    let mut hits: Vec<(bool, LinkTarget)> = Vec::new();
    for n in sorted {
        let title = title_of(n);
        let lower = title.to_lowercase();
        if title.is_empty() || (!q.is_empty() && !lower.contains(&q)) {
            continue;
        }
        if !seen.insert(lower.clone()) {
            continue;
        }
        hits.push((
            lower.starts_with(&q),
            LinkTarget { id: n.id.clone(), title, pinned: n.pinned },
        ));
    }
    hits.sort_by_key(|(prefix, _)| !prefix); // stable: keeps newest-first within groups
    hits.into_iter().map(|(_, t)| t).take(limit).collect()
}

/// Every non-trashed note whose text contains `[[title-of-target]]`
/// (case-insensitive), newest first. Untitled notes have no backlinks.
pub fn backlinks_of(notes: &[NoteState], id: &str) -> Vec<LinkTarget> {
    let Some(target) = notes.iter().find(|n| n.id == id) else {
        return Vec::new();
    };
    let title = title_of(target).to_lowercase();
    if title.is_empty() {
        return Vec::new();
    }
    let needle = format!("[[{title}]]");
    let mut hits: Vec<&NoteState> = notes
        .iter()
        .filter(|n| n.id != id && !n.trashed && n.text.to_lowercase().contains(&needle))
        .collect();
    hits.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    hits.iter()
        .map(|n| LinkTarget { id: n.id.clone(), title: title_of(n), pinned: n.pinned })
        .collect()
}

/// Rewrite every `[[old]]` link in `text` to `[[new_title]]` (old matched
/// case-insensitively on the trimmed inner title, mirroring `resolve`).
/// Returns None when nothing matched. Parses like the frontend LINK_RE:
/// inner titles contain no brackets or newlines.
pub fn rewrite_links_text(text: &str, old_lc: &str, new_title: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(i) = rest.find("[[") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        if let Some(j) = after.find("]]") {
            let inner = &after[..j];
            if !inner.contains('[')
                && !inner.contains(']')
                && !inner.contains('\n')
                && inner.trim().to_lowercase() == old_lc
            {
                out.push_str("[[");
                out.push_str(new_title);
                out.push_str("]]");
                changed = true;
                rest = &after[j + 2..];
                continue;
            }
        }
        out.push_str("[[");
        rest = after;
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// After a LOCAL edit to note `id`, retarget links if its title changed:
/// every `[[old title]]` elsewhere becomes `[[new title]]`. Cascades — a
/// rewrite can change the linking note's own first line — with a visited set
/// so cycles terminate. Returns the ids of rewritten notes (empty when the
/// title didn't change or links were left alone).
///
/// Deliberately NOT called when applying remote sync ops: the rewrites are
/// ordinary ItemTextChanged ops that sync themselves; rewriting again on the
/// receiving side would duplicate them.
pub fn propagate_rename(store: &Store, id: &str, old_title: &str) -> Vec<String> {
    let mut queue = vec![(id.to_string(), old_title.trim().to_string())];
    let mut visited = std::collections::HashSet::new();
    let mut rewritten = Vec::new();
    while let Some((nid, old)) = queue.pop() {
        if old.is_empty() || !visited.insert(nid.clone()) {
            continue;
        }
        let notes = store.all_notes();
        let Some(renamed) = notes.iter().find(|n| n.id == nid) else { continue };
        let new = title_of(renamed);
        let new = new.trim();
        let old_lc = old.to_lowercase();
        if new.is_empty() || new.to_lowercase() == old_lc {
            continue;
        }
        // If another live note still bears the old title, links still resolve
        // to it — leave them pointing there.
        if notes
            .iter()
            .any(|n| n.id != nid && !n.trashed && title_of(n).to_lowercase() == old_lc)
        {
            continue;
        }
        for n in &notes {
            if n.id == nid || n.trashed {
                continue;
            }
            let before_title = title_of(n);
            let mut any_item = false;
            for item in &n.items {
                if let pinq_core::ItemContent::Text { text } = &item.content {
                    if let Some(updated) = rewrite_links_text(text, &old_lc, new) {
                        let _ = store.set_item_text(&n.id, &item.item_id, updated);
                        any_item = true;
                    }
                }
            }
            if any_item {
                rewritten.push(n.id.clone());
                // The link may sit in this note's first line — its own title
                // just changed, so its inbound links need the same treatment.
                if let Some(after) = store.get(&n.id) {
                    if title_of(&after).to_lowercase() != before_title.to_lowercase() {
                        queue.push((n.id.clone(), before_title));
                    }
                }
            }
        }
    }
    rewritten
}

/// Command-side wrapper: run the mutation's rename propagation and let every
/// open window know. Reuses the sync-applied event — note windows already
/// reload their items from it (preserving uncommitted typing) and the library
/// refreshes.
pub fn sync_links_after_edit(app: &AppHandle, store: &Store, id: &str, old_title: &str) {
    // Hot path — most edits don't touch the title; one note clone, no scan.
    let new_title = store.get(id).map(|n| title_of(&n)).unwrap_or_default();
    if new_title.trim().to_lowercase() == old_title.trim().to_lowercase() {
        return;
    }
    let rewritten = propagate_rename(store, id, old_title);
    if !rewritten.is_empty() {
        use tauri::Emitter;
        let _ = app.emit("pinq-sync-applied", &rewritten);
    }
}

/// Autocomplete for the `[[` popup in a note.
#[tauri::command]
pub fn link_targets(store: State<'_, Arc<Store>>, query: String) -> Vec<LinkTarget> {
    targets(&store.all_notes(), &query, 8)
}

/// Notes that link to this one, for the note's backlinks strip.
#[tauri::command]
pub fn get_backlinks(store: State<'_, Arc<Store>>, id: String) -> Vec<LinkTarget> {
    backlinks_of(&store.all_notes(), &id)
}

/// Follow a `[[title]]` link: bring the target note to the desktop (pin,
/// untuck, or focus as needed). If no note has that title, create one —
/// wiki-style — seeded with the title, near the source note. Returns the
/// target's id. Async: opens windows, must stay off the main thread.
#[tauri::command]
pub async fn open_link(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    from_id: String,
    title: String,
) -> Result<String, String> {
    let title = title.trim().to_string();
    let existing = resolve(&store.all_notes(), &title).map(|n| n.id.clone());

    if let Some(id) = existing {
        if id != from_id {
            crate::notes::open_note_by_id(&app, &store, &id)?;
        }
        return Ok(id);
    }

    // No note has this title yet: create it next to the source, like a wiki.
    let mut rect = store
        .get(&from_id)
        .map(|src| pinq_core::Rect {
            x: src.rect.x + 48,
            y: src.rect.y + 48,
            ..src.rect
        })
        .unwrap_or(pinq_core::Rect { x: 160, y: 160, w: 280, h: 240 });
    let wa = crate::monitors::work_area_at(&app, rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2);
    rect = crate::notes::clamp_to_area(rect, wa);

    let note = store.create_note(rect).map_err(|e| e.to_string())?;
    store
        .add_item(&note.id, pinq_core::CanvasItem::text_item(8.0, 8.0, 248.0, title))
        .map_err(|e| e.to_string())?;
    let note = store.get(&note.id).ok_or("note vanished")?;
    crate::notes::open_note_window(&app, &note).map_err(|e| e.to_string())?;
    Ok(note.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinq_core::Rect;

    fn note(id: &str, text: &str, pinned: bool, modified_ms: u64) -> NoteState {
        NoteState {
            id: id.into(),
            rect: Rect { x: 0, y: 0, w: 280, h: 240 },
            text: text.into(),
            pinned,
            trashed: false,
            color: None,
            paper: None,
            tucked: None,
            remind_ms: None,
            tags: Vec::new(),
            attachments: Vec::new(),
            items: Vec::new(),
            created_ms: 0,
            modified_ms,
        }
    }

    #[test]
    fn resolve_is_case_insensitive_and_prefers_pinned_then_newest() {
        let mut old = note("a", "Groceries\nmilk", false, 1);
        let newer = note("b", "  groceries  ", false, 2);
        let pinned = note("c", "GROCERIES", true, 1);
        let notes = vec![old.clone(), newer.clone(), pinned.clone()];
        assert_eq!(resolve(&notes, "Groceries").unwrap().id, "c");

        // Without the pinned one, newest wins; trashed never resolves.
        old.trashed = true;
        let notes = vec![old, newer];
        assert_eq!(resolve(&notes, "gRoCeRiEs").unwrap().id, "b");
        assert!(resolve(&[], "anything").is_none());
        assert!(resolve(&[note("x", "", true, 5)], "").is_none());
    }

    #[test]
    fn targets_prefix_first_then_newest_no_dupes() {
        let notes = vec![
            note("a", "Project pinq", false, 1),
            note("b", "Pinq ideas", false, 3),
            note("c", "pinq ideas", false, 2), // duplicate title, older
            note("d", "Untitled\n", false, 9), // blank first line only if all blank
            note("e", "shopping", false, 4),
        ];
        let hits = targets(&notes, "pinq", 8);
        let titles: Vec<&str> = hits.iter().map(|t| t.title.as_str()).collect();
        // Prefix matches ("Pinq ideas") before substring matches; dupe collapsed.
        assert_eq!(titles, vec!["Pinq ideas", "Project pinq"]);
        assert_eq!(hits[0].id, "b");

        // Empty query lists everything titled, newest first.
        let all = targets(&notes, "", 8);
        assert_eq!(all.first().unwrap().id, "d");
        assert_eq!(all.len(), 4); // dupe still collapsed
    }

    #[test]
    fn rewrite_links_text_is_case_insensitive_and_leaves_non_links() {
        // Matching is ci on the trimmed inner title; other links untouched.
        let text = "see [[groceries]] and [[ GROCERIES ]] but not [[Grocery run]]\nplain groceries";
        let out = rewrite_links_text(text, "groceries", "Food list").unwrap();
        assert_eq!(
            out,
            "see [[Food list]] and [[Food list]] but not [[Grocery run]]\nplain groceries"
        );
        // No match → None; malformed/nested brackets are not links.
        assert!(rewrite_links_text("nothing here", "groceries", "x").is_none());
        assert!(rewrite_links_text("[[a [[b]] c", "a [[b", "x").is_none());
        assert_eq!(rewrite_links_text("[[a [[groceries]]", "groceries", "x").unwrap(), "[[a [[x]]");
    }

    #[test]
    fn propagate_rename_rewrites_links_but_respects_duplicate_titles() {
        let dir = std::env::temp_dir().join(format!("pinq-links-test-{}", pinq_core::new_note_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        let r = Rect { x: 0, y: 0, w: 280, h: 240 };

        let a = store.create_note(r).unwrap();
        store
            .add_item(&a.id, pinq_core::CanvasItem::text_item(0.0, 0.0, 200.0, "Groceries\nmilk".into()))
            .unwrap();
        let b = store.create_note(r).unwrap();
        store
            .add_item(&b.id, pinq_core::CanvasItem::text_item(0.0, 0.0, 200.0, "see [[groceries]] later".into()))
            .unwrap();

        // Rename A's title, then propagate: B's link follows.
        let a_item = store.get(&a.id).unwrap().items[0].item_id.clone();
        store.set_item_text(&a.id, &a_item, "Food list\nmilk".into()).unwrap();
        let rewritten = propagate_rename(&store, &a.id, "Groceries");
        assert_eq!(rewritten, vec![b.id.clone()]);
        assert!(store.get(&b.id).unwrap().text.contains("[[Food list]]"));

        // A second note still titled "Food list" keeps links valid — renaming
        // one of the pair must NOT rewrite them.
        let c = store.create_note(r).unwrap();
        store
            .add_item(&c.id, pinq_core::CanvasItem::text_item(0.0, 0.0, 200.0, "Food list\nother".into()))
            .unwrap();
        let a_item = store.get(&a.id).unwrap().items[0].item_id.clone();
        store.set_item_text(&a.id, &a_item, "Menu\nmilk".into()).unwrap();
        assert!(propagate_rename(&store, &a.id, "Food list").is_empty());
        assert!(store.get(&b.id).unwrap().text.contains("[[Food list]]"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backlinks_scan_is_case_insensitive_and_skips_trash() {
        let target = note("t", "Big Plan\ndetails", false, 1);
        let linker = note("l", "see [[big plan]] soon", false, 5);
        let mut trashed = note("x", "also [[Big Plan]]", false, 3);
        trashed.trashed = true;
        let other = note("o", "unrelated", false, 4);
        let notes = vec![target, linker, trashed, other];

        let back = backlinks_of(&notes, "t");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "l");
        // A note never backlinks itself, and untitled notes have none.
        assert!(backlinks_of(&notes, "l")[..].iter().all(|b| b.id != "l"));
        assert!(backlinks_of(&[note("u", "", false, 1)], "u").is_empty());
    }
}
