# 03 — Main Application, Screens, Wireframes

## 12. Main application information architecture

```
Pinq (main window)
├── Quick actions bar: New note ▾ (template) · Quick find · Sync status chip
├── Sidebar
│   ├── Inbox            (recent + unfiled captures)
│   ├── All notes
│   ├── Desktop          (pinned, grouped by monitor; shows disconnected displays)
│   ├── Unpinned
│   ├── Favorites
│   ├── ── widgets zone ── (user-configurable, V1: tasks due, tag lists, queries…)
│   ├── Types            (V1: Note, Task, Person, Project, custom…)
│   ├── Collections      (V1)
│   ├── Queries          (V1, saved)
│   ├── Tags
│   ├── Calendar         (V1)
│   ├── Graph            (V1)
│   ├── Attachments      (V1 browser; MVP: simple list)
│   ├── Templates        (V1)
│   ├── Archive
│   └── Trash            (30-day retention, restore/purge)
├── Content pane: list/table/gallery/board/calendar of current source (tabs in V1)
├── Detail pane / editor (or opens as tab / separate window)
└── Settings: General · Sticky pad (per monitor) · Notes & appearance · Shortcuts ·
    Types & properties (V1) · Sync & devices (V1) · Storage · Import/Export ·
    Privacy & security · Accessibility
```

Key IA rules:
- **Inbox** = objects created in the last 14 days that are in no collection and have no
  type other than Note — the "process me" list. Items leave Inbox by aging out, being
  tagged/typed/collected, or being archived.
- **Desktop** is a live mirror of pinned state; drag a note row onto a monitor
  thumbnail to pin it there ("pin to monitor" also in context menu with a monitor
  picker showing named displays).
- Every list supports multi-select and batch: pin/unpin (with monitor picker), tag,
  archive, move to collection, export, delete-to-trash.
- Search (`Ctrl+F` in-app, `Ctrl+Alt+F` global): one field; scope chips (All / current
  view); filters appear as removable tokens (tag:, type:, is:pinned, monitor:, has:file,
  before:/after:). MVP searches title+body+tag+filename; V1 adds properties, link
  URLs/titles, and extracted attachment metadata.

## 13. Screen and window inventory

| # | Surface | Kind | Phase |
|---|---|---|---|
| S1 | Sticky pad (per monitor) | tiny always-on-top non-activating window | P |
| S2 | Desktop note | frameless always-on-top window | P |
| S3 | Note preview during pull/throw | ephemeral layered window | P |
| S4 | Toast/undo | per-monitor ephemeral window | P |
| S5 | Main window (sidebar+list+detail) | standard window | M |
| S6 | Quick find | centered ephemeral palette window | M |
| S7 | Quick capture (global shortcut) | small centered window with template picker | M |
| S8 | Settings | window/section of S5 | M |
| S9 | Template picker | popover | M(basic)/1 |
| S10 | Media viewer (image zoom/fullscreen, player) | window overlay | M(image)/1(av) |
| S11 | Pairing wizard (QR, safety number, recovery phrase) | modal flow in S5 | 1 |
| S12 | Conflict review | panel in S5 | 1 |
| S13 | Note history / version restore | panel | 1 |
| S14 | Database/view editor (filters, sorts, groups) | panel | 1 |
| S15 | Type & property editor | settings section | 1 |
| S16 | Graph view | tab in S5 | 1 |
| S17 | Onboarding (first-run: pick edge, try pull, try throw) | guided overlay | M |
| S18 | Crash-recovery dialog ("restored 3 unsaved changes") | dialog | M |
| S19 | Storage manager (usage by kind, cache purge) | settings section | 1 |
| S20 | Import/export wizards | modal flows | M |

## 14. Text wireframes (most important screens)

