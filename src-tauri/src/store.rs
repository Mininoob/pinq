//! Desktop-shell store, SQLite-backed (spec §18, prototype slice).
//!
//! Architecture is unchanged from the JSONL era: every mutation is an [`Action`]
//! appended to an append-only op-log; current state is a fold over it. SQLite adds
//! transactionality (op + projection commit atomically), WAL crash safety, and an
//! FTS5 full-text index over note text. The in-memory map stays the fast read path;
//! the `notes` table is the durable projection loaded at open.
//!
//! Legacy `actions.jsonl` logs are imported into the op-log on first open and the
//! file is renamed `.imported` (kept as a backup, never deleted).

use pinq_core::{now_ms, Action, NoteId, NoteState, Rect};
use rusqlite::Connection;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;

fn db_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// THIS device's layout for one note. Presentation is deliberately not
/// content: it never becomes ops and never syncs — pinning, tucking, and
/// window position are each device's own business. Content replay still
/// mutates the base NoteState's pinned/tucked/rect (legacy ops from older
/// devices), but this overlay wins at read time.
#[derive(Clone, Debug)]
pub struct Presentation {
    pub pinned: bool,
    pub tucked: Option<String>,
    pub rect: Rect,
}

pub struct Store {
    db: Mutex<Connection>,
    notes: Mutex<HashMap<NoteId, NoteState>>,
    /// Device-local presentation overlay; notes absent here read as
    /// unpinned/untucked (a note that synced in but was never pulled out).
    presentation: Mutex<HashMap<NoteId, Presentation>>,
    /// Latest unflushed rect per note; drained by the flusher thread once per second
    /// so a window drag doesn't write hundreds of actions.
    pending_place: Mutex<HashMap<NoteId, Rect>>,
    /// Replication actor id: random, minted once per database, independent of the
    /// crypto device key (so tests and key rotation never re-attribute ops).
    device_id: String,
    /// Fires after every local content commit (wakes the sync loop).
    commit_hook: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// Fires with the ids of notes a remote sync introduced (arrival toast).
    arrivals_hook: std::sync::OnceLock<Box<dyn Fn(&[NoteId]) + Send + Sync>>,
}

/// One op as it travels between devices: origin identity plus the action JSON.
/// `(device, ctr)` is globally unique — ctr is a per-device counter — so ops
/// dedupe on arrival and version vectors are just max-ctr-per-device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncOp {
    pub device: String,
    pub ctr: u64,
    pub at_ms: u64,
    pub action: String,
}

/// One signed device-registry record: "`device_pk` (named `name`) was
/// added/revoked by `signer_pk` at `at_ms`". Validation and membership
/// folding live in `registry.rs` — the store only holds the records.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegistryEntry {
    pub kind: String, // "add" | "revoke"
    pub device_pk: String,
    pub name: String,
    pub at_ms: u64,
    pub signer_pk: String,
    pub sig: String,
}

const BASE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS oplog (
    seq    INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms  INTEGER NOT NULL,
    action TEXT    NOT NULL,
    device TEXT    NOT NULL DEFAULT '',
    ctr    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS registry (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    space     TEXT NOT NULL,
    kind      TEXT NOT NULL,
    device_pk TEXT NOT NULL,
    name      TEXT NOT NULL,
    at_ms     INTEGER NOT NULL,
    signer_pk TEXT NOT NULL,
    sig       TEXT NOT NULL,
    UNIQUE(space, kind, device_pk, signer_pk)
);
CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY,
    x INTEGER, y INTEGER, w INTEGER, h INTEGER,
    text        TEXT NOT NULL DEFAULT '',
    pinned      INTEGER NOT NULL DEFAULT 1,
    trashed     INTEGER NOT NULL DEFAULT 0,
    tucked      TEXT,
    remind_ms   INTEGER,
    due_ms      INTEGER,
    status      TEXT,
    color       TEXT,
    paper       TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',
    attachments TEXT NOT NULL DEFAULT '[]',
    items       TEXT NOT NULL DEFAULT '[]',
    created_ms  INTEGER NOT NULL,
    modified_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS presentation (
    note_id TEXT PRIMARY KEY,
    pinned  INTEGER NOT NULL DEFAULT 0,
    tucked  TEXT,
    x INTEGER, y INTEGER, w INTEGER, h INTEGER
);
";

/// FTS over text AND tags (the JSON array tokenizes fine — brackets/quotes are
/// separators), so quick-find matches tags too.
const FTS_SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(text, tags, content='notes', content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, text, tags) VALUES (new.rowid, new.text, new.tags);
END;
CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, text, tags) VALUES ('delete', old.rowid, old.text, old.tags);
END;
CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE OF text, tags ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, text, tags) VALUES ('delete', old.rowid, old.text, old.tags);
    INSERT INTO notes_fts(rowid, text, tags) VALUES (new.rowid, new.text, new.tags);
END;
";

const BACKUP_KEEP: usize = 7;
const BACKUP_EVERY_MS: u64 = 24 * 60 * 60 * 1000;

fn passes_quick_check(conn: &Connection) -> bool {
    conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
        .map(|s| s == "ok")
        .unwrap_or(false)
}

/// Open the DB, verifying integrity. A corrupt file is renamed aside and the
/// newest healthy backup restored; a fresh DB is the last resort. Never
/// open-and-pray, never silently delete.
fn open_verified(data_dir: &Path, db_path: &Path) -> io::Result<Connection> {
    if let Ok(conn) = Connection::open(db_path) {
        if passes_quick_check(&conn) {
            return Ok(conn);
        }
        drop(conn);
    }
    // Corrupt or unreadable: keep the evidence, then try backups.
    let stamp = now_ms();
    for suffix in ["", "-wal", "-shm"] {
        let p = data_dir.join(format!("pinq.db{suffix}"));
        if p.exists() {
            let _ = std::fs::rename(&p, data_dir.join(format!("pinq.db{suffix}.corrupt-{stamp}")));
        }
    }
    eprintln!("pinq: database failed integrity check; attempting backup restore");

    let mut backups: Vec<std::path::PathBuf> = std::fs::read_dir(data_dir.join("backups"))
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == "db").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    backups.sort();
    for backup in backups.iter().rev() {
        if std::fs::copy(backup, db_path).is_ok() {
            if let Ok(conn) = Connection::open(db_path) {
                if passes_quick_check(&conn) {
                    eprintln!("pinq: restored from backup {}", backup.display());
                    return Ok(conn);
                }
                drop(conn);
            }
            let _ = std::fs::remove_file(db_path);
        }
    }
    eprintln!("pinq: no usable backup; starting with a fresh database");
    Connection::open(db_path).map_err(db_err)
}

