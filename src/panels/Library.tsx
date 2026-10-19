import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { tallyTags } from "../tags";
import { buildLinkIndex, linkSegments, stripLinks, titleOf } from "../links";
import { stripMd } from "../md";
import Graph from "./Graph";
import mascot from "../assets/icons/pinq-icon-pack/pinq-icon-transparent.svg";

interface TagSuggestion {
  tag: string;
  notes: Array<{ id: string; title: string; pinned: boolean }>;
}

interface NoteState {
  id: string;
  text: string;
  pinned: boolean;
  trashed: boolean;
  color: string | null;
  paper: string | null;
  remind_ms: number | null;
  due_ms: number | null;
  status: string | null;
  favorite: boolean;
  archived: boolean;
  tags: string[];
  attachments: Array<{ hash: string; name: string; mime: string; size: number }>;
  created_ms: number;
  modified_ms: number;
}

const firstImage = (n: NoteState) => n.attachments.find((a) => a.mime.startsWith("image/"));

type SortBy = "modified" | "created" | "title" | "due";
type GroupBy = "none" | "color" | "tag";
// Sidebar views. Inbox = unprocessed captures: not on the desktop, untagged.
type LibView = "all" | "inbox" | "favorites" | "desktop" | "archive";
const LIB_VIEWS: Array<{ key: LibView; label: string; glyph: string }> = [
  { key: "all", label: "All notes", glyph: "🗂" },
  { key: "inbox", label: "Inbox", glyph: "📥" },
  { key: "favorites", label: "Favorites", glyph: "★" },
  { key: "desktop", label: "On desktop", glyph: "📌" },
  { key: "archive", label: "Archive", glyph: "🗃" },
];

// Per-device view preferences; pure display state, so localStorage not settings.
const viewPref = <T extends string>(key: string, fallback: T, valid: readonly T[]): T => {
  const v = localStorage.getItem(key);
  return valid.includes(v as T) ? (v as T) : fallback;
};

// Section order for group-by-color; matches the note palette picker.
const COLOR_ORDER = ["pinq", "amber", "mint", "sky", "rose", "violet", "graphite"];

// Deterministic small tilt per note so the stack looks organic but never shifts.
const tilt = (id: string) => {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return ((Math.abs(h) % 50) / 10 - 2.5).toFixed(1);
};

