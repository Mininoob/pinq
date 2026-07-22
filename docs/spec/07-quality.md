# 07 — Security, Accessibility, Performance, Failure Recovery, Metrics, Testing

## 27. Security threat model

Assets: note content, attachments, keys, metadata (titles, graph), user's screen
presence. Adversaries: device thief, co-located user, malicious local app (non-admin),
relay operator/compromiser, network attacker, malicious pairing attempt, malicious
file/link content.

| Threat | Mitigation | Residual risk (stated honestly) |
|---|---|---|
| Lost/stolen device | At-rest encryption keyed via OS keystore + OS login; app lock (PIN/passphrase) + biometric unlock (Windows Hello / Touch ID) + auto-lock timer; remote revoke + epoch rotation | Powered-on unlocked device ⇒ exposed; revocation can't reclaim already-synced data |
| Malicious local app (same user) | OS keystore ACLs; no secrets in argv/env/logs; DB not readable without keystore unwrap | Same-user malware with keystore access wins — OS boundary, we say so |
| Compromised relay | E2EE envelopes; relay auth key ≠ content key; signed device registry prevents relay-injected devices | Traffic-analysis metadata (sizes, timing, IPs) visible; documented |
| Network interception | QUIC-TLS with pinned device keys + PAKE pairing with safety-number verification | User skipping safety-number comparison |
| Unauthorized pairing | SPAKE2 (short code not offline-bruteforceable), 60 s expiry, both-sides confirm, all devices notified "New device added: <name>" | Social engineering of the user |
| Corrupted/malicious sync ops | Ops signed by device keys; schema-validated before apply; per-object apply is transactional; fuzzed decoder | A validly-signed compromised device can write garbage → mitigated by history/restore |
| Malicious files | Never execute; no macro/script preview; MotW on exported copies; mime/extension mismatch badges; parsers (image/EXIF) in Rust, fuzzed | User opens file in a vulnerable external app |
| Unsafe link previews | Core-side fetch: no JS, no cookies, SSRF-blocked private ranges, size/time caps, global disable switch | Fetch reveals IP to the linked site (disclosed in setting) |
| HTML/script injection (pasted content, bookmarks, imported HTML) | Strict sanitizer allow-list on ingest; CSP in every WebView (no remote content; only `pinq-blob://`); no `eval`; Tauri IPC allow-list | WebView 0-days — patched by OS vendor cadence |
| Media parser vulns | Prefer OS/WebView sandboxed decoders; Rust-side decoding limited + fuzzed; decode size caps | Codec CVEs in system stack |
| Clipboard privacy | No background clipboard reads — only on explicit paste; "clear clipboard after copy (30 s)" option for sensitive copy | OS clipboard history retains copies (noted in UI) |
| Screen sharing / shoulder surf | Capture-exclusion (§11) default-on; hide-all shortcut; **sensitive-note mode**: blurred until hovered+unlocked, excluded from capture, excluded from FTS *snippets* (still searchable by title unless "exclude from search" checked), notifications show "1 note updated" not content | Linux capture-exclusion unavailable |
| Crash reports | Local-only minidumps by default; opt-in upload with scrubbed paths, no note content ever serialized into crash scope (guard: content buffers tagged & excluded) | — |
| Temp files | Ingest temps in app-private dir, wiped on start; exported-for-open copies in per-session dir, cleaned on exit + startup sweep | OS indexers may touch exported copies while open |
| Search index leakage | FTS lives inside the (optionally encrypted) DB; no OS Spotlight/Windows Search indexing of blob dir (marked with attributes/noindex) | — |
| Backups | Encrypted archives by default (passphrase or space key); unencrypted export requires an explicit "I understand" step | User-chosen weak passphrases |
| Key loss | Mnemonic recovery phrase + multi-device redundancy + nag-until-confirmed | No phrase + no device = unrecoverable (by design; stated at setup) |
| Secure deletion | Purge = tombstone + blob deletion + `PRAGMA secure_delete` best effort | SSD wear-leveling & OS snapshots make true secure erase impossible — UI says "deleted from the app; disk-level remnants may persist" |

