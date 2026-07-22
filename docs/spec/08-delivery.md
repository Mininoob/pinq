# 08 — Delivery: Releases, Risks, Backlog, Acceptance, Decisions, Next Steps

## 33. Release and migration strategy

- **Channels**: dev (nightly, internal) → beta (opt-in, auto-update) → stable.
  Windows: signed MSIX + portable exe; macOS: notarized DMG; Linux: AppImage + Flatpak.
- **Updates**: staged rollout (5→25→100%), delta updates, side-by-side install with
  rollback; the running app never migrates data until the new binary has verified
  itself and snapshotted the DB (§18).
- **Data migrations**: forward-only, versioned, snapshot-first; op-log format carries
  a version byte — old devices refuse (with a clear message) to apply newer-format ops
  rather than corrupting; sync negotiates the minimum common op version so a
  mixed-version device fleet degrades to read-compatible behavior instead of forking.
- **Format documentation**: the on-disk and export formats are published in
  `docs/format/` from MVP on — the user-ownership promise is testable by outsiders.
- **Beta→1.0 promise**: from MVP onward, no release may strand data; every migration
  has an automated corpus test (§32).

## 34. Major risks and mitigations

| # | Risk | L×I | Mitigation |
|---|---|---|---|
| R1 | Fullscreen/borderless detection unreliable on Windows → notes over games | H×H | Prototype phase 1 focus; per-app remembered setting; conservative default (hide on whole-screen cover); hide-all hotkey as escape hatch |
| R2 | Gesture feels bad (pull mushy, throws misfire) | M×H | Prototype with tunable thresholds + trace-replay suite; click/keyboard paths mean gesture is never the only way |
| R3 | Many WebView note windows blow memory/CPU budget | M×H | Prototype measures 20-note idle; suspension design (§29); Electron fallback decision gate at prototype exit; worst case: native-drawn note surface (Rust + skia) for note windows only |
| R4 | CRDT + op-log storage bloat at 100k objects | M×M | Compaction snapshots; yrs GC; 100k seeded profile in CI from V1 alpha |
| R5 | Sync data-loss bug erodes the one promise that matters | M×H | Sync ships a full phase after storage; simulator + property tests; op-log append-only means recovery is always possible; beta cohort with known-good backups |
| R6 | Wayland/GNOME can't do the core interaction | H×M (Linux only) | Documented degraded mode; X11 first; layer-shell where present; don't promise parity |
| R7 | Scope creep toward Anytype-parity sinks MVP | H×H | Phase gates in §7; block editor is *on* the block model from MVP so deferral ≠ rework |
| R8 | macOS permission/notarization friction (a11y, screen-capture APIs) | M×M | Keep off permission-triggering APIs where possible (§11); early TestFlight-style notarized beta |
| R9 | Key/recovery UX failure → users lose data and blame the product | M×H | Nag-until-confirmed phrase; multi-device redundancy encouraged at pairing; plain-language warnings tested with users |
| R10 | Solo-platform team bus-factor on windowing arcana | M×M | Windowing traits + per-OS docs as they're built; harness tests encode the knowledge |

## 35. Prioritized backlog (epic level, order = priority)

1. **E1 Windowing foundation** (P): monitor topology service, pad window, note window,
   topmost/non-activating, DPI, persistence.
2. **E2 Gestures** (P): pull state machine, throw classifier, undo toast, trace replay
   harness.
3. **E3 Fullscreen & immersive handling** (P): detection per §11, per-app setting.
4. **E4 Storage core** (M): SQLite schema, op-log, projections, FTS, crash recovery,
   backups.
5. **E5 Minimal editor** (M): block-model text + checklist + basic image/file blocks,
   autosave journal, find-in-note, markdown shortcuts subset.
6. **E6 Library MVP** (M): sidebar sources, list, detail, batch ops, tags, search,
   settings, onboarding, import/export TXT/MD, quick find/capture.
