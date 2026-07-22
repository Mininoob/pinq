import { createSignal, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// Undo toast shown after a note is thrown/filed or trashed. The window is
// pre-created hidden; Rust positions and shows it, we run the 6s dismiss
// timer and the Undo action.
export default function Toast() {
  const [message, setMessage] = createSignal("Filed to Library");
  let timer: number | undefined;
  let unlisten: UnlistenFn | undefined;

  onMount(async () => {
    unlisten = await listen<{ id: string; kind: string }>("pinq-filed", (e) => {
      setMessage(e.payload.kind === "trashed" ? "Moved to Trash" : "Filed to Library");
      clearTimeout(timer);
      timer = window.setTimeout(() => void invoke("toast_dismiss"), 6000);
    });
  });

  onCleanup(() => unlisten?.());

  const undo = () => {
    clearTimeout(timer);
    void invoke("undo_last");
  };

  return (
    <div class="toast" role="status" aria-live="polite">
      <span class="toast-text">{message()}</span>
      <button class="toast-undo" onClick={undo}>
        Undo
      </button>
    </div>
  );
}
