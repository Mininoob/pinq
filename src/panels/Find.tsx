import { createEffect, createSignal, For, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { stripLinks } from "../links";
import { stripMd } from "../md";

interface NoteState {
  id: string;
  text: string;
  pinned: boolean;
  trashed: boolean;
  color: string | null;
  modified_ms: number;
}

const LIMIT = 10;

export default function Find() {
  const [results, setResults] = createSignal<NoteState[]>([]);
  const [query, setQuery] = createSignal("");
  const [sel, setSel] = createSignal(0);
  let input!: HTMLInputElement;
  let unlisten: UnlistenFn | undefined;

  // FTS-backed: word-prefix matching, ranked by relevance; recents when empty.
  const search = async (q: string) => {
    setResults((await invoke<NoteState[]>("search_notes", { query: q })).slice(0, LIMIT));
  };

  // Re-query whenever the input changes.
  createEffect(() => void search(query()));

  onMount(async () => {
    input.focus();
    unlisten = await listen("pinq-find-open", () => {
      setQuery("");
      setSel(0);
      void search("");
      input.focus();
      input.select();
    });
  });

  onCleanup(() => unlisten?.());

  const title = (n: NoteState) => {
    const line = n.text.split("\n").find((l) => l.trim().length > 0);
    return line ? stripMd(stripLinks(line)).slice(0, 70) : "Empty note";
  };

  const close = () => void invoke("close_find");

  // Enter: put the note on the desktop (or focus it if it's already there).
  const pinSelected = async () => {
    const n = results()[sel()];
    if (!n) return;
    await invoke("pin_note", { id: n.id });
    close();
  };

  const openLibrary = async () => {
    await invoke("open_library");
    close();
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel(Math.min(sel() + 1, results().length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel(Math.max(sel() - 1, 0));
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void openLibrary();
    } else if (e.key === "Enter") {
      e.preventDefault();
      void pinSelected();
    }
  };

  return (
    <div class="find-panel" onKeyDown={onKeyDown}>
      <input
        ref={input}
        class="find-input"
        type="search"
        placeholder="Find a note…"
        value={query()}
        onInput={(e) => {
          setQuery(e.currentTarget.value);
          setSel(0);
        }}
      />
      <div class="find-results">
        <For
          each={results()}
          fallback={
            <p class="find-empty">
              {query().trim() ? "No matches." : "No notes yet — pull one off the pad!"}
            </p>
          }
        >
          {(n, i) => (
            <div
              class="find-row"
              classList={{ selected: sel() === i() }}
              onPointerEnter={() => setSel(i())}
              onClick={() => void pinSelected()}
            >
              <span class="find-swatch" data-theme={n.color ?? undefined} />
              <span class="find-title" classList={{ empty: !n.text.trim() }}>
                {title(n)}
              </span>
              <span class="find-meta">
                {n.pinned ? "📌" : ""} {new Date(n.modified_ms).toLocaleDateString()}
              </span>
            </div>
          )}
        </For>
      </div>
      <footer class="find-hints">
        <span>↑↓ choose</span>
        <span>Enter — pin to desktop</span>
        <span>Ctrl+Enter — library</span>
        <span>Esc — close</span>
      </footer>
    </div>
  );
}
