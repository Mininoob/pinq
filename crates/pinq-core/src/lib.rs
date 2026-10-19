//! Pinq core — prototype spine.
//!
//! Every mutation is an [`Action`] appended to an [`ActionLog`]; current state is a
//! pure fold over the log ([`replay`]). The full product replaces the JSONL file with
//! SQLite + CRDTs, but the invariant established here is permanent: the UI never
//! mutates state except by appending an action.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub type NoteId = String;

pub fn new_note_id() -> NoteId {
    ulid::Ulid::new().to_string()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Physical-pixel rectangle on the virtual desktop (prototype simplification;
/// the product stores monitor-relative logical rects keyed by stable monitor id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// A file attached to a note; the bytes live in the content-addressed blob
/// store under `hash`, so identical files are stored once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub hash: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// What a canvas item holds: a text box or an attached file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemContent {
    Text { text: String },
    Attachment { attachment: AttachmentRef },
}

/// A freely positioned element on a note's canvas (logical px, note-relative).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasItem {
    pub item_id: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    #[serde(flatten)]
    pub content: ItemContent,
}

impl CanvasItem {
    pub fn text_item(x: f64, y: f64, w: f64, text: String) -> Self {
        Self { item_id: new_note_id(), x, y, w, content: ItemContent::Text { text } }
    }

    pub fn attachment_item(x: f64, y: f64, w: f64, attachment: AttachmentRef) -> Self {
        Self { item_id: new_note_id(), x, y, w, content: ItemContent::Attachment { attachment } }
    }
}

