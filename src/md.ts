// Inline-markdown accents for the note mirror. CRITICAL CONSTRAINT: the
// mirror must keep EXACTLY the textarea's glyph layout, so every style these
// runs map to must be width-neutral — faux-bold via text-shadow, colors,
// backgrounds, line-through. Never real bold/italic faces or font sizes.
// Markers (** ` ~~ #) are dimmed, not hidden: they stay visible for editing
// and their glyphs always stay in the layout.

export interface MdRun {
  text: string;
  /** Offset of this run within the ORIGINAL item text (for find marks). */
  offset: number;
  /** Space-separated mirror classes, or null for plain text. */
  cls: string | null;
}

// ** before * so bold wins; all inner groups forbid newlines and their own
// delimiter, mirroring how people actually type on sticky notes.
const INLINE_RE = /\*\*([^*\n]+)\*\*|`([^`\n]+)`|~~([^~\n]+)~~|\*([^*\n]+)\*/g;
const HEADER_RE = /^(#{1,3})[ \t]/;

const pushInline = (out: MdRun[], text: string, offset: number, extra: string) => {
  let last = 0;
  for (const m of text.matchAll(INLINE_RE)) {
    const i = m.index ?? 0;
    if (i > last) out.push({ text: text.slice(last, i), offset: offset + last, cls: extra || null });
    const token = m[0];
    const [delim, cls] = token.startsWith("**")
      ? [2, "md-bold"] as const
      : token.startsWith("~~")
        ? [2, "md-strike"] as const
        : token.startsWith("`")
          ? [1, "md-code"] as const
          : [1, "md-italic"] as const;
    const markerCls = `md-marker ${extra}`.trim();
    const innerCls = `${cls} ${extra}`.trim();
    out.push({ text: token.slice(0, delim), offset: offset + i, cls: markerCls });
    out.push({ text: token.slice(delim, token.length - delim), offset: offset + i + delim, cls: innerCls });
    out.push({ text: token.slice(token.length - delim), offset: offset + i + token.length - delim, cls: markerCls });
    last = i + token.length;
  }
  if (last < text.length) out.push({ text: text.slice(last), offset: offset + last, cls: extra || null });
};

/** Split a text run into markdown-styled sub-runs (offsets preserved). */
export const mdRuns = (text: string, offset: number): MdRun[] => {
  const out: MdRun[] = [];
  let pos = 0;
  // Line-by-line so `# headers` style whole lines; the \n stays a plain run.
  while (pos <= text.length) {
    const nl = text.indexOf("\n", pos);
    const end = nl === -1 ? text.length : nl;
    const line = text.slice(pos, end);
    const h = HEADER_RE.exec(line);
    if (h) {
      out.push({ text: h[0], offset: offset + pos, cls: "md-marker md-h" });
      pushInline(out, line.slice(h[0].length), offset + pos + h[0].length, "md-h");
    } else {
      pushInline(out, line, offset + pos, "");
    }
    if (nl === -1) break;
    out.push({ text: "\n", offset: offset + nl, cls: null });
    pos = nl + 1;
  }
  return out.filter((r) => r.text.length > 0);
};

/** Markdown markers removed, for one-line plain previews. */
export const stripMd = (s: string) =>
  s
    .replace(/\*\*([^*\n]+)\*\*/g, "$1")
    .replace(/~~([^~\n]+)~~/g, "$1")
    .replace(/`([^`\n]+)`/g, "$1")
    .replace(/(^|[^*])\*([^*\n]+)\*(?!\*)/g, "$1$2")
    .replace(/^#{1,3}[ \t]+/gm, "");
