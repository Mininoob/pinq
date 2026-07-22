//! Local semantic smarts: a small sentence-embedding model (quantized
//! all-MiniLM-L6-v2 via fastembed, ~25 MB) runs ON THIS DEVICE and indexes
//! note text into vectors, powering "related notes" and one-click tag
//! suggestions. Nothing leaves the machine: the model is cached in
//! app_data/models, vectors live in app_data/smart.db — device-local,
//! never synced, not part of backups (both are derived data).
//!
//! Suggestions only ever SUGGEST. Accepting one applies tags through the
//! ordinary op path (so it syncs like any manual tag); dismissing is
//! remembered per (tag, note) so the same pairing never nags twice.

use crate::store::Store;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// Cosine floor for "this note belongs with that tag" (vectors are unit
/// length, so dot == cosine). Tuned for short sticky notes; raise if
/// suggestions feel eager, lower if they feel blind.
const TAG_SIM: f32 = 0.55;
/// Per-note ghost chips are invitations, not batch actions — friendlier floor.
const NOTE_TAG_SIM: f32 = 0.50;
/// Keyword-cluster groups (brand-new tags) must be semantically coherent:
/// mean similarity of members to the group centroid.
const GROUP_COHERENCE: f32 = 0.45;
/// A brand-new keyword tag needs this many notes to be worth grouping.
const MIN_GROUP: usize = 3;
/// First lines longer than this read as sentences, not titles — offer one.
const TITLE_LONG: usize = 48;
/// Sentinel "tag" recording a dismissed title suggestion for a note.
const TITLE_DISMISS: &str = "__title__";
/// A tag needs this many hand-tagged examples before it can attract notes.
const MIN_ANCHORS: usize = 2;
const MAX_NOTES_PER_SUGGESTION: usize = 8;
const MAX_SUGGESTIONS: usize = 3;
/// Related-notes floor — softer than tags; we show at most a few chips.
const RELATED_SIM: f32 = 0.35;
const RELATED_LIMIT: usize = 4;
/// Notes embedded per index pass; keeps each pass short so disable reacts.
const BATCH: usize = 32;
/// Only this much text feeds the model (MiniLM truncates ~256 tokens anyway).
const EMBED_CHARS: usize = 1500;

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SmartStatus {
    pub enabled: bool,
    /// "off" | "loading" | "indexing" | "ready" | "error"
    pub phase: String,
    pub indexed: usize,
    pub total: usize,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct SmartState {
    pub status: Mutex<SmartStatus>,
    want_on: AtomicBool,
    worker_alive: AtomicBool,
    /// The loaded model, shared so commands (semantic search) can embed a
    /// query without owning a second copy. None until the worker loads it;
    /// cleared on disable to release the memory.
    model: Mutex<Option<fastembed::TextEmbedding>>,
}

impl SmartState {
    pub fn on(&self) -> bool {
        self.want_on.load(Ordering::SeqCst)
    }
}

/// Embed one query string with the shared model (short lock). None when the
/// feature is off or the model isn't loaded yet.
pub fn embed_query(app: &AppHandle, text: &str) -> Option<Vec<f32>> {
    let state = app.state::<SmartState>();
    if !state.on() {
        return None;
    }
    let mut guard = state.model.lock().unwrap();
    let model = guard.as_mut()?;
    let mut vec = model.embed(vec![text.to_string()], None).ok()?.pop()?;
    normalize(&mut vec);
    Some(vec)
}

#[derive(Clone, Serialize)]
pub struct TagSuggestion {
    pub tag: String,
    pub notes: Vec<crate::links::LinkTarget>,
}

// ---------- lifecycle ----------

pub fn init(app: &AppHandle) {
    let enabled = app.state::<crate::settings::SettingsStore>().current().smart_suggestions;
    if enabled {
        set_enabled(app, true);
    }
}

/// Flip the feature on/off (called at startup and from set_settings). The
/// worker exits between passes when switched off; switching on spawns it
/// (at most one alive at a time).
pub fn set_enabled(app: &AppHandle, on: bool) {
    let state = app.state::<SmartState>();
    state.want_on.store(on, Ordering::SeqCst);
    {
        let mut status = state.status.lock().unwrap();
        status.enabled = on;
        if !on {
            status.phase = "off".into();
            status.error = None;
        }
    }
    if on && state.worker_alive.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    {
        let app = app.clone();
        std::thread::spawn(move || {
            run_worker(&app);
            app.state::<SmartState>().worker_alive.store(false, Ordering::SeqCst);
        });
    }
}

fn set_phase(app: &AppHandle, phase: &str, error: Option<String>) {
    let state = app.state::<SmartState>();
    let mut status = state.status.lock().unwrap();
    status.phase = phase.into();
    status.error = error;
}

fn run_worker(app: &AppHandle) {
    set_phase(app, "loading", None);
    let Ok(data_dir) = app.path().app_data_dir() else {
        set_phase(app, "error", Some("no data dir".into()));
        return;
    };
    let model_dir = data_dir.join("models");
    let _ = std::fs::create_dir_all(&model_dir);

    // First enable downloads the model into app_data/models; afterwards this
    // loads from disk. Runs for seconds either way — worker thread only.
    let model = match fastembed::TextEmbedding::try_new(
        fastembed::InitOptions::new(fastembed::EmbeddingModel::AllMiniLML6V2Q)
            .with_cache_dir(model_dir)
            .with_show_download_progress(false),
    ) {
        Ok(m) => m,
        Err(e) => {
            set_phase(app, "error", Some(format!("model load failed: {e}")));
            return;
        }
    };
    // Park it in shared state so semantic search can embed queries too.
    {
        let state = app.state::<SmartState>();
        let mut slot = state.model.lock().unwrap();
        *slot = Some(model);
    }

    let conn = match open_db(app) {
        Ok(c) => c,
        Err(e) => {
            set_phase(app, "error", Some(format!("smart.db: {e}")));
            return;
        }
    };

    let store = app.state::<Arc<Store>>().inner().clone();
    let state = app.state::<SmartState>();
    while state.want_on.load(Ordering::SeqCst) {
        let more = match index_pass(app, &store, &conn) {
            Ok(more) => more,
            Err(e) => {
                set_phase(app, "error", Some(e));
                false
            }
        };
        if more {
            continue; // keep chewing through the backlog
        }
        // Settled: nap, but wake fast when the feature is switched off.
        for _ in 0..10 {
            if !state.want_on.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    // Release the model's memory when switched off.
    let mut slot = state.model.lock().unwrap();
    *slot = None;
}

/// Embed up to BATCH changed notes; returns whether more remain. Vectors are
/// keyed by a text hash so unchanged notes are never re-embedded.
fn index_pass(app: &AppHandle, store: &Store, conn: &Connection) -> Result<bool, String> {
    let notes = store.all_notes();
    let live: Vec<&pinq_core::NoteState> =
        notes.iter().filter(|n| !n.trashed && !n.text.trim().is_empty()).collect();

    let mut stored: HashMap<String, String> = HashMap::new();
    {
        let mut q = conn.prepare("SELECT id, hash FROM vecs").map_err(err)?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for row in rows {
            let (id, hash) = row.map_err(err)?;
            stored.insert(id, hash);
        }
    }

    // Drop vectors for purged/trashed/emptied notes.
    let live_ids: HashSet<&str> = live.iter().map(|n| n.id.as_str()).collect();
    for id in stored.keys().filter(|id| !live_ids.contains(id.as_str())) {
        conn.execute("DELETE FROM vecs WHERE id = ?1", [id]).map_err(err)?;
    }

    let pending: Vec<&&pinq_core::NoteState> = live
        .iter()
        .filter(|n| stored.get(&n.id).map(String::as_str) != Some(text_hash(&n.text).as_str()))
        .collect();

    {
        let state = app.state::<SmartState>();
        let mut status = state.status.lock().unwrap();
        status.total = live.len();
        status.indexed = live.len() - pending.len();
        status.phase = if pending.is_empty() { "ready".into() } else { "indexing".into() };
    }
    if pending.is_empty() {
        return Ok(false);
    }

    let batch: Vec<&&&pinq_core::NoteState> = pending.iter().take(BATCH).collect();
    let texts: Vec<String> =
        batch.iter().map(|n| n.text.chars().take(EMBED_CHARS).collect()).collect();
    let embeddings = {
        let state = app.state::<SmartState>();
        let mut guard = state.model.lock().unwrap();
        let Some(model) = guard.as_mut() else { return Ok(false) };
        model.embed(texts, None).map_err(|e| format!("embed failed: {e}"))?
    };

    for (n, mut vec) in batch.iter().zip(embeddings) {
        normalize(&mut vec);
        conn.execute(
            "INSERT INTO vecs (id, hash, dim, vec) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET hash = ?2, dim = ?3, vec = ?4",
            rusqlite::params![n.id, text_hash(&n.text), vec.len() as i64, vec_blob(&vec)],
        )
        .map_err(err)?;
    }
    let _ = app.emit("pinq-smart-updated", ());
    Ok(pending.len() > BATCH)
}

// ---------- storage ----------

fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("smart.db"))
}

fn open_db(app: &AppHandle) -> Result<Connection, String> {
    let conn = Connection::open(db_path(app)?).map_err(err)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS vecs (
             id   TEXT PRIMARY KEY,
             hash TEXT NOT NULL,
             dim  INTEGER NOT NULL,
             vec  BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS dismissed (
             tag     TEXT NOT NULL,
             note_id TEXT NOT NULL,
             PRIMARY KEY (tag, note_id)
         );",
    )
    .map_err(err)?;
    Ok(conn)
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

fn text_hash(text: &str) -> String {
    let clipped: String = text.chars().take(EMBED_CHARS).collect();
    blake3::hash(clipped.as_bytes()).to_hex().to_string()
}

fn vec_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn load_vecs(conn: &Connection) -> Result<HashMap<String, Vec<f32>>, String> {
    let mut q = conn.prepare("SELECT id, vec FROM vecs").map_err(err)?;
    let rows = q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))
        .map_err(err)?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, blob) = row.map_err(err)?;
        out.insert(id, blob_vec(&blob));
    }
    Ok(out)
}

