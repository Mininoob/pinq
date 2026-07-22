# 02 — Interaction Specifications

## 8. Detailed user journeys

### J1 — Interrupt capture (P1, the core loop)
Dana is mid-code-review; a colleague pings "don't forget the 3pm demo env reset."
1. Dana flicks the pointer to the right screen edge; the collapsed pad slides out in
   120 ms.
2. She presses down on the pad and pulls left. At 24 px of travel a note "peels" off
   under the cursor as a live preview.
3. She releases at a clear spot; the note lands, is created, focused, caret ready.
   Elapsed: < 1.5 s, zero window management.
4. She types "reset demo env before 3pm", clicks `[ ]` in the format strip to make it a
   checklist line.
5. At 2:55 she checks the box, grabs the note, and flings it at the right edge. It
   compresses and slides "into" the edge; a toast offers **Undo · Filed to Library**.
6. That evening in the library, Inbox shows the note, checked, with created/filed
   timestamps and `pinned-history: Monitor 2`.

### J2 — Meeting notes with structure (P2)
1. Global shortcut `Ctrl+Alt+N` → quick-capture window with template picker.
2. Kai picks **Meeting note** (template): pre-filled agenda blocks, `Attendees`
   (relation→Person), `Date` (auto-today), `Project` relation.
3. Kai types `@Priya` — mention autocomplete links the existing Person object.
4. Afterward, the note is visible in the "Meetings" saved query, on the Project page's
   inline view, and on Priya's backlink list. No filing step ever happened.

### J3 — Private sync between two machines (P3)
1. On the workstation: Settings → Devices → **Add device** → QR code appears (pairing
   secret + LAN hint).
2. On the laptop: **Join from another device**, scans QR (or types 8-group code).
   Both machines show the same 6-digit safety number; user confirms on both.
3. Initial sync runs over the LAN — progress bar with object/attachment counts. No
   account was created; no relay was contacted.
4. Later, on hotel Wi-Fi, edits sync through the encrypted relay; the sync panel says
   plainly: "Relay can see: that your devices exchanged 214 KB. Relay cannot see: any
   content, titles, or file names."
5. Kai writes down the 12-word recovery phrase shown during setup; Settings nags until
   the phrase is confirmed once.

### J4 — Monitor unplugged (resilience)
Dana undocks her laptop; 7 notes were pinned to the now-gone external monitor. They
vanish from screen but the pad badge shows "7 notes on a disconnected display." The
library's **Pinned** section lists them under "Monitor: Dell U2723 (disconnected)".
Redocking restores all 7 to their exact positions. Alternatively, right-click →
"Bring here" moves them, clamped to visible bounds.

### J5 — Keyboard-only user (accessibility)
1. `Ctrl+Alt+P` focuses the sticky pad (announced: "Sticky pad, Monitor 1, 3 actions").
2. `Enter` creates a note at the default landing position; caret ready. (Equivalent to
   pull.)
3. After editing, `Ctrl+Shift+U` unpins ("Note filed to library. Ctrl+Z to undo.") —
   equivalent to throw. Arrow keys with `Ctrl+Alt` move the focused note in 16 px
   steps; `Shift` for 1 px.

### J6 — Recovering a lost thought
"Where's that recipe link from last month?" → `Ctrl+Alt+F` global quick-find →
types "tahini" → results ranked across titles, body text, bookmark titles, file names
→ `Enter` opens in library; `Ctrl+Enter` re-pins it to the current monitor.

## 9. Sticky-pad interaction specification

### Placement & geometry
- One pad **instance per configured monitor**; each instance has independent: edge
  (left/right/top/bottom), position along edge (persisted as a **fraction 0.0–1.0 of
  usable edge length**, so resolution changes don't strand it), length (120–480 px
  logical, default 240), thickness (visible 8–28 px, default 12; collapsed 3 px),
  opacity (30–100%), color/theme, activation mode.
