---
name: Pinq (website)
description: The public launch site for Pinq, a pad of sticky notes docked to the edge of your desktop.
colors:
  ground: "#ecebe6"
  raised: "#faf9f6"
  library-sidebar: "#f4f2ee"
  ink: "#242322"
  ink-hover: "#0f0f0e"
  ink-2: "#57534d"
  ink-3: "#6e6962"
  line: "rgba(36, 35, 34, 0.12)"
  line-2: "rgba(36, 35, 34, 0.2)"
  paper: "#ffd45c"
  paper-hover: "#ffdc76"
  paper-edge: "#f3c446"
  paper-done: "#f6d572"
  coral: "#ff6f6c"
  coral-deep: "#dc4d4a"
  link: "#9e2522"
  note-pinq: "#fff8ee"
  note-amber: "#fdf0a4"
  note-mint: "#d9f7e8"
  note-sky: "#d9edfd"
  note-rose: "#fddce7"
  note-violet: "#e6defc"
  note-graphite: "#e5e7eb"
  band-text: "#f4f1ec"
  band-text-2: "#c9c3ba"
  band-code: "#2f2d2b"
  band-comment: "#a39d94"
typography:
  display:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "clamp(3rem, 5.1vw, 5.5rem)"
    fontWeight: 860
    lineHeight: 0.95
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "clamp(2.5rem, 5.4vw, 5rem)"
    fontWeight: 860
    lineHeight: 0.98
    letterSpacing: "-0.04em"
  headline-chapter:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "clamp(2.1rem, 3.5vw, 3.35rem)"
    fontWeight: 820
    lineHeight: 1.02
    letterSpacing: "-0.035em"
  title:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "19px"
    fontWeight: 700
    lineHeight: 1.3
    letterSpacing: "-0.015em"
  note-title:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "18px"
    fontWeight: 700
    letterSpacing: "-0.015em"
  lede:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "1.1rem"
    fontWeight: 400
    lineHeight: 1.55
  body:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.55
    fontFeature: "\"ss01\", \"cv11\""
  note-body:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "15.5px"
    fontWeight: 400
    lineHeight: 1.45
  button:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "15.5px"
    fontWeight: 600
    lineHeight: 1
    letterSpacing: "-0.01em"
  label-spec:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 650
    letterSpacing: "0.09em"
  label-pill:
    fontFamily: "Geist, ui-sans-serif, system-ui, sans-serif"
    fontSize: "12.5px"
    fontWeight: 600
  mono:
    fontFamily: "Geist Mono, ui-monospace, monospace"
    fontSize: "14.5px"
    fontWeight: 400
    lineHeight: 1.75
rounded:
  pill: "999px"
  note: "13%"
  note-figure: "9%"
  checkbox: "5px"
  inline: "6px"
  tile: "9px"
  field: "11px"
  card: "14px"
  code: "20px"
  window: "26px"
  desk: "28px"
spacing:
  gutter: "clamp(20px, 4vw, 56px)"
  max-width: "1240px"
  chip-gap: "8px"
  action-gap: "10px"
  chapter-gap: "48px"
  section-y: "12vh"
  section-y-lg: "16vh"