/// Recompute the derived fields (`text` = text items joined; `attachments` =
/// refs in item order) after any item mutation. Search, previews, history, and
/// export all read the derived text, so they keep working unchanged.
pub fn rebuild_derived(n: &mut NoteState) {
    n.text = n
        .items
        .iter()
        .filter_map(|i| match &i.content {
            ItemContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    n.attachments = n
        .items
        .iter()
        .filter_map(|i| match &i.content {
            ItemContent::Attachment { attachment } => Some(attachment.clone()),
            _ => None,
        })
        .collect();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    NoteCreated { id: NoteId, at_ms: u64, rect: Rect },
    /// Covers both move and resize.
    NotePlaced { id: NoteId, at_ms: u64, rect: Rect },
    NoteTextChanged { id: NoteId, at_ms: u64, text: String },
    /// Per-note palette override; None returns the note to the global theme.
    NoteColorChanged { id: NoteId, at_ms: u64, color: Option<String> },
    /// Per-note paper override; None follows the global note-paper setting.
    NotePaperChanged { id: NoteId, at_ms: u64, paper: Option<String> },
    NoteTagged { id: NoteId, at_ms: u64, tag: String },
    NoteUntagged { id: NoteId, at_ms: u64, tag: String },
    AttachmentAdded { id: NoteId, at_ms: u64, attachment: AttachmentRef },
    AttachmentRemoved { id: NoteId, at_ms: u64, hash: String },
    ItemAdded { id: NoteId, at_ms: u64, item: CanvasItem },
    ItemTextChanged { id: NoteId, at_ms: u64, item_id: String, text: String },
    ItemMoved { id: NoteId, at_ms: u64, item_id: String, x: f64, y: f64, w: f64 },
    ItemRemoved { id: NoteId, at_ms: u64, item_id: String },
    /// Unpinned from the desktop (filed away). Data is kept.
    NoteFiled { id: NoteId, at_ms: u64 },
    /// Thrown at a screen edge: stays pinned but parks as a sliver peeking out
    /// of that edge ("left" | "right" | "top" | "bottom").
    NoteTucked { id: NoteId, at_ms: u64, edge: String, rect: Rect },
    /// Pulled back out of the edge to a normal on-screen position.
    NoteUntucked { id: NoteId, at_ms: u64, rect: Rect },
    NoteRepinned { id: NoteId, at_ms: u64, rect: Rect },
    /// Moved to trash: recoverable, unpinned, hidden from normal lists.
    NoteTrashed { id: NoteId, at_ms: u64 },
    NoteRestored { id: NoteId, at_ms: u64 },
    /// Permanent purge. Only ever emitted from an explicit, confirmed action.
    NoteDeleted { id: NoteId, at_ms: u64 },
    /// Reminder set (Some = when to fire, in ms epoch) or cleared (None).
    /// Firing commits a clear, so replicas that sync it never re-fire.
    ReminderSet { id: NoteId, at_ms: u64, remind_ms: Option<u64> },
    /// Task due date (ms epoch) set or cleared. Independent of the reminder:
    /// due is what the note IS about, remind is when it comes find you.
    DueSet { id: NoteId, at_ms: u64, due_ms: Option<u64> },
    /// Task status ("todo" | "doing" | "done") set or cleared (None = not a task).
    StatusSet { id: NoteId, at_ms: u64, status: Option<String> },
    /// Starred in the library.
    FavoriteSet { id: NoteId, at_ms: u64, fav: bool },
    /// Put away: hidden from the library's default views, search, and the
    /// graph, but never gone. Archiving closes the note's windows everywhere.
    ArchiveSet { id: NoteId, at_ms: u64, archived: bool },
}

impl Action {
    pub fn note_id(&self) -> &NoteId {
        match self {
            Action::NoteCreated { id, .. }
            | Action::NotePlaced { id, .. }
            | Action::NoteTextChanged { id, .. }
            | Action::NoteColorChanged { id, .. }
            | Action::NotePaperChanged { id, .. }
            | Action::NoteTagged { id, .. }
            | Action::NoteUntagged { id, .. }
            | Action::AttachmentAdded { id, .. }
            | Action::AttachmentRemoved { id, .. }
            | Action::ItemAdded { id, .. }
            | Action::ItemTextChanged { id, .. }
            | Action::ItemMoved { id, .. }
            | Action::ItemRemoved { id, .. }
            | Action::NoteFiled { id, .. }
            | Action::NoteTucked { id, .. }
            | Action::NoteUntucked { id, .. }
            | Action::NoteRepinned { id, .. }
            | Action::NoteTrashed { id, .. }
            | Action::NoteRestored { id, .. }
            | Action::NoteDeleted { id, .. }
            | Action::ReminderSet { id, .. }
            | Action::DueSet { id, .. }
            | Action::StatusSet { id, .. }
            | Action::FavoriteSet { id, .. }
            | Action::ArchiveSet { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NoteState {
    pub id: NoteId,
    pub rect: Rect,
    pub text: String,
    pub pinned: bool,
    #[serde(default)]
    pub trashed: bool,
    /// Palette id overriding the global theme, if set.
    #[serde(default)]
    pub color: Option<String>,
    /// Paper texture overriding the global note-paper setting, if set.
    #[serde(default)]
    pub paper: Option<String>,
    /// Edge this note is tucked into ("left"…"bottom"), if thrown at one.
    #[serde(default)]
    pub tucked: Option<String>,
    /// When to bring this note back (ms epoch); cleared once it fires.
    #[serde(default)]
    pub remind_ms: Option<u64>,
    /// Task due date (ms epoch), if this note is a task with one.
    #[serde(default)]
    pub due_ms: Option<u64>,
    /// Task status ("todo" | "doing" | "done"); None = not a task.
    #[serde(default)]
    pub status: Option<String>,
    /// Starred in the library.
    #[serde(default)]
    pub favorite: bool,
    /// Put away — excluded from default views and search, never deleted.
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Derived from `items`; kept for search, previews, history, and export.
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    /// The canvas: positioned text boxes and attachments.
    #[serde(default)]
    pub items: Vec<CanvasItem>,
    pub created_ms: u64,
    pub modified_ms: u64,
}

/// Fold a log into current state. Unknown ids in non-create actions are tolerated
/// (a truncated log must never wedge startup).
pub fn replay(actions: impl IntoIterator<Item = Action>) -> HashMap<NoteId, NoteState> {
    let mut notes: HashMap<NoteId, NoteState> = HashMap::new();
    for action in actions {
        apply(&mut notes, action);
    }
    notes
}

/// Fold a single action into state — the one place mutation semantics live.
pub fn apply(notes: &mut HashMap<NoteId, NoteState>, action: Action) {
    {
        match action {
            Action::NoteCreated { id, at_ms, rect } => {
                notes.insert(
                    id.clone(),
                    NoteState {
                        id,
                        rect,
                        text: String::new(),
                        pinned: true,
                        trashed: false,
                        tucked: None,
                        remind_ms: None,
                        due_ms: None,
                        status: None,
                        favorite: false,
                        archived: false,
                        color: None,
                        paper: None,
                        tags: Vec::new(),
                        attachments: Vec::new(),
                        items: Vec::new(),
                        created_ms: at_ms,
                        modified_ms: at_ms,
                    },
                );
            }
            Action::ItemAdded { id, at_ms, item } => {
                if let Some(n) = notes.get_mut(&id) {
                    if !n.items.iter().any(|i| i.item_id == item.item_id) {
                        n.items.push(item);
                    }
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            Action::ItemTextChanged { id, at_ms, item_id, text } => {
                if let Some(n) = notes.get_mut(&id) {
                    if let Some(i) = n.items.iter_mut().find(|i| i.item_id == item_id) {
                        i.content = ItemContent::Text { text };
                    }
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            Action::ItemMoved { id, at_ms, item_id, x, y, w } => {
                if let Some(n) = notes.get_mut(&id) {
                    if let Some(i) = n.items.iter_mut().find(|i| i.item_id == item_id) {
                        i.x = x;
                        i.y = y;
                        i.w = w;
                    }
                    n.modified_ms = at_ms;
                }
            }
            Action::ItemRemoved { id, at_ms, item_id } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.items.retain(|i| i.item_id != item_id);
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            // Legacy attachment actions (old logs): become cascaded items with
            // hash-derived ids (deterministic for the SQL projection).
            Action::AttachmentAdded { id, at_ms, attachment } => {
                if let Some(n) = notes.get_mut(&id) {
                    if !n.attachments.iter().any(|a| a.hash == attachment.hash) {
                        let k = n.items.len() as f64;
                        n.items.push(CanvasItem {
                            item_id: format!("a-{}", attachment.hash),
                            x: 12.0 + k * 16.0,
                            y: 140.0 + k * 12.0,
                            w: 120.0,
                            content: ItemContent::Attachment { attachment },
                        });
                    }
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            Action::AttachmentRemoved { id, at_ms, hash } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.items.retain(|i| !matches!(&i.content, ItemContent::Attachment { attachment } if attachment.hash == hash));
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteTagged { id, at_ms, tag } => {
                if let Some(n) = notes.get_mut(&id) {
                    if !n.tags.contains(&tag) {
                        n.tags.push(tag);
                    }
                    n.modified_ms = at_ms;
                }
            }
            Action::ReminderSet { id, at_ms, remind_ms } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.remind_ms = remind_ms;
                    n.modified_ms = at_ms;
                }
            }
            Action::DueSet { id, at_ms, due_ms } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.due_ms = due_ms;
                    n.modified_ms = at_ms;
                }
            }
            Action::StatusSet { id, at_ms, status } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.status = status;
                    n.modified_ms = at_ms;
                }
            }
            Action::FavoriteSet { id, at_ms, fav } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.favorite = fav;
                    n.modified_ms = at_ms;
                }
            }
            Action::ArchiveSet { id, at_ms, archived } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.archived = archived;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteUntagged { id, at_ms, tag } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.tags.retain(|t| t != &tag);
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteColorChanged { id, at_ms, color } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.color = color;
                    n.modified_ms = at_ms;
                }
            }
            Action::NotePaperChanged { id, at_ms, paper } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.paper = paper;
                    n.modified_ms = at_ms;
                }
            }
            Action::NotePlaced { id, at_ms, rect } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.rect = rect;
                    n.modified_ms = at_ms;
                }
            }
            // Legacy whole-note text (old logs + history restore): becomes the
            // single text item, replacing any existing text items. Item ids are
            // deterministic so the SQL projection replays to identical state.
            Action::NoteTextChanged { id, at_ms, text } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.items.retain(|i| !matches!(i.content, ItemContent::Text { .. }));
                    if !text.is_empty() {
                        n.items.insert(
                            0,
                            CanvasItem {
                                item_id: format!("t-{id}"),
                                x: 8.0,
                                y: 8.0,
                                w: 248.0,
                                content: ItemContent::Text { text },
                            },
                        );
                    }
                    rebuild_derived(n);
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteFiled { id, at_ms } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.pinned = false;
                    n.tucked = None;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteTucked { id, at_ms, edge, rect } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.tucked = Some(edge);
                    n.rect = rect;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteUntucked { id, at_ms, rect } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.tucked = None;
                    n.rect = rect;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteRepinned { id, at_ms, rect } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.pinned = true;
                    n.tucked = None;
                    n.rect = rect;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteTrashed { id, at_ms } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.trashed = true;
                    n.pinned = false;
                    n.tucked = None;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteRestored { id, at_ms } => {
                if let Some(n) = notes.get_mut(&id) {
                    n.trashed = false;
                    n.modified_ms = at_ms;
                }
            }
            Action::NoteDeleted { id, .. } => {
                notes.remove(&id);
            }
        }
    }
}