/// Snapshot the DB into backups/ at most once a day; keep the newest few.
fn maybe_backup(conn: &Connection, data_dir: &Path) {
    let dir = data_dir.join("backups");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut existing: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == "db").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    existing.sort();

    let fresh_enough = existing.last().and_then(|p| {
        let name = p.file_stem()?.to_str()?;
        let ts: u64 = name.strip_prefix("pinq-")?.parse().ok()?;
        Some(now_ms().saturating_sub(ts) < BACKUP_EVERY_MS)
    });
    if fresh_enough == Some(true) {
        return;
    }

    let target = dir.join(format!("pinq-{}.db", now_ms()));
    let sql = format!("VACUUM INTO '{}'", target.display().to_string().replace('\'', "''"));
    if conn.execute_batch(&sql).is_ok() {
        existing.push(target);
        while existing.len() > BACKUP_KEEP {
            let _ = std::fs::remove_file(existing.remove(0));
        }
    }
}

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("pinq.db");
        let mut conn = open_verified(data_dir, &db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(db_err)?;
        conn.execute_batch(BASE_SCHEMA).map_err(db_err)?;

        // Sync identity (v4): every op is stamped (device, ctr). Columns must
        // exist before any migration writes ops; pre-v4 DBs gain them here
        // (the ALTERs no-op on fresh DBs, which have them via BASE_SCHEMA).
        let _ = conn.execute("ALTER TABLE oplog ADD COLUMN device TEXT NOT NULL DEFAULT ''", []);
        let _ = conn.execute("ALTER TABLE oplog ADD COLUMN ctr INTEGER NOT NULL DEFAULT 0", []);
        let device_id: String = {
            use rusqlite::OptionalExtension;
            let existing = conn
                .query_row("SELECT value FROM meta WHERE key='device_id'", [], |r| r.get(0))
                .optional()
                .map_err(db_err)?;
            match existing {
                Some(id) => id,
                None => {
                    let mut raw = [0u8; 8];
                    getrandom::fill(&mut raw).map_err(io::Error::other)?;
                    let id: String = raw.iter().map(|b| format!("{b:02x}")).collect();
                    conn.execute(
                        "INSERT INTO meta (key, value) VALUES ('device_id', ?1)",
                        [&id],
                    )
                    .map_err(db_err)?;
                    id
                }
            }
        };
        // Backfill: pre-sync local ops become ours, ctr = seq (already unique
        // and in order). Must run before later migrations append stamped ops.
        conn.execute(
            "UPDATE oplog SET device = ?1, ctr = seq WHERE device = ''",
            [&device_id],
        )
        .map_err(db_err)?;
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS oplog_device_ctr ON oplog(device, ctr);",
        )
        .map_err(db_err)?;

        // v1 migration: tags column + two-column FTS. Runs once (user_version).
        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_err)?;
        if ver < 1 {
            // Fresh DBs already have the column; pre-tags DBs gain it here.
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN tags TEXT NOT NULL DEFAULT '[]'", []);
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS notes_ai;
                 DROP TRIGGER IF EXISTS notes_ad;
                 DROP TRIGGER IF EXISTS notes_au;
                 DROP TABLE IF EXISTS notes_fts;",
            )
            .map_err(db_err)?;
        }
        conn.execute_batch(FTS_SCHEMA).map_err(db_err)?;
        if ver < 1 {
            conn.execute("INSERT INTO notes_fts(notes_fts) VALUES ('rebuild')", [])
                .map_err(db_err)?;
        }
        // v2: attachments column (fresh DBs already have it via BASE_SCHEMA).
        if ver < 2 {
            let _ = conn.execute(
                "ALTER TABLE notes ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
                [],
            );
        }
        // v3: canvas items — seed each legacy note's text/attachments as items
        // (real ItemAdded ops, so every device replays identically).
        if ver < 3 {
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN items TEXT NOT NULL DEFAULT '[]'", []);
            let legacy: Vec<(String, String, String)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, text, attachments FROM notes
                         WHERE items = '[]' AND (text != '' OR attachments != '[]')",
                    )
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .map_err(db_err)?;
                rows.collect::<Result<_, _>>().map_err(db_err)?
            };
            let tx = conn.transaction().map_err(db_err)?;
            for (id, text, atts_json) in legacy {
                let mut y = 8.0;
                if !text.is_empty() {
                    let item = pinq_core::CanvasItem::text_item(8.0, 8.0, 248.0, text);
                    log_and_project(&tx, &device_id, &Action::ItemAdded { id: id.clone(), at_ms: now_ms(), item })?;
                    y = 160.0;
                }
                let atts: Vec<pinq_core::AttachmentRef> =
                    serde_json::from_str(&atts_json).unwrap_or_default();
                for (k, attachment) in atts.into_iter().enumerate() {
                    let item = pinq_core::CanvasItem::attachment_item(
                        12.0 + k as f64 * 18.0,
                        y + k as f64 * 14.0,
                        130.0,
                        attachment,
                    );
                    log_and_project(&tx, &device_id, &Action::ItemAdded { id: id.clone(), at_ms: now_ms(), item })?;
                }
            }
            tx.commit().map_err(db_err)?;
        }
        // v5: tucked column (fresh DBs already have it via BASE_SCHEMA).
        if ver < 5 {
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN tucked TEXT", []);
            conn.pragma_update(None, "user_version", 5).map_err(db_err)?;
        }
        // v6: reminder column (fresh DBs already have it via BASE_SCHEMA).
        if ver < 6 {
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN remind_ms INTEGER", []);
            conn.pragma_update(None, "user_version", 6).map_err(db_err)?;
        }
        // v7: task fields (fresh DBs already have them via BASE_SCHEMA).
        if ver < 7 {
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN due_ms INTEGER", []);
            let _ = conn.execute("ALTER TABLE notes ADD COLUMN status TEXT", []);
            conn.pragma_update(None, "user_version", 7).map_err(db_err)?;
        }

        // One-time import of the legacy JSONL log into a fresh op-log.
        let empty: bool = conn
            .query_row("SELECT COUNT(*) = 0 FROM oplog", [], |r| r.get(0))
            .map_err(db_err)?;
        let legacy = data_dir.join("actions.jsonl");
        if empty && legacy.exists() {
            let actions = pinq_core::ActionLog::open(&legacy)?.load()?;
            let tx = conn.transaction().map_err(db_err)?;
            for action in &actions {
                log_and_project(&tx, &device_id, action)?;
            }
            tx.commit().map_err(db_err)?;
            std::fs::rename(&legacy, data_dir.join("actions.jsonl.imported"))?;
        }

        maybe_backup(&conn, data_dir);

        let notes = load_projection(&conn)?;

        // Presentation overlay: seed once from the op-derived state so a
        // device upgrading to per-device layout keeps the layout it had.
        let seeded: bool = conn
            .query_row("SELECT COUNT(*) FROM meta WHERE key='presentation-seeded'", [], |r| {
                r.get::<_, i64>(0).map(|n| n > 0)
            })
            .map_err(db_err)?;
        if !seeded {
            let tx = conn.transaction().map_err(db_err)?;
            for n in notes.values() {
                tx.execute(
                    "INSERT OR REPLACE INTO presentation (note_id, pinned, tucked, x, y, w, h)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        n.id,
                        n.pinned as i64,
                        n.tucked,
                        n.rect.x,
                        n.rect.y,
                        n.rect.w,
                        n.rect.h
                    ],
                )
                .map_err(db_err)?;
            }
            tx.execute("INSERT INTO meta (key, value) VALUES ('presentation-seeded', '1')", [])
                .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
        }
        let presentation = {
            let mut map = HashMap::new();
            let mut stmt = conn
                .prepare("SELECT note_id, pinned, tucked, x, y, w, h FROM presentation")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        Presentation {
                            pinned: r.get::<_, i64>(1)? != 0,
                            tucked: r.get(2)?,
                            rect: Rect {
                                x: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as i32,
                                y: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as i32,
                                w: r.get::<_, Option<i64>>(5)?.unwrap_or(280) as u32,
                                h: r.get::<_, Option<i64>>(6)?.unwrap_or(220) as u32,
                            },
                        },
                    ))
                })
                .map_err(db_err)?;
            for row in rows {
                let (id, p) = row.map_err(db_err)?;
                map.insert(id, p);
            }
            drop(stmt);
            map
        };

        Ok(Self {
            db: Mutex::new(conn),
            notes: Mutex::new(notes),
            presentation: Mutex::new(presentation),
            pending_place: Mutex::new(HashMap::new()),
            device_id,
            commit_hook: std::sync::OnceLock::new(),
            arrivals_hook: std::sync::OnceLock::new(),
        })
    }

    /// Overlay this device's layout onto an op-derived note. No overlay entry
    /// = the note lives in the library here (unpinned, untucked); its
    /// op-derived rect is kept as a starting point for when it's pulled out.
    /// Trashed notes always read unpinned.
    fn overlaid(&self, mut n: NoteState) -> NoteState {
        let pres = self.presentation.lock().unwrap();
        match pres.get(&n.id) {
            Some(p) => {
                n.pinned = p.pinned && !n.trashed;
                n.tucked = if n.trashed { None } else { p.tucked.clone() };
                n.rect = p.rect;
            }
            None => {
                n.pinned = false;
                n.tucked = None;
            }
        }
        n
    }

    /// Persist one overlay row (in-mem map is the source of truth in between).
    fn save_presentation(&self, id: &str) {
        let entry = self.presentation.lock().unwrap().get(id).cloned();
        let Some(p) = entry else { return };
        let conn = self.db.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO presentation (note_id, pinned, tucked, x, y, w, h)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, p.pinned as i64, p.tucked, p.rect.x, p.rect.y, p.rect.w, p.rect.h],
        );
    }

    /// Update (or create) this note's overlay entry and persist it.
    fn set_presentation(&self, id: &str, f: impl FnOnce(&mut Presentation)) {
        let base_rect = self
            .notes
            .lock()
            .unwrap()
            .get(id)
            .map(|n| n.rect)
            .unwrap_or(Rect { x: 120, y: 120, w: 280, h: 220 });
        {
            let mut pres = self.presentation.lock().unwrap();
            let entry = pres.entry(id.to_string()).or_insert(Presentation {
                pinned: false,
                tucked: None,
                rect: base_rect,
            });
            f(entry);
        }
        self.save_presentation(id);
    }

    /// Replication actor id (used by tests today; the device registry in the
    /// next sync spike surfaces it in the UI).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Small device-local key/value scratch (relay cursors, etc.) — lives in
    /// the DB so it can't drift from the op-log it describes.
    pub fn meta_get(&self, key: &str) -> Option<String> {
        use rusqlite::OptionalExtension;
        let conn = self.db.lock().unwrap();
        conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> io::Result<()> {
        let conn = self.db.lock().unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// All registry records for one space, in a stable order.
    pub fn registry_entries(&self, space: &str) -> io::Result<Vec<RegistryEntry>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT kind, device_pk, name, at_ms, signer_pk, sig FROM registry
                 WHERE space = ?1 ORDER BY at_ms, device_pk, sig",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([space], |r| {
                Ok(RegistryEntry {
                    kind: r.get(0)?,
                    device_pk: r.get(1)?,
                    name: r.get(2)?,
                    at_ms: r.get::<_, i64>(3)? as u64,
                    signer_pk: r.get(4)?,
                    sig: r.get(5)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Union-merge registry records (duplicates ignored). Returns how many
    /// were new. Callers verify signatures before trusting the fold — storing
    /// a bad record is harmless, it just never becomes valid.
    pub fn add_registry_entries(
        &self,
        space: &str,
        entries: &[RegistryEntry],
    ) -> io::Result<usize> {
        let mut conn = self.db.lock().unwrap();
        let tx = conn.transaction().map_err(db_err)?;
        let mut added = 0;
        for e in entries {
            added += tx
                .execute(
                    "INSERT OR IGNORE INTO registry (space, kind, device_pk, name, at_ms, signer_pk, sig)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![space, e.kind, e.device_pk, e.name, e.at_ms as i64, e.signer_pk, e.sig],
                )
                .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(added)
    }

    /// The single write path: op-log append + projection update in one transaction,
    /// then the in-memory fold. Nothing mutates state any other way.
    fn commit(&self, action: Action) -> io::Result<()> {
        {
            let mut conn = self.db.lock().unwrap();
            let tx = conn.transaction().map_err(db_err)?;
            log_and_project(&tx, &self.device_id, &action)?;
            tx.commit().map_err(db_err)?;
        }
        pinq_core::apply(&mut self.notes.lock().unwrap(), action);
        // Every local content commit nudges the sync loop (hook wired at
        // setup; no-op in tests). Remote applies don't come through here,
        // so there's no sync ping-pong.
        if let Some(hook) = self.commit_hook.get() {
            hook();
        }
        Ok(())
    }

    /// Called after every local content commit. Set once at app setup.
    pub fn set_commit_hook(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.commit_hook.set(Box::new(f));
    }

    /// Called with the ids of notes a remote sync introduced (previously
    /// unknown here, now live). Set once at app setup.
    pub fn set_arrivals_hook(&self, f: impl Fn(&[NoteId]) + Send + Sync + 'static) {
        let _ = self.arrivals_hook.set(Box::new(f));
    }

    /// Max ctr per device across the whole op-log — "everything I know about".
    pub fn version_vector(&self) -> io::Result<HashMap<String, u64>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT device, MAX(ctr) FROM oplog GROUP BY device")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    /// Every op the peer's version vector says it hasn't seen, in local arrival
    /// order (so a note's ops from one device stay causally ordered in transit).
    pub fn ops_missing_from(&self, theirs: &HashMap<String, u64>) -> io::Result<Vec<SyncOp>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT device, ctr, at_ms, action FROM oplog ORDER BY seq")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SyncOp {
                    device: r.get(0)?,
                    ctr: r.get::<_, i64>(1)? as u64,
                    at_ms: r.get::<_, i64>(2)? as u64,
                    action: r.get(3)?,
                })
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let op = row.map_err(db_err)?;
            if op.ctr > theirs.get(&op.device).copied().unwrap_or(0) {
                out.push(op);
            }
        }
        Ok(out)
    }

    /// Merge ops from a peer. Duplicates are ignored (unique (device, ctr));
    /// each touched note is then rebuilt by replaying ALL of its ops in the
    /// canonical order (at_ms, device, ctr) — the same total order on every
    /// device — so replicas converge no matter which direction synced first.
    /// Returns the ids of notes whose state actually changed hands.
    pub fn apply_remote_ops(&self, ops: &[SyncOp]) -> io::Result<Vec<NoteId>> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let mut affected: std::collections::BTreeSet<NoteId> = Default::default();
        let mut rebuilt: Vec<(NoteId, Option<NoteState>)> = Vec::new();
        // Snapshot which notes exist before the merge, so brand-new arrivals
        // (created on another device) can be told apart from edits.
        let known: std::collections::HashSet<NoteId> =
            self.notes.lock().unwrap().keys().cloned().collect();
        {
            let mut conn = self.db.lock().unwrap();
            let tx = conn.transaction().map_err(db_err)?;
            for op in ops {
                // Unknown action kinds from a newer peer are skipped, not fatal.
                let Ok(action) = serde_json::from_str::<Action>(&op.action) else { continue };
                let inserted = tx
                    .execute(
                        "INSERT OR IGNORE INTO oplog (at_ms, action, device, ctr)
                         VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![op.at_ms as i64, op.action, op.device, op.ctr as i64],
                    )
                    .map_err(db_err)?;
                if inserted > 0 {
                    affected.insert(action.note_id().clone());
                }
            }
            for id in &affected {
                let state = replay_note(&tx, id)?;
                write_note_row(&tx, id, state.as_ref())?;
                rebuilt.push((id.clone(), state));
            }
            tx.commit().map_err(db_err)?;
        }
        let mut arrivals: Vec<NoteId> = Vec::new();
        {
            let mut notes = self.notes.lock().unwrap();
            for (id, state) in rebuilt {
                match state {
                    Some(n) => {
                        if !known.contains(&id) && !n.trashed {
                            arrivals.push(id.clone());
                        }
                        notes.insert(id, n);
                    }
                    None => {
                        notes.remove(&id);
                        self.pending_place.lock().unwrap().remove(&id);
                    }
                }
            }
        }
        if !arrivals.is_empty() {
            if let Some(hook) = self.arrivals_hook.get() {
                hook(&arrivals);
            }
        }
        Ok(affected.into_iter().collect())
    }

    pub fn get(&self, id: &str) -> Option<NoteState> {
        let n = self.notes.lock().unwrap().get(id).cloned();
        n.map(|n| self.overlaid(n))
    }

    pub fn pinned_notes(&self) -> Vec<NoteState> {
        let base: Vec<NoteState> = self.notes.lock().unwrap().values().cloned().collect();
        base.into_iter().map(|n| self.overlaid(n)).filter(|n| n.pinned).collect()
    }

    pub fn all_notes(&self) -> Vec<NoteState> {
        let base: Vec<NoteState> = self.notes.lock().unwrap().values().cloned().collect();
        let mut v: Vec<NoteState> = base.into_iter().map(|n| self.overlaid(n)).collect();
        v.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
        v
    }

    /// FTS5 token-prefix search over non-trashed note text, best match first.
    pub fn search(&self, query: &str, limit: usize) -> io::Result<Vec<NoteState>> {
        let fts_query = build_fts_query(query);
        if fts_query.is_empty() {
            return Ok(self.all_notes().into_iter().filter(|n| !n.trashed).take(limit).collect());
        }
        let ids: Vec<String> = {
            let conn = self.db.lock().unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT n.id FROM notes n JOIN notes_fts f ON n.rowid = f.rowid
                     WHERE notes_fts MATCH ?1 AND n.trashed = 0
                     ORDER BY rank LIMIT ?2",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(rusqlite::params![fts_query, limit as i64], |r| r.get(0))
                .map_err(db_err)?;
            rows.collect::<Result<_, _>>().map_err(db_err)?
        };
        let hits: Vec<NoteState> = {
            let notes = self.notes.lock().unwrap();
            ids.iter().filter_map(|id| notes.get(id).cloned()).collect()
        };
        Ok(hits.into_iter().map(|n| self.overlaid(n)).collect())
    }

    /// Every text version of a note, newest first — straight from the op-log.
    pub fn text_history(&self, id: &str, limit: usize) -> io::Result<Vec<(u64, String)>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT at_ms, json_extract(action, '$.text') FROM oplog
                 WHERE json_extract(action, '$.kind') IN ('note_text_changed', 'item_text_changed')
                   AND json_extract(action, '$.id') = ?1
                 ORDER BY seq DESC LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![id, limit as i64], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        rows.collect::<Result<_, _>>().map_err(db_err)
    }

    pub fn create_note(&self, rect: Rect) -> io::Result<NoteState> {
        let id = pinq_core::new_note_id();
        let at_ms = now_ms();
        self.commit(Action::NoteCreated { id: id.clone(), at_ms, rect })?;
        // The creating device pins its own new note; other devices receive it
        // as library content only (no overlay entry until they pull it out).
        self.set_presentation(&id, |p| {
            p.pinned = true;
            p.tucked = None;
            p.rect = rect;
        });
        Ok(self.get(&id).expect("created note present"))
    }

    pub fn set_text(&self, id: &str, text: String) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.text != text => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NoteTextChanged { id: id.to_string(), at_ms: now_ms(), text })
    }

    /// Set (Some) or clear (None) a note's reminder. Idempotent.
    pub fn set_reminder(&self, id: &str, remind_ms: Option<u64>) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.remind_ms != remind_ms => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::ReminderSet { id: id.to_string(), at_ms: now_ms(), remind_ms })
    }

    /// Set (Some) or clear (None) a task due date. Idempotent.
    pub fn set_due(&self, id: &str, due_ms: Option<u64>) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.due_ms != due_ms => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::DueSet { id: id.to_string(), at_ms: now_ms(), due_ms })
    }

    /// Set (Some) or clear (None) a task status. Idempotent.
    pub fn set_status(&self, id: &str, status: Option<String>) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.status != status => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::StatusSet { id: id.to_string(), at_ms: now_ms(), status })
    }

    pub fn set_color(&self, id: &str, color: Option<String>) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.color != color => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NoteColorChanged { id: id.to_string(), at_ms: now_ms(), color })
    }

    pub fn set_paper(&self, id: &str, paper: Option<String>) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.paper != paper => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NotePaperChanged { id: id.to_string(), at_ms: now_ms(), paper })
    }

    pub fn add_tag(&self, id: &str, tag: &str) -> io::Result<()> {
        let tag = tag.to_lowercase();
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if !n.tags.contains(&tag) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NoteTagged { id: id.to_string(), at_ms: now_ms(), tag })
    }

    pub fn remove_tag(&self, id: &str, tag: &str) -> io::Result<()> {
        let tag = tag.to_lowercase();
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.tags.contains(&tag) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NoteUntagged { id: id.to_string(), at_ms: now_ms(), tag })
    }

    /// Legacy attachment ops — still valid in old logs; exercised by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_attachment(&self, id: &str, attachment: pinq_core::AttachmentRef) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if !n.attachments.iter().any(|a| a.hash == attachment.hash) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::AttachmentAdded { id: id.to_string(), at_ms: now_ms(), attachment })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn remove_attachment(&self, id: &str, hash: &str) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.attachments.iter().any(|a| a.hash == hash) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::AttachmentRemoved {
            id: id.to_string(),
            at_ms: now_ms(),
            hash: hash.to_string(),
        })
    }

    pub fn add_item(&self, id: &str, item: pinq_core::CanvasItem) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if !n.items.iter().any(|i| i.item_id == item.item_id) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::ItemAdded { id: id.to_string(), at_ms: now_ms(), item })
    }

    pub fn set_item_text(&self, id: &str, item_id: &str, text: String) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            let Some(n) = notes.get(id) else { return Ok(()) };
            match n.items.iter().find(|i| i.item_id == item_id) {
                Some(i) if i.content != pinq_core::ItemContent::Text { text: text.clone() } => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::ItemTextChanged {
            id: id.to_string(),
            at_ms: now_ms(),
            item_id: item_id.to_string(),
            text,
        })
    }

    pub fn move_item(&self, id: &str, item_id: &str, x: f64, y: f64, w: f64) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.items.iter().any(|i| i.item_id == item_id) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::ItemMoved { id: id.to_string(), at_ms: now_ms(), item_id: item_id.to_string(), x, y, w })
    }

    pub fn remove_item(&self, id: &str, item_id: &str) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.items.iter().any(|i| i.item_id == item_id) => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::ItemRemoved {
            id: id.to_string(),
            at_ms: now_ms(),
            item_id: item_id.to_string(),
        })
    }

    /// Recreate a note from a backup as real ops, preserving its id and
    /// timestamps. Returns false (no-op) if the id already exists, which makes
    /// restoring the same archive twice idempotent.
    pub fn import_note(&self, n: &NoteState) -> io::Result<bool> {
        if self.notes.lock().unwrap().contains_key(&n.id) {
            return Ok(false);
        }
        self.commit(Action::NoteCreated { id: n.id.clone(), at_ms: n.created_ms, rect: n.rect })?;
        for item in &n.items {
            self.commit(Action::ItemAdded {
                id: n.id.clone(),
                at_ms: n.modified_ms,
                item: item.clone(),
            })?;
        }
        if n.color.is_some() {
            self.commit(Action::NoteColorChanged {
                id: n.id.clone(),
                at_ms: n.modified_ms,
                color: n.color.clone(),
            })?;
        }
        if n.paper.is_some() {
            self.commit(Action::NotePaperChanged {
                id: n.id.clone(),
                at_ms: n.modified_ms,
                paper: n.paper.clone(),
            })?;
        }
        for tag in &n.tags {
            self.commit(Action::NoteTagged {
                id: n.id.clone(),
                at_ms: n.modified_ms,
                tag: tag.clone(),
            })?;
        }
        if !n.pinned {
            self.commit(Action::NoteFiled { id: n.id.clone(), at_ms: n.modified_ms })?;
        }
        if n.trashed {
            self.commit(Action::NoteTrashed { id: n.id.clone(), at_ms: n.modified_ms })?;
        }
        Ok(true)
    }

    // ----- presentation mutators: device-local, never ops, never sync -----

    pub fn queue_place(&self, id: &str, rect: Rect) {
        {
            let mut pres = self.presentation.lock().unwrap();
            // No overlay entry = not on this device's desktop; a stray move
            // event for it changes nothing.
            let Some(p) = pres.get_mut(id) else { return };
            // A tucked note's window slides for hover-peeks; its logical rect
            // only changes through tuck/untuck, never incidental moves.
            if p.tucked.is_some() {
                return;
            }
            p.rect = rect;
        }
        self.pending_place.lock().unwrap().insert(id.to_string(), rect);
    }

    /// Persist rects the in-memory overlay already has (dirty ids only) —
    /// a window drag stays one DB write per second, not one per move event.
    ///
    /// Position is device-local, but a note's SIZE shapes how its text lays
    /// out, so size changes also commit a synced NotePlaced op: a note pulled
    /// out on another device then spawns at the size it was last worked on,
    /// with its text wrapped exactly as it looked there. Pure moves emit
    /// nothing (no sync traffic, no library reordering from drags).
    pub fn flush_places(&self) {
        let drained: Vec<(NoteId, Rect)> = self.pending_place.lock().unwrap().drain().collect();
        for (id, rect) in drained {
            self.save_presentation(&id);
            let base = self.notes.lock().unwrap().get(&id).map(|n| n.rect);
            if let Some(base) = base {
                if base.w != rect.w || base.h != rect.h {
                    let _ = self.commit(Action::NotePlaced {
                        id: id.clone(),
                        at_ms: now_ms(),
                        rect,
                    });
                }
            }
        }
    }

    /// Re-pin a previously filed note at its last known rect (undo of a throw/file).
    /// Trashed notes must be restored first.
    pub fn repin(&self, id: &str) -> io::Result<Option<NoteState>> {
        match self.get(id) {
            None => return Ok(None),
            Some(n) if n.trashed => return Ok(None),
            Some(n) if n.pinned => return Ok(Some(n)),
            Some(_) => {}
        }
        self.set_presentation(id, |p| {
            p.pinned = true;
            p.tucked = None;
        });
        Ok(self.get(id))
    }

    /// Park a pinned note as a sliver at a screen edge (throw-to-stick). The
    /// tuck rect supersedes any pending drag placement.
    pub fn tuck_note(&self, id: &str, edge: &str, rect: Rect) -> io::Result<()> {
        match self.get(id) {
            Some(n) if n.pinned && !n.trashed => {}
            _ => return Ok(()),
        }
        self.pending_place.lock().unwrap().remove(id);
        self.set_presentation(id, |p| {
            p.tucked = Some(edge.to_string());
            p.rect = rect;
        });
        Ok(())
    }

    /// Pull a tucked note back out to a normal on-screen rect.
    pub fn untuck_note(&self, id: &str, rect: Rect) -> io::Result<()> {
        match self.get(id) {
            Some(n) if n.tucked.is_some() => {}
            _ => return Ok(()),
        }
        self.pending_place.lock().unwrap().remove(id);
        self.set_presentation(id, |p| {
            p.tucked = None;
            p.rect = rect;
        });
        Ok(())
    }

    /// Unpin (throw/file/close). Idempotent, and local to this device.
    pub fn file_note(&self, id: &str) -> io::Result<()> {
        match self.get(id) {
            Some(n) if n.pinned => {}
            _ => return Ok(()),
        }
        self.pending_place.lock().unwrap().remove(id);
        self.set_presentation(id, |p| {
            p.pinned = false;
            p.tucked = None;
        });
        Ok(())
    }

    /// Local overlay cleanup after REMOTE lifecycle ops: a note trashed
    /// elsewhere unpins here (its window closes); a purged note drops its row.
    pub fn reconcile_presentation(&self, id: &str) {
        let base = self.notes.lock().unwrap().get(id).cloned();
        match base {
            None => {
                self.presentation.lock().unwrap().remove(id);
                let conn = self.db.lock().unwrap();
                let _ = conn.execute("DELETE FROM presentation WHERE note_id=?1", [id]);
            }
            Some(n) if n.trashed => {
                let showing = {
                    let pres = self.presentation.lock().unwrap();
                    pres.get(id).map(|p| p.pinned || p.tucked.is_some()).unwrap_or(false)
                };
                if showing {
                    self.set_presentation(id, |p| {
                        p.pinned = false;
                        p.tucked = None;
                    });
                }
            }
            _ => {}
        }
    }

    // ----- content lifecycle: synced ops (with local presentation ripple) -----

    /// Move to trash: synced (deleting is data, not layout); the local window
    /// state clears too.
    pub fn trash_note(&self, id: &str) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if !n.trashed => {}
                _ => return Ok(()),
            }
        }
        self.pending_place.lock().unwrap().remove(id);
        self.commit(Action::NoteTrashed { id: id.to_string(), at_ms: now_ms() })?;
        self.set_presentation(id, |p| {
            p.pinned = false;
            p.tucked = None;
        });
        Ok(())
    }

    /// Restore from trash (stays unpinned; pin separately if wanted).
    pub fn restore_note(&self, id: &str) -> io::Result<()> {
        {
            let notes = self.notes.lock().unwrap();
            match notes.get(id) {
                Some(n) if n.trashed => {}
                _ => return Ok(()),
            }
        }
        self.commit(Action::NoteRestored { id: id.to_string(), at_ms: now_ms() })
    }

    /// Permanent delete. Callers must have confirmed with the user.
    pub fn purge_note(&self, id: &str) -> io::Result<()> {
        if !self.notes.lock().unwrap().contains_key(id) {
            return Ok(());
        }
        self.pending_place.lock().unwrap().remove(id);
        self.commit(Action::NoteDeleted { id: id.to_string(), at_ms: now_ms() })?;
        self.presentation.lock().unwrap().remove(id);
        let conn = self.db.lock().unwrap();
        let _ = conn.execute("DELETE FROM presentation WHERE note_id=?1", [id]);
        Ok(())
    }

    /// Permanently delete everything in the trash. Returns how many were purged.
    pub fn purge_trashed(&self) -> io::Result<usize> {
        let ids: Vec<NoteId> = {
            let notes = self.notes.lock().unwrap();
            notes.values().filter(|n| n.trashed).map(|n| n.id.clone()).collect()
        };
        for id in &ids {
            self.purge_note(id)?;
        }
        Ok(ids.len())
    }
}