components:
  button-ink:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.raised}"
    typography: "{typography.button}"
    rounded: "{rounded.pill}"
    padding: "0 22px"
    height: "48px"
  button-ink-hover:
    backgroundColor: "{colors.ink-hover}"
  button-ghost:
    backgroundColor: "rgba(36, 35, 34, 0.06)"
    textColor: "{colors.ink}"
    typography: "{typography.button}"
    rounded: "{rounded.pill}"
    padding: "0 22px"
    height: "48px"
  button-ghost-hover:
    backgroundColor: "rgba(36, 35, 34, 0.11)"
  button-paper:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    typography: "{typography.button}"
    rounded: "{rounded.pill}"
    padding: "0 22px"
    height: "48px"
  button-paper-hover:
    backgroundColor: "{colors.paper-hover}"
  button-sm:
    rounded: "{rounded.pill}"
    padding: "0 16px"
    height: "38px"
  button-lg:
    rounded: "{rounded.pill}"
    padding: "0 28px"
    height: "58px"
  chip:
    backgroundColor: "rgba(250, 249, 246, 0.6)"
    textColor: "{colors.ink-2}"
    rounded: "{rounded.pill}"
    padding: "0 13px"
    height: "32px"
  nav:
    backgroundColor: "rgba(250, 249, 246, 0.8)"
    textColor: "{colors.ink-2}"
    rounded: "{rounded.pill}"
    padding: "6px 6px 6px 14px"
  legend-pill:
    textColor: "{colors.ink-3}"
    typography: "{typography.label-pill}"
    rounded: "{rounded.pill}"
    padding: "0 10px"
    height: "30px"
  legend-pill-active:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.raised}"
  status-pill-done:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.paper}"
    rounded: "{rounded.pill}"
    padding: "0 10px"
    height: "22px"
  sticky-note:
    backgroundColor: "{colors.paper}"
    textColor: "{colors.ink}"
    typography: "{typography.note-body}"
    rounded: "{rounded.note}"
    padding: "7% 8% 9%"
  library-window:
    backgroundColor: "{colors.raised}"
    rounded: "{rounded.window}"
  library-row-active:
    backgroundColor: "rgba(255, 212, 92, 0.42)"
    textColor: "{colors.ink}"
    rounded: "{rounded.tile}"
    padding: "8px 12px"
  library-card:
    backgroundColor: "{colors.note-pinq}"
    textColor: "{colors.ink}"
    rounded: "{rounded.card}"
    padding: "16px"
  fan-note:
    backgroundColor: "{colors.note-pinq}"
    textColor: "{colors.ink}"
    rounded: "{rounded.note}"
    padding: "14px"
  ink-band:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.band-text}"
    padding: "14vh 0"
  code-block:
    backgroundColor: "{colors.band-code}"
    textColor: "{colors.band-text}"
    typography: "{typography.mono}"
    rounded: "{rounded.code}"
    padding: "26px 28px"
  toast:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.raised}"
    rounded: "{rounded.pill}"
    padding: "0 18px"
    height: "44px"
---

# Design System: Pinq (website)