- Scope setting: all monitors / selected monitors / one monitor. Default: primary only.
- **Usable edge** = monitor work area (Windows `MONITORINFO.rcWork`, macOS
  `NSScreen.visibleFrame`, X11 `_NET_WORKAREA`) minus a 48 px margin at each end. The
  pad never overlaps taskbar/dock/menu bar because it is placed inside the work area;
  on edge conflicts (pad on same edge as taskbar) it sits flush against the work-area
  boundary, not the physical edge. Corner hot zones (Win11 snap, macOS hot corners)
  are avoided by the 48 px end margins.
- All geometry is computed in **logical/DPI-independent units** and converted per
  monitor; on WM_DPICHANGED / screen-parameter events the pad re-anchors from its
  stored fraction.

### States
`hidden → peek (3 px line) → revealed (full thickness) → armed (pointer down) →
pulling (drag in progress) → spawning`. Plus `disabled` (fullscreen app active on that
monitor — pad hides entirely).

- **Hover-reveal mode:** pad renders as a 3 px translucent line; pointer dwelling in an
  activation strip (pad bounds inflated to min 16 px) for 150 ms reveals it (120 ms
  ease-out slide). Leaves after 400 ms grace → collapses. A modifier-free flick through
  the strip does NOT reveal (dwell requirement prevents accidental flashes).
- **Always-visible mode:** full thickness, opacity as configured.
- The pad window itself is a small always-on-top, non-activating (doesn't steal focus),
  frameless window. It never takes keyboard focus except in keyboard mode (J5).

### Pull-to-create: full pointer state machine
1. **pointer-down** on pad → state `armed`. Pad face depresses 1 px (150 ms), cursor
   becomes closed-hand. A press that ends < 150 ms later with < 4 px movement is a
   **click** (see click-to-create mode; otherwise a click toggles reveal/collapse).
2. **drag begins** when movement exceeds the OS drag slop (4 px logical). Direction
   gate: initial movement must be **away from the edge** within ±60°; movement along
   the edge instead slides the pad along the edge (pad repositioning uses the same
   press — along-edge = move pad, off-edge = pull note).
3. **tear threshold** at 24 px perpendicular travel: a live **preview note** (default
   size, 85% opacity, slight sticky-note curl at the pad-side corner, 2° tilt in drag
   direction) tears off and tracks the cursor, offset so the grab point is the note's
   near-edge midpoint. Feedback: 6 ms haptic tick on supporting trackpads/pens
   (macOS `NSHapticFeedbackManager`, pen via Windows Ink where available); a soft
   "peel" sound (optional, off by default); pad face rebounds.
4. **pulling**: preview follows at full frame rate. Snap guides appear near other
   notes/edges (8 px snap). `Esc` at any time cancels.
5. **release**:
   - **≥ 48 px** from the pad edge → **create**: preview animates to 100% opacity and
     0° tilt (90 ms), real note window materializes at that exact rect, editor focused,
     caret blinking. An empty note auto-discards silently if it loses focus for 60 s
     with zero content (undo-able from Inbox for the session).
   - **< 48 px** (released "too close") → **cancel**: preview snaps back into the pad
     with an elastic 150 ms animation communicating "not created." No object is created.
   - **Esc / pointer-cancel / focus loss** (e.g., UAC prompt) → same cancel path.
6. **Initial size & position:** size from the active template (default 280×220 logical;
   remembers per-user last manual resize as the new default). Position = release rect,
   clamped fully inside the work area, nudged 12 px off any exact overlap with an
   existing note (cascade).

### Accidental-creation prevention
- 24 px tear + 48 px release thresholds (both configurable in Accessibility settings).
- Direction gate (±60° away from edge).
- Empty-note auto-discard.
- The activation strip never spans OS-reserved corners.

### Alternative inputs
- **Click-to-create (setting, default off; auto-suggested when OS reports assistive
  pointer settings):** single click on pad creates a note at the default landing
  position — no drag needed. Long-press (500 ms) opens the template picker.
- **Keyboard:** `Ctrl+Alt+P` focus pad → `Enter` create · `T` template picker ·
  arrows move pad along edge · `1–9` jump between monitors' pads. All announced via
  the accessibility tree.
- **Touch:** same state machine; tear threshold raised to 32 px, release threshold to
  64 px (fatter pointers); touch-and-hold 350 ms as alternative to reveal-dwell. Pad
  minimum touch thickness 20 px when a touchscreen is present.
- **Stylus:** identical to mouse; pressure ignored for v1; barrel button = template
  picker.

## 10. Throw-to-unpin gesture specification

### Intent detection (conceptual algorithm)
While a pinned note is being dragged, keep a 100 ms rolling window of pointer samples.
On release compute:
- `v` = mean velocity over the last 80 ms (logical px/s), and its direction θ.
- `d_edge` = distance from the note's leading edge to the nearest screen edge along θ.
- Projected rest point `P = release_pos + v·k` (k = 180 ms of coasting).

Classify as **throw** only if ALL hold:
1. `|v| ≥ 1500 logical px/s` (configurable 1000–2500; "off" disables throwing).
2. θ is within ±35° of the outward normal of the edge being approached.
3. `P` lies ≥ 40% of the note's width/height beyond the screen edge (i.e., the note
   would coast mostly off-screen).