## 28. Accessibility plan

- **No gesture-only functionality**: pull ↔ `Ctrl+Alt+P`→`Enter` / click-to-create;
  throw ↔ `Ctrl+Shift+U` / menu Unpin; move/resize ↔ `Ctrl+Alt+arrows` (+`Shift` fine,
  `R` toggles resize mode). All shortcuts rebindable.
- Full keyboard navigation across pad, notes, library, views; roving tabindex in
  lists; visible focus ring (2 px, 3:1 contrast); logical order = visual order; focus
  returns predictably after dialogs/toasts.
- Screen readers: WebView exposes ARIA → UIA/AX/AT-SPI. Named landmarks; notes are
  labeled windows ("Sticky note: Demo env reset, pinned, monitor 2"); live-region
  announcements for create/file/undo/sync events; tables and database views with
  proper row/column headers, `aria-rowcount` for virtualized grids, board columns as
  labeled lists with keyboard move ("move card: arrows, confirm Enter").
- High-contrast: respects OS themes (forced-colors media query on Windows); all
  states also encoded by icon/shape/text, never color alone (sync ✓/↻/⚠ glyphs;
  status dots carry letters in colorblind-safe mode; default palette checked for
  deuteranopia/protanopia/tritanopia).
- Reduced motion: OS-detected + in-app override; disables coast/peel/parallax,
  replaces with ≤ 90 ms fades; **animations never gate function** anywhere.
- Text scaling: whole-UI zoom 80–200%; respects OS text-size; min body text 13 px
  logical at 100%; touch targets ≥ 24×24 px pointer / ≥ 44×44 px touch (pad thickness
  auto-raises with touch present).
- Voice control: every actionable has an accessible name ⇒ Windows Voice
  Access/macOS Voice Control number overlays work; commands avoid drag-only paths.
- WCAG 2.1 AA is the bar for the main app; the pad/notes layer is audited with NVDA +
  Narrator (Windows), VoiceOver (macOS), Orca (Linux) each release.

## 29. Performance targets (measured, budgeted in CI)

| Metric | Target (p95, mid-tier 2020 laptop) |
|---|---|
| Cold start → pad visible | ≤ 1.5 s (library window may lazy-load after) |
| Warm start | ≤ 700 ms |
| Pad reveal on hover | ≤ 120 ms |
| Pull: pointer-down → preview tracking | ≤ 50 ms, thereafter 60 fps |
| Release → editable note (caret ready) | ≤ 250 ms |
| Typing latency (keystroke → glyph) | ≤ 16 ms median, ≤ 33 ms p95 |
| Autosave (commit txn) | ≤ 30 ms, async off input path |
| Quick-find first results | ≤ 120 ms over 100k objects |
| Full FTS query | ≤ 300 ms over 100k objects |
| Scroll 10k-row view | 60 fps, no row > 8 ms |
| Thumbnail visible (cached / cold) | ≤ 50 ms / ≤ 400 ms |
| Memory: core+pad idle | ≤ 150 MB |
| Memory: per pinned note | ≤ 25 MB avg, ≤ 40 MB max (20 notes ≤ 600 MB total incl. shared) |
| CPU idle (notes pinned, no interaction) | ≤ 0.5% average, zero periodic timers > 1/min |
| Battery | no wakeups on DC beyond input/sync events |
| LAN sync convergence (1k pending ops) | ≤ 10 s |
| Crash → recovered & interactive | ≤ 5 s, ≤ 1 s of typing lost |

**Virtualization & suspension:** collapsed/occluded/hidden notes drop their WebView to
a suspended state (Win: `ICoreWebView2_3.TrySuspend`; frozen DOM elsewhere) retaining
a last-render bitmap; media pauses when occluded; all list surfaces virtualized;
FTS/thumbnail/metadata work runs at idle priority with backpressure; animations are
compositor-driven (transform/opacity only).

## 30. Failure states and recovery behavior

- **Renderer/WebView crash**: shell detects, restarts the surface, replays editor
  journal; note shows a brief "recovered" chip. Repeated crash (3× in 5 min) → note
  opens in safe text-only mode.