/// Append-only JSONL action log.
pub struct ActionLog {
    path: PathBuf,
}

impl ActionLog {
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    /// Load all actions. A corrupt or partially-written trailing line is skipped
    /// (crash mid-append), but corruption elsewhere is also tolerated line-by-line —
    /// the log must always load.
    pub fn load(&self) -> std::io::Result<Vec<Action>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let reader = BufReader::new(File::open(&self.path)?);
        let mut actions = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Action>(&line) {
                Ok(a) => actions.push(a),
                Err(_) => continue,
            }
        }
        Ok(actions)
    }

    pub fn append(&self, action: &Action) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        serde_json::to_writer(&mut file, action)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        file.write_all(b"\n")?;
        file.flush()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 280, h: 220 }
    }

    #[test]
    fn replay_folds_lifecycle() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(10, 10) },
            Action::NoteTextChanged { id: id.clone(), at_ms: 2, text: "hi".into() },
            Action::NotePlaced { id: id.clone(), at_ms: 3, rect: rect(50, 60) },
            Action::NoteColorChanged { id: id.clone(), at_ms: 4, color: Some("mint".into()) },
            Action::NotePaperChanged { id: id.clone(), at_ms: 5, paper: Some("ruled".into()) },
            Action::NoteFiled { id: id.clone(), at_ms: 6 },
        ]);
        let n = &notes[&id];
        assert_eq!(n.text, "hi");
        assert_eq!(n.rect, rect(50, 60));
        assert_eq!(n.color.as_deref(), Some("mint"));
        assert_eq!(n.paper.as_deref(), Some("ruled"));
        assert!(!n.pinned);
        assert_eq!(n.created_ms, 1);
        assert_eq!(n.modified_ms, 6);
    }

    #[test]
    fn filing_never_deletes() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteFiled { id: id.clone(), at_ms: 2 },
        ]);
        assert!(notes.contains_key(&id));
    }

    #[test]
    fn trash_hides_but_keeps_and_restore_recovers() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteTextChanged { id: id.clone(), at_ms: 2, text: "keep me".into() },
            Action::NoteTrashed { id: id.clone(), at_ms: 3 },
        ]);
        let n = &notes[&id];
        assert!(n.trashed);
        assert!(!n.pinned);
        assert_eq!(n.text, "keep me");

        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteTrashed { id: id.clone(), at_ms: 2 },
            Action::NoteRestored { id: id.clone(), at_ms: 3 },
        ]);
        assert!(!notes[&id].trashed);
    }

    #[test]
    fn tags_add_dedupe_and_remove() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteTagged { id: id.clone(), at_ms: 2, tag: "work".into() },
            Action::NoteTagged { id: id.clone(), at_ms: 3, tag: "urgent".into() },
            Action::NoteTagged { id: id.clone(), at_ms: 4, tag: "work".into() },
            Action::NoteUntagged { id: id.clone(), at_ms: 5, tag: "urgent".into() },
        ]);
        assert_eq!(notes[&id].tags, vec!["work"]);
    }

    #[test]
    fn attachments_add_dedupe_and_remove() {
        let id = new_note_id();
        let att = AttachmentRef {
            hash: "abc123".into(),
            name: "photo.png".into(),
            mime: "image/png".into(),
            size: 1234,
        };
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::AttachmentAdded { id: id.clone(), at_ms: 2, attachment: att.clone() },
            Action::AttachmentAdded { id: id.clone(), at_ms: 3, attachment: att.clone() },
        ]);
        assert_eq!(notes[&id].attachments.len(), 1);
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::AttachmentAdded { id: id.clone(), at_ms: 2, attachment: att },
            Action::AttachmentRemoved { id: id.clone(), at_ms: 3, hash: "abc123".into() },
        ]);
        assert!(notes[&id].attachments.is_empty());
    }

    #[test]
    fn canvas_items_drive_derived_text_and_attachments() {
        let id = new_note_id();
        let t1 = CanvasItem::text_item(8.0, 8.0, 200.0, "first box".into());
        let t2 = CanvasItem::text_item(30.0, 90.0, 180.0, "second box".into());
        let att = CanvasItem::attachment_item(
            50.0,
            150.0,
            120.0,
            AttachmentRef { hash: "h1".into(), name: "a.png".into(), mime: "image/png".into(), size: 1 },
        );
        let t2_id = t2.item_id.clone();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::ItemAdded { id: id.clone(), at_ms: 2, item: t1 },
            Action::ItemAdded { id: id.clone(), at_ms: 3, item: t2 },
            Action::ItemAdded { id: id.clone(), at_ms: 4, item: att },
            Action::ItemTextChanged { id: id.clone(), at_ms: 5, item_id: t2_id.clone(), text: "edited".into() },
            Action::ItemMoved { id: id.clone(), at_ms: 6, item_id: t2_id.clone(), x: 99.0, y: 77.0, w: 150.0 },
        ]);
        let n = &notes[&id];
        assert_eq!(n.text, "first box\n\nedited");
        assert_eq!(n.attachments.len(), 1);
        let moved = n.items.iter().find(|i| i.item_id == t2_id).unwrap();
        assert_eq!((moved.x, moved.y, moved.w), (99.0, 77.0, 150.0));

        // Removing an item updates the derived text too.
        let gone = CanvasItem::text_item(0.0, 0.0, 100.0, "bye".into());
        let gone_id = gone.item_id.clone();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::ItemAdded { id: id.clone(), at_ms: 2, item: gone },
            Action::ItemRemoved { id: id.clone(), at_ms: 3, item_id: gone_id },
        ]);
        assert_eq!(notes[&id].text, "");
        assert!(notes[&id].items.is_empty());
    }

    #[test]
    fn reminder_set_fire_clear_roundtrip() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::ReminderSet { id: id.clone(), at_ms: 2, remind_ms: Some(5000) },
        ]);
        assert_eq!(notes[&id].remind_ms, Some(5000));
        // Firing commits a clear; replay converges on "no reminder".
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::ReminderSet { id: id.clone(), at_ms: 2, remind_ms: Some(5000) },
            Action::ReminderSet { id: id.clone(), at_ms: 5, remind_ms: None },
        ]);
        assert_eq!(notes[&id].remind_ms, None);
    }

    #[test]
    fn due_and_status_roundtrip() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::DueSet { id: id.clone(), at_ms: 2, due_ms: Some(9000) },
            Action::StatusSet { id: id.clone(), at_ms: 3, status: Some("doing".into()) },
        ]);
        assert_eq!(notes[&id].due_ms, Some(9000));
        assert_eq!(notes[&id].status.as_deref(), Some("doing"));
        // Clearing both returns the note to "not a task".
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::DueSet { id: id.clone(), at_ms: 2, due_ms: Some(9000) },
            Action::StatusSet { id: id.clone(), at_ms: 3, status: Some("doing".into()) },
            Action::DueSet { id: id.clone(), at_ms: 4, due_ms: None },
            Action::StatusSet { id: id.clone(), at_ms: 5, status: None },
        ]);
        assert_eq!(notes[&id].due_ms, None);
        assert_eq!(notes[&id].status, None);
    }

    #[test]
    fn item_readd_after_remove_restores() {
        // Undo of an item delete re-commits ItemAdded with the ORIGINAL item_id;
        // canonical replay must land on "item present" with derived text rebuilt.
        let id = new_note_id();
        let item = CanvasItem::text_item(8.0, 8.0, 200.0, "back again".into());
        let item_id = item.item_id.clone();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::ItemAdded { id: id.clone(), at_ms: 2, item: item.clone() },
            Action::ItemRemoved { id: id.clone(), at_ms: 3, item_id: item_id.clone() },
            Action::ItemAdded { id: id.clone(), at_ms: 4, item },
        ]);
        let n = &notes[&id];
        assert_eq!(n.items.len(), 1);
        assert_eq!(n.items[0].item_id, item_id);
        assert_eq!(n.text, "back again");
    }

    #[test]
    fn tuck_parks_at_an_edge_and_untuck_restores() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(100, 100) },
            Action::NoteTucked { id: id.clone(), at_ms: 2, edge: "right".into(), rect: rect(1900, 300) },
        ]);
        let n = &notes[&id];
        assert_eq!(n.tucked.as_deref(), Some("right"));
        assert!(n.pinned, "tucked notes stay pinned");
        assert_eq!(n.rect, rect(1900, 300));

        // Untuck restores; filing a tucked note clears the tuck too.
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(100, 100) },
            Action::NoteTucked { id: id.clone(), at_ms: 2, edge: "right".into(), rect: rect(1900, 300) },
            Action::NoteUntucked { id: id.clone(), at_ms: 3, rect: rect(1500, 300) },
        ]);
        assert!(notes[&id].tucked.is_none());
        assert_eq!(notes[&id].rect, rect(1500, 300));

        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteTucked { id: id.clone(), at_ms: 2, edge: "left".into(), rect: rect(-260, 0) },
            Action::NoteFiled { id: id.clone(), at_ms: 3 },
        ]);
        assert!(notes[&id].tucked.is_none());
        assert!(!notes[&id].pinned);
    }

    #[test]
    fn purge_removes_entirely() {
        let id = new_note_id();
        let notes = replay([
            Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(0, 0) },
            Action::NoteDeleted { id: id.clone(), at_ms: 2 },
        ]);
        assert!(!notes.contains_key(&id));
    }

    #[test]
    fn log_roundtrip_and_corrupt_tail() {
        let dir = std::env::temp_dir().join(format!("pinq-core-test-{}", new_note_id()));
        let log = ActionLog::open(dir.join("actions.jsonl")).unwrap();
        let id = new_note_id();
        log.append(&Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect(1, 2) }).unwrap();
        log.append(&Action::NoteTextChanged { id: id.clone(), at_ms: 2, text: "x".into() }).unwrap();
        // Simulate a crash mid-append.
        {
            let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
            f.write_all(b"{\"kind\":\"note_text_ch").unwrap();
        }
        let actions = log.load().unwrap();
        assert_eq!(actions.len(), 2);
        let notes = replay(actions);
        assert_eq!(notes[&id].text, "x");
        fs::remove_dir_all(dir).ok();
    }
}
