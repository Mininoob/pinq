import { createSignal, For, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getSettings } from "../theme";

type Phase = "idle" | "armed" | "pulling";
export type PadEdge = "left" | "right" | "top" | "bottom";

// Spec §9: tear at 24px of travel away from the edge, direction-gated to ±60°
// from the outward normal (into the screen).
const TEAR_PX = 24;
const DIRECTION_TAN = Math.tan((60 * Math.PI) / 180);
// Hover-reveal: 150ms dwell before revealing, 400ms grace before collapsing.
const REVEAL_DWELL_MS = 150;
const COLLAPSE_GRACE_MS = 400;
// Press-and-hold without moving pulls the pad out with its menu on it.
const MENU_HOLD_MS = 500;
const HOLD_SLOP_PX = 4;
// The menu forgives brief mouse exits before sliding back in.
const MENU_LEAVE_GRACE_MS = 650;

// Views on the pulled-out pad; future views are one entry here. Icons are
// factories so every open mints fresh nodes.
const MENU_ENTRIES = [
  {
    label: "Library",
    action: "open_library",
    icon: () => (
      <svg
        width="14"
        height="14"
        viewBox="0 0 14 14"
        fill="none"
        stroke="currentColor"
        stroke-width="1.4"
        stroke-linecap="round"
        stroke-linejoin="round"
      >
        <rect x="2" y="2" width="10" height="10" rx="2" />
        <path d="M5 5.2h4M5 7.6h4M5 10h2.5" />
      </svg>
    ),
  },
  {
    label: "Settings",
    action: "open_settings",
    icon: () => (
      <svg
        width="14"
        height="14"
        viewBox="0 0 14 14"
        fill="none"
        stroke="currentColor"
        stroke-width="1.4"
        stroke-linecap="round"
      >
        <path d="M2 4.2h10M2 9.8h10" />
        <circle cx="5.2" cy="4.2" r="1.7" fill="var(--note-bg)" />
        <circle cx="8.8" cy="9.8" r="1.7" fill="var(--note-bg)" />
      </svg>
    ),
  },
];

// Travel into the screen ("away") and along the edge, per docked edge.
const travel = (edge: PadEdge, sx: number, sy: number, x: number, y: number) => {
  switch (edge) {
    case "left":
      return { away: x - sx, along: Math.abs(y - sy) };
    case "top":
      return { away: y - sy, along: Math.abs(x - sx) };
    case "bottom":
      return { away: sy - y, along: Math.abs(x - sx) };
    default:
      return { away: sx - x, along: Math.abs(y - sy) };
  }
};