/// Apply a mutation to a note's canvas items and rewrite the derived `text`
/// and `attachments` columns (which feed FTS, previews, and export). Mirrors
/// `pinq_core::rebuild_derived`.
fn update_items(
    tx: &rusqlite::Transaction,
    id: &str,
    at_ms: u64,
    mutate: impl FnOnce(&mut Vec<pinq_core::CanvasItem>),
) -> Result<usize, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    let Some(cur) = tx
        .query_row("SELECT items FROM notes WHERE id=?1", [id], |r| r.get::<_, String>(0))
        .optional()?
    else {
        return Ok(0);
    };
    let mut items: Vec<pinq_core::CanvasItem> = serde_json::from_str(&cur).unwrap_or_default();
    mutate(&mut items);
    let text = items
        .iter()
        .filter_map(|i| match &i.content {
            pinq_core::ItemContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let atts: Vec<&pinq_core::AttachmentRef> = items
        .iter()
        .filter_map(|i| match &i.content {
            pinq_core::ItemContent::Attachment { attachment } => Some(attachment),
            _ => None,
        })
        .collect();
    tx.execute(
        "UPDATE notes SET items=?2, text=?3, attachments=?4, modified_ms=?5 WHERE id=?1",
        rusqlite::params![
            id,
            serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()),
            text,
            serde_json::to_string(&atts).unwrap_or_else(|_| "[]".into()),
            at_ms as i64
        ],
    )
}