### S5 — Main window (MVP)
```
┌────────────────────────────────────────────────────────────────────────────┐
│ [＋ New note ▾]  [🔍 Search notes…                    ]        [⟳ synced ✓] │
├──────────────┬─────────────────────────────────────────────────────────────┤
│ Inbox     12 │  All notes                       [Sort: Modified ▾] [⋮ view] │
│ All notes    │  ┌───────────────────────────────────────────────────────┐  │
│ Desktop    5 │  │ ◻ Demo env reset      #ops     📌 Mon 2   2:41 pm     │  │
│  · Mon 1   2 │  │   reset demo env before 3pm …                         │  │
│  · Mon 2   3 │  ├───────────────────────────────────────────────────────┤  │
│ Unpinned     │  │ ◻ Tahini pasta        #recipe  🔗1 🖼2    Jun 30      │  │
│ Favorites    │  ├───────────────────────────────────────────────────────┤  │
│ Tags       ▾ │  │ ◻ Q3 planning ideas   #work    📎1        Jun 28      │  │
│  #ops        │  └───────────────────────────────────────────────────────┘  │
│  #recipe     │   ── 3 selected: [📌 Pin ▾] [🏷 Tag] [🗄 Archive] [🗑]      │
│ Archive      ├─────────────────────────────────────────────────────────────┤
│ Trash      3 │  ▸ Detail: "Demo env reset"   [📌 Pinned: Monitor 2] [⋯]    │
│──────────────│    ☑ reset demo env before 3pm                              │
│ ⚙ Settings   │    created 2:38 pm · filed 2:55 pm · #ops                   │
└──────────────┴─────────────────────────────────────────────────────────────┘
```

### S2 — Desktop note
```
┌ ⣿ ───────────────────────── ⋯ ─ ┐   ⣿ drag handle (whole header drags)
│ Demo env reset                  │   ⋯ menu: Pin/Unpin·Lock·Collapse·Color·
│ ☑ reset demo env before 3pm     │      Duplicate·Archive·Open in Library
│ next: notify #release channel_  │   corners/edges: resize
│                                 │   footer appears on hover:
│ B i ☐ 🔗 📷 ＋        #ops  ⤢  │   format strip · tag chip · expand
└─────────────────────────────────┘
```

### S6 — Quick find (global)
```
┌──────────────────────────────────────────────┐
│ 🔍 tahini▌                                   │
│ ──────────────────────────────────────────── │
│ 📄 Tahini pasta            #recipe   Jun 30  │  Enter: open in Library
│ 📄 Grocery run             "…tahini…" Jun 12 │  Ctrl+Enter: pin to this monitor
│ 🔗 "Tahini 101" bookmark   in: Reading list  │  Tab: filter tokens
└──────────────────────────────────────────────┘
```

### S11 — Pairing wizard (V1), step 2 of 4
```
┌ Add a device ────────────────────────────────────┐
│  Scan this code with Pinq on your other device,  │
│  or type the code shown below it.                │
│            ┌─────────────┐                       │
│            │  ▓▓ QR ▓▓   │                       │
│            └─────────────┘                       │
│   words:  crane-topaz-orbit-manor-…  (8 groups)  │
│                                                  │
│  Both devices will show a safety number.         │
│  Continue only if the numbers match.             │
│                                  [Cancel] [Next] │
└──────────────────────────────────────────────────┘
```

### S14 — View editor (V1), Board view over query "type:Task"
```
┌ Tasks · Board ──────────────────────────────── [＋ New view] [⚙ Edit view] ┐
│ Filter: type is Task AND status is not Done   Group by: Status   Sort: Due │
│ ┌ Todo ──────────┐ ┌ Doing ─────────┐ ┌ Blocked ────────┐                 │
│ │ ▢ Ship pad v2  │ │ ▢ Fix DPI bug  │ │ ▢ Relay TLS     │   [＋ card]     │
│ │   due Jul 18   │ │   due Jul 16 ⚠ │ │   @waiting-infra│                 │
│ └────────────────┘ └────────────────┘ └─────────────────┘                 │
└────────────────────────────────────────────────────────────────────────────┘
```