7. **E7 MVP hardening** (M): perf budgets, chaos tests, a11y audit, installer/updater.
8. **E8 Block editor full** (1): all block types, slash menu, tables, toggles, history UI.
9. **E9 Object system** (1): types, properties, schema evolution, templates.
10. **E10 Query/view layer** (1): prop index, live queries, table/board/gallery/
    calendar/graph views, inline views, widgets.
11. **E11 Media** (1): viewer, AV player, galleries, bookmarks, attachment browser.
12. **E12 Sync** (1): identity/pairing/crypto, LAN P2P, relay, selective sync,
    conflict UI, device management, encrypted backup.
13. **E13 V1 polish**: exports (HTML/PDF/CSV/JSON), storage manager, i18n
    infrastructure, macOS parity, Linux X11.

## 36. Acceptance criteria for every MVP feature (Given/When/Then, abridged to the decisive cases)

**Sticky pad**
- Given a two-monitor mixed-DPI setup, when the pad is enabled on both, then each pad
  renders at correct physical size and position fraction, and survives
  disconnect/reconnect with positions intact.
- Given hover-reveal mode, when the pointer crosses the strip in < 150 ms, then the
  pad does NOT reveal; when it dwells ≥ 150 ms, it reveals within 120 ms.
- Given any pad, then it never overlaps the taskbar/dock/menu bar in any tested
  arrangement (automated topology fixtures).

**Pull-to-create**
- Given pointer down on the pad, when dragged ≥ 24 px off-edge and released ≥ 48 px
  away, then exactly one note exists at the release rect with focused caret in
  ≤ 250 ms; when released closer, then no object exists and the preview snaps back.
- Given Esc mid-pull, then no object is created.
- Given click-to-create mode ON, when the pad is clicked, then a note is created at
  the default landing rect. Given keyboard only, J5 path works end-to-end with screen-
  reader announcements (NVDA scripted test).

**Throw-to-unpin**
- Given a drag released at ≥ 1500 px/s within ±35° of an outward edge normal with
  projected rest ≥ 40% off-screen, then the note unpins, is NOT trashed/archived,
  appears in Unpinned, and an undo toast shows for 6 s; Ctrl+Z restores the exact rect.
- Given the same speed toward an edge adjacent to another monitor, then it does NOT
  unpin (moves monitors).
- Given 500 recorded real-user "move fast" traces, then false-positive throws < 3%.

**Pin/unpin & multi-monitor**
- Given a note pinned to monitor B, when the app restarts, then it reappears at the
  same rect on B; when B is absent, it is parked and listed under its display name;
  when B returns, it restores unprompted.

**Always-on-top / fullscreen**
- Given a video in fullscreen (YouTube/VLC), an exclusive-fullscreen D3D app, and
  PowerPoint presenting, then all notes+pad on that monitor hide within 500 ms and
  restore within 500 ms of exit — automated via the fullscreen test harness.
- Given a borderless-maximized app and setting "keep visible", then notes stay.

**Persistence & crash recovery**
- Given `kill -9` at any moment during a typing storm (chaos test), when the app
  restarts, then the DB opens clean and at most 1 s of typing is lost, with the S18
  dialog accurate.

**Editor (MVP scope)**
- Typing latency budget met with 10 notes open (perf CI); autosave txn ≤ 30 ms;
  undo/redo across 200 mixed operations is consistent; find-in-note highlights all
  matches ≥ 2 chars.

**Images/files (MVP scope)**
- Drag-drop, paste, and picker each produce a deduped blob (same file twice = one
  blob, two attachments); EXIF-rotated JPEG displays upright; a 100 MP image never
  decodes fully in a note and opens in the viewer.

**Library, tags, search**
- 10k-object library scrolls at 60 fps; search returns first results ≤ 120 ms; tag
  add/remove via batch ops on 500 selected objects completes ≤ 2 s with one undo step.

**Import/export**
- MD import of a 1k-file folder preserves headings/lists/checkboxes/images (fixture
  corpus diff); export→import round-trip is lossless for MVP content; backup archive
  restores on a clean machine to a byte-identical object set (IDs preserved).

**Trash**
- Deleted notes are restorable for 30 days with tags/placement metadata intact; purge
  requires typed confirmation ("delete 3 notes permanently").