export default function Library() {
  const [notes, setNotes] = createSignal<NoteState[]>([]);
  const [query, setQuery] = createSignal("");
  const [activeTag, setActiveTag] = createSignal<string | null>(null);
  const [showTrash, setShowTrash] = createSignal(false);
  // Two-step destructive confirm: holds a note id, or "__all__" for Empty Trash.
  const [confirmId, setConfirmId] = createSignal<string | null>(null);
  const [dropping, setDropping] = createSignal(false);
  const [message, setMessage] = createSignal<string | null>(null);
  // Smart tag suggestions ([] while the feature is off — card simply hides).
  const [suggestions, setSuggestions] = createSignal<TagSuggestion[]>([]);
  const [sortBy, setSortBy] = createSignal<SortBy>(
    viewPref("pinq.lib.sort", "modified", ["modified", "created", "title", "due"]),
  );
  const [groupBy, setGroupBy] = createSignal<GroupBy>(
    viewPref("pinq.lib.group", "none", ["none", "color", "tag"]),
  );
  const pickSort = (v: SortBy) => {
    setSortBy(v);
    localStorage.setItem("pinq.lib.sort", v);
  };
  const pickGroup = (v: GroupBy) => {
    setGroupBy(v);
    localStorage.setItem("pinq.lib.group", v);
  };
  const [view, setView] = createSignal<"cards" | "graph">(
    viewPref("pinq.lib.view", "cards", ["cards", "graph"]),
  );
  const toggleView = () => {
    const v = view() === "cards" ? "graph" : "cards";
    setView(v);
    localStorage.setItem("pinq.lib.view", v);
  };
  const [libView, setLibView] = createSignal<LibView>(
    viewPref("pinq.lib.sideview", "all", ["all", "inbox", "favorites", "desktop", "archive"]),
  );
  const pickLibView = (v: LibView) => {
    setLibView(v);
    localStorage.setItem("pinq.lib.sideview", v);
    setSelected(new Set<string>());
  };
  // Multi-select: ctrl+click toggles, shift+click extends from the last pick.
  const [selected, setSelected] = createSignal<Set<string>>(new Set<string>(), { equals: false });
  let lastSel: string | null = null;
  const [batchTag, setBatchTag] = createSignal("");
  // Saved searches: per-device, like the other view preferences.
  const [savedSearches, setSavedSearches] = createSignal<string[]>(
    (() => {
      try {
        const v = JSON.parse(localStorage.getItem("pinq.lib.saved") ?? "[]");
        return Array.isArray(v) ? v.filter((s) => typeof s === "string") : [];
      } catch {
        return [];
      }
    })(),
  );
  const persistSaved = (list: string[]) => {
    setSavedSearches(list);
    localStorage.setItem("pinq.lib.saved", JSON.stringify(list));
  };
  const saveSearch = () => {
    const q = query().trim();
    if (!q) return;
    persistSaved([...new Set([...savedSearches(), q])]);
    flashMessage(`Saved search: ${q}`);
  };
  const removeSearch = (q: string) => persistSaved(savedSearches().filter((s) => s !== q));
  let timer: number | undefined;
  let suggestTimer: number | undefined;
  let confirmTimer: number | undefined;
  let messageTimer: number | undefined;
  let unlistenDrop: UnlistenFn | undefined;
  let unlistenSync: UnlistenFn | undefined;
  let unlistenSmart: UnlistenFn | undefined;

  const refresh = async () => {
    setNotes(await invoke<NoteState[]>("get_all_notes"));
  };

  const refreshSuggestions = async () => {
    try {
      setSuggestions(await invoke<TagSuggestion[]>("tag_suggestions"));
    } catch {
      setSuggestions([]);
    }
  };

  const acceptSuggestion = async (s: TagSuggestion) => {
    await invoke("accept_tag_suggestion", { tag: s.tag, ids: s.notes.map((n) => n.id) });
    flashMessage(`Tagged ${s.notes.length} note${s.notes.length === 1 ? "" : "s"} #${s.tag}`);
    await refresh();
    await refreshSuggestions();
  };

  const dismissSuggestion = async (s: TagSuggestion) => {
    await invoke("dismiss_tag_suggestion", { tag: s.tag, ids: s.notes.map((n) => n.id) });
    await refreshSuggestions();
  };

  const flashMessage = (m: string) => {
    setMessage(m);
    clearTimeout(messageTimer);
    messageTimer = window.setTimeout(() => setMessage(null), 5000);
  };

  let refreshDebounce: number | undefined;
  let unlistenChanged: UnlistenFn | undefined;

  onMount(async () => {
    void refresh();
    void refreshSuggestions();
    // Event-driven: every local commit broadcasts pinq-changed, syncs emit
    // pinq-sync-applied. The slow interval is only a safety net.
    timer = window.setInterval(() => void refresh(), 30000);
    suggestTimer = window.setInterval(() => void refreshSuggestions(), 60000);
    unlistenChanged = await listen("pinq-changed", () => {
      clearTimeout(refreshDebounce);
      refreshDebounce = window.setTimeout(() => void refresh(), 250);
    });
    // Fresh embeddings can surface new suggestions right away.
    unlistenSmart = await listen("pinq-smart-updated", () => void refreshSuggestions());
    // Notes that just arrived from another device show up immediately.
    unlistenSync = await listen("pinq-sync-applied", () => void refresh());
    // Drag .md/.txt files anywhere onto the library to import them.
    unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setDropping(true);
      } else if (payload.type === "drop") {
        setDropping(false);
        void (async () => {
          const n = await invoke<number>("import_files", { paths: payload.paths });
          flashMessage(n > 0 ? `Imported ${n} note${n === 1 ? "" : "s"}` : "Nothing importable (.md / .txt)");
          await refresh();
        })();
      } else {
        setDropping(false);
      }
    });
  });

  onCleanup(() => {
    clearInterval(timer);
    clearInterval(suggestTimer);
    clearTimeout(confirmTimer);
    clearTimeout(messageTimer);
    clearTimeout(refreshDebounce);
    unlistenDrop?.();
    unlistenSync?.();
    unlistenSmart?.();
    unlistenChanged?.();
  });

  const exportAll = async () => {
    const dir = await invoke<string>("export_all");
    flashMessage(`Exported to ${dir}`);
  };

  const backupAll = async () => {
    const path = await invoke<string>("backup_archive");
    flashMessage(`Backup saved: ${path}`);
  };

  // Search supports filter tokens alongside free text:
  //   tag:x  is:fav|task|todo|doing|done|pinned|overdue|archived
  //   has:file|image|due  due:today|week|overdue  before:/after:<date>
  const parsedQuery = createMemo(() => {
    const toks = {
      tags: [] as string[],
      is: [] as string[],
      has: [] as string[],
      due: null as string | null,
      before: null as number | null,
      after: null as number | null,
    };
    const text: string[] = [];
    for (const part of query().trim().split(/\s+/).filter(Boolean)) {
      const m = part.match(/^(tag|is|has|due|before|after):(.+)$/i);
      if (!m) {
        text.push(part.toLowerCase());
        continue;
      }
      const v = m[2].toLowerCase();
      switch (m[1].toLowerCase()) {
        case "tag":
          toks.tags.push(v.replace(/^#/, ""));
          break;
        case "is":
          toks.is.push(v);
          break;
        case "has":
          toks.has.push(v);
          break;
        case "due":
          toks.due = v;
          break;
        case "before": {
          const ms = Date.parse(m[2]);
          if (!Number.isNaN(ms)) toks.before = ms;
          break;
        }
        case "after": {
          const ms = Date.parse(m[2]);
          if (!Number.isNaN(ms)) toks.after = ms;
          break;
        }
      }
    }
    return { toks, text: text.join(" ") };
  });

  const matches = (n: NoteState) => {
    const { toks, text } = parsedQuery();
    const tag = activeTag();
    if (tag !== null && !n.tags.includes(tag)) return false;
    if (text && !(n.text.toLowerCase().includes(text) || n.tags.some((t) => t.includes(text))))
      return false;
    for (const t of toks.tags) if (!n.tags.some((x) => x.includes(t))) return false;
    for (const f of toks.is) {
      const ok =
        f === "fav" || f === "favorite" ? n.favorite
        : f === "task" ? n.status !== null
        : f === "todo" || f === "doing" || f === "done" ? n.status === f
        : f === "pinned" ? n.pinned
        : f === "overdue" ? n.due_ms !== null && n.due_ms < Date.now() && n.status !== "done"
        : f === "archived" ? n.archived
        : true;
      if (!ok) return false;
    }
    for (const h of toks.has) {
      const ok =
        h === "file" || h === "attachment" ? n.attachments.length > 0
        : h === "image" ? n.attachments.some((a) => a.mime.startsWith("image/"))
        : h === "due" ? n.due_ms !== null
        : true;
      if (!ok) return false;
    }
    if (toks.due) {
      if (n.due_ms === null) return false;
      const now = new Date();
      if (toks.due === "today" && new Date(n.due_ms).toDateString() !== now.toDateString())
        return false;
      if (toks.due === "overdue" && !(n.due_ms < Date.now() && n.status !== "done")) return false;
      if (toks.due === "week" && n.due_ms > now.getTime() + 7 * 86_400_000) return false;
    }
    if (toks.before !== null && n.modified_ms >= toks.before) return false;
    if (toks.after !== null && n.modified_ms <= toks.after) return false;
    return true;
  };

  // Which notes belong to the current sidebar view, before search filtering.
  const inView = (n: NoteState) => {
    switch (libView()) {
      case "inbox":
        return !n.trashed && !n.archived && !n.pinned && n.tags.length === 0;
      case "favorites":
        return !n.trashed && !n.archived && n.favorite;
      case "desktop":
        return !n.trashed && n.pinned;
      case "archive":
        return !n.trashed && n.archived;
      default:
        return !n.trashed && !n.archived;
    }
  };

  const active = createMemo(() => notes().filter((n) => inView(n) && matches(n)));
  const trashed = createMemo(() => notes().filter((n) => n.trashed && matches(n)));
  const activeTotal = createMemo(() => notes().filter(inView).length);
  const trashedTotal = createMemo(() => notes().filter((n) => n.trashed).length);

  const viewCounts = createMemo(() => {
    const c: Record<LibView, number> = { all: 0, inbox: 0, favorites: 0, desktop: 0, archive: 0 };
    for (const n of notes()) {
      if (n.trashed) continue;
      if (n.archived) {
        c.archive += 1;
        if (n.pinned) c.desktop += 1;
        continue;
      }
      c.all += 1;
      if (!n.pinned && n.tags.length === 0) c.inbox += 1;
      if (n.favorite) c.favorites += 1;
      if (n.pinned) c.desktop += 1;
    }
    return c;
  });

  const tags = createMemo(() =>
    tallyTags(notes().filter((n) => !n.trashed && !n.archived).map((n) => n.tags)),
  );

  // Backlink counts + edges, recomputed on refresh. Cheap: one pass
  // over already-loaded note texts.
  const linkIndex = createMemo(() => buildLinkIndex(notes()));

  // The graph always shows every non-trashed, non-archived note (layout stays
  // stable); search/tag filtering dims non-matching nodes instead of removing.
  const allActive = createMemo(() => notes().filter((n) => !n.trashed && !n.archived));
  const dimmedIds = createMemo(() => {
    if (!query().trim() && activeTag() === null) return new Set<string>();
    return new Set(allActive().filter((n) => !matches(n)).map((n) => n.id));
  });

  const sorted = createMemo(() => {
    const list = [...active()];
    switch (sortBy()) {
      case "created":
        return list.sort((a, b) => b.created_ms - a.created_ms);
      case "due":
        // Soonest due first; dateless notes sink to the end (by recency there).
        return list.sort((a, b) => {
          if (a.due_ms === null || b.due_ms === null) {
            if (a.due_ms !== null) return -1;
            if (b.due_ms !== null) return 1;
            return b.modified_ms - a.modified_ms;
          }
          return a.due_ms - b.due_ms;
        });
      case "title":
        // Untitled notes sink to the end.
        return list.sort((a, b) => {
          const ta = titleOf(a.text);
          const tb = titleOf(b.text);
          if (!ta || !tb) return ta ? -1 : tb ? 1 : 0;
          return ta.localeCompare(tb, undefined, { sensitivity: "base" });
        });
      default:
        return list; // backend already sorts by modified, newest first
    }
  });

  // Sections for the grid. A multi-tag note appears under each of its tags.
  const groups = createMemo((): Array<{ key: string; label: string; notes: NoteState[] }> => {
    const list = sorted();
    switch (groupBy()) {
      case "color": {
        const cap = (s: string) => s[0].toUpperCase() + s.slice(1);
        const out = [
          { key: "theme", label: "Theme", notes: list.filter((n) => !n.color) },
          ...COLOR_ORDER.map((c) => ({
            key: c,
            label: cap(c),
            notes: list.filter((n) => n.color === c),
          })),
        ];
        return out.filter((g) => g.notes.length > 0);
      }
      case "tag": {
        const out = tags().map((t) => ({
          key: `#${t.tag}`,
          label: `#${t.tag}`,
          notes: list.filter((n) => n.tags.includes(t.tag)),
        }));
        out.push({ key: "untagged", label: "Untagged", notes: list.filter((n) => n.tags.length === 0) });
        return out.filter((g) => g.notes.length > 0);
      }
      default:
        return [{ key: "all", label: "", notes: list }];
    }
  });

  const toggleTag = (tag: string) => setActiveTag(activeTag() === tag ? null : tag);

  const preview = (n: NoteState) => n.text.trim() || "Empty note";
  const when = (n: NoteState) => new Date(n.modified_ms).toLocaleDateString();

  const armConfirm = (key: string): boolean => {
    if (confirmId() === key) {
      setConfirmId(null);
      return true;
    }
    setConfirmId(key);
    clearTimeout(confirmTimer);
    confirmTimer = window.setTimeout(() => setConfirmId(null), 3000);
    return false;
  };

  const togglePin = async (n: NoteState) => {
    await invoke(n.pinned ? "file_note" : "pin_note", { id: n.id });
    await refresh();
  };

  // Drag a card out of the library: past a small threshold the REAL note
  // window appears under the cursor and follows it (same session as a header
  // drag — release drops it, a fling at an edge tucks it). Pointer capture
  // keeps events coming even once the cursor leaves the library window.
  let cardDrag: { id: string; sx: number; sy: number; started: boolean } | null = null;
  let cardDragStart: Promise<unknown> | null = null;

  const onCardPointerDown = (n: NoteState, e: PointerEvent) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    cardDrag = { id: n.id, sx: e.clientX, sy: e.clientY, started: false };
  };

  const onCardPointerMove = (e: PointerEvent) => {
    if (!cardDrag || cardDrag.started) return;
    if (Math.hypot(e.clientX - cardDrag.sx, e.clientY - cardDrag.sy) < 6) return;
    cardDrag.started = true;
    cardDragStart = invoke("library_pull_start", { id: cardDrag.id });
  };

  // The start command opens a window; a quick release must wait for it or the
  // end call would find no session and the follow thread would run forever.
  const onCardPointerUp = () => {
    if (cardDrag?.started) {
      const started = cardDragStart;
      void (async () => {
        await started?.catch(() => {});
        await invoke("note_drag_end");
        await refresh();
      })();
    }
    cardDrag = null;
    cardDragStart = null;
  };

  const onCardPointerCancel = () => {
    if (cardDrag?.started) {
      const started = cardDragStart;
      void (async () => {
        await started?.catch(() => {});
        await invoke("note_drag_cancel");
      })();
    }
    cardDrag = null;
    cardDragStart = null;
  };

  // ----- multi-select + batch actions -----

  const toggleSelect = (n: NoteState, e: MouseEvent | PointerEvent) => {
    const next = new Set(selected());
    if (e.shiftKey && lastSel) {
      const order = sorted().map((x) => x.id);
      const a = order.indexOf(lastSel);
      const b = order.indexOf(n.id);
      if (a !== -1 && b !== -1) {
        for (let i = Math.min(a, b); i <= Math.max(a, b); i++) next.add(order[i]);
      } else {
        next.add(n.id);
      }
    } else if (next.has(n.id)) {
      next.delete(n.id);
    } else {
      next.add(n.id);
    }
    lastSel = n.id;
    setSelected(next);
  };

  const batch = async (fn: (id: string) => Promise<unknown>, done: (n: number) => string) => {
    const ids = [...selected()];
    for (const id of ids) await fn(id);
    setSelected(new Set<string>());
    flashMessage(done(ids.length));
    await refresh();
  };
  const plural = (n: number) => `${n} note${n === 1 ? "" : "s"}`;
  const batchFavorite = () =>
    batch((id) => invoke("set_favorite", { id, fav: true }), (n) => `Starred ${plural(n)}`);
  const batchArchive = () => {
    const unarchive = libView() === "archive";
    return batch(
      (id) => invoke("set_archived", { id, archived: !unarchive }),
      (n) => `${unarchive ? "Unarchived" : "Archived"} ${plural(n)}`,
    );
  };
  const batchTrash = () =>
    batch((id) => invoke("trash_note", { id }), (n) => `Trashed ${plural(n)}`);
  const batchApplyTag = () => {
    const tag = batchTag().trim().replace(/^#/, "").toLowerCase();
    if (!tag) return;
    setBatchTag("");
    return batch((id) => invoke("tag_note", { id, tag }), (n) => `Tagged ${plural(n)} #${tag}`);
  };

  const toggleFavorite = async (n: NoteState) => {
    await invoke("set_favorite", { id: n.id, fav: !n.favorite });
    await refresh();
  };

  const toggleArchive = async (n: NoteState) => {
    await invoke("set_archived", { id: n.id, archived: !n.archived });
    flashMessage(n.archived ? "Returned to the library" : "Archived — find it under 🗃");
    await refresh();
  };

  const trash = async (n: NoteState) => {
    await invoke("trash_note", { id: n.id });
    await refresh();
  };

  const restore = async (n: NoteState) => {
    await invoke("restore_note", { id: n.id });
    await refresh();
  };

  const purge = async (n: NoteState) => {
    if (!armConfirm(n.id)) return;
    await invoke("purge_note", { id: n.id });
    await refresh();
  };

  const emptyTrash = async () => {
    if (!armConfirm("__all__")) return;
    await invoke("empty_trash");
    setShowTrash(false);
    await refresh();
  };

  return (
    <div class="library">
      <Show when={dropping()}>
        <div class="drop-overlay">Drop to import (.md / .txt / .zip backup)</div>
      </Show>
      <header class="library-topbar">
        <input
          class="library-search"
          type="search"
          placeholder="Search…  (tag:x  is:fav  due:week  has:file)"
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
        <Show when={query().trim()}>
          <button
            class="library-icon-btn"
            title="Save this search to the sidebar"
            aria-label="Save search"
            onClick={saveSearch}
          >
            🔖
          </button>
        </Show>
        <Show when={view() === "cards"}>
          <select
            class="library-select"
            title="Sort notes"
            value={sortBy()}
            onChange={(e) => pickSort(e.currentTarget.value as SortBy)}
          >
            <option value="modified">Recent</option>
            <option value="created">Created</option>
            <option value="title">Title</option>
            <option value="due">Due</option>
          </select>
          <select
            class="library-select"
            title="Group notes"
            value={groupBy()}
            onChange={(e) => pickGroup(e.currentTarget.value as GroupBy)}
          >
            <option value="none">No groups</option>
            <option value="color">By color</option>
            <option value="tag">By tag</option>
          </select>
        </Show>
        <button
          class="library-icon-btn"
          classList={{ active: view() === "graph" }}
          title={view() === "cards" ? "Link map" : "Back to cards"}
          aria-label="Toggle graph view"
          onClick={toggleView}
        >
          {view() === "cards" ? "🕸" : "🗂"}
        </button>
        <button
          class="library-icon-btn"
          title="Export all notes as Markdown"
          aria-label="Export Markdown"
          onClick={() => void exportAll()}
        >
          ⤓
        </button>
        <button
          class="library-icon-btn"
          title="Full backup (.zip with notes + attachments) — drop it back here to restore"
          aria-label="Backup archive"
          onClick={() => void backupAll()}
        >
          🗄
        </button>
        <button
          class="library-icon-btn"
          title="Settings"
          aria-label="Settings"
          onClick={() => void invoke("open_settings")}
        >
          ⚙
        </button>
      </header>

      <div class="library-body">
        <aside class="library-sidebar">
          <For each={LIB_VIEWS}>
            {(v) => (
              <button
                class="lib-view-btn"
                classList={{ active: libView() === v.key }}
                onClick={() => pickLibView(v.key)}
              >
                <span class="lib-view-glyph">{v.glyph}</span>
                <span class="lib-view-label">{v.label}</span>
                <span class="lib-view-count">{viewCounts()[v.key]}</span>
              </button>
            )}
          </For>
          <Show when={savedSearches().length > 0}>
            <div class="lib-side-heading">Saved searches</div>
            <For each={savedSearches()}>
              {(q) => (
                <div class="lib-saved-row">
                  <button class="lib-saved-btn" title={q} onClick={() => setQuery(q)}>
                    🔖 <span>{q}</span>
                  </button>
                  <button class="lib-saved-x" title="Remove" onClick={() => removeSearch(q)}>
                    ✕
                  </button>
                </div>
              )}
            </For>
          </Show>
        </aside>
        <div class="library-main-col">

      <Show when={selected().size > 0}>
        <div class="batch-bar">
          <span class="batch-count">{selected().size} selected</span>
          <button class="library-btn" onClick={() => void batchFavorite()}>
            ★ Favorite
          </button>
          <button class="library-btn" onClick={() => void batchArchive()}>
            {libView() === "archive" ? "Unarchive" : "🗃 Archive"}
          </button>
          <input
            class="batch-tag-input"
            placeholder="#tag"
            value={batchTag()}
            onInput={(e) => setBatchTag(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void batchApplyTag();
            }}
          />
          <button class="library-btn" onClick={() => void batchApplyTag()}>
            Tag
          </button>
          <button class="library-btn library-btn-danger" onClick={() => void batchTrash()}>
            🗑 Trash
          </button>
          <button class="library-btn" onClick={() => setSelected(new Set<string>())}>
            ✕
          </button>
        </div>
      </Show>

      <Show when={tags().length > 0}>
        <div class="tag-chips">
          <button
            class="tag-chip"
            classList={{ active: activeTag() === null }}
            onClick={() => setActiveTag(null)}
          >
            All
          </button>
          <For each={tags()}>
            {(t) => (
              <button
                class="tag-chip"
                classList={{ active: activeTag() === t.tag }}
                onClick={() => toggleTag(t.tag)}
              >
                #{t.tag} <span class="tag-count">{t.count}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={suggestions()[0]}>
        {(s) => (
          <div class="suggest-card">
            <span class="suggest-icon">✨</span>
            <span class="suggest-text">
              {s().notes.length === 1
                ? `“${s().notes[0].title}” looks like `
                : `${s().notes.length} notes look like `}
              <b>#{s().tag}</b>
              <Show when={s().notes.length > 1}>
                <span class="suggest-titles">
                  {" — "}
                  {s()
                    .notes.slice(0, 3)
                    .map((n) => n.title)
                    .join(", ")}
                  {s().notes.length > 3 ? "…" : ""}
                </span>
              </Show>
            </span>
            <button class="library-btn suggest-accept" onClick={() => void acceptSuggestion(s())}>
              Tag {s().notes.length === 1 ? "it" : "them"}
            </button>
            <button
              class="library-btn"
              title="Don't suggest this again for these notes"
              onClick={() => void dismissSuggestion(s())}
            >
              No thanks
            </button>
          </div>
        )}
      </Show>

      <div class="library-list" classList={{ "library-list-graph": view() === "graph" }}>
        <Show
          when={view() === "cards"}
          fallback={
            <Graph
              notes={allActive()}
              edges={linkIndex().edges}
              backlinkCount={linkIndex().backlinkCount}
              dimmed={dimmedIds()}
            />
          }
        >
        <Show
          when={active().length > 0}
          fallback={
            <div class="library-empty">
              <img class="library-empty-mascot" src={mascot} alt="" draggable={false} />
              <p>
                {query().trim() || activeTag()
                  ? "No notes match."
                  : libView() === "inbox"
                    ? "Inbox zero — untagged, unpinned captures land here."
                    : libView() === "favorites"
                      ? "No favorites yet — star a note with ☆."
                      : libView() === "desktop"
                        ? "Nothing pinned on this desktop."
                        : libView() === "archive"
                          ? "The archive is empty."
                          : "No notes yet — pull one off the pad!"}
              </p>
            </div>
          }
        >
          <For each={groups()}>
            {(g) => (
              <section class="stack-group">
                <Show when={groupBy() !== "none"}>
                  <h3 class="stack-group-label">
                    {g.label} <span class="stack-group-count">{g.notes.length}</span>
                  </h3>
                </Show>
                <div class="stack-grid">
                  <For each={g.notes}>
                    {(n) => (
                <div
                  class="sticky-card"
                  classList={{ selected: selected().has(n.id) }}
                  data-theme={n.color ?? undefined}
                  data-paper={n.paper ?? undefined}
                  style={{ "--tilt": `${tilt(n.id)}deg` }}
                  title="Drag out to the desktop — Ctrl+click to select"
                  onPointerDown={(e) => {
                    if (e.ctrlKey || e.metaKey || e.shiftKey) {
                      e.preventDefault();
                      toggleSelect(n, e);
                      return;
                    }
                    onCardPointerDown(n, e);
                  }}
                  onPointerMove={onCardPointerMove}
                  onPointerUp={onCardPointerUp}
                  onPointerCancel={onCardPointerCancel}
                >
                  <Show when={firstImage(n)}>
                    {(img) => (
                      <img
                        class="sticky-card-img"
                        src={convertFileSrc(img().hash, "pinq-blob")}
                        alt={img().name}
                        loading="lazy"
                        draggable={false}
                      />
                    )}
                  </Show>
                  <div class="sticky-card-text" classList={{ empty: !n.text.trim() }}>
                    <For each={linkSegments(stripMd(preview(n)))}>
                      {(s) => (s.link ? <span class="lib-link">{s.text}</span> : s.text)}
                    </For>
                  </div>
                  <div class="sticky-card-meta">
                    <span>
                      {n.pinned ? "📌" : ""}
                      {n.attachments.length > 0 ? ` 📎${n.attachments.length}` : ""}
                      <Show when={n.status}>
                        {(s) => (
                          <span class={`card-status st-${s()}`} title={`Status: ${s()}`}>
                            {" "}
                            {s() === "done" ? "✓" : s() === "doing" ? "◐" : "○"}
                          </span>
                        )}
                      </Show>
                      <Show when={n.due_ms}>
                        {(ms) => (
                          <span
                            class="card-due"
                            classList={{ overdue: ms() < Date.now() && n.status !== "done" }}
                            title={`Due: ${new Date(ms()).toLocaleString()}`}
                          >
                            {" "}
                            📅
                            {new Date(ms()).toLocaleDateString([], { month: "short", day: "numeric" })}
                          </span>
                        )}
                      </Show>
                      <Show when={n.remind_ms}>
                        {(ms) => (
                          <span title={`Reminder: ${new Date(ms()).toLocaleString()}`}> ⏰</span>
                        )}
                      </Show>
                      <Show when={linkIndex().backlinkCount.get(n.id)}>
                        {(c) => (
                          <span
                            class="sticky-card-links"
                            title={`${c()} note${c() === 1 ? "" : "s"} link here`}
                          >
                            {" "}
                            🔗{c()}
                          </span>
                        )}
                      </Show>
                    </span>
                    <span>{when(n)}</span>
                  </div>
                  <div class="sticky-card-actions">
                    <button
                      class="library-btn card-star"
                      classList={{ faved: n.favorite }}
                      title={n.favorite ? "Unstar" : "Star"}
                      onClick={() => void toggleFavorite(n)}
                    >
                      {n.favorite ? "★" : "☆"}
                    </button>
                    <button
                      class="library-btn"
                      onClick={() => void togglePin(n)}
                      title={n.pinned ? "Unpin from desktop" : "Pin to desktop"}
                    >
                      {n.pinned ? "Unpin" : "Pin"}
                    </button>
                    <button
                      class="library-btn"
                      title={n.archived ? "Return to the library" : "Archive (put away, keep forever)"}
                      onClick={() => void toggleArchive(n)}
                    >
                      {n.archived ? "Unarchive" : "🗃"}
                    </button>
                    <button
                      class="library-btn library-btn-danger"
                      title="Move to trash (recoverable)"
                      onClick={() => void trash(n)}
                    >
                      🗑
                    </button>
                  </div>
                </div>
                    )}
                  </For>
                </div>
              </section>
            )}
          </For>
        </Show>
        </Show>
      </div>
        </div>
      </div>

      <footer class="library-footer">
        <div class="library-status-bar">
          <span class="library-count">
            {message() ??
              (query().trim()
                ? `${active().length} of ${activeTotal()} notes`
                : `${activeTotal()} notes`)}
          </span>
          <Show when={trashedTotal() > 0}>
            <div class="library-trash-controls">
              <button class="library-trash-toggle" onClick={() => setShowTrash(!showTrash())}>
                {showTrash() ? "▾" : "▸"} Trash ({trashedTotal()})
              </button>
              <button
                class="library-btn library-btn-danger"
                classList={{ confirming: confirmId() === "__all__" }}
                onClick={() => void emptyTrash()}
              >
                {confirmId() === "__all__" ? "Click again to confirm" : "Empty Trash"}
              </button>
            </div>
          </Show>
        </div>
        <Show when={trashedTotal() > 0 && showTrash()}>
          <div class="library-trash-list">
              <For each={trashed()}>
                {(n) => (
                  <div class="library-row library-row-trashed">
                    <div class="library-swatch" data-theme={n.color ?? undefined} />
                    <div class="library-main">
                      <div class="library-title" classList={{ empty: !n.text.trim() }}>
                        {stripMd(stripLinks(preview(n).split("\n")[0])).slice(0, 80)}
                      </div>
                      <div class="library-meta">In trash · {when(n)}</div>
                    </div>
                    <button class="library-btn" onClick={() => void restore(n)}>
                      Restore
                    </button>
                    <button
                      class="library-btn library-btn-danger"
                      classList={{ confirming: confirmId() === n.id }}
                      onClick={() => void purge(n)}
                    >
                      {confirmId() === n.id ? "Click again to confirm" : "Delete forever"}
                    </button>
                  </div>
                )}
            </For>
          </div>
        </Show>
      </footer>
    </div>
  );
}