Scope: this system governs the public website in `site/` only. The desktop app's own interface (`src/`) is a separate, older incumbent system and is not documented here; do not import its tokens into the site, or this site's tokens into the app, without a deliberate decision. The one shared source is the brand itself (the mascot sticky, its yellow paper and coral peel, the ink, and the app's seven real note palettes).

## Overview

**Creative North Star: "The Pad You Pull From"**

The page is a pad of sticky notes. A calm, warm stone ground carries heavy ink headlines at poster scale, and the only colour on it is paper: a yellow sticky with a coral peeling corner, and, further down, the app's real pastel note colours. One sheet tears off the hero pad as the visitor scrolls, travels the page, and becomes each capability in turn by doing it, then lands in the library. Everything else (pill nav, pill buttons, hairline spec strips) is quiet stationery that stays out of the note's way.

Density is generous: one idea per viewport, short copy columns (about 30rem), big vertical rests measured in viewport height. Paper is rendered as a lit material, not a flat fill: a soft inner glow from the upper left, a faint warm settle at the bottom, and a fine grain tooth. Depth comes from soft, low-opacity ink shadows that pool beneath objects; nothing on the page has a hard edge shadow.

The site refuses the category default of a centred headline over an app screenshot and a three-card feature grid: features are demonstrated by the note itself.

**Key Characteristics:**
- Warm stone ground, ink text, colour reserved for notes.
- Geist at 820 to 860 weight with tight negative tracking for every heading.
- Pills everywhere for interface chrome; squares (13% corner) for notes.
- Paper as material: glow, settle, grain, curl, torn edge.
- One signature scroll interaction (rip, travel, six morphs, land), with a complete static equivalent.

## Colors

A near-neutral warm stone and ink system in which every saturated colour is a piece of paper.

### Primary
- **Pad Yellow** (paper): the sticky itself. The hero pad, the traveling note, chapter figures, the landed library card, the first fan note, the paper pill button on the ink band, text selection, and translucent highlights (active library row at 42%, search token at 45%). Its hover is a lighter yellow (paper-hover); a completed task desaturates the sheet to paper-done.

### Secondary
- **Peel Coral** (coral): the curling corner of the sticky, drawn as a gradient from a pale coral highlight through coral to coral-deep. Also the "current chapter" square in the note's ledger.
- **Deep Coral** (coral-deep): the darker fold of the peel and the typing caret.
- **Overdue Red** (#b83431): the overdue due-date chip only, darker than the peel so its white 12px text clears 4.5:1.
- **Wiki-Link Red** (link): resolved `[[links]]` inside a note, bold with a dotted underline. Used only on paper.

### Tertiary
- **The seven note palettes** (note-pinq cream, note-amber, note-mint, note-sky, note-rose, note-violet, note-graphite): the app's real note colours, used on the site only for notes shown as notes (library cards, the closing six-note fan, the linked mini-notes). Cool pastel notes take the neutral grain, not the warm one.

### Neutral
- **Stone Ground** (ground): page background, scrollbar track, the mobile stage band. Warm stone, never cream.
- **Raised Paper White** (raised): the library window surface, text on ink pills, the nav glass base.
- **Sidebar Stone** (library-sidebar): the library window's sidebar.
- **Ink** (ink): all headings and primary text, ink buttons, active legend pill, toast, the one dark band. Hover darkens to ink-hover.
- **Ink 2** (ink-2): body copy, ledes, chip text, nav links, spec strip labels.
- **Ink 3** (ink-3): inactive legend numbers, counts, sidebar group label. Lowest text tone allowed on the ground.
- **Hairlines** (line, line-2): borders and rules. line-2 for spec strips, facts and FAQ rules; line for section dividers, chips and the library frame.
- **Ink band tones** (band-text, band-text-2, band-code, band-comment): text, lede, code panel and code comment inside the open-source band only.

### Named Rules
**The Colour Means a Note Rule.** Saturated colour appears only as paper (a note, a note's peel, or a note-colour highlight). Chrome, sections and text stay in ground, raised and ink. If a coloured element is not a note or part of one, it is wrong. The single exception is the hero desk frame's one atmospheric wallpaper photograph, which is the desktop the pad is docked to (with a calm wash so paper reads against it).

**The One Ink Band Rule.** The page has exactly one full-bleed ink section (open source). Inside it the paper button is the accent and focus rings switch to paper.

### Dark Theme
The page follows the system setting until the visitor uses the theme toggle; their choice is saved under `pinq-theme` and applied by an inline head script before first paint. Dark only re-points the neutral tokens: ground #181715, raised #22201d, sidebar #1d1b19, ink #eeeae3, ink-2 #b5aea3, ink-3 #958e83, hairlines in white at 10% and 18%, and the ink band drops to #0e0d0c so it still reads as the one deeper section. Paper never changes: every note, the paper button and the desk hint re-point their ink to the fixed paper-ink tones (#242322, #57534d, #6e6962), so notes look the same in both themes. Labels and wires beside a note (device names, link lines) keep the page ink, not the paper ink.

**The Paper Stays Paper Rule.** Dark mode darkens the room, not the notes. No note colour, grain or peel gets a dark variant.

## Typography

**Display Font:** Geist (variable, self-hosted, with ui-sans-serif, system-ui fallback)
**Body Font:** Geist
**Label/Mono Font:** Geist Mono, only for code, search queries and filter tokens

**Character:** One family carries everything. Headings are Geist at near-black weights (820 to 860) with tight negative tracking, set at poster scale; body is the same face at 400 in a warm grey ink with stylistic sets ss01 and cv11 enabled.

### Hierarchy
- **Display** (860, clamp(3rem, 5.1vw, 5.5rem), 0.95): the hero headline only; on desktop each line is held unbroken. Mobile drops to clamp(2.6rem, 11vw, 3.4rem).
- **Headline** (860, clamp(2.5rem, 5.4vw, 5rem), 0.98): section headlines (library, local-first, open source, download, FAQ), max 15ch.
- **Headline, chapter** (820, clamp(2.1rem, 3.5vw, 3.35rem), 1.02): the six journey chapter headings.
- **Title** (700, 19px, -0.015em): fact terms and FAQ questions (questions at 650).
- **Note title** (700, 18px): titles written on a note.
- **Lede** (400, 1.1rem): one paragraph under a section headline, max 40rem, ink-2.
- **Body** (400, 17px, 1.55): copy columns at 30 to 34rem, ink-2.
- **Note body** (400, 15.5px, 1.45): text on a note; secondary note text 13 to 13.5px.
- **Label, spec** (650, 12px, 0.09em, uppercase): the chapter spec strip and the library sidebar group label. Numbers inside are tabular, 13px, ink.
- **Label, pill** (600, 12.5px): legend, sync lock, chips (500, 13.5px).

### Named Rules
**The Heavy Ink Rule.** Every heading is Geist 820 or heavier with tracking between -0.035em and -0.04em, and text-wrap balance. No light or medium display weights.

**The Mono-Free Labels Rule.** Stationery labels (spec strips, legend, ledger) are Geist small uppercase with hairlines, never monospace. Geist Mono is reserved for literal code, queries and filter tokens.

## Layout

A single centred column capped at 1240px with a fluid gutter (clamp(20px, 4vw, 56px)). The hero is a two-column grid (1.14fr / 1fr, capped at 1440px) of copy and a rounded desktop frame; it sits sticky inside a 165vh run so the rip plays while the hero holds. The journey is a two-column grid of six chapters (each at least 100vh, centred, 48px gap) beside a sticky stage that holds the note slot and the 01 to 06 legend; on desktop chapters soft-snap to centre (scroll-snap proximity). Later sections use asymmetric two-column grids (local-first 1fr / 1fr, open source 1.1fr / 1fr, FAQ 0.8fr / 1.2fr) with gaps of clamp(32px, 6vw, 96px).

Vertical rhythm is viewport-based: sections breathe at 12vh to 18vh top and bottom. Interior rhythm is small and steady: 8px between chips, 10px between actions, 20 to 30px between headline, copy and actions.

Breakpoint at 900px. Below it: nav links hide (brand plus Download only), the hero stacks and stops pinning, the desk frame drops to min(50vh, 440px), chapters stack with the figure first, the stage pins at the top of the viewport as a 454px ground-coloured band (note above, legend below, masked fade at its lower edge) while captions scroll beneath it, the library sidebar becomes a horizontal scroller and the grid goes to two columns, the fan wraps into two rows, and download buttons go full width.

### Named Rules
**The Square Module Rule.** Every note pose (pad sheet, traveler slot, figure, library card, fan note) is a square or a square-derived shape; only chapter 01's note widens, because that is the feature.

## Elevation & Depth

Depth is soft and ambient, always cast by ink (rgba of the ink colour) with large negative spread so shadows pool under objects rather than outlining them. Paper additionally carries an inner light (a white radial glow from the upper left and, for yellow paper only, a warm bottom settle) and a grain texture: warm grain on yellow paper, a neutral, lighter grain on the cool pastels. The traveling note uses layered drop-shadows so the curl and torn edge cast correctly.

### Shadow Vocabulary
- **Pill lift** (`0 8px 20px -10px rgba(36,35,34,.55)`): ink buttons at rest; hover deepens to `0 14px 26px -12px rgba(36,35,34,.6)` with a 1px rise.
- **Nav glass** (`0 1px 0 rgba(255,255,255,.7) inset, 0 10px 30px -14px rgba(36,35,34,.35)` plus 14px backdrop blur).
- **Resting note** (`0 14px 24px -18px` to `0 30px 50px -28px`, ink at .55): library cards, fan notes, chapter figures.
- **Travelling note** (`drop-shadow(0 22px 26px rgba(36,35,34,.26)) drop-shadow(0 2px 3px rgba(36,35,34,.18))`).
- **Window** (`0 1px 0 rgba(255,255,255,.8) inset, 0 50px 90px -50px rgba(36,35,34,.5), 0 0 0 1px` line): the library window and the hero desk frame.

### Named Rules
**The Pooled Shadow Rule.** Box shadows have large blur and a negative spread of roughly half the blur; masked or clipped paper (the traveler, the peel) uses soft drop-shadow filters instead. No hard offset shadows, and no coloured shadows except the faint yellow glow under the paper button.

**The Lit Paper Rule.** Yellow paper is always drawn as glow + warm settle + warm grain over the paper colour; pastel notes as glow + cool grain over their colour. Never a flat fill.

## Shapes

Two silhouettes. Interface chrome is a full pill (999px): nav, buttons, chips, legend, status and due pills, toast, search field. Notes are rounded squares whose corner scales with size (13% radius; 9% for static figures, 14px for library cards). Windows and frames get large soft corners (library window 26px, desk frame 28px, code panel 20px). Small inner objects step down: 11px for file chips and mini-notes, 9px for list rows, 6px for inline code and tokens, 5px for checkboxes.

The sticky has a fixed anatomy: a lower-right corner peel drawn as a triangle clipped from a coral gradient with a soft drop-shadow, the sheet masked diagonally beneath it; once torn, an irregular torn top edge (an uneven SVG mask, not a perforation) with a lighter fibre lip. Notes sit at small rotations (about -1.8 to +1.6 degrees in the library, up to ±10 degrees in the fan).

## Components

### Buttons
Confident, heavy and pill-shaped.
- **Shape:** full pill (999px), 48px tall, 9px icon gap, Geist 600 at 15.5px.
- **Ink (primary):** ink fill, raised-white text, pill lift shadow. The one CTA style for Download.
- **Hover / Focus / Active:** ink and paper rise 1px; ink darkens and its shadow deepens; ghost darkens its tint. Active presses down 1px and scales to 0.985. Focus is a 2px ink outline at 3px offset (paper inside the ink band).
- **Ghost:** a 6% ink tint for the secondary action (View on GitHub).
- **Paper:** yellow fill with ink text, only on the ink band.
- **Sizes:** sm 38px (nav), lg 58px (download close); mobile hero buttons compress to 44px.

### Chips
- **Style:** 32px pill, 1px hairline border, translucent raised fill, 13.5px 500 ink-2 text, optional 15px ink SVG icon.
- **State:** informational only (platforms, license, no account); no selected state.

### Cards / Containers
- **Library window:** raised surface, 26px corners, window shadow with a 1px hairline ring, 210px stone sidebar with pill-ish rows (active row in 42% yellow) and a mono search pill with a yellow token.
- **Library cards:** square notes in the seven note palettes, 14px corners, 16px padding, bold 16px title over 13.5px ink-2 text, slight rotations. An empty slot is a 1.5px inset hairline square where the travelling note lands.
- **Code panel:** a slightly lifted ink tone inside the ink band, 20px corners, Geist Mono 14.5px at 1.75 leading, comments in band-comment.

### Navigation
- **Style:** a floating pill fixed 14px from the top, centred, translucent raised glass with backdrop blur. Brand (mark plus "Pinq", 750 weight) left, 14.5px 500 ink-2 links that turn ink on hover, small ink Download pill right.
- **Mobile:** stretches from 12px on the left to just short of the theme toggle; links hide.
- **Theme toggle:** a 50px round glass button fixed top right (14px, 16px; 12px on mobile), same glass as the nav, showing the moon in light and the sun in dark. Its aria-label names the theme it switches to.

### Spec Strip
The stationery footer under each chapter's copy: a line-2 hairline rule, then a tabular chapter number in ink, an uppercase label at 0.09em, and a right-aligned plain-case descriptor. It numbers chapters 01 to 06 and is the only place chapter numbers appear in copy.

### Legend
A row of 30px pills above the stage note: inactive pills show only the number in ink-3; the active chapter becomes an ink pill and reveals its name. Each is a deep link to its chapter.

### Sticky Note (signature)
The traveling note is the system's hero component. Yellow lit paper with a smiley face (ink, rounded strokes) until it has content; a coral corner peel whose size is driven by scroll; a torn top edge with fibre lip after the rip; and a **ledger**, six 9px rounded squares at the bottom left that fill ink as chapters are visited and coral for the current one. Note content uses in-world parts: status pill (todo, doing half-filled, done in ink with yellow text), due pill (overdue in overdue red, white text), 19px ink checkboxes that fill ink with a yellow tick, an ink progress bar, inline image (10px corners), file chip (11px, translucent white, ink download tile), wiki links in link red, and pastel mini-notes joined by thin ink wires. On tuck it folds into a 30px edge tab with a left-edge shade; a "Filed to Library / Undo" ink toast appears at the bottom.

**Scroll choreography** (hand-rolled, scroll-scrubbed, requestAnimationFrame): **rip** (the first 42% of a viewport on desktop, 30% on mobile; the corner curls back, then the sheet lifts 18px, rotates -5 degrees and tears), **travel** (eased arc from the pad to the stage with a sideways swing and a wobble), **six chapter morphs** driven by progress through each chapter (01 types a long line and widens instead of wrapping; 02 ticks boxes, fills progress, moves status to done and clears the overdue chip; 03 drops a photo then a file; 04 turns brackets into links, pops linked notes and draws wires; 05 flings to the screen edge, folds into a tab, peeks and returns, with the undo toast; 06 shows a twin note on a second device typing in sync with an "end-to-end encrypted" pill), then **land** (eases into the empty slot in the library grid and the slot's static card takes over). Reversing the scroll plays it backwards. Easing for CSS transitions is a soft expo-out (cubic-bezier(0.16, 1, 0.3, 1)).

**Reduced motion and no-JS:** the controller never starts, the hero does not pin, and each chapter shows its own static figure (the note in its completed pose) beside the copy; all CSS animation and transitions are disabled. Content is complete without the motion.

### FAQ and Facts
Hairline-ruled lists (line-2 between items). Facts: 19px 700 terms over ink-2 definitions. FAQ: 19px 650 summaries with a CSS-drawn plus that rotates 45 degrees when open.

### Fan
The closing constellation: the six chapter capabilities as six square notes in an arc (yellow lit paper, then amber, mint, sky, rose, violet), rotated -10 to +10 degrees, each numbered 01 to 06; the whole fan lifts 6px on hover and the hovered note 14px.

## Do's and Don'ts

### Do:
- **Do** keep the ground warm stone and every heading in ink at Geist 820 to 860 with -0.035em to -0.04em tracking.
- **Do** reserve saturated colour for paper: the yellow sticky, its coral peel, and the seven note palettes shown as notes.
- **Do** draw paper as lit material (glow, settle, grain) with a pooled ink shadow beneath it.
- **Do** use full pills (999px) for nav, buttons, chips, legend and toasts, and rounded squares for notes.
- **Do** number chapters only through the spec strip, legend and ledger (01 to 06, tabular).
- **Do** give every scroll-driven demonstration a static, complete pose for reduced motion and no-JS.

### Don't:
- **Don't** use cream as the page ground; cream (note-pinq) is a note colour.
- **Don't** introduce colour that is not a note (beyond the one hero wallpaper photograph): no coloured section backgrounds, gradient text, or accent-coloured chrome.
- **Don't** use hard offset shadows or flat, untextured yellow.
- **Don't** set stationery labels in monospace; mono is for code and queries.
- **Don't** add a second full-bleed ink band.
- **Don't** place an eyebrow or kicker line above headlines; the hero opens directly on the headline.