// ---------- keywords (pure, unit-tested) ----------

/// Small english stopword list — enough to keep glue words out of tags/titles.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "her", "was", "one", "our",
    "out", "his", "has", "had", "have", "this", "that", "with", "they", "them", "then", "than",
    "from", "into", "will", "would", "there", "their", "what", "when", "where", "which", "while",
    "your", "about", "also", "just", "like", "some", "more", "over", "only", "very", "been",
    "being", "does", "doing", "each", "other", "these", "those", "here", "how", "who", "why",
    "get", "got", "need", "needs", "todo", "don", "its", "it's", "any", "may", "should", "could",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Lowercased word tokens worth tagging on: letters/digits, length ≥ 3,
/// not a stopword, not pure digits.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !is_stopword(w) && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

/// Document frequency per token across the corpus (unique per note).
pub fn doc_freq<'a>(texts: impl Iterator<Item = &'a str>) -> HashMap<String, usize> {
    let mut df = HashMap::new();
    for text in texts {
        let unique: HashSet<String> = tokenize(text).into_iter().collect();
        for w in unique {
            *df.entry(w).or_insert(0) += 1;
        }
    }
    df
}

/// A keyword is a plausible tag when it groups something (df ≥ 2) without
/// being wallpaper (df ≤ ~30% of notes, floor 2).
fn taggable(dfw: usize, n_notes: usize) -> bool {
    dfw >= 2 && dfw <= (n_notes * 3 / 10).max(2)
}

