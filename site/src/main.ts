import "./styles.css";

/*
 * One sticky note, driven by scroll.
 *
 * rip      the top sheet's coral corner curls back, then it tears off the pad
 * travel   it drifts from the pad to the stage
 * chapter  it becomes each thing a Pinq note can be (01–06), scrubbed by scroll
 * land     it drops into the empty slot in the library
 *
 * Everything is a pure function of scroll position, so scrolling backwards
 * plays it backwards. With reduced motion the class `motion` is never added
 * and the page shows each chapter's static figure instead.
 */

const reduce = window.matchMedia("(prefers-reduced-motion: reduce)");
const narrowQ = window.matchMedia("(max-width: 899px)");

type Pose = { x: number; y: number; w: number; h: number; r: number; c: number };

const clamp = (v: number, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);
const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const lerpPose = (a: Pose, b: Pose, t: number): Pose => ({
  x: lerp(a.x, b.x, t),
  y: lerp(a.y, b.y, t),
  w: lerp(a.w, b.w, t),
  h: lerp(a.h, b.h, t),
  r: lerp(a.r, b.r, t),
  c: lerp(a.c, b.c, t),
});
const rectPose = (el: Element, r = 0, cFrac = 0.12): Pose => {
  const b = el.getBoundingClientRect();
  return { x: b.left, y: b.top, w: b.width, h: b.height, r, c: b.width * cFrac };
};

