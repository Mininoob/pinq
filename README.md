# Pinq

**Sticky notes that live on your desktop, backed by a real knowledge base — local-first, end-to-end encrypted, no account, ever.**

A slim colored pad docks to your screen edge. Pull on it and a sticky note tears off under your cursor. Throw the note to file it, toss it at an edge to tuck it into a tab, and everything you write becomes part of a searchable, linkable, syncable library that never leaves your machines.

## The five moves

| Gesture | What happens |
|---|---|
| **Pull** the pad | A fresh note tears off under your cursor, caret ready |
| **Throw** a note hard | It files itself into the Library (with an Undo toast) |
| **Toss** at a screen edge | It tucks into a slim tab — hover to peek, pull to restore |
| **Hold** the pad | Menu card: Library, Settings, quick todo / meeting templates |
| `Ctrl+Alt+N / F / H` | New note · quick find · hide everything (remappable in Settings) |

## What's inside

- **Canvas notes** — free-form text boxes that never soft-wrap (the note grows with your line), checklists, paste/drop images and files up to 5 GB, markdown accents, per-note color & paper.
- **Tasks** — any sticky can carry a status (todo → doing → done), a due date with one-click reminder, and a checklist progress bar. Reminders bring the note back to your desktop when they fire.
- **Library** — card grid with Inbox / Favorites / On-desktop / Archive views, multi-select batch actions, saved searches, and filter tokens (`tag:x is:overdue has:file due:week before:2026-01-01`) on top of full-text search.
- **Links & graph** — `[[Wiki links]]` with autocomplete, backlinks, rename propagation, and a zero-dependency force-directed link map.
- **On-device AI** — a small embedding model (runs locally, ~25 MB, optional) suggests tags, titles, and related notes. Nothing about your notes ever leaves the machine.
- **Sync** — device-to-device over LAN (mDNS discovery, SPAKE2 pairing with safety numbers) or through a self-hostable end-to-end-encrypted relay. Live co-editing when the same note is open on two machines; concurrent offline edits are *merged*, never silently dropped. Layout (pins, tucks, positions) stays per-device; content syncs.
- **Trust** — a 24-word recovery phrase IS your space key; backups are encrypted `.pinqbak` archives that only that phrase unlocks; a signed device registry with revocation and key rotation.
- **Desktop citizenship** — notes hide over fullscreen apps, keep per-virtual-desktop homes on Windows, follow Spaces when tucked on macOS, tray + menu bar presence, launch at login, single-instance.

## Install

Grab the installer for your platform from [Releases](../../releases): `.msi` / `-setup.exe` on Windows, `.dmg` on macOS (unsigned for now — first launch needs right-click → Open, or `xattr -cr Pinq.app`). In-app updates check the release feed from Settings → System.

## Build from source

Prereqs: Rust (stable), Node 20+, and on Windows the WebView2 runtime (ships with Windows 11).

```bash
npm ci
npm run tauri dev     # run with hot reload
npm run tauri build   # produce installers in src-tauri/target/release/bundle/
```

Tests: `cargo test --workspace` from `src-tauri/`, `npx tsc --noEmit` for the frontend.

## Data & privacy

Everything lives in your OS app-data directory: an append-only op-log + SQLite projection (`pinq.db`), content-addressed attachment blobs, and rotating daily DB backups. Sync peers exchange only end-to-end-encrypted frames; the relay stores sealed envelopes it cannot read. There is no account, no telemetry, and no cloud.

**Write down your recovery phrase** (Settings → Sync → Recovery). It is the only way to read your encrypted backups or rejoin your space if every device is lost.

## Releasing (maintainers)

1. Bump `version` in `Cargo.toml` (workspace), `src-tauri/tauri.conf.json`, and `package.json`.
2. Commit, tag `vX.Y.Z`, push the tag.
3. CI builds both platforms, signs the updater artifacts (`TAURI_SIGNING_PRIVATE_KEY` secret), and publishes a draft release with `latest.json`. Review and publish it.

The full product blueprint lives in [`docs/spec/`](docs/spec/).
