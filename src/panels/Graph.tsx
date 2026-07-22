// Link map: every non-trashed note as a node, [[wiki links]] as edges, laid
// out by a tiny force simulation (no deps). Positions live OUTSIDE Solid
// reactivity in a plain Map keyed by note id, so the library's 2s poll never
// resets a settled layout — a per-frame `tick` signal drives the SVG instead.
import { createEffect, createMemo, createSignal, Index, onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { stripLinks, titleOf } from "../links";
import { stripMd } from "../md";

export interface GraphNote {
  id: string;
  text: string;
  color: string | null;
}

interface Body {
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Set while dragged: the sim pins the node here. */
  fx?: number;
  fy?: number;
}

// Sim tuning: springs pull linked notes to ~rest distance, every pair repels,
// mild gravity keeps stray clusters on screen. Alpha cools each frame and the
// RAF loop stops entirely once settled.
const REST = 96;
const REPEL = 2600;
const SPRING = 0.04;
const GRAVITY = 0.012;
const DAMPING = 0.85;
const ALPHA_START = 1;
const ALPHA_REHEAT = 0.5;
const ALPHA_MIN = 0.003;
const ALPHA_DECAY = 0.975;

const LABEL_MAX = 18;
const label = (text: string) => {
  const t = stripMd(stripLinks(titleOf(text)));
  if (!t) return "(untitled)";
  return t.length > LABEL_MAX ? `${t.slice(0, LABEL_MAX - 1)}…` : t;
};

// Deterministic seed angle per id so a note reappears where it first spawned.
const idHash = (id: string) => {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h);
};