4. Total drag distance this gesture ≥ 80 px (a twitchy 5 px flick never throws).
5. The gesture wasn't started from a resize handle or a control (checkbox, link).

Otherwise it is a normal move: the note coasts briefly with friction and stops,
clamped inside the work area (a fast-but-not-throw drag never leaves the screen).

Edges adjacent to another monitor in the virtual desktop arrangement are **not throw
edges by default** (a fast drag there is monitor-to-monitor movement); a setting can
enable "throw through interior edges" with threshold ×1.5.

### Throw sequence
1. Classification succeeds at pointer-up → note continues along θ with slight
   acceleration, scales to 92%, rotates ≤ 4° toward the edge, and slides past the edge
   with an opacity fade over 220 ms — reading as *filed into the edge*, not popped or
   shattered. Reduced-motion: 90 ms straight fade, no rotation.
2. The object is atomically updated: `pinned=false`, desktop placement snapshot saved
   (monitor, rect, z, collapsed) into `lastPlacement` for future re-pin. It is **not**
   archived, **not** trashed; it appears in Library → Unpinned and Inbox/Recents.
3. A toast appears bottom-center of that monitor for 6 s: "**Filed to Library** —
   *Undo* (Ctrl+Z)". Undo restores the window at its pre-throw rect with a reverse
   animation. `Ctrl+Z` works while any Pinq surface has focus; the toast is
   screen-reader announced.
4. The pad on that edge gives a subtle 80 ms "absorb" pulse (skipped under
   reduced-motion).