/// Brand-new tag candidates for one note: its most distinctive corpus-wise
/// keywords (tf·idf), excluding tags it already carries. At most `limit`.
pub fn keyword_tags_for(
    text: &str,
    own_tags: &[String],
    df: &HashMap<String, usize>,
    n_notes: usize,
    limit: usize,
) -> Vec<String> {
    let mut tf: HashMap<String, usize> = HashMap::new();
    for w in tokenize(text) {
        *tf.entry(w).or_insert(0) += 1;
    }
    let mut scored: Vec<(String, f32)> = tf
        .into_iter()
        .filter(|(w, _)| !own_tags.contains(w))
        .filter_map(|(w, tfw)| {
            let dfw = *df.get(&w)?;
            taggable(dfw, n_notes)
                .then(|| (w, tfw as f32 * (n_notes as f32 / dfw as f32).ln()))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(limit).map(|(w, _)| w).collect()
}

/// A concise title for a note whose first line runs on like a sentence:
/// its top-scored keywords, in order of appearance, title-cased. None when
/// the current first line already looks like a title.
pub fn title_suggestion(text: &str, df: &HashMap<String, usize>, n_notes: usize) -> Option<String> {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    if first.chars().count() <= TITLE_LONG {
        return None;
    }
    let mut tf: HashMap<String, usize> = HashMap::new();
    for w in tokenize(text) {
        *tf.entry(w).or_insert(0) += 1;
    }
    let mut scored: Vec<(String, f32)> = tf
        .into_iter()
        .map(|(w, tfw)| {
            let dfw = *df.get(&w).unwrap_or(&1);
            let idf = (n_notes.max(2) as f32 / dfw as f32).ln().max(0.1);
            (w, tfw as f32 * idf)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut top: Vec<String> = scored.into_iter().take(3).map(|(w, _)| w).collect();
    if top.is_empty() {
        return None;
    }
    // Read in the order the note says them, not by score.
    let lower = text.to_lowercase();
    top.sort_by_key(|w| lower.find(w.as_str()).unwrap_or(usize::MAX));
    let title = top
        .iter()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(title)
}

/// Library-card groups for BRAND-NEW tags: a keyword whose notes are also
/// semantically coherent (mean similarity to their centroid). This is what
/// lets suggestions appear before the user has tagged anything at all.
pub fn keyword_groups(
    notes: &[(String, Vec<String>, String)], // (id, tags, text)
    vecs: &HashMap<String, Vec<f32>>,
    dismissed: &HashSet<(String, String)>,
) -> Vec<(String, Vec<String>)> {
    let n = notes.len();
    let df = doc_freq(notes.iter().map(|(_, _, t)| t.as_str()));
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (word, &dfw) in df.iter() {
        // Groups get a looser ubiquity cap than per-note chips (half the
        // corpus): a pad that's 40% recipes deserves a #recipe suggestion.
        if dfw < MIN_GROUP || dfw > (n / 2).max(MIN_GROUP) {
            continue;
        }
        let members: Vec<&String> = notes
            .iter()
            .filter(|(id, tags, text)| {
                !tags.contains(word)
                    && !dismissed.contains(&(word.clone(), id.clone()))
                    && vecs.contains_key(id)
                    && tokenize(text).contains(word)
            })
            .map(|(id, _, _)| id)
            .collect();
        if members.len() < MIN_GROUP {
            continue;
        }
        let member_vecs: Vec<&Vec<f32>> = members.iter().map(|id| &vecs[id.as_str()]).collect();
        let c = centroid(&member_vecs);
        let mean_sim =
            member_vecs.iter().map(|v| dot(&c, v)).sum::<f32>() / member_vecs.len() as f32;
        if mean_sim < GROUP_COHERENCE {
            continue;
        }
        let mut ids: Vec<String> = members.into_iter().cloned().collect();
        ids.truncate(MAX_NOTES_PER_SUGGESTION);
        out.push((word.clone(), ids));
    }
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    out
}

// ---------- pure vector math (unit-tested, model-free) ----------

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn centroid(vecs: &[&Vec<f32>]) -> Vec<f32> {
    let dim = vecs.first().map_or(0, |v| v.len());
    let mut c = vec![0.0f32; dim];
    for v in vecs {
        for (ci, vi) in c.iter_mut().zip(v.iter()) {
            *ci += vi;
        }
    }
    normalize(&mut c);
    c
}

/// Query→note similarity floor for quick-find blending; queries score lower
/// than note-to-note comparisons, so this sits below RELATED_SIM.
const SEARCH_SIM: f32 = 0.30;

/// Note ids semantically matching a free-text query, best first. Empty when
/// the feature is off or the model isn't loaded.
pub fn semantic_search_ids(app: &AppHandle, query: &str, limit: usize) -> Vec<String> {
    let Some(qv) = embed_query(app, query) else { return Vec::new() };
    let Ok(conn) = open_db(app) else { return Vec::new() };
    let Ok(vecs) = load_vecs(&conn) else { return Vec::new() };
    let mut scored: Vec<(String, f32)> = vecs
        .iter()
        .map(|(id, v)| (id.clone(), dot(&qv, v)))
        .filter(|(_, s)| *s >= SEARCH_SIM)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);
    scored.into_iter().map(|(id, _)| id).collect()
}

/// Semantic top-k: other note ids by similarity to `target`, floor applied.
pub fn top_related<'a>(
    target: &[f32],
    others: impl Iterator<Item = (&'a str, &'a Vec<f32>)>,
    k: usize,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = others
        .map(|(id, v)| (id.to_string(), dot(target, v)))
        .filter(|(_, s)| *s >= RELATED_SIM)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored
}

/// Classify untagged notes against existing tags' centroids. Each candidate
/// joins only its best-matching tag, and only above TAG_SIM; tags need
/// MIN_ANCHORS hand-tagged examples. `dismissed` pairs never resurface.
/// Centroid per existing tag with enough hand-tagged, indexed examples.
pub fn tag_centroids(
    notes: &[(String, Vec<String>)],
    vecs: &HashMap<String, Vec<f32>>,
) -> Vec<(String, Vec<f32>)> {
    let mut centroids: Vec<(String, Vec<f32>)> = Vec::new();
    let all_tags: HashSet<&String> = notes.iter().flat_map(|(_, tags)| tags).collect();
    for tag in all_tags {
        let anchors: Vec<&Vec<f32>> = notes
            .iter()
            .filter(|(id, tags)| tags.contains(tag) && vecs.contains_key(id))
            .map(|(id, _)| &vecs[id])
            .collect();
        if anchors.len() >= MIN_ANCHORS {
            centroids.push((tag.clone(), centroid(&anchors)));
        }
    }
    centroids
}

pub fn suggest_tags(
    notes: &[(String, Vec<String>)],
    vecs: &HashMap<String, Vec<f32>>,
    dismissed: &HashSet<(String, String)>,
) -> Vec<(String, Vec<String>)> {
    let centroids = tag_centroids(notes, vecs);
    if centroids.is_empty() {
        return Vec::new();
    }

    let mut groups: HashMap<&str, Vec<String>> = HashMap::new();
    for (id, tags) in notes {
        let Some(vec) = vecs.get(id) else { continue };
        let best = centroids
            .iter()
            .filter(|(tag, _)| !tags.contains(tag) && !dismissed.contains(&(tag.clone(), id.clone())))
            .map(|(tag, c)| (tag.as_str(), dot(vec, c)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((tag, sim)) = best {
            if sim >= TAG_SIM {
                groups.entry(tag).or_default().push(id.clone());
            }
        }
    }

    let mut out: Vec<(String, Vec<String>)> = groups
        .into_iter()
        .map(|(tag, mut ids)| {
            ids.truncate(MAX_NOTES_PER_SUGGESTION);
            (tag.to_string(), ids)
        })
        .collect();
    // Biggest cluster first; stable tie order by tag name for determinism.
    out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    out.truncate(MAX_SUGGESTIONS);
    out
}

// ---------- commands ----------

#[tauri::command]
pub async fn smart_status(state: State<'_, SmartState>) -> Result<SmartStatus, ()> {
    Ok(state.status.lock().unwrap().clone())
}

/// Semantically similar notes for the note window's "related" strip.
/// Empty when the feature is off or the index hasn't caught up yet.
#[tauri::command]
pub async fn related_notes(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<Vec<crate::links::LinkTarget>, String> {
    if !app.state::<SmartState>().want_on.load(Ordering::SeqCst) {
        return Ok(Vec::new());
    }
    let conn = open_db(&app)?;
    let vecs = load_vecs(&conn)?;
    let Some(target) = vecs.get(&id) else { return Ok(Vec::new()) };

    let notes = store.all_notes();
    let live: HashMap<&str, &pinq_core::NoteState> =
        notes.iter().filter(|n| !n.trashed).map(|n| (n.id.as_str(), n)).collect();
    let hits = top_related(
        target,
        vecs.iter()
            .filter(|(vid, _)| vid.as_str() != id && live.contains_key(vid.as_str()))
            .map(|(vid, v)| (vid.as_str(), v)),
        RELATED_LIMIT,
    );
    Ok(hits
        .into_iter()
        .filter_map(|(vid, _)| live.get(vid.as_str()))
        .map(|n| crate::links::LinkTarget {
            id: n.id.clone(),
            title: crate::links::title_of(n),
            pinned: n.pinned,
        })
        .filter(|t| !t.title.is_empty())
        .collect())
}

/// Clusters of untagged notes that look like an existing tag, for the
/// library's suggestion card.
#[tauri::command]
pub async fn tag_suggestions(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
) -> Result<Vec<TagSuggestion>, String> {
    if !app.state::<SmartState>().want_on.load(Ordering::SeqCst) {
        return Ok(Vec::new());
    }
    let conn = open_db(&app)?;
    let vecs = load_vecs(&conn)?;
    let mut dismissed = HashSet::new();
    {
        let mut q = conn.prepare("SELECT tag, note_id FROM dismissed").map_err(err)?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for row in rows {
            dismissed.insert(row.map_err(err)?);
        }
    }

    let notes = store.all_notes();
    let live: Vec<&pinq_core::NoteState> = notes.iter().filter(|n| !n.trashed).collect();
    let meta: Vec<(String, Vec<String>)> =
        live.iter().map(|n| (n.id.clone(), n.tags.clone())).collect();
    let by_id: HashMap<&str, &&pinq_core::NoteState> =
        live.iter().map(|n| (n.id.as_str(), n)).collect();

    // Existing-tag matches first, then coherent brand-new keyword groups —
    // the latter is what surfaces suggestions in a barely-tagged space.
    let mut groups = suggest_tags(&meta, &vecs, &dismissed);
    let full: Vec<(String, Vec<String>, String)> =
        live.iter().map(|n| (n.id.clone(), n.tags.clone(), n.text.clone())).collect();
    for (tag, ids) in keyword_groups(&full, &vecs, &dismissed) {
        if groups.len() >= MAX_SUGGESTIONS {
            break;
        }
        if !groups.iter().any(|(t, _)| t == &tag) {
            groups.push((tag, ids));
        }
    }

    Ok(groups
        .into_iter()
        .map(|(tag, ids)| TagSuggestion {
            tag,
            notes: ids
                .iter()
                .filter_map(|id| by_id.get(id.as_str()))
                .map(|n| crate::links::LinkTarget {
                    id: n.id.clone(),
                    title: crate::links::title_of(n),
                    pinned: n.pinned,
                })
                .collect(),
        })
        .filter(|s| !s.notes.is_empty())
        .collect())
}

/// Everything the note window's ghost chips need: tag suggestions (existing
/// tags by centroid + distinctive keywords) and an optional concise title.
#[derive(Clone, Serialize, Default)]
pub struct NoteSuggestions {
    pub tags: Vec<String>,
    pub title: Option<String>,
}

#[tauri::command]
pub async fn note_suggestions(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
) -> Result<NoteSuggestions, String> {
    if !app.state::<SmartState>().want_on.load(Ordering::SeqCst) {
        return Ok(NoteSuggestions::default());
    }
    let Some(note) = store.get(&id).filter(|n| !n.trashed) else {
        return Ok(NoteSuggestions::default());
    };
    let conn = open_db(&app)?;
    let vecs = load_vecs(&conn)?;
    let mut dismissed_tags: HashSet<String> = HashSet::new();
    {
        let mut q =
            conn.prepare("SELECT tag FROM dismissed WHERE note_id = ?1").map_err(err)?;
        let rows = q.query_map([&id], |r| r.get::<_, String>(0)).map_err(err)?;
        for row in rows {
            dismissed_tags.insert(row.map_err(err)?);
        }
    }

    let notes = store.all_notes();
    let live: Vec<&pinq_core::NoteState> = notes.iter().filter(|n| !n.trashed).collect();
    let n_notes = live.len();
    let mut tags: Vec<String> = Vec::new();

    // Existing tags whose cluster this note sits in.
    if let Some(vec) = vecs.get(&id) {
        let meta: Vec<(String, Vec<String>)> =
            live.iter().map(|n| (n.id.clone(), n.tags.clone())).collect();
        let mut scored: Vec<(String, f32)> = tag_centroids(&meta, &vecs)
            .into_iter()
            .filter(|(tag, _)| !note.tags.contains(tag) && !dismissed_tags.contains(tag))
            .map(|(tag, c)| (tag, dot(vec, &c)))
            .filter(|(_, sim)| *sim >= NOTE_TAG_SIM)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        tags.extend(scored.into_iter().map(|(t, _)| t));
    }

    // Distinctive keywords as brand-new tag ideas.
    let df = doc_freq(live.iter().map(|n| n.text.as_str()));
    for kw in keyword_tags_for(&note.text, &note.tags, &df, n_notes, 2) {
        if !tags.contains(&kw) && !dismissed_tags.contains(&kw) {
            tags.push(kw);
        }
    }
    tags.truncate(3);

    let title = (!dismissed_tags.contains(TITLE_DISMISS))
        .then(|| title_suggestion(&note.text, &df, n_notes))
        .flatten();

    Ok(NoteSuggestions { tags, title })
}

/// One-click title accept: a new first line on the first text box, so the
/// note gains the title without losing a word. Link rewrites ride along
/// (the note's title just changed).
#[tauri::command]
pub async fn apply_title(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("empty title".into());
    }
    let Some(note) = store.get(&id) else { return Err("note not found".into()) };
    let old_title = crate::links::title_of(&note);
    let first_text = note.items.iter().find_map(|i| match &i.content {
        pinq_core::ItemContent::Text { text } => Some((i.item_id.clone(), text.clone())),
        pinq_core::ItemContent::Attachment { .. } => None,
    });
    match first_text {
        Some((item_id, text)) => store
            .set_item_text(&id, &item_id, format!("{title}\n{text}"))
            .map_err(|e| e.to_string())?,
        None => store
            .add_item(&id, pinq_core::CanvasItem::text_item(8.0, 8.0, 248.0, title))
            .map_err(|e| e.to_string())?,
    }
    crate::links::sync_links_after_edit(&app, &store, &id, &old_title);
    Ok(())
}

/// One-click accept: tag every listed note through the normal op path (this
/// syncs like a manual tag) and remember the pairing so removing the tag
/// later never re-triggers the same suggestion.
#[tauri::command]
pub async fn accept_tag_suggestion(
    app: AppHandle,
    store: State<'_, Arc<Store>>,
    tag: String,
    ids: Vec<String>,
) -> Result<(), String> {
    let conn = open_db(&app)?;
    for id in &ids {
        store.add_tag(id, &tag).map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT OR IGNORE INTO dismissed (tag, note_id) VALUES (?1, ?2)",
            rusqlite::params![tag.to_lowercase(), id],
        )
        .map_err(err)?;
    }
    // Open note windows refresh their tag chips off this event.
    let _ = app.emit("pinq-sync-applied", &ids);
    Ok(())
}

#[tauri::command]
pub async fn dismiss_title_suggestion(app: AppHandle, id: String) -> Result<(), String> {
    let conn = open_db(&app)?;
    conn.execute(
        "INSERT OR IGNORE INTO dismissed (tag, note_id) VALUES (?1, ?2)",
        rusqlite::params![TITLE_DISMISS, id],
    )
    .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub async fn dismiss_tag_suggestion(
    app: AppHandle,
    tag: String,
    ids: Vec<String>,
) -> Result<(), String> {
    let conn = open_db(&app)?;
    for id in &ids {
        conn.execute(
            "INSERT OR IGNORE INTO dismissed (tag, note_id) VALUES (?1, ?2)",
            rusqlite::params![tag.to_lowercase(), id],
        )
        .map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: [f32; 3]) -> Vec<f32> {
        let mut v = v.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn related_ranks_by_similarity_with_floor() {
        let target = unit([1.0, 0.0, 0.0]);
        let close = unit([0.9, 0.1, 0.0]);
        let mid = unit([0.5, 0.5, 0.0]);
        let far = unit([0.0, 1.0, 0.0]); // cosine 0 — under the floor
        let others = vec![
            ("far".to_string(), far),
            ("close".to_string(), close),
            ("mid".to_string(), mid),
        ];
        let hits = top_related(&target, others.iter().map(|(id, v)| (id.as_str(), v)), 10);
        let ids: Vec<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["close", "mid"]);
    }

    #[test]
    fn keyword_tags_pick_distinctive_words_not_wallpaper() {
        // "recipe" appears in 2 of 10 notes (taggable); "milk" is in 8 (wallpaper).
        let texts = [
            "sourdough recipe with flour",
            "curry recipe tonight",
            "milk", "milk", "milk", "milk", "milk again", "buy milk",
            "milk run", "get milk now",
        ];
        let df = doc_freq(texts.iter().copied());
        let tags = keyword_tags_for("sourdough recipe with flour", &[], &df, texts.len(), 2);
        assert!(tags.contains(&"recipe".to_string()), "got {tags:?}");
        assert!(!tags.contains(&"milk".to_string()));
        // A tag the note already has is never re-suggested.
        let tags = keyword_tags_for("curry recipe tonight", &["recipe".into()], &df, texts.len(), 2);
        assert!(!tags.contains(&"recipe".to_string()));
    }

    #[test]
    fn title_suggested_only_for_long_first_lines() {
        let texts = ["remember to pick up the giant sourdough starter from marks bakery on friday"];
        let df = doc_freq(texts.iter().copied());
        let t = title_suggestion(texts[0], &df, 5).expect("long first line gets a title");
        assert!(!t.is_empty() && t.chars().count() < 48);
        // Word order follows the note, first letter capitalized.
        assert!(t.chars().next().unwrap().is_uppercase());
        // A short first line is already a title.
        assert!(title_suggestion("Groceries\nmilk and eggs", &df, 5).is_none());
    }

    #[test]
    fn keyword_groups_need_coherence_and_size() {
        // Four "standup" notes clustered around x̂ → group; two "misc" notes
        // with a shared word but opposite vectors → no group (incoherent).
        let notes: Vec<(String, Vec<String>, String)> = vec![
            ("a".into(), vec![], "standup notes monday".into()),
            ("b".into(), vec![], "standup notes tuesday".into()),
            ("c".into(), vec![], "standup blockers".into()),
            ("d".into(), vec![], "standup summary".into()),
            ("e".into(), vec![], "scatter word apple".into()),
            ("f".into(), vec![], "scatter word banana".into()),
            ("g".into(), vec![], "scatter again cherry".into()),
            ("h".into(), vec![], "filler filler".into()),
            ("i".into(), vec![], "filler more".into()),
            ("j".into(), vec![], "filler most".into()),
        ];
        let vecs: HashMap<String, Vec<f32>> = [
            ("a".to_string(), unit([1.0, 0.05, 0.0])),
            ("b".to_string(), unit([0.95, 0.0, 0.05])),
            ("c".to_string(), unit([0.9, 0.1, 0.0])),
            ("d".to_string(), unit([1.0, 0.0, 0.1])),
            ("e".to_string(), unit([0.0, 1.0, 0.0])),
            ("f".to_string(), unit([0.0, -1.0, 0.0])),
            ("g".to_string(), unit([0.0, 0.0, 1.0])),
            ("h".to_string(), unit([0.3, 0.3, 0.3])),
            ("i".to_string(), unit([0.2, 0.5, 0.1])),
            ("j".to_string(), unit([0.1, 0.2, 0.7])),
        ]
        .into();
        let groups = keyword_groups(&notes, &vecs, &HashSet::new());
        assert!(groups.iter().any(|(t, ids)| t == "standup" && ids.len() == 4), "got {groups:?}");
        assert!(!groups.iter().any(|(t, _)| t == "scatter"));
    }

    #[test]
    fn suggest_tags_classifies_by_best_centroid_and_respects_dismissals() {
        // Two anchor notes around x̂ carry #work; one around ŷ carries #home
        // (only one anchor — below MIN_ANCHORS, so #home can't attract).
        let notes = vec![
            ("w1".to_string(), vec!["work".to_string()]),
            ("w2".to_string(), vec!["work".to_string()]),
            ("h1".to_string(), vec!["home".to_string()]),
            ("candidate".to_string(), vec![]),
            ("unrelated".to_string(), vec![]),
            ("snubbed".to_string(), vec![]),
        ];
        let vecs: HashMap<String, Vec<f32>> = [
            ("w1".to_string(), unit([1.0, 0.05, 0.0])),
            ("w2".to_string(), unit([0.95, 0.0, 0.05])),
            ("h1".to_string(), unit([0.0, 1.0, 0.0])),
            ("candidate".to_string(), unit([0.9, 0.1, 0.0])),
            ("unrelated".to_string(), unit([0.0, 0.1, 1.0])),
            ("snubbed".to_string(), unit([0.92, 0.0, 0.08])),
        ]
        .into();
        let dismissed: HashSet<(String, String)> =
            [("work".to_string(), "snubbed".to_string())].into();

        let out = suggest_tags(&notes, &vecs, &dismissed);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "work");
        assert_eq!(out[0].1, vec!["candidate"]);

        // Already-tagged notes never get their own tag re-suggested.
        assert!(out[0].1.iter().all(|id| id != "w1" && id != "w2"));
    }
}