function start() {
  const note = document.getElementById("note") as HTMLElement;
  const twin = document.getElementById("twin") as HTMLElement;
  const toast = document.getElementById("toast") as HTMLElement;
  const content = note.querySelector(".t-content") as HTMLElement;
  const outside = note.querySelector(".t-outside") as HTMLElement;
  const twinContent = twin.querySelector(".t-content") as HTMLElement;
  const ledger = [...note.querySelectorAll<HTMLElement>(".ledger li")];
  const heroSlot = document.querySelector(".stack-top") as HTMLElement;
  const stageSlot = document.querySelector(".stage-slot") as HTMLElement;
  const landSlot = document.getElementById("land-slot") as HTMLElement;
  const chapters = [...document.querySelectorAll<HTMLElement>(".chapter")];
  const legend = [...document.querySelectorAll<HTMLAnchorElement>(".legend a")];
  const names = ["write", "tasks", "files", "links", "throw", "sync"];

  let shown = -2; // content currently mounted: -1 = the bare sticky face
  let queued = false;

  // ---- content mounting: each chapter's static figure is the template ----
  const mount = (k: number) => {
    if (k === shown) return;
    shown = k;
    content.replaceChildren();
    outside.replaceChildren();
    note.classList.remove("is-done", "drop-hover", "tabbed");
    note.classList.toggle("has-content", k >= 0);
    if (k < 0) return;
    const fig = document.querySelector(`.fig[data-fig="${names[k]}"] .fig-note`) as HTMLElement;
    for (const child of [...fig.children]) {
      const clone = child.cloneNode(true) as HTMLElement;
      const outer =
        clone.classList.contains("n-mini") ||
        clone.classList.contains("n-wires") ||
        clone.classList.contains("n-device");
      (outer ? outside : content).append(clone);
    }
    if (k === 5) {
      twinContent.replaceChildren(
        ...[...fig.children]
          .filter((c) => !c.classList.contains("n-device"))
          .map((c) => c.cloneNode(true)),
      );
    }
    note.classList.add("swap");
    requestAnimationFrame(() => note.classList.remove("swap"));
  };

  const typeInto = (el: Element | null, t: number, caret: boolean) => {
    if (!el) return;
    const full = (el as HTMLElement).dataset.type ?? "";
    const n = Math.round(full.length * clamp(t));
    const text = full.slice(0, n);
    if (el.firstChild?.nodeValue !== text || !!el.querySelector(".caret") !== caret) {
      el.textContent = text;
      if (caret) {
        const c = document.createElement("span");
        c.className = "caret";
        el.append(c);
      }
    }
  };

  // ---- per-chapter renderers: content as a function of local progress ----
  type Ctx = { base: Pose; vw: number; vh: number; narrow: boolean };
  const renderers: Array<(p: number, ctx: Ctx) => Partial<Pose> | void> = [
    // 01 write: types one long line; the note widens instead of wrapping
    (p, { base, vw, narrow }) => {
      const t = clamp(p / 0.5);
      const line = content.querySelector(".n-line");
      typeInto(line, t, t < 1 || p < 0.62);
      const el = line as HTMLElement | null;
      // The text's own width (the line is inline-block in the traveler), not
      // its box: a block's scrollWidth never drops below the container.
      const textW = el?.offsetWidth ?? 0;
      const need = textW / 0.84 + 6;
      // Narrow screens: the note may grow to both screen edges; past that the
      // line scrolls to follow the caret, as the app's canvas does.
      const x = narrow ? Math.max(12, Math.min(base.x, base.x + base.w / 2 - need / 2)) : base.x;
      const maxW = narrow ? vw - 12 - x : Math.min(vw - base.x - 28, base.w * 1.9);
      const w = Math.min(maxW, Math.max(base.w, need));
      if (el) {
        const overflow = textW - w * 0.84;
        const scrolls = overflow > 2;
        el.style.transform = scrolls ? `translateX(${-overflow}px)` : "";
        el.style.maskImage = scrolls ? "linear-gradient(90deg, transparent, #000 36px)" : "";
      }
      return { x, w, h: narrow ? base.h * 0.7 : base.h * 0.62, y: base.y + base.h * (narrow ? 0.15 : 0.19) };
    },
    // 02 tasks: boxes tick, progress fills, status moves, due chip clears
    (p) => {
      const lis = [...content.querySelectorAll(".n-checks li")];
      let on = 0;
      lis.forEach((li, i) => {
        const checked = p > 0.16 + i * 0.13;
        li.classList.toggle("on", checked);
        if (checked) on++;
      });
      const bar = content.querySelector(".n-progress span") as HTMLElement | null;
      bar?.style.setProperty("--w", `${(on / Math.max(1, lis.length)) * 100}%`);
      const em = content.querySelector(".n-progress em");
      if (em) em.textContent = `${on}/${lis.length}`;
      const status = p < 0.1 ? "todo" : on < lis.length ? "doing" : "done";
      const pill = content.querySelector("[data-status]");
      if (pill) {
        pill.textContent = status;
        pill.className = `pill st ${status}`;
      }
      content.querySelector("[data-due]")?.classList.toggle("overdue", status !== "done");
      note.classList.toggle("is-done", status === "done");
    },
    // 03 files: a photo, then a file, fall onto the note
    (p) => {
      note.classList.toggle("drop-hover", p > 0.01 && p < 0.2);
      const img = content.querySelector(".n-img") as HTMLElement | null;
      const file = content.querySelector(".n-file") as HTMLElement | null;
      const a = easeOut(clamp((p - 0.04) / 0.2));
      const b = easeOut(clamp((p - 0.26) / 0.2));
      if (img) {
        img.style.opacity = `${a}`;
        img.style.transform = `translateY(${(1 - a) * -34}px) rotate(${(1 - a) * -6}deg)`;
      }
      if (file) {
        file.style.opacity = `${b}`;
        file.style.transform = `translateY(${(1 - b) * -28}px) rotate(${(1 - b) * 4}deg)`;
      }
    },
    // 04 links: brackets become links, linked notes pop out, wires draw
    (p) => {
      const wl = [...content.querySelectorAll(".wl")];
      wl[0]?.classList.toggle("on", p > 0.1);
      wl[1]?.classList.toggle("on", p > 0.2);
      outside.querySelectorAll<HTMLElement>(".n-mini").forEach((m, i) => {
        const t = easeOut(clamp((p - (0.32 + i * 0.12)) / 0.14));
        m.style.opacity = `${t}`;
        m.style.scale = `${0.85 + 0.15 * t}`;
      });
      outside.querySelectorAll<SVGPathElement>(".n-wires path").forEach((path, i) => {
        path.style.opacity = `${0.55 * easeOut(clamp((p - (0.3 + i * 0.12)) / 0.12))}`;
      });
    },
    // 05 throw: fling to the screen edge, fold into a tab, peek, come back
    (p, { base, vw }) => {
      const out = easeInOut(clamp((p - 0.08) / 0.2));
      const back = easeInOut(clamp((p - 0.62) / 0.2));
      const tabW = 30;
      const tab: Pose = { x: vw - tabW, y: base.y + base.h * 0.18, w: tabW + 12, h: base.h * 0.64, r: 0, c: 0 };
      const peek = Math.sin(Math.PI * clamp((p - 0.38) / 0.18)) * 26;
      const t = out * (1 - back);
      const pose = lerpPose(base, tab, t);
      pose.x -= peek * (t > 0.95 ? 1 : 0);
      pose.y -= Math.sin(Math.PI * t) * 70 * (back > 0 ? 0.4 : 1);
      pose.r = Math.sin(Math.PI * t) * (back > 0 ? -8 : 16) + base.r * (1 - t);
      note.classList.toggle("tabbed", t > 0.55);
      toast.classList.toggle("show", p > 0.2 && p < 0.6);
      return pose;
    },
    // 06 sync: the same note on a second machine; typing shows up on both
    (p) => {
      typeInto(content.querySelector(".n-line"), (p - 0.1) / 0.4, p > 0.08 && p < 0.62);
      typeInto(twinContent.querySelector(".n-line"), (p - 0.16) / 0.4, false);
    },
  ];

  // ---- the frame ----
  const frame = () => {
    queued = false;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const s = window.scrollY;
    const narrow = narrowQ.matches;

    const ripLen = vh * (narrow ? 0.3 : 0.42);
    const rects = chapters.map((c) => c.getBoundingClientRect());
    // The reading line: mid-viewport on desktop; on narrow screens, the
    // middle of the band left below the pinned stage.
    const stageBottom = Math.max(0, (stageSlot.parentElement as HTMLElement).getBoundingClientRect().bottom);
    const mid = narrow ? (Math.min(stageBottom, vh * 0.55) + vh) / 2 : vh * 0.5;
    const k = rects.findIndex((r) => r.top <= mid && r.bottom > mid);
    const pk = k >= 0 ? clamp((mid - rects[k].top) / rects[k].height) : 0;
    const travelEnd = rects[0].top + s - mid;
    const u = clamp((s - ripLen) / Math.max(1, travelEnd - ripLen));
    const land = clamp((mid - rects[5].bottom) / (vh * 0.45));

    const journeyPose = (): Pose => rectPose(stageSlot, 1.5);

    let pose: Pose;
    let torn = false;
    let mountK = -1;
    let twinOpacity = 0;
    toast.classList.remove("show");

    if (s <= ripLen) {
      // RIP: the corner curls back, then the sheet pops off the pad
      const r = s / ripLen;
      const base = rectPose(heroSlot, 0, 0.22);
      const lift = easeOut(clamp((r - 0.8) / 0.2));
      torn = r > 0.8;
      pose = {
        ...base,
        c: base.w * (0.22 + 0.3 * easeInOut(clamp(r / 0.8)) - 0.12 * lift),
        y: base.y - 18 * lift,
        x: base.x - 6 * lift,
        r: -5 * lift,
      };
    } else if (k < 0 && land <= 0) {
      // TRAVEL: from the pad to the first chapter
      torn = true;
      const hero = rectPose(heroSlot, -5, 0.4);
      hero.y -= 18;
      hero.x -= 6;
      const target = journeyPose();
      const e = easeInOut(u);
      pose = lerpPose(hero, target, e);
      pose.x += Math.sin(Math.PI * u) * (narrow ? -30 : -90);
      pose.r = lerp(-5, 1.5, e) + Math.sin(Math.PI * 2 * u) * 5;
      pose.c = pose.w * lerp(0.4, 0.12, clamp(u / 0.5));
      mountK = u > 0.72 ? 0 : -1;
    } else if (k >= 0) {
      // CHAPTER k
      torn = true;
      const base = journeyPose();
      mountK = k;
      pose = { ...base };
      mount(k);
      const ov = renderers[k](pk, { base, vw, vh, narrow });
      if (ov) pose = { ...pose, ...ov };
      if (k === 5) twinOpacity = clamp((pk - 0.02) / 0.08);
    } else {
      // LAND: into the library
      torn = true;
      const from = journeyPose();
      const to = rectPose(landSlot, 1.2, 0.1);
      pose = lerpPose(from, to, easeInOut(land));
      mountK = 5;
      mount(5);
      renderers[5](1, { base: from, vw, vh, narrow });
      twinOpacity = clamp(1 - land * 2.5);
    }

    mount(mountK);
    // Arriving from the pad: chapter 01's line starts empty, not pre-typed.
    if (mountK === 0 && k < 0) typeInto(content.querySelector(".n-line"), 0, true);
    note.classList.toggle("torn", torn);
    const landed = land >= 0.995;
    note.style.visibility = landed ? "hidden" : "visible";
    landSlot.classList.toggle("landed", landed);

    note.style.width = `${pose.w}px`;
    note.style.height = `${pose.h}px`;
    note.style.transform = `translate3d(${pose.x}px, ${pose.y}px, 0) rotate(${pose.r}deg)`;
    note.style.setProperty("--cpx", `${Math.max(0, note.classList.contains("tabbed") ? 0 : pose.c)}px`);

    // twin (chapter 06): the same note on a second machine
    twin.style.opacity = `${twinOpacity}`;
    if (twinOpacity > 0) {
      const tw = pose.w * (narrow ? 0.74 : 0.8);
      const th = pose.h * (narrow ? 0.74 : 0.8);
      const tx = pose.x + pose.w * (narrow ? 0.3 : 0.52);
      const ty = pose.y + pose.h * (narrow ? 0.62 : 0.58);
      twin.style.width = `${tw}px`;
      twin.style.height = `${th}px`;
      twin.style.transform = `translate3d(${tx}px, ${ty}px, 0) rotate(-3deg)`;
    }

    // the accumulating record: one square per chapter visited
    const reached = land > 0 ? 6 : k;
    ledger.forEach((li, i) => {
      li.classList.toggle("done", i < reached);
      li.classList.toggle("now", i === k);
    });
    legend.forEach((a, i) => a.classList.toggle("on", i === k));
    stageSlot.classList.toggle("ghost", k === 4 && note.classList.contains("tabbed"));
    document.documentElement.classList.toggle(
      "journey-on",
      k >= 0 || (land > 0 && land < 1) || (s > ripLen && u > 0.85),
    );
  };

  const request = () => {
    if (!queued) {
      queued = true;
      requestAnimationFrame(frame);
    }
  };
  window.addEventListener("scroll", request, { passive: true });
  window.addEventListener("resize", request);
  narrowQ.addEventListener("change", request);
  document.fonts?.ready.then(request);
  frame();
}

// ---- theme toggle (top right): system default, explicit choice remembered ----
function initTheme() {
  const root = document.documentElement;
  const btn = document.getElementById("theme-toggle");
  const meta = document.querySelector('meta[name="theme-color"]');
  const systemDark = window.matchMedia("(prefers-color-scheme: dark)");
  const current = () => (root.dataset.theme as "dark" | "light" | undefined) ?? (systemDark.matches ? "dark" : "light");
  const sync = () => {
    const t = current();
    btn?.setAttribute("aria-label", t === "dark" ? "Switch to light mode" : "Switch to dark mode");
    meta?.setAttribute("content", t === "dark" ? "#181715" : "#ECEBE6");
  };
  btn?.addEventListener("click", () => {
    const next = current() === "dark" ? "light" : "dark";
    root.dataset.theme = next;
    try {
      localStorage.setItem("pinq-theme", next);
    } catch {
      /* private mode: the choice just won't persist */
    }
    sync();
  });
  systemDark.addEventListener("change", sync);
  sync();
}
initTheme();

if (!reduce.matches) {
  document.documentElement.classList.add("motion");
  start();
}
reduce.addEventListener("change", () => window.location.reload());
