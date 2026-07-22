# 06 — Media & Files, Technology Stack, Platform Feasibility, Module Boundaries

## 22. Image, video, audio, link, and file architecture

### Ingestion (all media kinds)
Paths: drag-and-drop (files, browser images, promised files on macOS) · clipboard
paste (bitmap → encode PNG; file paths; HTML with remote imgs → fetch on consent) ·
file picker · URL import (size-capped 100 MB default, streaming download with
progress/cancel/retry) · multi-file batches (parallelism 3, one aggregate progress
card, per-item retry).
Pipeline: stream to temp → BLAKE3 while streaming → dedup check (hash exists ⇒ link,
no copy) → move into `blobs/` → append attachment op → queue derived work
(thumbnail/poster/metadata) at idle priority. Cancel at any stage rolls back cleanly.

### Images
- Decode via image-rs in the Rust core for thumbnails/metadata; display via WebView
  (which hardware-decodes common formats) using `pinq-blob://<hash>` custom protocol.
- **EXIF orientation** applied at decode; original bytes never rewritten.
- Thumbnails: WebP, longest edge 256/1024, generated in background; note renders a
  blurhash placeholder → thumb → (on zoom) full image — progressive, lazy
  (IntersectionObserver), cancellable when scrolled away.
- **Large-image protection**: > 80 MP or > 512 MB decoded size ⇒ never fully decoded
  in a note; tiled/downsampled preview with "open full viewer." Per-note decoded-image
  memory budget 128 MB; LRU eviction of decoded bitmaps.
- Viewer (S10): zoom 10–1600% (wheel/pinch/±), pan (drag/arrows), fit / fill /
  actual-size modes, rotate 90° steps (stored as a view transform op, non-destructive),
  fullscreen, filmstrip for galleries. Animated GIF/APNG/WebP play in viewer, static
  first frame in notes (setting).
- Errors: unsupported format → typed card with filename + "open externally"; decode
  failure → retry + reveal-in-folder. No silent blanks.
- **Synced**: original blob + metadata + rotation ops. **Not synced**: thumbnails and
  decoded caches (regenerated locally).

### Video & audio
- Player features: play/pause, scrub with hover-thumbnails (keyframe strip generated
  in background where cheap), volume/mute, speed 0.5–2×, fullscreen, PiP (WebView2 &
  macOS support it; Linux best-effort), captions (WebVTT tracks; sidecar .vtt
  attachments auto-linked by filename), poster frame (first keyframe or user-chosen),
  keyboard (space, ←/→ 5 s, ↑/↓ volume, f, m), resume-position (stored per
  object+device), loop, progressive `Range`-request loading via the blob protocol,
  error recovery (decode error → retry once → external-open card).
- **Engine decision** — evaluated three options:
  1. **Native frameworks** (Media Foundation / AVFoundation / GStreamer): best codec
    licensing story, three integrations to build and maintain, inconsistent behavior.
  2. **Embedded engine (libmpv/ffmpeg)**: widest codecs, but ~40–80 MB binary, our
    security patch burden for a huge C parser surface, GPL/patent review.
  3. **Browser media stack (WebView `<video>/<audio>`)**: zero extra binary, sandboxed
    parsers patched by OS vendors, consistent API. Codec coverage: WebView2 (Chromium)
    — H.264/AAC/VP9/AV1/Opus ✓, HEVC only with OS codec pack; WKWebView — H.264/HEVC/
    AAC ✓, VP9/AV1 partial; WebKitGTK — weakest (GStreamer-plugin-dependent).
  **Recommendation: (3) for V1**, with a "codec not supported → open externally /
  optional transcode-on-import (ffmpeg CLI as an optional download, not bundled)"
  fallback. Revisit libmpv only if telemetry-free feedback shows real codec pain.

### Web links & rich bookmarks
Plain link block (MVP) → bookmark object (V1): background fetch of title, description,
favicon, og:image into `derived/previews/`. Security: previews fetched by the Rust
core (not the WebView) with no cookies, no JS execution, 5 s timeout, private-IP/
localhost URLs blocked (SSRF), image size-capped; per-space "disable link previews"
switch (privacy: fetching reveals your IP to the site — stated in the setting).

### File attachments
- **Managed copy is the default and only MVP mode**: file bytes copied into the blob
  store (dedup by hash). **Reference mode (V1, opt-in per attachment)**: store
  path+hash, watch for moves (best effort), show "missing" card with relink/search-
  by-hash when broken. Both modes offered; the UI is explicit about which is in use.
