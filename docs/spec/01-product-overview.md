# 01 — Product Overview

## 1. Executive summary

Pinq is a desktop application that makes capturing a thought as fast as grabbing a
physical sticky note, and keeps every captured thought inside a real personal
knowledge-management system instead of a junk drawer.

The core loop:

1. A slim **sticky pad** lives on a screen edge. The user pulls a note off it — a real
   drag gesture that peels a new note onto the desktop.
2. The note is a small always-on-top window with a lightweight block editor: text,
   checklists, images, files, links.
3. When the note has served its desktop purpose, the user **throws** it off the screen
   edge. It is filed — never deleted — into the main library.
4. The **main application** is a full PKM: every note is an object with a type, typed
   properties, relationships, tags, and history. Objects can be organized into
   collections, matched by saved queries, and displayed in table/board/gallery/calendar
   views. Templates make repeat capture (meeting notes, tasks, contacts) one gesture.
5. Everything is **local-first**: full offline operation, SQLite + content-addressed
   files on disk, end-to-end-encrypted device-to-device sync over LAN or via an
   optional (self-hostable) encrypted relay. The vendor never holds readable user data.

Delivery is phased: a 6–8 week interactive **prototype** proving the windowing and
gesture layer on Windows; an **MVP** (~6 months) that is a genuinely useful sticky-note
manager with search, tags, and import/export; a **V1** (~12–15 months) adding the block
editor, object system, database views, widgets, and P2P sync; and later releases for
collaboration, mobile companions, and extensions.

The riskiest technical areas — always-on-top vs. fullscreen apps, multi-monitor DPI,
gesture feel, and CRDT-based sync — are addressed first and are the subject of the
prototype.

## 2. Product principles

1. **Capture must be faster than forgetting.** Pad-to-typing in under 250 ms. Nothing —
   sync, indexing, thumbnails — may ever block the input path.
2. **Nothing is ever lost by a gesture.** Throwing, closing, unpinning never destroy
   data. Delete is always explicit, always goes to Trash, always undoable for 30 days.
3. **The user owns the data.** Readable on-disk formats documented publicly; full
   export at all times; sync keys held by the user; the app works forever with no
   account and no network.
4. **Physical, but predictable.** Animations communicate what happened (filed, not
   destroyed); they never gate interaction and always respect reduced-motion.
5. **A note is an object.** Every sticky note is a first-class object in the knowledge
   graph from the moment it exists — no "migrate this scrap into the real system" step.
6. **Honest about the OS.** Where platforms differ (fullscreen detection, Wayland
   layer-shell, virtual desktops), we document the difference and degrade gracefully
   instead of pretending.
7. **Accessible is not an add-on.** Every drag gesture has a keyboard and click
   equivalent shipped in the same release.
8. **Crash-safe by construction.** Any process may be killed at any instant with at
   most the last ~1 s of typing lost.

## 3. Assumptions (explicitly labeled)

- **A1 — Single-user product.** MVP and V1 sync one person's devices. Collaboration is
  a later release; the data model reserves room for it (author IDs on ops, space-scoped
  keys) but no sharing UI ships before then.
- **A2 — Desktop OS floor:** Windows 10 1903+ / Windows 11, macOS 13+, Ubuntu 22.04+
  (X11 fully, Wayland best-effort). No Windows 7/8.
- **A3 — Scale envelope:** up to ~50 simultaneously pinned desktop notes, ~100k objects,
  ~50 GB attachments, ~10 devices per user. Targets in §29 are set against this.
- **A4 — Monetization** (assumed, not decided): free local app; paid managed relay/backup
  subscription; self-hosted relay always free. Nothing in the architecture depends on this.
- **A5 — No cloud account required, ever.** Identity is a keypair, not an email.
- **A6 — Team:** ~4–6 engineers (2 Rust, 2 frontend, 1 platform/windowing, shared
  design/QA). Phase estimates assume this.
- **A7 — English-first UI, i18n-ready strings from day one; RTL layout deferred to post-V1.**
- **Uncertainties flagged throughout:** Wayland layer-shell coverage (§24), WebView
  media codec coverage (§22/§23), macOS notarization friction for a window-hooking app
  (§24), CRDT storage overhead at 100k objects (§19).

## 4. Target users and jobs to be done

**P1 — The desktop-centric professional** (developer, analyst, PM; 2–3 monitors).
- JTBD: "When something interrupts me mid-task, I need to capture it in one motion so I
  can stay in flow." / "Keep my working set — today's tasks, a meeting agenda, a build
  status — glanceably on screen without another app window to manage."

**P2 — The systematizer** (Notion/Anytype/Obsidian user who distrusts cloud silos).
- JTBD: "Give me structured types, properties, and views over my data without renting
  my own notes back from a SaaS." / "Let quick captures land in the same graph as my
  structured knowledge so nothing rots in an inbox app."

**P3 — The privacy-required user** (lawyer, clinician, journalist, researcher).
- JTBD: "Sync between my laptop and workstation without any third party being able to
  read it." / "Prove to myself (and my compliance officer) where the data physically is."

**P4 — The visual thinker / student.**
- JTBD: "Pin the three things I must not forget where my eyes already are." / "Make
  capture feel good enough that I actually do it."

Non-targets for V1: real-time team collaboration, mobile-primary users, publishing/blogging.

## 5. Key differentiators

