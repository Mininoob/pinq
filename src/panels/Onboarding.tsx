import { For } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

// First-run tour (spec S17): the five moves that make Pinq Pinq. Shown once,
// replayable from Settings → "Replay the welcome tour".
const TIPS = [
  {
    glyph: "✋",
    title: "Pull",
    body: "Drag inward from the colored pad at your screen edge — a fresh sticky tears off under your cursor.",
  },
  {
    glyph: "🌀",
    title: "Throw",
    body: "Fling a note hard and let go: it files itself into the Library. Nothing is ever lost — there's an Undo toast.",
  },
  {
    glyph: "📌",
    title: "Tuck",
    body: "Toss a note gently at a screen edge and it folds into a slim tab. Hover to peek, pull to bring it back.",
  },
  {
    glyph: "🗂",
    title: "Hold the pad",
    body: "Press and hold the pad for the menu: Library, Settings, and quick todo / meeting notes.",
  },
  {
    glyph: "⌨️",
    title: "From anywhere",
    body: "Ctrl+Alt+N makes a note, Ctrl+Alt+F finds one, Ctrl+Alt+H hides them all. Change these in Settings.",
  },
];

export default function Onboarding() {
  const done = () => void invoke("onboarding_complete");
  return (
    <div class="onboard" role="dialog" aria-label="Welcome to Pinq">
      <h1 class="onboard-title">Welcome to Pinq</h1>
      <p class="onboard-sub">Sticky notes that live on your desktop — five moves and you know it all.</p>
      <div class="onboard-tips">
        <For each={TIPS}>
          {(t) => (
            <div class="onboard-tip">
              <span class="onboard-glyph" aria-hidden="true">
                {t.glyph}
              </span>
              <div>
                <div class="onboard-tip-title">{t.title}</div>
                <div class="onboard-tip-body">{t.body}</div>
              </div>
            </div>
          )}
        </For>
      </div>
      <button class="onboard-go" onClick={done} autofocus>
        Got it — let's go
      </button>
    </div>
  );
}