export default function Pad(props: { edge: PadEdge }) {
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [revealed, setRevealed] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal(false);
  let start = { x: 0, y: 0 };
  let revealTimer: number | undefined;
  let collapseTimer: number | undefined;
  let holdTimer: number | undefined;
  let menuLeaveTimer: number | undefined;
  // Accessibility mode: a quick, still click creates a note.
  let clickCandidate = false;
  let downAt = 0;

  const openMenu = () => {
    clickCandidate = false;
    setPhase("idle");
    setMenuOpen(true);
    void invoke("expand_pad", { expand: true });
  };

  const closeMenu = () => {
    clearTimeout(menuLeaveTimer);
    if (!menuOpen()) return;
    setMenuOpen(false);
    void invoke("expand_pad", { expand: false });
    collapseTimer = window.setTimeout(() => setRevealed(false), COLLAPSE_GRACE_MS);
  };

  const pick = async (action: string) => {
    closeMenu();
    await invoke(action);
  };

  // Quick capture (spec S7/S9): a template note appears beside the pad.
  const pickTemplate = async (kind: string) => {
    closeMenu();
    await invoke("create_note_from_template", { kind });
  };

  const onPointerEnter = () => {
    clearTimeout(collapseTimer);
    clearTimeout(menuLeaveTimer);
    if (menuOpen()) return;
    revealTimer = window.setTimeout(() => setRevealed(true), REVEAL_DWELL_MS);
  };

  const onPointerLeave = () => {
    clearTimeout(revealTimer);
    if (menuOpen()) {
      menuLeaveTimer = window.setTimeout(closeMenu, MENU_LEAVE_GRACE_MS);
      return;
    }
    if (phase() !== "idle") return;
    collapseTimer = window.setTimeout(() => setRevealed(false), COLLAPSE_GRACE_MS);
  };

  const onPointerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    if (menuOpen()) {
      // Clicks on menu buttons handle themselves; anywhere else tucks it back.
      if (!(e.target as HTMLElement).closest("button")) closeMenu();
      return;
    }
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    start = { x: e.clientX, y: e.clientY };
    clickCandidate = true;
    downAt = performance.now();
    setRevealed(true);
    setPhase("armed");
    holdTimer = window.setTimeout(() => {
      // Held still long enough: the pad slides out with its menu.
      if (phase() === "armed") openMenu();
    }, MENU_HOLD_MS);
  };

  const onPointerMove = (e: PointerEvent) => {
    if (menuOpen()) return;
    const p = phase();
    const { away, along } = travel(props.edge, start.x, start.y, e.clientX, e.clientY);

    if (p === "armed") {
      // Any real movement means it's a note pull, not a menu hold/click.
      if (Math.hypot(away, along) > HOLD_SLOP_PX) {
        clearTimeout(holdTimer);
        clickCandidate = false;
      }
      if (away >= TEAR_PX && along <= away * DIRECTION_TAN) {
        setPhase("pulling");
        void invoke("pull_start");
      }
    }
  };

  const settle = () => {
    clearTimeout(holdTimer);
    setPhase("idle");
    collapseTimer = window.setTimeout(() => setRevealed(false), COLLAPSE_GRACE_MS);
  };

  const onPointerUp = () => {
    if (menuOpen()) return;
    const p = phase();
    if (p === "pulling") void invoke("pull_end");
    // Quick, still click with the accessibility setting on: create a note.
    if (
      p === "armed" &&
      clickCandidate &&
      performance.now() - downAt < 400 &&
      getSettings().clickCreates
    ) {
      void invoke("click_create_note");
    }
    settle();
  };

  const cancel = () => {
    if (menuOpen()) return closeMenu();
    if (phase() === "pulling") void invoke("pull_cancel");
    settle();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") cancel();
    // Keyboard path (spec §28): the pull gesture has no keyboard equivalent,
    // so Enter/Space on the focused pad creates a note directly.
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void invoke("click_create_note");
    }
  };

  return (
    <div
      class={`pad-host edge-${props.edge}`}
      role="button"
      aria-label="Sticky pad — pull inward for a new note (or press Enter); press and hold for the menu"
      tabIndex={0}
      onPointerEnter={onPointerEnter}
      onPointerLeave={onPointerLeave}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={cancel}
      onKeyDown={onKeyDown}
    >
      <div
        class={`pad edge-${props.edge}`}
        classList={{ [phase()]: true, revealed: revealed(), "menu-open": menuOpen() }}
      >
        <div class="pad-grip">
          <span /><span /><span />
        </div>
      </div>
      <Show when={menuOpen()}>
        <div class="pad-card">
          <For each={MENU_ENTRIES}>
            {(entry) => (
              <button class="pad-card-btn" onClick={() => void pick(entry.action)}>
                {entry.icon()}
                <span>{entry.label}</span>
              </button>
            )}
          </For>
          <div class="pad-card-rule" />
          <button class="pad-card-btn" onClick={() => void pickTemplate("todo")}>
            <span class="pad-card-glyph">☐</span>
            <span>New todo</span>
          </button>
          <button class="pad-card-btn" onClick={() => void pickTemplate("meeting")}>
            <span class="pad-card-glyph">✎</span>
            <span>New meeting</span>
          </button>
        </div>
      </Show>
    </div>
  );
}