- Metadata: display name (renamable, original name kept), size, mime, added-by/at,
  origin path (reference mode), extracted metadata (bg queue).
- Type icons by mime class; preview generation: images/PDF first page/text head/audio
  waveform (V1); everything else = icon card.
- Actions: open in default app (a **copy** is exported to a temp dir for open, so the
  canonical blob is never edited in place — "Replace with edited copy" prompt appears
  if the temp file changes) · reveal in file manager (managed mode reveals the
  exported copy; explained in UI) · save a copy · rename display name · replace
  (versioned — old hash retained in slot history) · delete (refcount--).
- Safety: attachments are never executed; no preview execution of scripts/macros;
  Windows `MotW` zone identifier applied to exported copies; extension/mime mismatch
  badge; optional size limit per attachment (default 2 GB) and per-space quota
  warnings at configurable disk headroom.
- Sync: blob plane per §19 (policy-based); progress and failure per file in the sync
  panel; a note with an un-synced attachment shows the placeholder-with-fetch card.

## 23. Recommended technology stack

**Frontend:** TypeScript + **Solid.js** (fine-grained reactivity suits hundreds of
live-updating small surfaces; ~7 KB runtime; virtualized lists via `@tanstack/virtual`).
CSS: vanilla-extract or Tailwind (team choice). Editor: **custom block editor on
ProseMirror** per block-row with Yjs binding (`y-prosemirror`) — proven CRDT-editor
pairing.
**Core:** **Rust** crate `pinq-core` — storage (rusqlite + FTS5), op-log, CRDT (`yrs`),
sync (quinn QUIC, mdns-sd), crypto (ring / RustCrypto: ed25519-dalek, x25519-dalek,
chacha20poly1305, spake2, blake3), media ingest (image-rs, kamadak-exif, symphonia for
audio metadata), link previews (reqwest with SSRF guard).
**Shell:** **Tauri 2.x** — window management, tray, global shortcuts (tauri-plugin-
global-shortcut), autostart, updater; custom `pinq-blob://` protocol streams blobs
with Range support. Direct Win32/AppKit/X11 calls via `windows-rs` / `objc2` / `x11rb`
where Tauri's abstraction is insufficient (topmost tuning, fullscreen hooks,
`WDA_EXCLUDEFROMCAPTURE`, monitor stable IDs).
**Relay:** Rust (axum + quinn), single container, SQLite or S3-compatible envelope
store.

### Technology comparison (required evaluation)

| Criterion | Native ×3 | **Tauri 2 (rec.)** | Electron | Flutter desktop |
|---|---|---|---|---|
| Transparent/frameless windows | ✓✓ | ✓ (per-OS quirks) | ✓ | ✓ (plugins, rough on Linux) |
| Always-on-top | ✓✓ | ✓ (+native calls) | ✓ | ✓ |
| Fullscreen-app detection | native APIs anyway | same native calls | same | same, harder FFI |
| Multi-monitor / per-monitor DPI | ✓✓ | ✓ (v2 good; verify mixed-DPI in prototype) | ✓ | △ known multi-window/DPI gaps |
| Virtual desktops | ✓✓ | via native calls | via native calls | weak |
| Drag gestures / input | ✓✓ | ✓ (pointer events) | ✓ | ✓ |
| Global shortcuts | ✓✓ | ✓ plugin | ✓ | plugin, patchy |
| Native file access | ✓✓ | ✓ (Rust side) | ✓ | ✓ |
| Media playback | best | WebView (varies by OS, see §22) | Chromium (best web) | weak video story |
| Accessibility | best | WebView a11y tree (good) | Chromium (good) | improving, historically weak |
| Memory (20 note windows) | lowest | low-mid (WebView2 shares one browser process; each window ~10–25 MB) | high (each BrowserWindow heavy; ~1–1.5 GB risk) | mid |
| Binary size | ~10–30 MB | ~15–30 MB | ~150–250 MB | ~40–80 MB |
| Security posture | ours to build | Rust core + OS-patched WebView | Chromium bundled — we must ship patch releases | Google-patched engine |
| Cross-platform consistency | none (3 codebases) | good (WebView deltas) | best | good |
| Dev speed | slowest (×3 teams) | fast | fastest | mid |
| Long-term maintenance | heaviest | moderate | moderate + constant Chromium churn | bet on Google's desktop interest |