/// Replay every op that touches one note, in canonical (at_ms, device, ctr)
/// order, into its current state. None = the note doesn't exist (never created
/// here, or deleted).
fn replay_note(tx: &rusqlite::Transaction, id: &str) -> io::Result<Option<NoteState>> {
    let mut stmt = tx
        .prepare(
            "SELECT action FROM oplog
             WHERE json_extract(action, '$.id') = ?1
             ORDER BY at_ms, device, ctr",
        )
        .map_err(db_err)?;
    let rows = stmt.query_map([id], |r| r.get::<_, String>(0)).map_err(db_err)?;
    let mut actions = Vec::new();
    for row in rows {
        if let Ok(a) = serde_json::from_str::<Action>(&row.map_err(db_err)?) {
            actions.push(a);
        }
    }
    Ok(pinq_core::replay(actions).remove(id))
}

/// Write a replayed note state as its full projection row (or delete the row).
/// The FTS triggers keep the index in step.
fn write_note_row(
    tx: &rusqlite::Transaction,
    id: &str,
    state: Option<&NoteState>,
) -> io::Result<()> {
    match state {
        Some(n) => tx.execute(
            "INSERT OR REPLACE INTO notes
             (id, x, y, w, h, text, pinned, trashed, tucked, remind_ms, color, paper, tags, attachments, items, created_ms, modified_ms, due_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            rusqlite::params![
                n.id,
                n.rect.x,
                n.rect.y,
                n.rect.w,
                n.rect.h,
                n.text,
                n.pinned as i64,
                n.trashed as i64,
                n.tucked,
                n.remind_ms.map(|v| v as i64),
                n.color,
                n.paper,
                serde_json::to_string(&n.tags).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&n.attachments).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&n.items).unwrap_or_else(|_| "[]".into()),
                n.created_ms as i64,
                n.modified_ms as i64,
                n.due_ms.map(|v| v as i64),
                n.status
            ],
        ),
        None => tx.execute("DELETE FROM notes WHERE id=?1", rusqlite::params![id]),
    }
    .map_err(db_err)?;
    Ok(())
}