- **Core panic**: process supervisor restarts core; WAL + op watermark guarantee
  consistency; S18 dialog summarizes anything restored from journals.
- **DB corruption**: quick_check fail → auto-restore latest snapshot → replay from
  peers if paired → report what window (if any) was lost. Never open-and-pray.
- **Blob missing/hash mismatch**: placeholder card, re-fetch from peers/relay,
  quarantine mismatched bytes.
- **Disk full**: writes fail into a clear banner ("Pinq can't save — 0 MB free");
  editor keeps journaling to the smallest possible sidecar; blob ingest refuses
  before starting when headroom < 500 MB.
- **Monitor loss** → parked notes (§11). **Shortcut conflicts** → detected at
  registration, non-blocking prompt to rebind.
- **Sync failures**: per-item retry with backoff; poison ops (repeatedly failing to
  apply) quarantined and surfaced ("1 change from Laptop couldn't be applied —
  view details / discard"), never wedging the queue.
- **Relay unreachable / offline**: silent queueing; status chip shows "offline —
  changes saved locally"; nothing degrades functionally.
- **Update failed mid-install**: staged updates with rollback (Tauri updater +
  side-by-side install dirs); DB migration only after update success, preceded by
  snapshot.

## 31. Analytics and privacy-respecting product metrics

Defaults: **all telemetry off**. First-run asks once, plainly, with the full event
list visible. If enabled:
- Strictly no content, titles, file names, URLs, key material, or peer identifiers.
- Events limited to: feature counters (pull-created, click-created, throws, undo-of-
  throw, search performed), latency histograms (locally pre-bucketed), crash minidumps
  (scrubbed, separately opt-in), OS/version/monitor-count coarse device class.
- Random per-install ID, rotatable & resettable in Settings; no cross-linking to
  relay accounts (relay sees only quota identity).
- Local "Insights" page shows the user *their own* stats regardless of upload opt-in
  — telemetry is a copy of what they can already see, building trust.
- KPIs we steer by: pull→keep rate (created notes with content after 60 s), throw-undo
  rate (< 3% target — gesture false-positive proxy), day-7 pad retention, search
  success (result clicked), crash-free sessions (> 99.5%).

## 32. Testing strategy

- **Rust core**: property-based tests (proptest) on op-log/CRDT invariants —
  *convergence*: random op interleavings across N simulated devices must reach
  identical state; fuzzing (cargo-fuzz) on op decoder, importers, EXIF/image paths;
  migration tests over corpus DBs from every released schema.
- **Sync**: deterministic network simulator (latency, partition, reorder, duplicate,
  device clock skew, months-offline replay) in CI; interop matrix Win↔mac↔Linux
  nightly on real VMs.
- **Windowing** (the hard-to-automate layer): a scripted **host harness** per OS that
  creates fake monitor topologies (Win: dev drivers/virtual display; CI fallback:
  recorded topology fixtures), spawns fullscreen test apps (exclusive D3D, borderless,
  video) and asserts pad/note visibility transitions; manual test matrix (checklist
  in repo) for docking/undocking, rotation, DPI 100/150/200 mixed — run before every
  release.
- **UI**: component tests (Vitest + testing-library), Playwright-driven WebView e2e
  for library flows, visual regression on themes/high-contrast/reduced-motion.
- **Gesture feel**: recorded pointer traces (real users, all input types) replayed
  against the classifier — a regression suite for throw/pull thresholds; false-
  positive/negative rates tracked per change.
- **Accessibility**: axe-core in CI on every screen; scripted NVDA/VoiceOver smoke
  runs; keyboard-only e2e path for J5 as a release gate.
- **Performance**: §29 table encoded as benchmarks (criterion for core; tracing
  spans + scripted scenarios for UI) with CI budgets; 100k-object and 50 GB-blob
  seeded profiles.
- **Chaos/recovery**: kill -9 storms during writes, disk-full injection, blob
  truncation, WAL deletion — recovery behavior per §30 asserted, not hoped.