**Clear recommendation: Tauri 2.x.** The two decisive factors: (a) this app keeps
*many small always-on-top windows alive all day* — Electron's per-window memory and
idle CPU cost directly attack our §29 targets, while system WebViews share processes;
(b) we need a Rust core anyway (CRDT, QUIC, crypto, SQLite) — Tauri makes that core
first-class instead of a sidecar. Accepted costs: WebView media/codec variance
(mitigated §22), WebKitGTK weakness on Linux (Linux is 3rd platform anyway), and some
native window-code per OS (needed under every framework — no framework abstracts
fullscreen detection or layer-shell). Electron is the fallback if the prototype finds
WebView2 blockers; the frontend ports unchanged.

## 24. Windows / macOS / Linux feasibility comparison

| Capability | Windows | macOS | Linux X11 | Linux Wayland |
|---|---|---|---|---|
| Always-on-top | ✓ WS_EX_TOPMOST | ✓ NSWindow.level | ✓ EWMH | ✗ generally; layer-shell on wlroots/KDE; **GNOME ✗** |
| Fullscreen detection | △ heuristic + QUNS (borderless ambiguity) | ✓ Spaces model mostly sidesteps it | △ EWMH state | ✗ restricted |
| Non-activating windows | ✓ WS_EX_NOACTIVATE | ✓ NSPanel/nonactivating | ✓ hints | △ |
| Virtual desktops control | △ documented subset only | ✓ collectionBehavior | ✓ EWMH | ✗ |
| Global shortcuts | ✓ | ✓ (Input Monitoring not required for hotkeys via Carbon/NSEvent global monitor — some paths need Accessibility permission; flag) | ✓ | △ compositor-dependent (GlobalShortcuts portal emerging) |
| Per-monitor DPI | ✓ (complex but complete) | ✓ (backing scale) | △ (X11 global scale hacks) | ✓ protocol-level |
| Capture exclusion | ✓ WDA_EXCLUDEFROMCAPTURE (Win10 2004+) | ✓ sharingType | ✗ | ✗ |
| Notarization/store friction | low | notarization + possible permission prompts | none | none |

## 25. Recommended first OS: **Windows**

Justification: (1) largest desktop share among P1/P2 personas and multi-monitor setups;
(2) every risky capability (topmost, non-activating, DPI, capture-exclusion,
fullscreen heuristics) is *available and documented* on Windows — hard, but buildable
and testable; (3) WebView2 is the strongest system WebView (media, a11y); (4) the
worst platform ambiguity (borderless fullscreen) is a *product-setting* problem there,
versus Wayland where core capabilities are simply absent; (5) the developing user is
on Windows — daily dogfooding. macOS follows in MVP-hardening (Spaces actually
simplify fullscreen), Linux X11 at V1, Wayland best-effort with documented degradation.

## 26. API and module boundaries

```
apps/desktop (Tauri shell)          crates/
├── ui/ (Solid)                     ├── pinq-core        facade, api types
│   ├── pad/  notes/  library/      ├── pinq-store       sqlite, oplog, projections, fts
│   ├── editor/ (ProseMirror+Yjs)   ├── pinq-model       objects, types, props, queries
│   └── viewers/ (image, av)        ├── pinq-crdt        yrs integration, order keys
├── shell/ (Rust)                   ├── pinq-sync        vv, merkle, quic, mdns, relay client
│   ├── windowing/   win32|appkit|x11  (traits: PadHost, NoteWindowHost,
│   │                                   FullscreenMonitor, MonitorTopology)
│   ├── shortcuts/ tray/ blobproto/ ├── pinq-crypto      identity, pairing, envelopes, keystore
└── services/relay (axum)           ├── pinq-media       ingest, thumbs, exif, previews
                                    └── pinq-import      md/txt/html/csv/json codecs
```

- **UI ↔ core API**: typed command/query/event IPC (tauri specta-generated TS types).
  Commands are intent-level (`create_object`, `apply_text_delta`, `set_prop`,
  `pin_object{monitor,rect}`, `throw_unpin`, `query_subscribe`) — never raw SQL.
  Events: `object.changed`, `query.delta`, `sync.status`, `monitor.topology`.
- **Everything below `pinq-core` is UI-agnostic and headless-testable**; a CLI test
  harness (`pinq-cli`) drives the full core for integration tests and power users.
- Windowing traits isolate per-OS code; adding an OS = implementing 4 traits.
- The op-log is the only write path (UI cannot bypass it) — guarantees history, sync,
  and undo see every mutation.