/// Append to the op-log and update the `notes` projection, inside the caller's
/// transaction. Mirrors `pinq_core::apply` exactly. The op is stamped with this
/// device's identity and its next counter value.
fn log_and_project(tx: &rusqlite::Transaction, device: &str, action: &Action) -> io::Result<()> {
    let json = serde_json::to_string(action).map_err(io::Error::other)?;
    let at_ms = match action {
        Action::NoteCreated { at_ms, .. }
        | Action::NotePlaced { at_ms, .. }
        | Action::NoteTextChanged { at_ms, .. }
        | Action::NoteColorChanged { at_ms, .. }
        | Action::NotePaperChanged { at_ms, .. }
        | Action::NoteTagged { at_ms, .. }
        | Action::NoteUntagged { at_ms, .. }
        | Action::AttachmentAdded { at_ms, .. }
        | Action::AttachmentRemoved { at_ms, .. }
        | Action::ItemAdded { at_ms, .. }
        | Action::ItemTextChanged { at_ms, .. }
        | Action::ItemMoved { at_ms, .. }
        | Action::ItemRemoved { at_ms, .. }
        | Action::NoteFiled { at_ms, .. }
        | Action::NoteTucked { at_ms, .. }
        | Action::NoteUntucked { at_ms, .. }
        | Action::NoteRepinned { at_ms, .. }
        | Action::NoteTrashed { at_ms, .. }
        | Action::NoteRestored { at_ms, .. }
        | Action::NoteDeleted { at_ms, .. }
        | Action::ReminderSet { at_ms, .. }
        | Action::DueSet { at_ms, .. }
        | Action::StatusSet { at_ms, .. } => *at_ms as i64,
    };
    let ctr: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(ctr), 0) + 1 FROM oplog WHERE device = ?1",
            [device],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    tx.execute(
        "INSERT INTO oplog (at_ms, action, device, ctr) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![at_ms, json, device, ctr],
    )
    .map_err(db_err)?;

    match action {
        Action::NoteCreated { id, at_ms, rect } => {
            tx.execute(
                "INSERT OR REPLACE INTO notes (id, x, y, w, h, text, pinned, trashed, color, paper, created_ms, modified_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, '', 1, 0, NULL, NULL, ?6, ?6)",
                rusqlite::params![id, rect.x, rect.y, rect.w, rect.h, *at_ms as i64],
            )
        }
        Action::NotePlaced { id, at_ms, rect } => tx.execute(
            "UPDATE notes SET x=?2, y=?3, w=?4, h=?5, modified_ms=?6 WHERE id=?1",
            rusqlite::params![id, rect.x, rect.y, rect.w, rect.h, *at_ms as i64],
        ),
        Action::NoteTextChanged { id, at_ms, text } => update_items(tx, id, *at_ms, |items| {
            items.retain(|i| !matches!(i.content, pinq_core::ItemContent::Text { .. }));
            if !text.is_empty() {
                items.insert(
                    0,
                    pinq_core::CanvasItem {
                        item_id: format!("t-{id}"),
                        x: 8.0,
                        y: 8.0,
                        w: 248.0,
                        content: pinq_core::ItemContent::Text { text: text.clone() },
                    },
                );
            }
        }),
        Action::ItemAdded { id, at_ms, item } => update_items(tx, id, *at_ms, |items| {
            if !items.iter().any(|i| i.item_id == item.item_id) {
                items.push(item.clone());
            }
        }),
        Action::ItemTextChanged { id, at_ms, item_id, text } => {
            update_items(tx, id, *at_ms, |items| {
                if let Some(i) = items.iter_mut().find(|i| &i.item_id == item_id) {
                    i.content = pinq_core::ItemContent::Text { text: text.clone() };
                }
            })
        }
        Action::ItemMoved { id, at_ms, item_id, x, y, w } => {
            update_items(tx, id, *at_ms, |items| {
                if let Some(i) = items.iter_mut().find(|i| &i.item_id == item_id) {
                    i.x = *x;
                    i.y = *y;
                    i.w = *w;
                }
            })
        }
        Action::ItemRemoved { id, at_ms, item_id } => update_items(tx, id, *at_ms, |items| {
            items.retain(|i| &i.item_id != item_id);
        }),
        Action::NoteColorChanged { id, at_ms, color } => tx.execute(
            "UPDATE notes SET color=?2, modified_ms=?3 WHERE id=?1",
            rusqlite::params![id, color, *at_ms as i64],
        ),
        Action::ReminderSet { id, at_ms, remind_ms } => tx.execute(
            "UPDATE notes SET remind_ms=?2, modified_ms=?3 WHERE id=?1",
            rusqlite::params![id, remind_ms.map(|v| v as i64), *at_ms as i64],
        ),
        Action::DueSet { id, at_ms, due_ms } => tx.execute(
            "UPDATE notes SET due_ms=?2, modified_ms=?3 WHERE id=?1",
            rusqlite::params![id, due_ms.map(|v| v as i64), *at_ms as i64],
        ),
        Action::StatusSet { id, at_ms, status } => tx.execute(
            "UPDATE notes SET status=?2, modified_ms=?3 WHERE id=?1",
            rusqlite::params![id, status, *at_ms as i64],
        ),
        Action::NotePaperChanged { id, at_ms, paper } => tx.execute(
            "UPDATE notes SET paper=?2, modified_ms=?3 WHERE id=?1",
            rusqlite::params![id, paper, *at_ms as i64],
        ),
        Action::NoteTagged { id, at_ms, tag } => {
            (|| -> Result<usize, rusqlite::Error> {
                use rusqlite::OptionalExtension;
                let Some(cur) = tx
                    .query_row("SELECT tags FROM notes WHERE id=?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()?
                else {
                    return Ok(0);
                };
                let mut tags: Vec<String> = serde_json::from_str(&cur).unwrap_or_default();
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
                tx.execute(
                    "UPDATE notes SET tags=?2, modified_ms=?3 WHERE id=?1",
                    rusqlite::params![id, serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()), *at_ms as i64],
                )
            })()
        }
        Action::NoteUntagged { id, at_ms, tag } => {
            (|| -> Result<usize, rusqlite::Error> {
                use rusqlite::OptionalExtension;
                let Some(cur) = tx
                    .query_row("SELECT tags FROM notes WHERE id=?1", [id], |r| {
                        r.get::<_, String>(0)
                    })
                    .optional()?
                else {
                    return Ok(0);
                };
                let mut tags: Vec<String> = serde_json::from_str(&cur).unwrap_or_default();
                tags.retain(|t| t != tag);
                tx.execute(
                    "UPDATE notes SET tags=?2, modified_ms=?3 WHERE id=?1",
                    rusqlite::params![id, serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into()), *at_ms as i64],
                )
            })()
        }
        Action::AttachmentAdded { id, at_ms, attachment } => {
            update_items(tx, id, *at_ms, |items| {
                let dup = items.iter().any(|i| matches!(&i.content,
                    pinq_core::ItemContent::Attachment { attachment: a } if a.hash == attachment.hash));
                if !dup {
                    let k = items.len() as f64;
                    items.push(pinq_core::CanvasItem {
                        item_id: format!("a-{}", attachment.hash),
                        x: 12.0 + k * 16.0,
                        y: 140.0 + k * 12.0,
                        w: 120.0,
                        content: pinq_core::ItemContent::Attachment { attachment: attachment.clone() },
                    });
                }
            })
        }
        Action::AttachmentRemoved { id, at_ms, hash } => {
            update_items(tx, id, *at_ms, |items| {
                items.retain(|i| !matches!(&i.content,
                    pinq_core::ItemContent::Attachment { attachment } if &attachment.hash == hash));
            })
        }
        Action::NoteFiled { id, at_ms } => tx.execute(
            "UPDATE notes SET pinned=0, tucked=NULL, modified_ms=?2 WHERE id=?1",
            rusqlite::params![id, *at_ms as i64],
        ),
        Action::NoteTucked { id, at_ms, edge, rect } => tx.execute(
            "UPDATE notes SET tucked=?2, x=?3, y=?4, w=?5, h=?6, modified_ms=?7 WHERE id=?1",
            rusqlite::params![id, edge, rect.x, rect.y, rect.w, rect.h, *at_ms as i64],
        ),
        Action::NoteUntucked { id, at_ms, rect } => tx.execute(
            "UPDATE notes SET tucked=NULL, x=?2, y=?3, w=?4, h=?5, modified_ms=?6 WHERE id=?1",
            rusqlite::params![id, rect.x, rect.y, rect.w, rect.h, *at_ms as i64],
        ),
        Action::NoteRepinned { id, at_ms, rect } => tx.execute(
            "UPDATE notes SET pinned=1, tucked=NULL, x=?2, y=?3, w=?4, h=?5, modified_ms=?6 WHERE id=?1",
            rusqlite::params![id, rect.x, rect.y, rect.w, rect.h, *at_ms as i64],
        ),
        Action::NoteTrashed { id, at_ms } => tx.execute(
            "UPDATE notes SET trashed=1, pinned=0, tucked=NULL, modified_ms=?2 WHERE id=?1",
            rusqlite::params![id, *at_ms as i64],
        ),
        Action::NoteRestored { id, at_ms } => tx.execute(
            "UPDATE notes SET trashed=0, modified_ms=?2 WHERE id=?1",
            rusqlite::params![id, *at_ms as i64],
        ),
        Action::NoteDeleted { id, .. } => {
            tx.execute("DELETE FROM notes WHERE id=?1", rusqlite::params![id])
        }
    }
    .map_err(db_err)?;
    Ok(())
}