1. **The pull/throw gesture pair.** No PKM ships a physical, edge-docked capture
   surface; no sticky-note app files thrown notes into a real knowledge system.
2. **Sticky notes with a full object model behind them** — a desktop note can *be* a
   Task with a due date that appears in a Kanban view, not a text scrap.
3. **Local-first with honest E2EE P2P sync** — LAN sync with no server at all;
   relay is optional, encrypted, and self-hostable.
4. **Multi-monitor as a first-class concept** — per-monitor pads, per-monitor pinning,
   "notes on this monitor" as a queryable property.
5. **No account, no lock-in** — documented storage format, complete export, works
   offline forever.

## 6. Complete feature inventory

Legend: **P** prototype · **M** MVP · **1** V1 · **L** later.

**Sticky pad:** edge docking L/R/T/B (P) · per-monitor config & position (M) · move
along edge (P) · size/opacity/color/appearance (M) · collapsed & hover-reveal modes
(M) · click-to-create accessibility mode (M) · keyboard create (M) · touch/pen create
(1) · OS-reserved-zone avoidance (M).

**Desktop notes:** pull-to-create (P) · throw-to-unpin (P) · always-on-top with
fullscreen exception (P) · pin/unpin/lock/collapse/resize/move/duplicate/archive/close
(M) · position persistence across restarts & monitor changes (M) · hide-all shortcut
(M) · virtual-desktop pinning option (1, per-OS) · click-through mode (1) · global
shortcuts: new note, quick find, show/hide all, pin last (M).

**Editor:** plain text + basic styling (M) · full block editor: headings, lists,
checklists, toggles, quotes, callouts, code, dividers, tables (1) · images (M basic /
1 full) · galleries, video, audio, attachments-rich (1) · web links (M), rich bookmarks
(1) · internal links & mentions (1) · dates (1) · embedded database views (1) · slash
commands & markdown shortcuts (1; core markdown shortcuts in M) · find-in-note (M) ·
note history (1) · spellcheck (M, via platform/WebView).

**Library:** inbox/recents, all notes, pinned, unpinned, favorites, archive, trash (M)
· tags (M) · search: title/body (M); properties, files, links (1) · filters & sort (M)
· batch operations (M) · tabs & multi-window (1) · saved views & sidebar widgets (1) ·
calendar browsing (1) · graph view (1) · attachment browser (1) · storage management
(1) · sync & device management (1).

**Object system (1):** custom types · typed properties (full list §15) · templates ·
collections · saved queries · table/list/gallery/board/calendar/graph/compact views ·
inline views · new-object-from-view.

**Sync (1):** LAN P2P · encrypted relay · pairing via QR/code · recovery phrase ·
device revocation · selective & attachment sync · metered/battery controls · conflict
surfacing · encrypted backup archives.

**Import/export:** plain text, Markdown (M) · HTML, CSV, JSON (1) · media/document
ingestion (M basic) · other sticky apps where feasible (1/L) · export TXT/MD (M),
HTML/PDF/JSON/CSV (1) · full backup archive, optionally encrypted (M unencrypted, 1
encrypted).

**Later:** collaboration & shared spaces · mobile companions · web clipper · public
sharing · plugins/API · formulas · reminders · OCR · AI features · desktop widgets ·
handwriting · voice notes.

## 7. Phase scope and intentional exclusions

### Prototype (6–8 weeks, Windows only, throwaway-permitted UI)
**Proves:** pad docking + reserved-zone avoidance; pull-to-create gesture feel; ~20
frameless always-on-top note windows at 60 fps; throw-to-unpin with undo; fullscreen
detection (game, video, presentation) hiding/restoring notes; per-monitor DPI
correctness; position persistence through monitor unplug/replug.
**Excluded and why:** all PKM features, sync, rich content, macOS/Linux — none of them
matter if the windowing layer can't be made to feel right; that's the go/no-go.

### MVP (the smallest reliable product)
Sticky pad · text notes with basic images/files · main library · pin/unpin ·
multi-monitor positioning · always-on-top with fullscreen exception · local
persistence (SQLite+WAL) · full-text search · basic tags · TXT/MD import/export +
unencrypted backup · crash recovery.
**Excluded and why:** block editor beyond basics (editor rewrite risk — MVP editor is
built *on* the block model but exposes few block types, so no migration later); custom
types/properties/views (needs the editor); **sync** (shipping sync before the storage
format is battle-tested risks corrupting data on every device — but every MVP write
already goes through the op-log so history is sync-ready); video/audio players (OS
default apps suffice); note history UI (op-log records it; UI later).

### Version 1
Full block editor · rich media & bookmarks · templates · custom types + properties ·
collections, queries, database views · widgets · P2P + relay sync · E2EE + device
management · note history UI · graph & calendar views · HTML/PDF/CSV/JSON export ·
encrypted backups.
**Excluded and why:** collaboration (multiplies CRDT/permission complexity; single-user
sync must be boring-reliable first); formulas (needs a dependency-tracking engine —
placeholder property type ships so schemas don't churn); mobile (different codebase;
desktop is the wedge); plugins (API must stabilize first).

### Later releases
Collaboration & shared spaces → mobile companions → browser clipper → public sharing →
plugin API → formulas → reminders → OCR → AI (local-first, opt-in) → desktop database
widgets → handwriting → voice notes. Each gated on the layer below being stable; each
excluded earlier because it multiplies surface area without validating the core loop.