### Cancellation & safety
- `Esc` mid-drag cancels coasting and returns the note to its drag-start rect.
- Locked notes cannot be thrown (they can't be dragged).
- A throw during multi-select drags files all selected notes (one toast, one undo).

### Accessible equivalents (always available)
- **Unpin** in the note's `⋯` menu and context menu.
- `Ctrl+Shift+U` on a focused note.
- Library: select notes → Unpin button (batch).
These are the *primary documented* path; the throw is the delightful shortcut.

## 11. Desktop window and fullscreen behavior

### Window model
Each pinned note = one frameless, transparent-cornered OS window (rounded 8 px,
drop shadow). Notes are **non-activating on hover**, activate on click. Z-order among
notes is user-managed (click brings to front within the note layer); the note layer
sits above normal windows, below system UI (menus, IME, notifications).

Per-note commands: pin/unpin · lock (position+size+content read-only overlay toggle) ·
minimize-to-pad (a small chip on the pad edge) · collapse to title bar (28 px) ·
resize (min 160×120, max = work area) · move · duplicate · archive · close (= unpin;
never delete) · color · opacity (per-note override) · click-through toggle (V1).

Global: `Ctrl+Alt+H` hide/show all notes (hidden state persists across restart if set)
· `Ctrl+Alt+N` new note · `Ctrl+Alt+F` quick find · `Ctrl+Alt+P` focus pad. All
global shortcuts rebindable and pre-flighted for conflicts.

### Always-on-top and fullscreen: per-OS truth

**Windows.**
- Above normal windows: `WS_EX_TOPMOST`. Reliable.
- Fullscreen detection: poll + event mix — `SHQueryUserNotificationState`
  (`QUNS_BUSY`/`QUNS_RUNNING_D3D_FULL_SCREEN`/`QUNS_PRESENTATION_MODE`) plus a
  foreground-window geometry check (`GetForegroundWindow` rect == monitor rect and
  style lacks `WS_CAPTION`) hooked on `EVENT_SYSTEM_FOREGROUND` via `SetWinEventHook`.
  **Limitation (documented, not hidden):** borderless-fullscreen games are
  indistinguishable from borderless-maximized apps by geometry alone; exclusive
  fullscreen usually covers topmost windows anyway. Heuristic + user setting below.
- Virtual desktops: `IVirtualDesktopManager` is officially limited (can query/move own
  windows; enumerating desktops uses undocumented COM — we will NOT depend on
  undocumented APIs; "show on all desktops" is implemented with the documented
  pin-window capability where available, else the option is hidden).
- Per-monitor DPI v2 awareness; handle `WM_DPICHANGED`, `WM_DISPLAYCHANGE`.

**macOS.**
- Above normal windows: `NSWindow.level = .floating`.
- Fullscreen: apps enter fullscreen into a **separate Space**; floating windows on the
  desktop Space simply aren't there — largely solves itself. Detect via
  `NSWorkspace.activeSpaceDidChangeNotification` + `CGWindowListCopyWindowInfo` to
  hide chips/pad on fullscreen Spaces. `collectionBehavior = [.canJoinAllSpaces]` for
  "all desktops" (works, documented). Exclusive-fullscreen games: same Space
  mechanics apply.
- Requires Screen Recording permission only if we read other windows' titles — we
  don't; geometry-level info suffices without that permission on current macOS, but
  this is fragile across releases (**uncertainty flagged**).

**Linux.**
- X11: `_NET_WM_STATE_ABOVE` for topmost; `_NET_WM_STATE_FULLSCREEN` on the active
  window (via EWMH `_NET_ACTIVE_WINDOW`) for detection; `_NET_WM_DESKTOP=0xFFFFFFFF`
  for all-desktops. Workable and testable.
- Wayland: arbitrary always-on-top is **not available to normal clients by design**.
  `wlr-layer-shell` gives edge-anchored layers (perfect for the pad, usable for notes)
  on wlroots compositors (Sway, Hyprland) and KDE; **GNOME does not support
  layer-shell** → on GNOME/Wayland, notes are normal windows and the pad falls back to
  a thin docked window without over-layering. Fullscreen detection is also restricted.
  This is documented in-product ("Your desktop environment limits floating notes").

### Borderless-maximized handling (user setting, per the requirement)
Setting "When an app covers the whole screen": **Hide notes** (treat like fullscreen,
default for detected games via a small process-category heuristic) / **Keep notes
visible** / **Per-app remembered choice** (first detection shows a one-time
non-blocking prompt on that monitor's pad).

### Immersive-context protections
- Presentation/Do-not-disturb (Windows `QUNS_PRESENTATION_MODE`, macOS Focus where
  queryable): auto-hide notes + pad.
- Screen sharing: where detectable (macOS `SCContentSharingState`/window capture
  exclusion via `sharingType = .none`; Windows `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`)
  notes can be **excluded from capture** (setting: exclude / hide / do nothing;
  default exclude). Linux: not reliably available — documented.
- Media/games: covered by fullscreen + borderless rules above.

### Persistence & monitor-change resilience
Persisted per note: monitor **stable ID** (EDID+adapter hash, not index), rect in
logical px relative to that monitor's work area, z-rank, collapsed/locked/opacity,
virtual-desktop preference. On restart/display-change:
1. Match monitor by stable ID → restore exact rect (clamped to current work area).
2. Monitor missing → note enters **parked** state (not shown; badge on pad + Library
   section "On disconnected display"), auto-restores on reconnect.
3. Same monitor, new resolution/rotation → rescale anchor point proportionally, clamp,
   ensure ≥ 60×40 px of the title area is visible and grabbable; else recenter.
4. A note whose saved rect is invalid for any other reason is restored to the primary
   monitor's cascade origin. Never silently dropped.