fn load_projection(conn: &Connection) -> io::Result<HashMap<NoteId, NoteState>> {
    let mut stmt = conn
        .prepare("SELECT id, x, y, w, h, text, pinned, trashed, tucked, remind_ms, color, paper, tags, attachments, items, created_ms, modified_ms, due_ms, status FROM notes")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(NoteState {
                id: r.get(0)?,
                rect: Rect { x: r.get(1)?, y: r.get(2)?, w: r.get(3)?, h: r.get(4)? },
                text: r.get(5)?,
                pinned: r.get::<_, i64>(6)? != 0,
                trashed: r.get::<_, i64>(7)? != 0,
                tucked: r.get(8)?,
                remind_ms: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
                color: r.get(10)?,
                paper: r.get(11)?,
                tags: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                attachments: serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or_default(),
                items: serde_json::from_str(&r.get::<_, String>(14)?).unwrap_or_default(),
                created_ms: r.get::<_, i64>(15)? as u64,
                modified_ms: r.get::<_, i64>(16)? as u64,
                due_ms: r.get::<_, Option<i64>>(17)?.map(|v| v as u64),
                status: r.get(18)?,
            })
        })
        .map_err(db_err)?;
    let mut notes = HashMap::new();
    for row in rows {
        let n = row.map_err(db_err)?;
        notes.insert(n.id.clone(), n);
    }
    Ok(notes)
}

