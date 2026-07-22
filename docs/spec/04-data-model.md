# 04 — Object Model, Schemas, Templates, Queries, Views, Widgets

## 15. Object and database model

### Core concepts
- **Object** — the universal unit. Sticky notes, pages, tasks, people, images, files,
  databases, collections, queries, templates, tags, types themselves — all objects.
- **Type** — an object (kind `type`) declaring a name, icon, default layout, property
  set, and templates. Built-in types are seeded objects, not hard-coded specials, so
  custom types are the same machinery.
- **Property definition** — an object declaring key, display name, value type,
  constraints (options for selects, target type for relations).
- **Block** — a node in an object's content tree (text, list item, image, embed…).
  Blocks belong to exactly one object; block text is a CRDT sequence (§20).
- **Relation** — a typed, directional link (property of value-type `relation`);
  backlinks are derived, not stored twice.
- **Collection** — an object holding an explicit ordered member list.
- **Query** — an object holding a filter tree + sort spec; membership is computed.
- **View** — a saved presentation (table/board/…) over a collection or query.

### Object envelope (every object)
```
id           ULID, immutable, globally unique (sorts by creation time)
typeId       → type object
title        string (CRDT text)
blocks       tree of block nodes (§20 for CRDT representation)
props        { propKey → typed value }
createdAt / modifiedAt   hybrid logical clock timestamps (HLC)
createdBy / modifiedBy   deviceId (userId reserved for future collaboration)
tags         set of tag-object ids
flags        favorite · archived · trashed(+trashedAt) 
pinning      { pinned: bool, monitorStableId, rect, zRank, collapsed, locked,
               opacityOverride, virtualDesktopMode, lastPlacement }
revision     head op-id + version vector (per-object)
syncStatus   derived: local-only | syncing | synced | conflict
```

Deletion model: `trashed` is a flag (30-day retention, restorable); **purge** writes a
tombstone op that propagates to all devices; blobs are garbage-collected when
unreferenced (§18).

### Property value types
text · number (unit-annotatable) · checkbox · date/datetime (with optional end →
ranges) · status (ordered options + category todo/doing/done) · select · multi-select
· url · email · phone · file (→ blob ref) · image (→ blob ref) · relation (target
type, one/many) · created-time (system) · modified-time (system) · **formula
(placeholder)** — storable, renders "formulas arrive in a later release," reserved so
schemas don't churn at V2.

