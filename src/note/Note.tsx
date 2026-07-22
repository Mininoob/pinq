import { createEffect, createMemo, createSignal, For, Index, onCleanup, onMount, Show } from "solid-js";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { setLocalNotePaper, setLocalTheme } from "../theme";
import { TAG_COMPLETE } from "../tags";

interface AttachmentRef {
  hash: string;
  name: string;
  mime: string;
  size: number;
}

type CanvasItem =
  | { item_id: string; x: number; y: number; w: number; type: "text"; text: string }
  | { item_id: string; x: number; y: number; w: number; type: "attachment"; attachment: AttachmentRef };

interface NoteState {
  id: string;
  text: string;
  color: string | null;
  paper: string | null;
  tucked: string | null;
  remind_ms: number | null;
  tags: string[];
  items: CanvasItem[];
}

interface HistoryEntry {
  at_ms: number;
  text: string;
}

import { LINK_RE, stripLinks } from "../links";
import { mdRuns, stripMd } from "../md";

interface LinkTarget {
  id: string;
  title: string;
  pinned: boolean;
}

const blobUrl = (hash: string) => convertFileSrc(hash, "pinq-blob");

const COLORS = ["pinq", "amber", "mint", "sky", "rose", "violet", "graphite"];
const PAPERS = ["plain", "ruled", "dotted", "grid", "dashed", "wavy", "zigzag", "hearts", "stars"];

// Checklist glyphs — plain text, so they show in cards and export to Markdown.
const BOX = "☐";
const CHECKED = "☑";
// Plain-list bullets ("-> " and "* " markdown shortcuts).
const ARROW = "→";
const DOT = "•";

// What a new line under this one should start with, if it's a list line.
const continuationFor = (line: string): string | null => {
  if (line.startsWith(`${BOX} `) || line.startsWith(`${CHECKED} `)) return `${BOX} `;
  if (line.startsWith(`${ARROW} `)) return `${ARROW} `;
  if (line.startsWith(`${DOT} `)) return `${DOT} `;
  return null;
};

// Each fresh note greets you with a random one of these.
const PHRASES = [
  "Jot it down…",
  "Catch that thought!",
  "Before it slips away…",
  "Note to self…",
  "Brain dump welcome",
  "What's the big idea?",
  "Quick! While it's fresh",
  "Sticky thoughts go here",
  "Small note, big plans",
  "Future you says thanks",
  "Ideas stick here",
  "Spill it",
  "Don't forget the thing",
  "A tiny note for future you",
  "Today's plot twist:",
  "Genius starts here",
  "Scribble something",
  "Whisper it to the note",
  "This space intentionally left blank (fix that)",
  "The pen is mightier. So is the pad.",
  "Okay, brain, go.",
  "One thought at a time",
  "Fresh square, fresh start",
  "Make it stick",
  "What did we come in here for?",
  "The napkin of the digital age",
  "Your move, muse",
  "Shh… it's listening",
  "Half-baked ideas welcome",
  "No thought too small",
  "Remember remember…",
  "Do the thing. Write the thing.",
  "It counted if you wrote it down",
  "Paper trail starts here",
  "Insert stroke of genius",
  "For your eyes only",
  "The to-do begins",
  "Something something…",
  "Save it for later, write it now",
  "Loose thoughts, tighten here",
  "A home for homeless ideas",
  "Squirrel it away",
  "Beep boop, note time",
  "Tell me everything",
  "Plot your plots",
  "The sticky abides",
  "Blank canvas, tiny frame",
  "Grocery list? Manifesto? Dealer's choice.",
  "Words go brrr",
  "Snack-sized thoughts",
  "Stick with it",
  "Now you're thinking with stickies",
  "First draft of everything",
  "Whatever it is, it fits",
  "Mark my words (literally)",
  "Present you, meet future you",
  "Zero judgment zone",
  "Ink it before you think it twice",
  "Little note, listening closely",
  "The best ideas fit on a sticky",
  "Type like nobody's reading",
  "It's not procrastinating if you write it down",
  "Add to the pile of brilliance",
  "One small note for you…",
  "Scrawl space available",
  "Thought incoming?",
  "Hold that thought — right here",
  "Your desk called, it wants notes",
  "Doodle in words",
  "New note smell",
  "Say it before it evaporates",
  "Every list starts somewhere",
  "Where thoughts get comfy",
  "Ready when you are",
  "Psst… write here",
  "Instructions unclear, write anyway",
  "The mighty morning reminder",
  "Three words or three hundred",
  "Once upon a sticky…",
  "That thing you'll forget? This is for it.",
  "Museum of tiny ideas, new exhibit",
  "Warning: may contain brilliance",
  "Sticky note, reporting for duty",
  "Go on, we're all ears",
  "It's a great day to remember stuff",
];

// Random pick that avoids the last few phrases shown — shared across all note
// windows via localStorage so back-to-back pulls never repeat.
const RECENT_KEY = "pinq.recentPhrases";
const RECENT_MAX = 5;

const randomPhrase = () => {
  let recent: number[] = [];
  try {
    recent = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
  } catch {
    /* fresh start */
  }
  let pick = Math.floor(Math.random() * PHRASES.length);
  while (recent.includes(pick)) {
    pick = Math.floor(Math.random() * PHRASES.length);
  }
  recent = [pick, ...recent].slice(0, RECENT_MAX);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
  } catch {
    /* storage unavailable — random alone is fine */
  }
  return PHRASES[pick];
};

