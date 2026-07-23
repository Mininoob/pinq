import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type PaperKind =
  | "plain"
  | "ruled"
  | "dotted"
  | "grid"
  | "dashed"
  | "wavy"
  | "zigzag"
  | "hearts"
  | "stars";

export interface PinqSettings {
  theme: string;
  libraryMode: "auto" | "light" | "dark";
  libraryPaper: PaperKind;
  notePaper: PaperKind;
  padEdge: "left" | "right" | "top" | "bottom";
  padPosition: number;
  padMonitors: "primary" | "all";
  clickCreates: boolean;
  relayAddr: string;
  relayHost: boolean;
  smartSuggestions: boolean;
  launchAtLogin: boolean;
  /** Where Pinq surfaces on macOS; ignored on Windows. */
  showIn: "both" | "menubar" | "dock";
}

const DEFAULTS: PinqSettings = {
  theme: "pinq",
  libraryMode: "auto",
  libraryPaper: "plain",
  notePaper: "plain",
  padEdge: "right",
  padPosition: 0.5,
  padMonitors: "primary",
  clickCreates: false,
  relayAddr: "",
  relayHost: false,
  smartSuggestions: false,
  launchAtLogin: false,
  showIn: "both",
};

/** Latest settings this window has seen (kept fresh by the settings event). */
export function getSettings(): PinqSettings {
  return current;
}

let current: PinqSettings = DEFAULTS;
let media: MediaQueryList | undefined;
let localTheme: string | undefined;
let localNotePaper: string | undefined;

function apply(s: PinqSettings) {
  document.body.dataset.theme = localTheme ?? s.theme;
  document.body.dataset.paper = s.libraryPaper;
  document.body.dataset.notePaper = localNotePaper ?? s.notePaper;
  const dark =
    s.libraryMode === "dark" || (s.libraryMode === "auto" && (media?.matches ?? false));
  document.body.dataset.mode = dark ? "dark" : "light";
}

// A note window with a per-note color pins its own palette; the global theme
// keeps driving mode/paper and every other window.
export function setLocalTheme(theme: string | undefined) {
  localTheme = theme;
  apply(current);
}

// Same idea for a note window with a per-note paper override.
export function setLocalNotePaper(paper: string | undefined) {
  localNotePaper = paper;
  apply(current);
}

// Every window applies the shared settings on load and follows live changes —
// palette, light/dark (auto tracks the OS), and paper — so all surfaces match.
export async function initTheme() {
  media = window.matchMedia("(prefers-color-scheme: dark)");
  media.addEventListener("change", () => apply(current));
  try {
    current = await invoke<PinqSettings>("get_settings");
  } catch {
    current = DEFAULTS;
  }
  apply(current);
  await listen<PinqSettings>("pinq-settings", (e) => {
    current = e.payload;
    apply(current);
  });
}