### Schema evolution (explicit rules)
Property definitions are versioned objects; values are stored **keyed by property-def
id, not name**, so:
- **Rename** = metadata edit; zero data migration; old exports keep both names.
- **Remove** = def marked `retired`; values are kept but hidden ("show retired
  properties" toggle in the type editor allows recovery); purged only by explicit
  user compaction.
- **Type change** = new property-def id + best-effort value conversion (text→number
  parses; number→text formats; select→multi wraps; incompatible → value moved to a
  `_migrated` text shadow, never destroyed). The old def retires. Conversions run
  lazily per object and are recorded as ops so they sync deterministically.
- Sync-safety: two devices editing schema concurrently converge because defs are
  objects under the same CRDT/op rules as everything else; a value written under a
  retired def is preserved and surfaced in the conflict panel if meaningful.

## 16. Example schema definitions

```jsonc
// Type: Task (seeded)
{ "id": "01J...TASK", "kind": "type", "name": "Task", "icon": "check-circle",
  "layout": "note", "properties": [
    { "id": "p.status",  "name": "Status", "type": "status",
      "options": [ {"v":"Todo","cat":"todo"}, {"v":"Doing","cat":"doing"},
                   {"v":"Blocked","cat":"doing"}, {"v":"Done","cat":"done"} ] },
    { "id": "p.due",     "name": "Due",      "type": "datetime" },
    { "id": "p.project", "name": "Project",  "type": "relation",
      "target": "01J...PROJ", "cardinality": "one" },
    { "id": "p.priority","name": "Priority", "type": "select",
      "options": ["Low","Medium","High"] } ] }

// Object: a sticky note that is a Task
{ "id": "01J...N1", "typeId": "01J...TASK", "title": "Demo env reset",
  "props": { "p.status": "Done", "p.due": "2026-07-15T15:00:00-07:00[HLC:…]" },
  "tags": ["01J...TAGOPS"],
  "pinning": { "pinned": false,
    "lastPlacement": { "monitorStableId": "DELU2723-a1b2", 
                       "rect": [1180, 240, 280, 220], "collapsed": false } },
  "blocks": [ { "id":"b1","kind":"todo","checked":true,
                "text":"reset demo env before 3pm" } ] }

// Query: "Due today"
{ "id":"01J...Q1", "kind":"query", "name":"Due today",
  "filter": { "and": [ { "prop":"typeId","op":"is","value":"01J...TASK" },
                       { "prop":"p.status","op":"category-not","value":"done" },
                       { "prop":"p.due","op":"on","value":"@today" } ] },
  "sort": [ {"prop":"p.due","dir":"asc"} ] }

// View: board over that query
{ "id":"01J...V1", "kind":"view", "source":"01J...Q1", "layout":"board",
  "groupBy":"p.status", "visibleProps":["p.due","p.priority"], "cardSize":"m" }
```

## 17. Templates, queries, collections, views, widgets

### Templates
A template is an object (`kind: template`, bound to a typeId) whose blocks and props
are cloned at creation. Templates may define: default blocks · default property values
· icon · cover · layout · suggested tags · default relations (e.g., Meeting →
current-week Project) · default pinned state · default sticky color & size. Each type
has an ordered template list + a default; the pad's long-press/`T` picker shows them.
Template edits never retroactively change existing objects (see §20 for concurrent
template-edit semantics).

**Original starter templates** (all original wording; none copied from other products):
Blank sticky · To-do note (checklist + Status) · Daily note (date-titled, agenda +
notes + carried-over todos) · Meeting note (attendees/agenda/decisions/actions →
spawns Tasks) · Project (goal, status, timeline, inline Task view) · Person (contact
props + "mentions of" backlink section) · Reading-list item (url, author, status,
rating) · Bookmark (url + auto-fetched title/preview) · Recipe (ingredients checklist,
steps, servings, source) · Habit tracker (habit + weekly checkboxes) · Weekly planner
(7 day sections + top-3) · Study note (topic, source, summary, Q&A toggles) · Idea
(one-liner + expansion prompts) · Decision log (context/options/decision/revisit-date)
· Bug report (steps/expected/actual/severity) · Media item (type, creator, status,
rating, notes).

### Collections vs. queries vs. views (hard distinction)
- **Collection**: explicit, ordered membership; drag in/out; an object can be in many.
- **Query**: rule-based, dynamic; membership recomputed on data change; supports
  nested AND/OR groups, all property operators, `@today`-style relative dates, tag,
  type, pin/monitor, and full-text terms.
- **View**: presentation over exactly one collection or query — layout (table · list ·
  gallery · board · calendar · graph · compact), saved filters *further narrowing* the
  source, sort, group-by, property visibility/order, card size. Multiple views per
  source. Views can be embedded inline in any object's blocks and placed in the
  sidebar. "＋ New" inside a view creates an object pre-filled to satisfy the source
  (type filter → that type; board column → that status; calendar cell → that date).
- Batch editing operates on the current selection in any view.

### Query engine (offline, efficient)
- Filters compile to SQL over the property index table (§18) — property values are
  shredded into `prop_index(object_id, prop_id, value_type, value_text, value_num,
  value_date)` with covering indexes, so a board over 100k objects is a single indexed
  query, not an object scan.
- Full-text terms delegate to FTS5 and intersect by rowid.
- **Live updates**: the store emits (objectId, changedFields) on commit; each open
  view keeps its compiled predicate and re-evaluates only changed objects —
  insert/remove/resort incrementally. Complete requery only on view-definition change.
- Relative dates (`@today`) re-evaluate on day rollover via a scheduled tick.

### Widgets (sidebar V1; desktop widgets later)
Widget = { source: object/collection/query/system-source, layout: compact | list |
tree | gallery | object-preview, limit, header action }. Shipped sources: Recent
notes · Favorites · Pinned notes (grouped by monitor) · Notes on monitor X · Tasks due
today · Overdue tasks · Tag X · any collection · any saved query · Recently opened
files · Mini calendar · Quick capture box · Sync status. Widgets render from the same
live-query layer, are keyboard-reorderable, and virtualize their lists.