## 37. Open product decisions (tracked, with current lean)

1. Monetization model (A4) — lean: paid managed relay; decide before V1 beta.
2. Product name/brand ("Pinq" is a working title) — decide before public beta.
3. Editor base: ProseMirror-per-block vs. fully custom — spike in E5; lean ProseMirror.
4. Default telemetry prompt placement (first-run vs. settings-only) — user test.
5. Reference-mode attachments in MVP or V1 — lean V1 (managed-copy only in MVP).
6. Note color semantics: freeform palette vs. color-as-property (queryable) — lean
   color-as-property; user test.
7. GNOME/Wayland: ship degraded mode or gate Linux on wlroots/KDE only — decide at V1.
8. Sensitive-note search exclusion default — lean: searchable titles, excluded
   snippets (as specced), revisit with P3 interviews.
9. Multiple spaces (work/personal vaults) in V1 or later — architecture supports it
   (spaceId everywhere); lean: single space until collaboration.
10. Relay protocol openness: publish spec for third-party relays at V1 or after
    hardening — lean: publish at V1 + security review.

## 38. Recommended next-step implementation sequence

Weeks 1–2: repo + CI scaffolding (Tauri app, pinq-core crate skeleton, windowing
traits), monitor-topology service with fixture playback, pad window rendering on one
monitor. Weeks 3–4: pull gesture end-to-end with tunable thresholds + trace recorder;
note windows with move/resize/persist (JSON persistence stub — SQLite comes with MVP).
Weeks 5–6: throw classifier + undo toast; fullscreen detection harness + hide/restore;
20-note perf measurement. Weeks 7–8: mixed-DPI + hotplug hardening; keyboard/click
paths; internal demo + go/no-go review against §29 prototype targets and R1–R3.

### The ten most important engineering tasks for the interactive prototype

1. **Monitor topology service (Win32)** — enumerate displays with stable IDs
   (EDID+adapter hash), work areas, per-monitor DPI; emit change events on
   WM_DISPLAYCHANGE/WM_DPICHANGED; fixture-record/replay mode for tests.
2. **Pad window** — frameless, topmost, non-activating (WS_EX_NOACTIVATE|TOPMOST),
   edge-anchored from a stored fraction, hover-reveal state machine, correct under
   100/150/200% mixed DPI.
3. **Note window shell** — frameless rounded always-on-top window: drag, resize,
   collapse, z-management within the note layer, non-activating hover, focus-on-click.
4. **Pull-to-create state machine** — §9 thresholds, direction gate, live preview
   window, cancel/snap-back, cascade placement; thresholds hot-tunable via a debug
   panel.
5. **Throw classifier** — rolling velocity sampler, §10 rules incl. interior-edge
   exclusion, coast-and-clamp for non-throws, file-away animation + undo toast;
   pointer-trace recorder and replay CLI for regression tuning.
6. **Fullscreen & immersive detection (Win)** — SHQueryUserNotificationState +
   foreground-geometry heuristic on SetWinEventHook; per-monitor hide/restore of pad
   and notes ≤ 500 ms; borderless-maximized user setting; test harness apps
   (exclusive D3D, borderless, video, PowerPoint).
7. **Placement persistence** — save/restore monitor-relative rects, z, collapsed
   state across restart, disconnect (parking), reconnect, resolution/rotation change,
   with clamping and never-lost guarantees.
8. **Keyboard & click alternative paths** — pad focus mode, Enter-create,
   Ctrl+Shift+U unpin, Ctrl+Alt+arrow move/resize, global shortcuts (new/hide-all/
   focus pad), UIA announcements verified with NVDA.
9. **Prototype persistence & undo spine** — minimal append-only action log (JSON)
   backing create/move/throw/undo, proving the "every mutation is an op" architecture
   the real store will inherit.
10. **Performance instrumentation** — tracing spans + frame-time overlay measuring §29
    prototype targets (pull latency, 60 fps drag, 20-note idle CPU/RAM), wired into CI
    as budget assertions; produces the go/no-go data for the Tauri-vs-fallback gate.