export default function Note(props: { id: string }) {
  const [items, setItems] = createSignal<CanvasItem[]>([]);
  const [tags, setTags] = createSignal<string[]>([]);
  const [color, setColor] = createSignal<string | null>(null);
  const [paper, setPaper] = createSignal<string | null>(null);
  const [showPalette, setShowPalette] = createSignal(false);
  const [showHistory, setShowHistory] = createSignal(false);
  const [showRemind, setShowRemind] = createSignal(false);
  const [remind, setRemind] = createSignal<number | null>(null);
  const [history, setHistory] = createSignal<HistoryEntry[]>([]);
  const [tucked, setTucked] = createSignal<string | null>(null);
  const placeholder = randomPhrase();
  let canvas!: HTMLDivElement;
  let dragging = false;
  let focusNext: string | null = null;
  // Only the note's very first (auto-seeded) text box greets with a phrase.
  let seededId: string | null = null;
  let unlisten: UnlistenFn | undefined;
  let unlistenDrop: UnlistenFn | undefined;
  let unlistenSync: UnlistenFn | undefined;
  let unlistenTuck: UnlistenFn | undefined;
  let unlistenSmart: UnlistenFn | undefined;

  // First non-empty line of the first text box — the tucked note's tab label.
  const tuckTitle = () => {
    for (const i of items()) {
      if (i.type === "text") {
        const line = i.text.split("\n").find((l) => l.trim());
        if (line) return stripMd(stripLinks(line.trim())).slice(0, 40);
      }
    }
    return "Sticky note";
  };

  const untuck = async () => {
    setTucked(null);
    await invoke("untuck_note", { id: props.id });
  };

  // ----- [[note links]] -----

  const [linkMenu, setLinkMenu] = createSignal<{
    itemId: string;
    query: string;
    targets: LinkTarget[];
    sel: number;
    x: number;
    y: number;
  } | null>(null);
  const [backlinks, setBacklinks] = createSignal<LinkTarget[]>([]);
  // The textarea the open link menu belongs to.
  let linkEl: HTMLTextAreaElement | null = null;

  const refreshBacklinks = async () =>
    setBacklinks(await invoke<LinkTarget[]>("get_backlinks", { id: props.id }));

  // Semantically similar notes (smart suggestions feature) — [] when it's off.
  const [related, setRelated] = createSignal<LinkTarget[]>([]);
  // Ghost chips: tags this note could carry, and a concise title when the
  // first line runs on like a sentence. Empty when the feature is off.
  const [suggest, setSuggest] = createSignal<{ tags: string[]; title: string | null }>({
    tags: [],
    title: null,
  });
  const refreshRelated = async () => {
    try {
      setRelated(await invoke<LinkTarget[]>("related_notes", { id: props.id }));
      setSuggest(
        await invoke<{ tags: string[]; title: string | null }>("note_suggestions", {
          id: props.id,
        }),
      );
    } catch {
      setRelated([]);
      setSuggest({ tags: [], title: null });
    }
  };

  const acceptSuggestedTag = (tag: string) => {
    setTags([...tags(), tag]);
    setSuggest({ ...suggest(), tags: suggest().tags.filter((t) => t !== tag) });
    void invoke("tag_note", { id: props.id, tag });
  };

  const dismissSuggestedTag = (tag: string) => {
    setSuggest({ ...suggest(), tags: suggest().tags.filter((t) => t !== tag) });
    void invoke("dismiss_tag_suggestion", { tag, ids: [props.id] });
  };

  const acceptSuggestedTitle = async () => {
    const title = suggest().title;
    if (!title) return;
    setSuggest({ ...suggest(), title: null });
    await flushAll();
    await invoke("apply_title", { id: props.id, title });
    const note = await invoke<NoteState | null>("get_note", { id: props.id });
    if (note) setItems(note.items);
  };

  const dismissSuggestedTitle = () => {
    setSuggest({ ...suggest(), title: null });
    void invoke("dismiss_title_suggestion", { id: props.id });
  };

  // An unclosed "[[query" immediately before the caret, if any.
  const linkContext = (v: string, caret: number) => {
    const open = v.lastIndexOf("[[", caret - 1);
    if (open === -1) return null;
    const between = v.slice(open + 2, caret);
    if (/[\][\n]/.test(between)) return null;
    return { open, query: between };
  };

  const updateLinkMenu = (item: CanvasItem, el: HTMLTextAreaElement) => {
    const ctx = linkContext(el.value, el.selectionStart);
    if (!ctx) {
      if (linkMenu()) setLinkMenu(null);
      return;
    }
    linkEl = el;
    const host = el.closest(".canvas-item") as HTMLElement | null;
    const x = host?.offsetLeft ?? item.x;
    const y = (host?.offsetTop ?? item.y) + (host?.offsetHeight ?? 24) + 2;
    const q = ctx.query;
    void invoke<LinkTarget[]>("link_targets", { query: q }).then((targets) => {
      // Stale reply guard: only show if the caret still sits in this context.
      const now = linkContext(el.value, el.selectionStart);
      if (!now || now.query !== q) return;
      setLinkMenu({
        itemId: item.item_id,
        query: q,
        targets: targets.filter((t) => t.id !== props.id),
        sel: 0,
        x,
        y,
      });
    });
  };

  // Menu rows: matching notes, plus a "link anyway" row for a fresh title
  // (the note gets created wiki-style on first click of the link).
  const linkRows = (m: NonNullable<ReturnType<typeof linkMenu>>) => {
    const rows: Array<{ title: string; create: boolean }> = m.targets.map((t) => ({
      title: t.title,
      create: false,
    }));
    const q = m.query.trim();
    if (q && !m.targets.some((t) => t.title.toLowerCase() === q.toLowerCase())) {
      rows.push({ title: q, create: true });
    }
    return rows;
  };

  const acceptLink = (title: string) => {
    const m = linkMenu();
    const el = linkEl;
    setLinkMenu(null);
    if (!m || !el) return;
    const item = items().find((i) => i.item_id === m.itemId);
    if (!item) return;
    const v = el.value;
    const caret = el.selectionStart;
    const ctx = linkContext(v, caret);
    if (!ctx) return;
    // Swallow a "]]" already sitting after the caret so we don't double it.
    const end = v.slice(caret, caret + 2) === "]]" ? caret + 2 : caret;
    const insert = `[[${title}]] `;
    applyTextTo(item, el, v.slice(0, ctx.open) + insert + v.slice(end), ctx.open + insert.length);
  };

  const followLink = async (title: string) => {
    await flushAll();
    await invoke("open_link", { fromId: props.id, title: title.trim() });
  };

  // While Ctrl (or ⌘ on mac) is held, rendered [[links]] become clickable.
  const [ctrlHeld, setCtrlHeld] = createSignal(false);
  const trackCtrl = (e: KeyboardEvent) => setCtrlHeld(e.ctrlKey || e.metaKey);
  const dropCtrl = () => setCtrlHeld(false);

  // Platform-neutral command modifier: Ctrl on Windows/Linux, ⌘ on mac.
  const cmdKey = (e: KeyboardEvent | MouseEvent) => e.ctrlKey || e.metaKey;

  // Split text into plain runs and [[link]] tokens for the styled mirror.
  const renderSegments = (text: string) => {
    const parts: Array<{ text: string; link: boolean; offset: number }> = [];
    let last = 0;
    for (const m of text.matchAll(LINK_RE)) {
      const i = m.index ?? 0;
      if (i > last) parts.push({ text: text.slice(last, i), link: false, offset: last });
      parts.push({ text: m[0], link: true, offset: i });
      last = i + m[0].length;
    }
    if (last < text.length) parts.push({ text: text.slice(last), link: false, offset: last });
    return parts;
  };

  // ----- find-in-note (Ctrl+F) -----

  const [findOpen, setFindOpen] = createSignal(false);
  const [findQuery, setFindQuery] = createSignal("");
  const [findIdx, setFindIdx] = createSignal(0);
  let findInput: HTMLInputElement | undefined;

  // Every occurrence of the query across the note's text boxes, in item order.
  const findMatches = createMemo(() => {
    const q = findQuery().toLowerCase();
    if (!findOpen() || !q) return [];
    const out: Array<{ itemId: string; start: number; end: number }> = [];
    for (const i of items()) {
      if (i.type !== "text") continue;
      const t = i.text.toLowerCase();
      let p = 0;
      while ((p = t.indexOf(q, p)) !== -1) {
        out.push({ itemId: i.item_id, start: p, end: p + q.length });
        p += q.length;
      }
    }
    return out;
  });

  // Keep the cursor valid as matches change, and keep the current one in view.
  createEffect(() => {
    if (findIdx() >= findMatches().length) setFindIdx(0);
  });
  createEffect(() => {
    findIdx();
    if (findMatches().length === 0) return;
    queueMicrotask(() =>
      canvas
        .querySelector(".find-hit.current")
        ?.scrollIntoView({ block: "nearest", inline: "nearest" }),
    );
  });

  const openFind = () => {
    setFindOpen(true);
    queueMicrotask(() => {
      findInput?.focus();
      findInput?.select();
    });
  };

  const closeFind = () => setFindOpen(false);

  const findStep = (d: number) => {
    const len = findMatches().length;
    if (len > 0) setFindIdx((findIdx() + d + len) % len);
  };

  const onFindKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      findStep(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      closeFind();
    }
  };

  // Wrap the query hits inside one mirror run with <mark>s. `offset` is the
  // run's start within its item's full text (match positions are item-global).
  const withMarks = (str: string, offset: number, itemId: string) => {
    const all = findMatches();
    const hits = all.filter(
      (m) => m.itemId === itemId && m.end > offset && m.start < offset + str.length,
    );
    if (hits.length === 0) return str;
    const cur = all[findIdx()];
    const out: any[] = [];
    let p = 0;
    for (const m of hits) {
      const s = Math.max(0, m.start - offset);
      const e = Math.min(str.length, m.end - offset);
      if (s > p) out.push(str.slice(p, s));
      out.push(
        <mark class="find-hit" classList={{ current: m === cur }}>
          {str.slice(s, e)}
        </mark>,
      );
      p = e;
    }
    if (p < str.length) out.push(str.slice(p));
    return out;
  };

  // Per-item debounced text commits.
  const timers = new Map<string, number>();
  const pendingText = new Map<string, string>();

  const commitItem = async (itemId: string) => {
    const text = pendingText.get(itemId);
    if (text === undefined) return;
    pendingText.delete(itemId);
    await invoke("set_item_text", { id: props.id, itemId, text });
  };

  const scheduleCommit = (itemId: string, text: string) => {
    pendingText.set(itemId, text);
    clearTimeout(timers.get(itemId));
    timers.set(itemId, window.setTimeout(() => void commitItem(itemId), 400));
  };

  const flushAll = async () => {
    for (const itemId of [...pendingText.keys()]) {
      clearTimeout(timers.get(itemId));
      await commitItem(itemId);
    }
  };

  const updateItem = (itemId: string, patch: Partial<CanvasItem>) =>
    setItems(items().map((i) => (i.item_id === itemId ? ({ ...i, ...patch } as CanvasItem) : i)));

  const canvasPoint = (clientX: number, clientY: number) => {
    const r = canvas.getBoundingClientRect();
    return {
      x: Math.max(0, clientX - r.left + canvas.scrollLeft),
      y: Math.max(0, clientY - r.top + canvas.scrollTop),
    };
  };

  const newTextItemAt = async (x: number, y: number) => {
    const w = Math.max(120, Math.min(220, canvas.clientWidth - x - 16));
    const item = await invoke<CanvasItem>("add_text_item", {
      id: props.id,
      x,
      y,
      w,
      text: "",
    });
    focusNext = item.item_id;
    setItems([...items(), item]);
  };

  onMount(async () => {
    const note = await invoke<NoteState | null>("get_note", { id: props.id });
    if (note) {
      setItems(note.items);
      setTags(note.tags);
      setColor(note.color);
      setPaper(note.paper);
      setTucked(note.tucked);
      setRemind(note.remind_ms ?? null);
      setLocalTheme(note.color ?? undefined);
      setLocalNotePaper(note.paper ?? undefined);
    }
    // A fresh note starts with one text box, caret ready — pull and type.
    if (!note || note.items.length === 0) {
      await newTextItemAt(8, 8);
      seededId = items()[0]?.item_id ?? null;
    }
    // A throw is about to move this window; save any pending edits first.
    unlisten = await listen("pinq-flush", () => void flushAll());
    // Thrown at an edge: switch to the tucked tab presentation.
    unlistenTuck = await listen<string>("pinq-tucked", (e) => setTucked(e.payload));
    // A sync session changed this note on another device: reload it. Items the
    // user is mid-typing (uncommitted debounce) keep their local text.
    unlistenSync = await listen<string[]>("pinq-sync-applied", async (e) => {
      // Any remote change can add/remove links pointing here.
      void refreshBacklinks();
      if (!e.payload.includes(props.id)) return;
      const note = await invoke<NoteState | null>("get_note", { id: props.id });
      if (!note) return;
      setTags(note.tags);
      setColor(note.color);
      setPaper(note.paper);
      setTucked(note.tucked);
      setRemind(note.remind_ms ?? null);
      setLocalTheme(note.color ?? undefined);
      setLocalNotePaper(note.paper ?? undefined);
      setItems(
        note.items.map((i) =>
          i.type === "text" && pendingText.has(i.item_id)
            ? { ...i, text: pendingText.get(i.item_id)! }
            : i,
        ),
      );
    });
    void refreshBacklinks();
    void refreshRelated();
    // Freshly indexed embeddings can change who's related.
    unlistenSmart = await listen("pinq-smart-updated", () => void refreshRelated());
    // Edits on other notes change who links here; catch up when we're revisited.
    window.addEventListener("focus", onWindowFocus);
    window.addEventListener("keydown", trackCtrl);
    window.addEventListener("keyup", trackCtrl);
    window.addEventListener("blur", dropCtrl);
    // Files dropped onto the note land where they were dropped.
    unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type !== "drop") return;
      const pos = event.payload.position;
      const scale = window.devicePixelRatio || 1;
      const p = canvasPoint(pos.x / scale, pos.y / scale);
      const paths = event.payload.paths;
      void (async () => {
        const added = await invoke<CanvasItem[]>("attach_files", {
          id: props.id,
          paths,
          x: p.x,
          y: p.y,
        });
        if (added.length) setItems([...items(), ...added]);
      })();
    });
  });

  const onWindowFocus = () => {
    void refreshBacklinks();
    void refreshRelated();
  };

  onCleanup(() => {
    window.removeEventListener("focus", onWindowFocus);
    window.removeEventListener("keydown", trackCtrl);
    window.removeEventListener("keyup", trackCtrl);
    window.removeEventListener("blur", dropCtrl);
    unlisten?.();
    unlistenDrop?.();
    unlistenSync?.();
    unlistenTuck?.();
    unlistenSmart?.();
    clearTimeout(undoChipTimer);
  });

  // ----- text smarts (per item) -----

  const lineStartOf = (v: string, pos: number) => v.lastIndexOf("\n", pos - 1) + 1;

  const applyTextTo = (item: CanvasItem, el: HTMLTextAreaElement, v: string, caret: number) => {
    el.value = v;
    el.setSelectionRange(caret, caret);
    updateItem(item.item_id, { text: v });
    fitTextItem(item, el);
    scheduleCommit(item.item_id, v);
  };

  const autosize = (el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  };

  // Text boxes hug their content: width follows the longest line (up to the
  // canvas edge), so the drag bar is exactly as wide as the text.
  let measurer: HTMLDivElement | undefined;
  const textWidth = (text: string) => {
    if (!measurer) {
      measurer = document.createElement("div");
      measurer.style.cssText =
        "position:absolute;left:-9999px;top:0;visibility:hidden;display:inline-block;" +
        "white-space:pre;font:14px/20px 'Segoe UI',system-ui,sans-serif";
      document.body.appendChild(measurer);
    }
    measurer.textContent = text || " ";
    return measurer.getBoundingClientRect().width;
  };

  const fitTextItem = (item: CanvasItem, el: HTMLTextAreaElement) => {
    const maxW = Math.max(80, canvas.clientWidth - item.x - 14);
    const min = el.value ? 60 : 160; // empty boxes stay wide enough for a placeholder
    const w = Math.min(maxW, Math.max(min, Math.ceil(textWidth(el.value)) + 20));
    if (Math.abs(w - item.w) > 1) updateItem(item.item_id, { w });
    autosize(el);
  };

  const extractCompletedTags = (
    item: CanvasItem,
    el: HTMLTextAreaElement,
    v: string,
    caret: number,
  ): boolean => {
    const found = [...v.matchAll(TAG_COMPLETE)];
    if (found.length === 0) return false;

    let out = "";
    let last = 0;
    let removedBefore = 0;
    const newTags: string[] = [];
    for (const m of found) {
      const start = m.index + m[1].length;
      let end = start + 1 + m[2].length;
      if (v[end] === " ") end += 1;
      out += v.slice(last, start);
      last = end;
      removedBefore += Math.max(0, Math.min(caret, end) - start);
      newTags.push(m[2].toLowerCase());
    }
    out += v.slice(last);
    applyTextTo(item, el, out, Math.max(0, caret - removedBefore));

    const current = tags();
    const added = [...new Set(newTags)].filter((t) => !current.includes(t));
    if (added.length) {
      setTags([...current, ...added]);
      for (const tag of added) void invoke("tag_note", { id: props.id, tag });
    }
    return true;
  };

  const onItemInput = (item: CanvasItem, e: InputEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const v = el.value;
    const caret = el.selectionStart;
    const ls = lineStartOf(v, caret);
    const subs: Array<[string, string]> = [
      ["[] ", `${BOX} `],
      ["[ ] ", `${BOX} `],
      ["[x] ", `${CHECKED} `],
      ["[X] ", `${CHECKED} `],
      ["-> ", `${ARROW} `],
      ["* ", `${DOT} `],
    ];
    for (const [pat, rep] of subs) {
      if (v.slice(ls).startsWith(pat)) {
        applyTextTo(
          item,
          el,
          v.slice(0, ls) + rep + v.slice(ls + pat.length),
          Math.max(ls + rep.length, caret - (pat.length - rep.length)),
        );
        return;
      }
    }
    if (extractCompletedTags(item, el, v, caret)) return;
    updateItem(item.item_id, { text: v });
    fitTextItem(item, el);
    scheduleCommit(item.item_id, v);
    updateLinkMenu(item, el);
  };

  const onItemKeyDown = (item: CanvasItem, e: KeyboardEvent) => {
    // While the [[link menu is up it owns the navigation keys.
    const m = linkMenu();
    if (m) {
      const rows = linkRows(m);
      if ((e.key === "ArrowDown" || e.key === "ArrowUp") && rows.length > 0) {
        e.preventDefault();
        const d = e.key === "ArrowDown" ? 1 : -1;
        setLinkMenu({ ...m, sel: (m.sel + d + rows.length) % rows.length });
        return;
      }
      if ((e.key === "Enter" || e.key === "Tab") && rows.length > 0) {
        e.preventDefault();
        acceptLink(rows[Math.min(m.sel, rows.length - 1)].title);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setLinkMenu(null);
        return;
      }
    }
    if (e.key !== "Enter") return;
    const el = e.currentTarget as HTMLTextAreaElement;
    const pos = el.selectionStart;
    if (pos !== el.selectionEnd) return;
    const v = el.value;
    const ls = lineStartOf(v, pos);
    const line = v.slice(ls, pos);
    const cont = continuationFor(line);
    if (cont) {
      e.preventDefault();
      if (line.trim() === cont.trim()) {
        applyTextTo(item, el, v.slice(0, ls) + v.slice(pos), ls);
      } else {
        applyTextTo(item, el, `${v.slice(0, pos)}\n${cont}${v.slice(pos)}`, pos + 1 + cont.length);
      }
    }
  };

  const onItemClick = (item: CanvasItem, e: MouseEvent) => {
    const el = e.currentTarget as HTMLTextAreaElement;
    const pos = el.selectionStart;
    const v = el.value;
    const ls = lineStartOf(v, pos);
    const ch = v[ls];
    if ((ch === BOX || ch === CHECKED) && pos <= ls + 1) {
      applyTextTo(item, el, v.slice(0, ls) + (ch === BOX ? CHECKED : BOX) + v.slice(ls + 1), ls + 2);
    }
    // [[links]] are followed via Ctrl+click on the styled mirror spans.
  };

  // Empty text boxes vanish when you leave them.
  const onItemBlur = (item: CanvasItem, e: FocusEvent) => {
    setLinkMenu(null);
    const el = e.currentTarget as HTMLTextAreaElement;
    if (el.value.trim() === "") {
      pendingText.delete(item.item_id);
      clearTimeout(timers.get(item.item_id));
      setItems(items().filter((i) => i.item_id !== item.item_id));
      void invoke("remove_item", { id: props.id, itemId: item.item_id });
    } else {
      void commitItem(item.item_id);
    }
  };

  const regTextRef = (el: HTMLTextAreaElement, item: CanvasItem) => {
    queueMicrotask(() => {
      fitTextItem(item, el);
      if (focusNext === item.item_id) {
        focusNext = null;
        el.focus();
      }
    });
  };

  // ----- item dragging -----

  // Media items drag by their body: a small threshold separates click (open)
  // from drag (move), so photos feel directly grabbable.
  const startMediaDrag = (e: PointerEvent, item: CanvasItem, onClick?: () => void) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const px = e.clientX;
    const py = e.clientY;
    const ox = item.x;
    const oy = item.y;
    let moved = false;

    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - px;
      const dy = ev.clientY - py;
      if (!moved && Math.hypot(dx, dy) < 4) return;
      moved = true;
      updateItem(item.item_id, { x: Math.max(0, ox + dx), y: Math.max(0, oy + dy) });
    };
    const finish = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", finish);
      el.removeEventListener("pointercancel", finish);
      if (moved) {
        const it = items().find((i) => i.item_id === item.item_id);
        if (it) {
          void invoke("move_item", { id: props.id, itemId: it.item_id, x: it.x, y: it.y, w: it.w });
        }
      } else {
        onClick?.();
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", finish);
    el.addEventListener("pointercancel", finish);
  };

  // Corner resize for attachments: width changes, images keep their aspect.
  const startResize = (e: PointerEvent, item: CanvasItem) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const px = e.clientX;
    const ow = item.w;
    const maxW = Math.max(60, canvas.clientWidth - item.x - 8);

    const move = (ev: PointerEvent) => {
      updateItem(item.item_id, {
        w: Math.max(48, Math.min(maxW, ow + ev.clientX - px)),
      });
    };
    const finish = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", finish);
      el.removeEventListener("pointercancel", finish);
      const it = items().find((i) => i.item_id === item.item_id);
      if (it) {
        void invoke("move_item", { id: props.id, itemId: it.item_id, x: it.x, y: it.y, w: it.w });
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", finish);
    el.addEventListener("pointercancel", finish);
  };

  const startItemDrag = (e: PointerEvent, item: CanvasItem) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    handle.setPointerCapture(e.pointerId);
    const px = e.clientX;
    const py = e.clientY;
    const ox = item.x;
    const oy = item.y;

    const move = (ev: PointerEvent) => {
      updateItem(item.item_id, {
        x: Math.max(0, ox + ev.clientX - px),
        y: Math.max(0, oy + ev.clientY - py),
      });
    };
    const finish = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      const it = items().find((i) => i.item_id === item.item_id);
      if (it) {
        void invoke("move_item", { id: props.id, itemId: it.item_id, x: it.x, y: it.y, w: it.w });
      }
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  };

  // ----- canvas-level interactions -----

  const onCanvasDblClick = (e: MouseEvent) => {
    if (e.target !== canvas) return;
    const p = canvasPoint(e.clientX, e.clientY);
    void newTextItemAt(Math.max(0, p.x - 4), Math.max(0, p.y - 10));
  };

  // Pasting an image drops it onto the canvas (text pastes go to the textarea).
  const onPaste = (e: ClipboardEvent) => {
    const item = [...(e.clipboardData?.items ?? [])].find(
      (i) => i.kind === "file" && i.type.startsWith("image/"),
    );
    if (!item) return;
    e.preventDefault();
    const file = item.getAsFile();
    if (!file) return;
    const mime = item.type;
    const n = items().length;
    const reader = new FileReader();
    reader.onload = () => {
      const data = String(reader.result).split(",", 2)[1] ?? "";
      void (async () => {
        const added = await invoke<CanvasItem>("attach_image_base64", {
          id: props.id,
          data,
          mime,
          x: 16 + (n % 5) * 22,
          y: 40 + (n % 5) * 22,
        });
        setItems([...items(), added]);
      })();
    };
    reader.readAsDataURL(file);
  };

  // ----- undo for item deletes (session-scoped, per note window) -----
  // Empty-box blur removals never land here — nothing is lost there.
  const UNDO_CAP = 20;
  const [undoStack, setUndoStack] = createSignal<CanvasItem[]>([]);
  const [lastDeleted, setLastDeleted] = createSignal<CanvasItem | null>(null);
  let undoChipTimer: number | undefined;

  const removeCanvasItem = (item: CanvasItem) => {
    // Snapshot for undo, folding in any not-yet-committed debounced edit so
    // deleting right after typing restores the latest text.
    const pending = pendingText.get(item.item_id);
    const snapshot: CanvasItem =
      item.type === "text" && pending !== undefined ? { ...item, text: pending } : item;
    setUndoStack([...undoStack().slice(-(UNDO_CAP - 1)), snapshot]);
    setLastDeleted(snapshot);
    clearTimeout(undoChipTimer);
    undoChipTimer = window.setTimeout(() => setLastDeleted(null), 6000);
    // Drop any in-flight debounced edit for this box before it disappears.
    clearTimeout(timers.get(item.item_id));
    timers.delete(item.item_id);
    pendingText.delete(item.item_id);
    setItems(items().filter((i) => i.item_id !== item.item_id));
    void invoke("remove_item", { id: props.id, itemId: item.item_id });
  };

  const undoDelete = () => {
    const stack = undoStack();
    const item = stack[stack.length - 1];
    if (!item) return;
    setUndoStack(stack.slice(0, -1));
    if (lastDeleted()?.item_id === item.item_id) {
      setLastDeleted(null);
      clearTimeout(undoChipTimer);
    }
    if (items().some((i) => i.item_id === item.item_id)) return;
    if (item.type === "text") focusNext = item.item_id;
    setItems([...items(), item]);
    void invoke("restore_item", { id: props.id, item });
  };

  const openAtt = (a: AttachmentRef) =>
    void invoke("open_attachment", { hash: a.hash, name: a.name });

  // ----- note-level commands (unchanged) -----

  const untag = (tag: string) => {
    setTags(tags().filter((t) => t !== tag));
    void invoke("untag_note", { id: props.id, tag });
  };

  // ----- reminders -----

  const applyReminder = (ms: number | null) => {
    setRemind(ms);
    setShowRemind(false);
    void invoke("set_reminder", { id: props.id, remindMs: ms });
  };

  const remindPresets = (): Array<{ label: string; ms: number }> => {
    const now = new Date();
    const inOneHour = now.getTime() + 3600_000;
    const evening = new Date(now);
    evening.setHours(18, 0, 0, 0);
    if (evening.getTime() <= now.getTime()) evening.setDate(evening.getDate() + 1);
    const tomorrow = new Date(now);
    tomorrow.setDate(tomorrow.getDate() + 1);
    tomorrow.setHours(9, 0, 0, 0);
    return [
      { label: "In 1 hour", ms: inOneHour },
      { label: evening.getDate() === now.getDate() ? "This evening (6 PM)" : "Tomorrow evening (6 PM)", ms: evening.getTime() },
      { label: "Tomorrow 9 AM", ms: tomorrow.getTime() },
    ];
  };

  // Local-timezone value for <input type="datetime-local">.
  const toLocalInput = (ms: number) => {
    const d = new Date(ms - new Date(ms).getTimezoneOffset() * 60_000);
    return d.toISOString().slice(0, 16);
  };

  const remindLabel = () => {
    const ms = remind();
    if (!ms) return "";
    const d = new Date(ms);
    const today = new Date();
    const sameDay = d.toDateString() === today.toDateString();
    const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    return sameDay ? time : `${d.toLocaleDateString([], { month: "short", day: "numeric" })} ${time}`;
  };

  const pickColor = async (c: string | null) => {
    setColor(c);
    setLocalTheme(c ?? undefined);
    await invoke("set_note_color", { id: props.id, color: c });
  };

  const pickPaper = async (p: string | null) => {
    setPaper(p);
    setLocalNotePaper(p ?? undefined);
    await invoke("set_note_paper", { id: props.id, paper: p });
  };

  const openHistory = async () => {
    setHistory(await invoke<HistoryEntry[]>("get_note_history", { id: props.id }));
    setShowPalette(false);
    setShowHistory(true);
  };

  // Restore replaces the text boxes with one holding the old text (attachments
  // stay); it appends a new version — history is never rewritten.
  const restoreVersion = async (h: HistoryEntry) => {
    await flushAll();
    await invoke("save_note_text", { id: props.id, text: h.text });
    const note = await invoke<NoteState | null>("get_note", { id: props.id });
    if (note) setItems(note.items);
    setShowHistory(false);
  };

  const fileAway = async () => {
    await flushAll();
    await invoke("file_note", { id: props.id });
  };

  // One-click copy: the note's full text lands on the clipboard, paste-ready.
  const [copied, setCopied] = createSignal(false);
  let copiedTimer: number | undefined;
  const copyNote = async () => {
    await flushAll();
    const text = items()
      .filter((i): i is Extract<CanvasItem, { type: "text" }> => i.type === "text")
      .map((i) => i.text.trim())
      .filter(Boolean)
      .join("\n\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // WebView2 fallback when the async clipboard is unavailable.
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    setCopied(true);
    clearTimeout(copiedTimer);
    copiedTimer = window.setTimeout(() => setCopied(false), 1200);
  };

  const trashAway = async () => {
    await flushAll();
    await invoke("trash_note", { id: props.id });
  };

  const onHeaderPointerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
    void invoke("note_drag_start", { id: props.id });
  };

  const onHeaderPointerUp = () => {
    if (!dragging) return;
    dragging = false;
    void invoke("note_drag_end");
  };

  const cancelDrag = () => {
    if (!dragging) return;
    dragging = false;
    void invoke("note_drag_cancel");
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      cancelDrag();
      setShowHistory(false);
      setShowPalette(false);
      setShowRemind(false);
      closeFind();
    }
    if (cmdKey(e) && !e.shiftKey && e.key.toLowerCase() === "f") {
      e.preventDefault();
      openFind();
    }
    if (e.key === "U" && cmdKey(e) && e.shiftKey) {
      e.preventDefault();
      void fileAway();
    }
    // Ctrl/⌘+Z outside a textarea restores the last deleted item; inside one,
    // the browser's native text undo proceeds untouched.
    if (cmdKey(e) && !e.shiftKey && e.key.toLowerCase() === "z") {
      if ((e.target as HTMLElement).tagName !== "TEXTAREA" && undoStack().length > 0) {
        e.preventDefault();
        undoDelete();
      }
    }
  };

  const AttachmentView = (p: { item: () => Extract<CanvasItem, { type: "attachment" }> }) => {
    const a = () => p.item().attachment;
    const kind = () => {
      const m = a().mime;
      if (m.startsWith("image/")) return "image";
      if (m.startsWith("video/")) return "video";
      if (m.startsWith("audio/")) return "audio";
      return "file";
    };
    return (
      <>
        <Show when={kind() === "image"}>
          <img
            class="att-img"
            src={blobUrl(a().hash)}
            alt={a().name}
            title={`${a().name} — drag to move, click to view`}
            loading="lazy"
            draggable={false}
            onPointerDown={(e) =>
              startMediaDrag(e, p.item(), () =>
                void invoke("open_viewer", { noteId: props.id, hash: a().hash }),
              )
            }
          />
        </Show>
        <Show when={kind() === "video"}>
          <video class="att-video" controls preload="metadata" src={blobUrl(a().hash)} />
        </Show>
        <Show when={kind() === "audio"}>
          <audio class="att-audio" controls src={blobUrl(a().hash)} />
        </Show>
        <Show when={kind() === "file"}>
          <button
            class="att-chip"
            title={`${a().name} — drag to move, click to open`}
            onPointerDown={(e) => startMediaDrag(e, p.item(), () => openAtt(a()))}
          >
            📎 <span class="att-chip-name">{a().name}</span>
          </button>
        </Show>
      </>
    );
  };

  return (
    <div
      class="note"
      classList={{ "note-tucked": !!tucked(), "ctrl-down": ctrlHeld() }}
      onKeyDown={onKeyDown}
      onPaste={onPaste}
    >
      <Show when={tucked()}>
        {(edge) => (
          <div
            class="tuck-overlay"
            data-edge={edge()}
            onMouseEnter={() => void invoke("peek_note", { id: props.id, out: true })}
            onMouseLeave={() => void invoke("peek_note", { id: props.id, out: false })}
            onClick={() => void untuck()}
            title="Click to pull the note back out"
          >
            <span class="tuck-title">{tuckTitle()}</span>
          </div>
        )}
      </Show>
      <header
        class="note-header"
        onPointerDown={onHeaderPointerDown}
        onPointerUp={onHeaderPointerUp}
        onPointerCancel={cancelDrag}
      >
        <button
          class="note-trash"
          title="Move to trash (recoverable)"
          onClick={() => void trashAway()}
        >
          🗑
        </button>
        <button
          class="note-color"
          title="Color & paper"
          onClick={() => setShowPalette(!showPalette())}
        />
        <div class="note-grip">
          <span /><span /><span />
        </div>
        <button
          class="note-remind-btn"
          classList={{ armed: remind() !== null }}
          title={remind() ? `Reminder set for ${remindLabel()}` : "Remind me…"}
          onClick={() => {
            setShowRemind(!showRemind());
            setShowPalette(false);
            setShowHistory(false);
          }}
        >
          ⏰
        </button>
        <button
          class="note-copy"
          classList={{ copied: copied() }}
          title="Copy note text"
          onClick={() => void copyNote()}
        >
          {copied() ? "✓" : "⧉"}
        </button>
        <button class="note-history-btn" title="History" onClick={() => void openHistory()}>
          🕐
        </button>
        <button
          class="note-file"
          title="File to library (kept, unpinned) — Ctrl+Shift+U"
          onClick={() => void fileAway()}
        >
          ⤵
        </button>
      </header>
      <Show when={tags().length > 0 || suggest().tags.length > 0 || suggest().title}>
        <div class="note-tags">
          <For each={tags()}>
            {(t) => (
              <button class="note-tag" title={`Remove #${t}`} onClick={() => untag(t)}>
                #{t} <span class="note-tag-x">✕</span>
              </button>
            )}
          </For>
          <For each={suggest().tags}>
            {(t) => (
              <span class="note-tag note-tag-ghost">
                <button
                  class="note-tag-add"
                  title={`Suggested tag — click to add #${t}`}
                  onClick={() => acceptSuggestedTag(t)}
                >
                  +#{t}
                </button>
                <button
                  class="note-tag-x"
                  title="Don't suggest this tag here again"
                  onClick={() => dismissSuggestedTag(t)}
                >
                  ✕
                </button>
              </span>
            )}
          </For>
          <Show when={suggest().title}>
            {(title) => (
              <span class="note-tag note-tag-ghost note-title-ghost">
                <button
                  class="note-tag-add"
                  title={`Suggested title — click to add "${title()}" as the first line`}
                  onClick={() => void acceptSuggestedTitle()}
                >
                  ✎ {title()}
                </button>
                <button
                  class="note-tag-x"
                  title="Don't suggest a title for this note"
                  onClick={dismissSuggestedTitle}
                >
                  ✕
                </button>
              </span>
            )}
          </Show>
        </div>
      </Show>
      <Show when={showPalette()}>
        <div class="note-palette">
          <div class="note-palette-row">
            <button
              class="note-palette-dot auto"
              classList={{ active: color() === null }}
              title="Follow theme"
              onClick={() => void pickColor(null)}
            >
              A
            </button>
            <For each={COLORS}>
              {(c) => (
                <button
                  class="note-palette-dot"
                  classList={{ active: color() === c }}
                  data-theme={c}
                  title={c}
                  onClick={() => void pickColor(c)}
                />
              )}
            </For>
          </div>
          <div class="note-palette-row">
            <button
              class="note-paper-chip auto"
              classList={{ active: paper() === null }}
              title="Default paper"
              onClick={() => void pickPaper(null)}
            >
              A
            </button>
            <For each={PAPERS}>
              {(p) => (
                <button
                  class={`note-paper-chip paper-tex-${p}`}
                  classList={{ active: paper() === p }}
                  title={p}
                  onClick={() => void pickPaper(p)}
                />
              )}
            </For>
          </div>
        </div>
      </Show>
      <Show when={showRemind()}>
        <div class="note-remind">
          <Show when={remind()}>
            {(ms) => (
              <div class="note-remind-current">
                ⏰ {new Date(ms()).toLocaleString([], {
                  month: "short",
                  day: "numeric",
                  hour: "numeric",
                  minute: "2-digit",
                })}
                <button class="note-remind-clear" onClick={() => applyReminder(null)}>
                  Clear
                </button>
              </div>
            )}
          </Show>
          <For each={remindPresets()}>
            {(p) => (
              <button class="note-remind-option" onClick={() => applyReminder(p.ms)}>
                {p.label}
              </button>
            )}
          </For>
          <div class="note-remind-custom">
            <input
              class="note-remind-input"
              type="datetime-local"
              value={remind() ? toLocalInput(remind()!) : ""}
              onChange={(e) => {
                const v = e.currentTarget.value;
                if (!v) return;
                const ms = new Date(v).getTime();
                if (!Number.isNaN(ms) && ms > Date.now()) applyReminder(ms);
              }}
            />
          </div>
        </div>
      </Show>
      <div class="note-surface">
        <Show when={findOpen()}>
          <div class="note-find">
            <span class="note-find-glyph">🔍</span>
            <input
              ref={findInput}
              class="note-find-input"
              type="search"
              placeholder="find…"
              value={findQuery()}
              onInput={(e) => {
                setFindQuery(e.currentTarget.value);
                setFindIdx(0);
              }}
              onKeyDown={onFindKeyDown}
            />
            <span class="note-find-count">
              {findQuery()
                ? `${findMatches().length ? findIdx() + 1 : 0}/${findMatches().length}`
                : ""}
            </span>
            <button class="note-find-btn" title="Previous (Shift+Enter)" onClick={() => findStep(-1)}>
              ‹
            </button>
            <button class="note-find-btn" title="Next (Enter)" onClick={() => findStep(1)}>
              ›
            </button>
            <button class="note-find-btn" title="Close (Esc)" onClick={closeFind}>
              ✕
            </button>
          </div>
        </Show>
        <div class="note-canvas" ref={canvas} onDblClick={onCanvasDblClick}>
          {/* Index (not For): rows are keyed by position, so editing or moving
              an item mutates the same DOM node instead of recreating it —
              recreation would drop focus and pointer capture mid-gesture. */}
          <Index each={items()}>
            {(item) => (
              <div
                class="canvas-item"
                classList={{
                  "is-text": item().type === "text",
                  "is-player":
                    item().type === "attachment" &&
                    ((item() as Extract<CanvasItem, { type: "attachment" }>).attachment.mime.startsWith("video/") ||
                      (item() as Extract<CanvasItem, { type: "attachment" }>).attachment.mime.startsWith("audio/")),
                }}
                style={{ left: `${item().x}px`, top: `${item().y}px`, width: `${item().w}px` }}
              >
                <div
                  class="item-handle"
                  title="Drag to move"
                  onPointerDown={(e) => startItemDrag(e, item())}
                />
                <button class="att-x" title="Remove" onClick={() => removeCanvasItem(item())}>
                  ✕
                </button>
                <Show when={item().type === "attachment"}>
                  <div
                    class="item-resize"
                    title="Drag to resize"
                    onPointerDown={(e) => startResize(e, item())}
                  />
                </Show>
                <Show
                  when={item().type === "text"}
                  fallback={<AttachmentView item={item as () => Extract<CanvasItem, { type: "attachment" }>} />}
                >
                  {/* The textarea's glyphs are transparent; the mirror redraws
                      them on top (same font/box) so [[links]] can be styled. */}
                  <div class="item-text-wrap">
                    <textarea
                      class="item-text"
                      ref={(el) => regTextRef(el, item())}
                      value={(item() as Extract<CanvasItem, { type: "text" }>).text}
                      placeholder={item().item_id === seededId ? placeholder : ""}
                      rows={1}
                      spellcheck={true}
                      onInput={(e) => onItemInput(item(), e)}
                      onKeyDown={(e) => onItemKeyDown(item(), e)}
                      onClick={(e) => onItemClick(item(), e)}
                      onBlur={(e) => onItemBlur(item(), e)}
                    />
                    <div class="item-text-render" aria-hidden="true">
                      <For each={renderSegments((item() as Extract<CanvasItem, { type: "text" }>).text)}>
                        {(seg) =>
                          seg.link ? (
                            <span
                              class="wiki-link"
                              title="Ctrl+click to open (⌘+click on mac)"
                              onClick={(e) => {
                                if (!cmdKey(e)) return;
                                e.preventDefault();
                                void followLink(seg.text.slice(2, -2));
                              }}
                            >
                              <span class="wiki-brackets">[[</span>
                              {withMarks(seg.text.slice(2, -2), seg.offset + 2, item().item_id)}
                              <span class="wiki-brackets">]]</span>
                            </span>
                          ) : (
                            <For each={mdRuns(seg.text, seg.offset)}>
                              {(r) =>
                                r.cls ? (
                                  <span class={r.cls}>
                                    {withMarks(r.text, r.offset, item().item_id)}
                                  </span>
                                ) : (
                                  withMarks(r.text, r.offset, item().item_id)
                                )
                              }
                            </For>
                          )
                        }
                      </For>
                    </div>
                  </div>
                </Show>
              </div>
            )}
          </Index>
          <Show when={items().length === 0}>
            <div class="canvas-hint">
              {placeholder}
              <span>double-click to write · paste or drop files</span>
            </div>
          </Show>
          <Show when={linkMenu()}>
            {(m) => (
              <Show when={linkRows(m()).length > 0}>
                {/* pointerdown is swallowed so the textarea keeps focus. */}
                <div
                  class="link-menu"
                  style={{ left: `${m().x}px`, top: `${m().y}px` }}
                  onPointerDown={(e) => e.preventDefault()}
                >
                  <For each={linkRows(m())}>
                    {(row, i) => (
                      <button
                        class="link-menu-row"
                        classList={{ selected: m().sel === i() }}
                        onPointerEnter={() => setLinkMenu({ ...m(), sel: i() })}
                        onClick={() => acceptLink(row.title)}
                      >
                        {row.title}
                        <Show when={row.create}>
                          <span class="link-menu-new">new</span>
                        </Show>
                      </button>
                    )}
                  </For>
                </div>
              </Show>
            )}
          </Show>
        </div>
        <Show when={showHistory()}>
          <div class="note-history">
            <header class="note-history-header">
              <span>History</span>
              <button class="note-history-close" onClick={() => setShowHistory(false)}>
                ✕
              </button>
            </header>
            <div class="note-history-list">
              <For
                each={history()}
                fallback={<p class="note-history-empty">No earlier versions yet.</p>}
              >
                {(h) => (
                  <button class="note-history-row" onClick={() => void restoreVersion(h)}>
                    <span class="note-history-time">{new Date(h.at_ms).toLocaleString()}</span>
                    <span class="note-history-preview">
                      {stripMd(stripLinks(h.text.split("\n").find((l) => l.trim()) ?? "(empty)"))}
                    </span>
                  </button>
                )}
              </For>
            </div>
          </div>
        </Show>
      </div>
      <Show when={lastDeleted()}>
        <button class="note-undo-chip" onClick={undoDelete}>
          Deleted — <b>Undo</b>
        </button>
      </Show>
      <Show when={backlinks().length > 0}>
        <div class="note-backlinks" title="Notes that link here">
          <span class="backlinks-label">↩</span>
          <For each={backlinks()}>
            {(b) => (
              <button
                class="backlink-chip"
                title={`Open “${stripLinks(b.title)}”`}
                onClick={() => void invoke("open_note", { id: b.id })}
              >
                {stripLinks(b.title)}
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={related().length > 0}>
        <div class="note-backlinks note-related" title="Similar notes (found on-device)">
          <span class="backlinks-label">≈</span>
          <For each={related()}>
            {(r) => (
              <button
                class="backlink-chip"
                title={`Open “${stripLinks(r.title)}”`}
                onClick={() => void invoke("open_note", { id: r.id })}
              >
                {stripLinks(r.title)}
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
