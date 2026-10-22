# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

(This record governs the public website in `site/`. The product itself is a desktop app — Tauri 2, Windows and macOS — whose UI lives in `src/`.)

## Stack

`site/`: Vite + TypeScript, no framework, static output, deployed on Vercel (Root Directory = `site`). Chosen by Josh.

## Users

1. **Everyday desktop people (primary).** Anyone who keeps stickies, lists, reminders, and half-thoughts on their screen and wants that habit to feel nicer and stop losing things. They decide in seconds, want to see it work, and want a download button.
2. **Note-taking / local-first fans and open-source contributors (secondary).** People who use linked-note tools and care about wiki links, search, sync, encryption, data ownership, and reading the source.

## Product Purpose

Pinq is a free, open-source desktop sticky-note app with a real knowledge base underneath. The website exists so a visitor understands the gestures, believes the notes stay theirs, and downloads it; contributors find the code and the license.

## Positioning

You physically *pull* a note off a pad docked to your screen edge and *throw* it away to file it — a tactile gesture no other notes app has — and every note quietly becomes part of a searchable, linked, synced, end-to-end-encrypted library that lives on your own machines. No account, ever.

## Operating Context

The app lives on the desktop: a colored pad on a screen edge; notes float over other apps, hide over fullscreen apps, tuck into edge tabs, and come back on reminders. The library window holds everything filed. Devices sync over the local network or a self-hostable encrypted relay.

## Capabilities and Constraints

Shipped in v0.2.0 (the only claims the site may make):
- Gestures: pull a note off the pad, throw to file (with undo), toss at an edge to tuck, press-and-hold the pad for the menu; Ctrl+Alt+N / F / H (remappable).
- Canvas notes: text boxes that break only at Enter (the note widens instead of wrapping), checklists, markdown accents, paste/drop images and files up to 5 GB, per-note colors and paper.
- Tasks: status todo → doing → done, due dates, reminders that bring the note back, checklist progress; quick todo / meeting templates.
- Library: Inbox, Favorites, On desktop, Archive views; multi-select batch actions; saved searches; filter tokens on full-text search.
- `[[Wiki links]]`, backlinks, rename propagation, a link-map graph.
- Optional on-device AI suggestions (local ~25 MB model) for tags, titles, related notes.
- Sync: LAN pairing with safety numbers, live co-editing, optional self-hosted end-to-end-encrypted relay; concurrent offline edits merge instead of overwriting.
- Trust: 24-word recovery phrase, encrypted `.pinqbak` backups, device revocation and key rotation.
- Signed in-app updates, launch at login, tray / menu bar.

Constraints: Windows x64 and macOS Apple Silicon builds exist; the macOS build is **unsigned** (first launch needs right-click → Open). Downloads come from GitHub Releases (`Mininoob/pinq`). License: **MIT**. Linux is untested; mobile does not exist.

## Brand Commitments

- Name **Pinq**; mascot = rounded yellow sticky (#FFD45C) with a smiley face and a **coral (#FF6F6C) peeling corner**; ink #242322; cream #FFF8EE. Assets: `src/assets/icons/pinq-icon-pack/`.
- Gesture vocabulary: **pull, throw, tuck, hold**.
- Website references Josh made binding: the landing page opens on a sticky pad whose top note **rips off and travels down the page**, transforming into the things you can make in a note (scroll grammar after a pinned-hero-object reference); aesthetic cues from **cooldock.app** (calm, warm, heavy confident headlines, pill navigation, generous space).

## Evidence on Hand

- The real app and its UI (`src/`), the icon pack, the v0.2.0 release with real installers, the README feature list.
- **None of the following exist and must never be fabricated:** testimonials, user or download counts, awards, press, benchmarks, customer logos, pricing (it's free).

## Product Principles

1. **Show the gesture, don't describe it.** Every claim is demonstrated by a note doing the thing.
2. **Only true claims.** If v0.2.0 doesn't do it, the site doesn't say it.
3. **Your notes are yours.** Local-first, encrypted, no account — stated plainly, never as marketing fog.
4. **Delight first, depth on request.** Everyday visitors get the joy and the download; power users and contributors find the depth further down.

## Accessibility & Inclusion

Respect `prefers-reduced-motion` with a fully static, complete alternative to the scroll animation; all content readable without JavaScript; keyboard-reachable navigation and download links.