export default function Graph(props: {
  notes: GraphNote[];
  edges: Array<{ from: string; to: string }>;
  backlinkCount: Map<string, number>;
  /** Ids failing the library's search/tag filter — dimmed, not removed. */
  dimmed: Set<string>;
}) {
  const bodies = new Map<string, Body>();
  const [tick, setTick] = createSignal(0);
  let host!: HTMLDivElement;
  let raf = 0;
  let alpha = 0;
  let signature = "";

  const size = () => ({
    w: Math.max(200, host?.clientWidth ?? 600),
    h: Math.max(200, host?.clientHeight ?? 400),
  });

  const linked = createMemo(() => {
    const s = new Set<string>();
    for (const e of props.edges) {
      s.add(e.from);
      s.add(e.to);
    }
    return s;
  });

  const step = () => {
    const { w, h } = size();
    const cx = w / 2;
    const cy = h / 2;
    const list = [...bodies.values()];

    // Pairwise repulsion. O(n²) but a few hundred notes is well under 1ms.
    for (let i = 0; i < list.length; i++) {
      for (let j = i + 1; j < list.length; j++) {
        const a = list[i];
        const b = list[j];
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        const d2 = Math.max(64, dx * dx + dy * dy);
        const f = (REPEL / d2) * alpha;
        const d = Math.sqrt(d2);
        dx /= d;
        dy /= d;
        a.vx += dx * f;
        a.vy += dy * f;
        b.vx -= dx * f;
        b.vy -= dy * f;
      }
    }
    // Springs along edges.
    for (const e of props.edges) {
      const a = bodies.get(e.from);
      const b = bodies.get(e.to);
      if (!a || !b) continue;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const d = Math.max(1, Math.hypot(dx, dy));
      const f = SPRING * (d - REST) * alpha;
      a.vx += (dx / d) * f;
      a.vy += (dy / d) * f;
      b.vx -= (dx / d) * f;
      b.vy -= (dy / d) * f;
    }
    // Gravity toward the center + integrate.
    for (const b of bodies.values()) {
      b.vx += (cx - b.x) * GRAVITY * alpha;
      b.vy += (cy - b.y) * GRAVITY * alpha;
      b.vx *= DAMPING;
      b.vy *= DAMPING;
      if (b.fx !== undefined && b.fy !== undefined) {
        b.x = b.fx;
        b.y = b.fy;
        b.vx = 0;
        b.vy = 0;
      } else {
        b.x = Math.min(w - 16, Math.max(16, b.x + b.vx));
        b.y = Math.min(h - 16, Math.max(16, b.y + b.vy));
      }
    }
    alpha *= ALPHA_DECAY;
    setTick(tick() + 1);
    raf = alpha > ALPHA_MIN ? requestAnimationFrame(step) : 0;
  };

  const reheat = (to: number) => {
    alpha = Math.max(alpha, to);
    if (!raf) raf = requestAnimationFrame(step);
  };

  // Seed/drop bodies as notes come and go; only an actual node/edge change
  // reheats, so the 2s refresh leaves a settled layout alone.
  createEffect(() => {
    const { w, h } = size();
    const ids = new Set(props.notes.map((n) => n.id));
    for (const id of [...bodies.keys()]) if (!ids.has(id)) bodies.delete(id);
    for (const n of props.notes) {
      if (bodies.has(n.id)) continue;
      const a = (idHash(n.id) % 360) * (Math.PI / 180);
      const r = Math.min(w, h) * 0.28;
      bodies.set(n.id, {
        x: w / 2 + Math.cos(a) * r,
        y: h / 2 + Math.sin(a) * r,
        vx: 0,
        vy: 0,
      });
    }
    const sig =
      [...ids].sort().join(",") +
      "|" +
      props.edges.map((e) => `${e.from}>${e.to}`).sort().join(",");
    if (sig !== signature) {
      signature = sig;
      reheat(ALPHA_REHEAT);
      setTick(tick() + 1);
    }
  });

  onMount(() => reheat(ALPHA_START));

  onCleanup(() => {
    if (raf) cancelAnimationFrame(raf);
  });

  // Drag to reposition; a press that barely moves opens the note instead.
  let drag: { id: string; sx: number; sy: number; moved: boolean } | null = null;

  const onNodePointerDown = (id: string, e: PointerEvent) => {
    if (e.button !== 0) return;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { id, sx: e.clientX, sy: e.clientY, moved: false };
    const b = bodies.get(id);
    if (b) {
      b.fx = b.x;
      b.fy = b.y;
    }
  };

  const onNodePointerMove = (e: PointerEvent) => {
    if (!drag) return;
    if (Math.hypot(e.clientX - drag.sx, e.clientY - drag.sy) >= 4) drag.moved = true;
    if (!drag.moved) return;
    const b = bodies.get(drag.id);
    if (b) {
      const r = host.getBoundingClientRect();
      b.fx = e.clientX - r.left;
      b.fy = e.clientY - r.top;
      reheat(0.15);
    }
  };

  const onNodePointerUp = () => {
    if (!drag) return;
    const b = bodies.get(drag.id);
    if (b) {
      b.fx = undefined;
      b.fy = undefined;
    }
    if (!drag.moved) void invoke("open_note", { id: drag.id });
    drag = null;
  };

  const at = (id: string) => {
    tick();
    return bodies.get(id);
  };

  const radius = (id: string) => 8 + Math.min(5, props.backlinkCount.get(id) ?? 0);

  return (
    <div class="graph-host" ref={host}>
      {/* <Index>, not <For>: the 2s poll rebuilds these arrays, and position-
          keyed reuse keeps the SVG elements (and any pointer capture on a
          dragged node) alive across refreshes. */}
      <svg class="graph-svg" width="100%" height="100%">
        <Index each={props.edges}>
          {(e) => (
            <line
              class="graph-edge"
              classList={{ dim: props.dimmed.has(e().from) || props.dimmed.has(e().to) }}
              x1={at(e().from)?.x ?? 0}
              y1={at(e().from)?.y ?? 0}
              x2={at(e().to)?.x ?? 0}
              y2={at(e().to)?.y ?? 0}
            />
          )}
        </Index>
        <Index each={props.notes}>
          {(n) => (
            <g
              class="graph-node"
              data-theme={n().color ?? undefined}
              classList={{
                isolated: !linked().has(n().id),
                dim: props.dimmed.has(n().id),
              }}
              transform={`translate(${at(n().id)?.x ?? 0}, ${at(n().id)?.y ?? 0})`}
              onPointerDown={(e) => onNodePointerDown(n().id, e)}
              onPointerMove={onNodePointerMove}
              onPointerUp={onNodePointerUp}
              onPointerCancel={onNodePointerUp}
            >
              <title>{titleOf(n().text) || "(untitled)"} — click to open</title>
              <circle class="graph-dot" r={radius(n().id)} />
              <text class="graph-label" y={radius(n().id) + 12}>
                {label(n().text)}
              </text>
            </g>
          )}
        </Index>
      </svg>
    </div>
  );
}
