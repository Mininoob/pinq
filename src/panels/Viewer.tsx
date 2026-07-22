import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface AttachmentRef {
  hash: string;
  name: string;
  mime: string;
  size: number;
}

interface NoteState {
  id: string;
  attachments: AttachmentRef[];
}

const blobUrl = (hash: string) => convertFileSrc(hash, "pinq-blob");

const MIN_Z = 0.1;
const MAX_Z = 16;

export default function Viewer(props: { noteId: string; hash: string }) {
  const [images, setImages] = createSignal<AttachmentRef[]>([]);
  const [idx, setIdx] = createSignal(0);
  const [zoom, setZoom] = createSignal(1);
  const [pan, setPan] = createSignal({ x: 0, y: 0 });
  const [rot, setRot] = createSignal(0);
  const [mode, setMode] = createSignal<"fit" | "fill" | "actual" | "free">("fit");
  const [natural, setNatural] = createSignal({ w: 1, h: 1 });
  let stage!: HTMLDivElement;
  let unlisten: UnlistenFn | undefined;

  const current = () => images()[idx()];

  const load = async (noteId: string, hash: string) => {
    const note = await invoke<NoteState | null>("get_note", { id: noteId });
    const imgs = (note?.attachments ?? []).filter((a) => a.mime.startsWith("image/"));
    setImages(imgs);
    const i = imgs.findIndex((a) => a.hash === hash);
    setIdx(i >= 0 ? i : 0);
    setRot(0);
    setMode("fit");
  };

  onMount(async () => {
    await load(props.noteId, props.hash);
    unlisten = await listen<{ noteId: string; hash: string }>("pinq-view", (e) => {
      void load(e.payload.noteId, e.payload.hash);
    });
  });

  onCleanup(() => unlisten?.());

  // Fit/fill account for rotation (90°/270° swap the effective dimensions).
  const fittedZoom = (kind: "fit" | "fill") => {
    const n = natural();
    const swapped = rot() % 180 !== 0;
    const iw = swapped ? n.h : n.w;
    const ih = swapped ? n.w : n.h;
    const cw = stage.clientWidth - 24;
    const ch = stage.clientHeight - 24;
    const zx = cw / iw;
    const zy = ch / ih;
    return Math.max(MIN_Z, Math.min(MAX_Z, kind === "fit" ? Math.min(zx, zy) : Math.max(zx, zy)));
  };

  const applyMode = (m: "fit" | "fill" | "actual") => {
    setMode(m);
    setPan({ x: 0, y: 0 });
    setZoom(m === "actual" ? 1 : fittedZoom(m));
  };

  const onImgLoad = (e: Event) => {
    const el = e.currentTarget as HTMLImageElement;
    setNatural({ w: el.naturalWidth || 1, h: el.naturalHeight || 1 });
    applyMode(mode() === "free" ? "fit" : (mode() as "fit" | "fill" | "actual"));
  };

  // Wheel zoom anchored at the cursor.
  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    const r = stage.getBoundingClientRect();
    const cx = e.clientX - r.left - r.width / 2;
    const cy = e.clientY - r.top - r.height / 2;
    const z = zoom();
    const nz = Math.max(MIN_Z, Math.min(MAX_Z, z * (e.deltaY < 0 ? 1.12 : 1 / 1.12)));
    const p = pan();
    setPan({ x: cx - ((cx - p.x) * nz) / z, y: cy - ((cy - p.y) * nz) / z });
    setZoom(nz);
    setMode("free");
  };

  const onStagePointerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const sx = e.clientX;
    const sy = e.clientY;
    const op = pan();
    const move = (ev: PointerEvent) =>
      setPan({ x: op.x + ev.clientX - sx, y: op.y + ev.clientY - sy });
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };

  const step = (dir: number) => {
    if (images().length < 2) return;
    setIdx((idx() + dir + images().length) % images().length);
    setRot(0);
    applyMode("fit");
  };

  const rotate = () => {
    setRot((rot() + 90) % 360);
    if (mode() !== "free" && mode() !== "actual") applyMode(mode() as "fit" | "fill");
  };

  const zoomBy = (f: number) => {
    setZoom(Math.max(MIN_Z, Math.min(MAX_Z, zoom() * f)));
    setMode("free");
  };

  const close = () => void invoke("close_viewer");

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") close();
    else if (e.key === "ArrowLeft") step(-1);
    else if (e.key === "ArrowRight") step(1);
    else if (e.key === "+" || e.key === "=") zoomBy(1.25);
    else if (e.key === "-") zoomBy(1 / 1.25);
    else if (e.key.toLowerCase() === "f") applyMode("fit");
    else if (e.key.toLowerCase() === "l") applyMode("fill");
    else if (e.key === "1") applyMode("actual");
    else if (e.key.toLowerCase() === "r") rotate();
  };

  return (
    <div class="viewer" tabIndex={0} onKeyDown={onKeyDown} ref={(el) => queueMicrotask(() => el.focus())}>
      <header class="viewer-bar" data-tauri-drag-region>
        <span class="viewer-name" data-tauri-drag-region>
          {current()?.name ?? ""}
        </span>
        <span class="viewer-zoom">{Math.round(zoom() * 100)}%</span>
        <button classList={{ active: mode() === "fit" }} onClick={() => applyMode("fit")} title="Fit (F)">
          Fit
        </button>
        <button classList={{ active: mode() === "fill" }} onClick={() => applyMode("fill")} title="Fill (L)">
          Fill
        </button>
        <button classList={{ active: mode() === "actual" }} onClick={() => applyMode("actual")} title="Actual size (1)">
          1:1
        </button>
        <button onClick={rotate} title="Rotate (R)">
          ⟳
        </button>
        <button
          onClick={() =>
            current() && void invoke("open_attachment", { hash: current().hash, name: current().name })
          }
          title="Open in default app"
        >
          ↗
        </button>
        <button onClick={close} title="Close (Esc)">
          ✕
        </button>
      </header>
      <div
        class="viewer-stage"
        ref={stage}
        onWheel={onWheel}
        onPointerDown={onStagePointerDown}
        onDblClick={() => applyMode(mode() === "actual" ? "fit" : "actual")}
      >
        <Show when={current()}>
          <img
            class="viewer-img"
            src={blobUrl(current().hash)}
            alt={current().name}
            draggable={false}
            onLoad={onImgLoad}
            style={{
              transform: `translate(${pan().x}px, ${pan().y}px) scale(${zoom()}) rotate(${rot()}deg)`,
            }}
          />
        </Show>
      </div>
      <Show when={images().length > 1}>
        <footer class="viewer-strip">
          <For each={images()}>
            {(img, i) => (
              <img
                class="viewer-thumb"
                classList={{ active: i() === idx() }}
                src={blobUrl(img.hash)}
                alt={img.name}
                loading="lazy"
                onClick={() => {
                  setIdx(i());
                  setRot(0);
                  applyMode("fit");
                }}
              />
            )}
          </For>
        </footer>
      </Show>
    </div>
  );
}