/// Turn free text into an FTS5 token-prefix query: `demo env` → `"demo"* "env"*`.
fn build_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pinq-store-test-{}", pinq_core::new_note_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rect() -> Rect {
        Rect { x: 10, y: 20, w: 280, h: 220 }
    }

    #[test]
    fn persists_across_reopen() {
        let dir = temp_dir();
        let id = {
            let store = Store::open(&dir).unwrap();
            let note = store.create_note(rect()).unwrap();
            store.set_text(&note.id, "remember the milk".into()).unwrap();
            store.set_color(&note.id, Some("mint".into())).unwrap();
            store.file_note(&note.id).unwrap();
            note.id
        };
        let store = Store::open(&dir).unwrap();
        let n = store.get(&id).expect("note survived reopen");
        assert_eq!(n.text, "remember the milk");
        assert_eq!(n.color.as_deref(), Some("mint"));
        assert!(!n.pinned);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn imports_legacy_jsonl_once() {
        let dir = temp_dir();
        let id = pinq_core::new_note_id();
        let log = pinq_core::ActionLog::open(dir.join("actions.jsonl")).unwrap();
        log.append(&Action::NoteCreated { id: id.clone(), at_ms: 1, rect: rect() }).unwrap();
        log.append(&Action::NoteTextChanged { id: id.clone(), at_ms: 2, text: "legacy".into() })
            .unwrap();

        let store = Store::open(&dir).unwrap();
        assert_eq!(store.get(&id).unwrap().text, "legacy");
        assert!(!dir.join("actions.jsonl").exists());
        assert!(dir.join("actions.jsonl.imported").exists());

        // Reopen: import must not run twice (no duplicate ops).
        drop(store);
        let store = Store::open(&dir).unwrap();
        assert_eq!(store.all_notes().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tags_persist_and_are_searchable() {
        let dir = temp_dir();
        let id = {
            let store = Store::open(&dir).unwrap();
            let note = store.create_note(rect()).unwrap();
            store.set_text(&note.id, "quarterly planning doc".into()).unwrap();
            store.add_tag(&note.id, "Work").unwrap(); // lowercased on store
            store.add_tag(&note.id, "work").unwrap(); // deduped
            store.add_tag(&note.id, "urgent").unwrap();
            store.remove_tag(&note.id, "urgent").unwrap();
            note.id
        };
        let store = Store::open(&dir).unwrap();
        assert_eq!(store.get(&id).unwrap().tags, vec!["work"]);
        // FTS finds the note by its tag even though "work" isn't in the text.
        let hits = store.search("work", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn attachments_persist_across_reopen() {
        let dir = temp_dir();
        let att = pinq_core::AttachmentRef {
            hash: "a".repeat(64),
            name: "photo.png".into(),
            mime: "image/png".into(),
            size: 42,
        };
        let id = {
            let store = Store::open(&dir).unwrap();
            let note = store.create_note(rect()).unwrap();
            store.add_attachment(&note.id, att.clone()).unwrap();
            store.add_attachment(&note.id, att.clone()).unwrap(); // deduped
            note.id
        };
        let store = Store::open(&dir).unwrap();
        let n = store.get(&id).unwrap();
        assert_eq!(n.attachments.len(), 1);
        assert_eq!(n.attachments[0].name, "photo.png");
        store.remove_attachment(&id, &att.hash).unwrap();
        assert!(store.get(&id).unwrap().attachments.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn backup_created_and_corruption_recovers() {
        let dir = temp_dir();
        let id = {
            let store = Store::open(&dir).unwrap();
            let note = store.create_note(rect()).unwrap();
            store.set_text(&note.id, "survive the crash".into()).unwrap();
            note.id
        };
        // First open created a daily backup — but before our note existed.
        // Reopen refreshes nothing (too soon), so force a snapshot for the test.
        {
            let store = Store::open(&dir).unwrap();
            let conn = store.db.lock().unwrap();
            let target = dir.join("backups").join(format!("pinq-{}.db", now_ms()));
            conn.execute_batch(&format!("VACUUM INTO '{}'", target.display())).unwrap();
        }
        // Corrupt the database file outright.
        std::fs::write(dir.join("pinq.db"), b"definitely not a sqlite file").unwrap();
        let _ = std::fs::remove_file(dir.join("pinq.db-wal"));
        let _ = std::fs::remove_file(dir.join("pinq.db-shm"));

        let store = Store::open(&dir).unwrap();
        let n = store.get(&id).expect("note restored from backup");
        assert_eq!(n.text, "survive the crash");
        // The corrupt file was kept as evidence, not deleted.
        let kept = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(kept);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fts_search_finds_token_prefixes() {
        let dir = temp_dir();
        let store = Store::open(&dir).unwrap();
        let a = store.create_note(rect()).unwrap();
        store.set_text(&a.id, "reset the demo environment".into()).unwrap();
        let b = store.create_note(rect()).unwrap();
        store.set_text(&b.id, "buy groceries: tahini and lemons".into()).unwrap();
        let c = store.create_note(rect()).unwrap();
        store.set_text(&c.id, "environment variables for staging".into()).unwrap();
        store.trash_note(&c.id).unwrap();

        let hits = store.search("env", 10).unwrap();
        assert_eq!(hits.len(), 1, "trashed note excluded");
        assert_eq!(hits[0].id, a.id);

        let hits = store.search("tahini", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, b.id);

        // Empty query returns recents.
        assert_eq!(store.search("", 10).unwrap().len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tuck_persists_and_untuck_restores() {
        let dir = temp_dir();
        let id = {
            let store = Store::open(&dir).unwrap();
            let note = store.create_note(rect()).unwrap();
            store
                .tuck_note(&note.id, "right", Rect { x: 1900, y: 300, w: 280, h: 220 })
                .unwrap();
            // Incidental window moves must not disturb a tucked rect (hover peeks).
            store.queue_place(&note.id, Rect { x: 1700, y: 300, w: 280, h: 220 });
            store.flush_places();
            note.id
        };
        let store = Store::open(&dir).unwrap();
        let n = store.get(&id).unwrap();
        assert_eq!(n.tucked.as_deref(), Some("right"));
        assert_eq!(n.rect.x, 1900, "peek moves never stick");
        assert!(n.pinned);

        store.untuck_note(&id, Rect { x: 1500, y: 300, w: 280, h: 220 }).unwrap();
        let n = store.get(&id).unwrap();
        assert!(n.tucked.is_none());
        assert_eq!(n.rect.x, 1500);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Pull everything B is missing from A and apply it — one sync direction.
    fn sync_a_to_b(a: &Store, b: &Store) -> Vec<String> {
        let ops = a.ops_missing_from(&b.version_vector().unwrap()).unwrap();
        b.apply_remote_ops(&ops).unwrap()
    }

    /// Due date and status are content: they sync, persist across reopen,
    /// and clear back to "not a task".
    #[test]
    fn task_fields_sync_and_persist() {
        let (da, db_) = (temp_dir(), temp_dir());
        let id = {
            let a = Store::open(&da).unwrap();
            let b = Store::open(&db_).unwrap();
            let note = a.create_note(rect()).unwrap();
            a.set_due(&note.id, Some(777_000)).unwrap();
            a.set_status(&note.id, Some("doing".into())).unwrap();
            sync_a_to_b(&a, &b);
            let on_b = b.get(&note.id).unwrap();
            assert_eq!(on_b.due_ms, Some(777_000));
            assert_eq!(on_b.status.as_deref(), Some("doing"));
            note.id
        };
        let a = Store::open(&da).unwrap();
        let n = a.get(&id).unwrap();
        assert_eq!(n.due_ms, Some(777_000), "due survives reopen");
        assert_eq!(n.status.as_deref(), Some("doing"));
        a.set_status(&id, None).unwrap();
        assert_eq!(a.get(&id).unwrap().status, None);
        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }

    #[test]
    fn hooks_fire_on_commits_and_arrivals_only() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir_a = temp_dir();
        let dir_b = temp_dir();
        let a = Store::open(&dir_a).unwrap();
        let b = Store::open(&dir_b).unwrap();

        let commits = std::sync::Arc::new(AtomicUsize::new(0));
        {
            let c = commits.clone();
            a.set_commit_hook(move || {
                c.fetch_add(1, Ordering::SeqCst);
            });
        }
        let arrivals = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        {
            let ar = arrivals.clone();
            b.set_arrivals_hook(move |ids| ar.lock().unwrap().extend(ids.to_vec()));
        }

        let note = a.create_note(rect()).unwrap();
        a.set_text(&note.id, "hello from A".into()).unwrap();
        assert!(commits.load(Ordering::SeqCst) >= 2, "each local commit wakes sync");

        // First sync introduces the note on B — exactly one arrival.
        sync_a_to_b(&a, &b);
        assert_eq!(arrivals.lock().unwrap().as_slice(), &[note.id.clone()]);

        // A later edit syncs a known note — no new arrival.
        a.set_text(&note.id, "edited on A".into()).unwrap();
        sync_a_to_b(&a, &b);
        assert_eq!(arrivals.lock().unwrap().len(), 1, "edits are not arrivals");

        // Remote applies never fire B's commit hook path (no ping-pong): B's
        // own commit count is untracked here, but A pulling B's (empty) delta
        // must not re-report the note either.
        sync_a_to_b(&b, &a);
        assert_eq!(arrivals.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    /// CONTENT must converge across replicas. Presentation (pinned, tucked,
    /// rect) is per-device by design, so it's normalized out before comparing.
    fn assert_same_state(a: &Store, b: &Store) {
        let strip = |mut notes: Vec<pinq_core::NoteState>| {
            notes.sort_by(|x, y| x.id.cmp(&y.id));
            for n in &mut notes {
                n.pinned = false;
                n.tucked = None;
                n.rect = Rect { x: 0, y: 0, w: 0, h: 0 };
            }
            notes
        };
        assert_eq!(
            serde_json::to_string(&strip(a.all_notes())).unwrap(),
            serde_json::to_string(&strip(b.all_notes())).unwrap(),
            "replicas diverged"
        );
    }

    #[test]
    fn presentation_is_device_local() {
        let (da, db_) = (temp_dir(), temp_dir());
        let a = Store::open(&da).unwrap();
        let b = Store::open(&db_).unwrap();

        // A creates a note: pinned for A, but B receives it as library content.
        let note = a.create_note(rect()).unwrap();
        assert!(a.get(&note.id).unwrap().pinned);
        sync_a_to_b(&a, &b);
        let on_b = b.get(&note.id).unwrap();
        assert!(!on_b.pinned, "remote create must land unpinned");
        assert!(on_b.tucked.is_none());

        // Presentation moves create NO ops: the version vector stays put.
        let vv_before = a.version_vector().unwrap();
        a.tuck_note(&note.id, "right", Rect { x: 1900, y: 300, w: 280, h: 220 }).unwrap();
        a.file_note(&note.id).unwrap();
        a.repin(&note.id).unwrap();
        a.queue_place(&note.id, Rect { x: 5, y: 5, w: 280, h: 220 });
        a.flush_places();
        assert_eq!(a.version_vector().unwrap(), vv_before, "presentation leaked into the op-log");

        // B pins its copy; A's layout is untouched by B's choice.
        b.repin(&note.id).unwrap();
        assert!(b.get(&note.id).unwrap().pinned);
        assert_eq!(a.get(&note.id).unwrap().rect.x, 5);

        // Trash IS content: A trashes, B (after sync) unpins + trashes.
        a.trash_note(&note.id).unwrap();
        for id in sync_a_to_b(&a, &b) {
            b.reconcile_presentation(&id);
        }
        let on_b = b.get(&note.id).unwrap();
        assert!(on_b.trashed && !on_b.pinned);

        // Overlay survives reopen: A restores + repins, then reopens.
        a.restore_note(&note.id).unwrap();
        a.repin(&note.id).unwrap();
        drop(a);
        let a = Store::open(&da).unwrap();
        assert!(a.get(&note.id).unwrap().pinned, "overlay must persist");

        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }

    /// Position is device-local, but SIZE shapes text layout, so it syncs:
    /// a note pulled out elsewhere spawns at the size it was last worked on.
    #[test]
    fn note_size_syncs_as_content() {
        let (da, db_) = (temp_dir(), temp_dir());
        let a = Store::open(&da).unwrap();
        let b = Store::open(&db_).unwrap();
        let note = a.create_note(rect()).unwrap();
        sync_a_to_b(&a, &b);

        // Pure move: no op leaves the device.
        let vv = a.version_vector().unwrap();
        a.queue_place(&note.id, Rect { x: 900, y: 40, w: 280, h: 220 });
        a.flush_places();
        assert_eq!(a.version_vector().unwrap(), vv, "moves stay device-local");

        // Resize: becomes content, and B's library copy learns the new size.
        a.queue_place(&note.id, Rect { x: 900, y: 40, w: 560, h: 400 });
        a.flush_places();
        assert_ne!(a.version_vector().unwrap(), vv, "size change must sync");
        sync_a_to_b(&a, &b);
        let on_b = b.get(&note.id).unwrap();
        assert_eq!((on_b.rect.w, on_b.rect.h), (560, 400));
        assert!(!on_b.pinned, "size sync must not pin the remote copy");

        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }

    #[test]
    fn ops_replicate_between_stores() {
        let (da, db_) = (temp_dir(), temp_dir());
        let a = Store::open(&da).unwrap();
        let b = Store::open(&db_).unwrap();
        assert_ne!(a.device_id(), b.device_id());

        let note = a.create_note(rect()).unwrap();
        a.set_text(&note.id, "made on A".into()).unwrap();
        a.add_tag(&note.id, "shared").unwrap();

        let affected = sync_a_to_b(&a, &b);
        assert_eq!(affected, vec![note.id.clone()]);
        let on_b = b.get(&note.id).expect("note crossed over");
        assert_eq!(on_b.text, "made on A");
        assert_eq!(on_b.tags, vec!["shared"]);

        // Idempotent: nothing new the second time.
        assert!(sync_a_to_b(&a, &b).is_empty());

        // Edit on B flows back to A.
        b.set_text(&note.id, "edited on B".into()).unwrap();
        sync_a_to_b(&b, &a);
        assert_eq!(a.get(&note.id).unwrap().text, "edited on B");
        assert_same_state(&a, &b);

        // Both stores must survive reopen with the merged log intact.
        drop(a);
        let a = Store::open(&da).unwrap();
        assert_eq!(a.get(&note.id).unwrap().text, "edited on B");

        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }

    #[test]
    fn concurrent_edits_converge() {
        let (da, db_) = (temp_dir(), temp_dir());
        let a = Store::open(&da).unwrap();
        let b = Store::open(&db_).unwrap();

        let note = a.create_note(rect()).unwrap();
        sync_a_to_b(&a, &b);

        // Offline, both sides edit the same note.
        a.set_color(&note.id, Some("mint".into())).unwrap();
        a.set_text(&note.id, "version A".into()).unwrap();
        b.set_paper(&note.id, Some("ruled".into())).unwrap();
        b.add_tag(&note.id, "from-b").unwrap();
        b.set_text(&note.id, "version B".into()).unwrap();

        // Sync both ways (either order) — canonical replay must converge.
        sync_a_to_b(&a, &b);
        sync_a_to_b(&b, &a);
        assert_same_state(&a, &b);

        let n = a.get(&note.id).unwrap();
        assert_eq!(n.color.as_deref(), Some("mint"));
        assert_eq!(n.paper.as_deref(), Some("ruled"));
        assert_eq!(n.tags, vec!["from-b"]);

        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }

    #[test]
    fn remote_delete_and_trash_propagate() {
        let (da, db_) = (temp_dir(), temp_dir());
        let a = Store::open(&da).unwrap();
        let b = Store::open(&db_).unwrap();

        let keep = a.create_note(rect()).unwrap();
        let gone = a.create_note(rect()).unwrap();
        sync_a_to_b(&a, &b);
        assert_eq!(b.all_notes().len(), 2);

        a.trash_note(&keep.id).unwrap();
        a.purge_note(&gone.id).unwrap();
        sync_a_to_b(&a, &b);

        assert!(b.get(&keep.id).unwrap().trashed);
        assert!(b.get(&gone.id).is_none(), "purge crossed over");
        assert_same_state(&a, &b);

        std::fs::remove_dir_all(da).ok();
        std::fs::remove_dir_all(db_).ok();
    }
}
