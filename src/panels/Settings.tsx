import { createSignal, For, Index, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { PinqSettings } from "../theme";

interface SyncInfo {
  deviceName: string;
  deviceShort: string;
  deviceFingerprint: string;
  spaceFingerprint: string;
  keyStorage: string;
  discoveryError: string | null;
  peers: Array<{
    deviceName: string;
    deviceShort: string;
    addr: string;
    sameSpace: boolean;
    lastSeenMs: number;
    lastSync: SyncStatusView | null;
  }>;
  relay: SyncStatusView | null;
  relayHost: { port: number; id: string; error: string | null } | null;
  relayUsing: string | null;
}

interface SyncStatusView {
  atMs: number;
  ok: boolean;
  error: string | null;
  pulled: number;
  pushed: number;
}

interface PairingUi {
  active: boolean;
  role: string | null;
  code: string | null;
  safety: string | null;
  status: string | null;
  error: string | null;
  /** Name of a device asking to pair with this one (Accept/Decline prompt). */
  request: string | null;
}

interface DeviceView {
  pk: string;
  short: string;
  name: string;
  addedMs: number;
  revoked: boolean;
  isSelf: boolean;
}

interface SmartStatus {
  enabled: boolean;
  phase: "off" | "loading" | "indexing" | "ready" | "error";
  indexed: number;
  total: number;
  error: string | null;
}

const THEMES = [
  { id: "pinq", label: "Pinq" },
  { id: "amber", label: "Amber" },
  { id: "mint", label: "Mint" },
  { id: "sky", label: "Sky" },
  { id: "rose", label: "Rose" },
  { id: "violet", label: "Violet" },
  { id: "graphite", label: "Graphite" },
];

const MODES = [
  { id: "auto", label: "Auto" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
] as const;

const PAPERS = [
  { id: "plain", label: "Plain" },
  { id: "ruled", label: "Ruled" },
  { id: "dotted", label: "Dotted" },
  { id: "grid", label: "Grid" },
  { id: "dashed", label: "Dashed" },
  { id: "wavy", label: "Wavy" },
  { id: "zigzag", label: "Zigzag" },
  { id: "hearts", label: "Hearts" },
  { id: "stars", label: "Sparkles" },
] as const;

const EDGES = [
  { id: "left", label: "Left" },
  { id: "right", label: "Right" },
  { id: "top", label: "Top" },
  { id: "bottom", label: "Bottom" },
] as const;

const MONITOR_SCOPES = [
  { id: "primary", label: "Primary only" },
  { id: "all", label: "All monitors" },
] as const;

const SHOW_IN = [
  { id: "both", label: "Menu bar & Dock" },
  { id: "menubar", label: "Menu bar" },
  { id: "dock", label: "Dock" },
] as const;

/** The show-in choice only exists on macOS; Windows always keeps the tray. */
const IS_MAC = navigator.userAgent.includes("Mac");

export default function Settings() {
  const [settings, setSettings] = createSignal<PinqSettings>({
    theme: "amber",
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
  });

  const [sync, setSync] = createSignal<SyncInfo | null>(null);
  const [smart, setSmart] = createSignal<SmartStatus | null>(null);
  const [pairing, setPairing] = createSignal<PairingUi | null>(null);
  const [devices, setDevices] = createSignal<DeviceView[]>([]);
  const [removeArm, setRemoveArm] = createSignal<string | null>(null);
  const [rotateArm, setRotateArm] = createSignal(false);
  const [rotateMsg, setRotateMsg] = createSignal<string | null>(null);
  // The peer we clicked "Pair" on, so its row can show the waiting hint.
  const [requestAddr, setRequestAddr] = createSignal<string | null>(null);
  let syncTimer: number | undefined;
  let unlistenPair: UnlistenFn | undefined;

  onMount(async () => {
    setSettings(await invoke<PinqSettings>("get_settings"));
    const poll = async () => {
      setSync(await invoke<SyncInfo>("get_sync_info"));
      setPairing(await invoke<PairingUi>("pairing_state"));
      setDevices(await invoke<DeviceView[]>("get_registry"));
      setSmart(await invoke<SmartStatus>("smart_status"));
    };
    void poll();
    syncTimer = window.setInterval(() => void poll(), 1500);
    // Incoming pair requests show up instantly, not on the next poll.
    unlistenPair = await listen("pinq-pair-request", async () => {
      setPairing(await invoke<PairingUi>("pairing_state"));
    });
  });

  onCleanup(() => {
    clearInterval(syncTimer);
    unlistenPair?.();
  });

  const hostStart = async () => {
    await invoke("pairing_host_start");
    setPairing(await invoke<PairingUi>("pairing_state"));
  };

  // Click-to-pair: ask the other device; its user accepts, the code exchanges
  // automatically, and both sides land on the safety-number confirm.
  const requestPair = async (addr: string) => {
    setRequestAddr(addr);
    try {
      await invoke("pair_request", { addr });
    } catch {
      // Already pairing — the state below will say so.
    }
    setPairing(await invoke<PairingUi>("pairing_state"));
  };

  const respond = async (accept: boolean) => {
    await invoke("pair_respond", { accept });
    setPairing(await invoke<PairingUi>("pairing_state"));
  };

  const syncNow = () => void invoke("sync_now");

  // Two-step rotate: mints a new space key sealed to each remaining device.
  const rotateSpace = async () => {
    if (!rotateArm()) {
      setRotateArm(true);
      window.setTimeout(() => setRotateArm(false), 4000);
      return;
    }
    setRotateArm(false);
    setRotateMsg("Rotating…");
    try {
      setRotateMsg(await invoke<string>("rotate_space"));
    } catch (e) {
      setRotateMsg(`Rotation failed: ${e}`);
    }
  };

  // Two-step remove: first click arms, second signs the revocation.
  const removeDevice = async (pk: string) => {
    if (removeArm() !== pk) {
      setRemoveArm(pk);
      window.setTimeout(() => setRemoveArm((c) => (c === pk ? null : c)), 4000);
      return;
    }
    setRemoveArm(null);
    await invoke("revoke_device", { pk });
    setDevices(await invoke<DeviceView[]>("get_registry"));
  };

  const agoLabel = (atMs: number) => {
    const s = Math.max(0, Math.round((Date.now() - atMs) / 1000));
    if (s < 5) return "just now";
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.round(s / 60)}m ago`;
    return `${Math.round(s / 3600)}h ago`;
  };

  const pairConfirm = () => void invoke("pairing_confirm");
  const pairCancel = async () => {
    await invoke("pairing_cancel");
    setPairing(await invoke<PairingUi>("pairing_state"));
  };

  const update = async (patch: Partial<PinqSettings>) => {
    const next = { ...settings(), ...patch };
    setSettings(next);
    await invoke("set_settings", { settings: next });
  };

  return (
    <div class="settings-panel">
      <header class="settings-header" data-tauri-drag-region>
        <span data-tauri-drag-region>Pinq Settings</span>
        <button
          class="settings-close"
          title="Close"
          onClick={() => void invoke("close_settings")}
        >
          ✕
        </button>
      </header>
      <section class="settings-body">
        <h2>Color</h2>
        <p class="settings-hint">One color for the pad, notes, and library accents.</p>
        <div class="swatch-grid">
          <For each={THEMES}>
            {(t) => (
              <button
                class="swatch"
                classList={{ active: settings().theme === t.id }}
                data-theme={t.id}
                onClick={() => void update({ theme: t.id })}
              >
                <span class="swatch-pad" />
                <span class="swatch-note" />
                <span class="swatch-label">{t.label}</span>
              </button>
            )}
          </For>
        </div>

        <h2>Library theme</h2>
        <p class="settings-hint">Auto follows your system's light or dark mode.</p>
        <div class="seg">
          <For each={[...MODES]}>
            {(m) => (
              <button
                class="seg-btn"
                classList={{ active: settings().libraryMode === m.id }}
                onClick={() => void update({ libraryMode: m.id })}
              >
                {m.label}
              </button>
            )}
          </For>
        </div>

        <h2>Note paper</h2>
        <p class="settings-hint">Default for all sticky notes; each note can override it.</p>
        <div class="paper-grid">
          <For each={[...PAPERS]}>
            {(p) => (
              <button
                class="paper-btn"
                classList={{ active: settings().notePaper === p.id }}
                onClick={() => void update({ notePaper: p.id })}
              >
                <span class={`paper-preview paper-tex-${p.id}`} />
                <span class="swatch-label">{p.label}</span>
              </button>
            )}
          </For>
        </div>

        <h2>Sticky pad</h2>
        <p class="settings-hint">Which screen edge the pad docks to.</p>
        <div class="seg">
          <For each={[...EDGES]}>
            {(e) => (
              <button
                class="seg-btn"
                classList={{ active: settings().padEdge === e.id }}
                onClick={() => void update({ padEdge: e.id })}
              >
                {e.label}
              </button>
            )}
          </For>
        </div>
        <p class="settings-hint" style={{ "margin-top": "12px" }}>
          Position along the edge (snaps to 5%, or type an exact value).
        </p>
        <div class="settings-position">
          <input
            class="settings-slider"
            type="range"
            min="0"
            max="100"
            step="5"
            value={Math.round(settings().padPosition * 100)}
            onChange={(e) => void update({ padPosition: Number(e.currentTarget.value) / 100 })}
          />
          <input
            class="settings-num"
            type="number"
            min="0"
            max="100"
            value={Math.round(settings().padPosition * 100)}
            onChange={(e) => {
              const v = Math.min(100, Math.max(0, Number(e.currentTarget.value) || 0));
              e.currentTarget.value = String(v);
              void update({ padPosition: v / 100 });
            }}
          />
          <span class="settings-unit">%</span>
        </div>
        <p class="settings-hint" style={{ "margin-top": "12px" }}>
          Monitors that show the pad.
        </p>
        <div class="seg">
          <For each={[...MONITOR_SCOPES]}>
            {(m) => (
              <button
                class="seg-btn"
                classList={{ active: settings().padMonitors === m.id }}
                onClick={() => void update({ padMonitors: m.id })}
              >
                {m.label}
              </button>
            )}
          </For>
        </div>

        <p class="settings-hint" style={{ "margin-top": "12px" }}>
          Click to create: a plain click on the pad makes a note — no pull needed.
        </p>
        <div class="seg">
          <button
            class="seg-btn"
            classList={{ active: !settings().clickCreates }}
            onClick={() => void update({ clickCreates: false })}
          >
            Off
          </button>
          <button
            class="seg-btn"
            classList={{ active: settings().clickCreates }}
            onClick={() => void update({ clickCreates: true })}
          >
            On
          </button>
        </div>

        <h2>Library paper</h2>
        <div class="paper-grid">
          <For each={[...PAPERS]}>
            {(p) => (
              <button
                class="paper-btn"
                classList={{ active: settings().libraryPaper === p.id }}
                onClick={() => void update({ libraryPaper: p.id })}
              >
                <span class={`paper-preview paper-tex-${p.id}`} />
                <span class="swatch-label">{p.label}</span>
              </button>
            )}
          </For>
        </div>

        <h2>System</h2>
        <p class="settings-hint">Start Pinq automatically when you sign in.</p>
        <div class="seg">
          <button
            class="seg-btn"
            classList={{ active: !settings().launchAtLogin }}
            onClick={() => void update({ launchAtLogin: false })}
          >
            Off
          </button>
          <button
            class="seg-btn"
            classList={{ active: settings().launchAtLogin }}
            onClick={() => void update({ launchAtLogin: true })}
          >
            Start at login
          </button>
        </div>
        <Show when={IS_MAC}>
          <p class="settings-hint" style={{ "margin-top": "12px" }}>
            Where Pinq shows: the menu bar icon, the Dock icon, or both.
          </p>
          <div class="seg">
            <For each={[...SHOW_IN]}>
              {(s) => (
                <button
                  class="seg-btn"
                  classList={{ active: settings().showIn === s.id }}
                  onClick={() => void update({ showIn: s.id })}
                >
                  {s.label}
                </button>
              )}
            </For>
          </div>
        </Show>

        <h2>Smart suggestions</h2>
        <p class="settings-hint">
          A small language model runs entirely on this device to suggest tags and
          related notes. Notes never leave your machine. First enable downloads the
          model (~25 MB).
        </p>
        <div class="seg">
          <button
            class="seg-btn"
            classList={{ active: !settings().smartSuggestions }}
            onClick={() => void update({ smartSuggestions: false })}
          >
            Off
          </button>
          <button
            class="seg-btn"
            classList={{ active: settings().smartSuggestions }}
            onClick={() => void update({ smartSuggestions: true })}
          >
            On
          </button>
        </div>
        <Show when={settings().smartSuggestions && smart()}>
          {(s) => (
            <p class="settings-hint smart-status">
              {s().phase === "loading" && "Loading model… (downloads on first enable)"}
              {s().phase === "indexing" && `Indexing notes… ${s().indexed}/${s().total}`}
              {s().phase === "ready" && `Ready — ${s().total} notes indexed.`}
              {s().phase === "error" && `Problem: ${s().error ?? "unknown"}`}
            </p>
          )}
        </Show>

        <h2>Sync &amp; devices</h2>
        <Show when={sync()}>
          {(s) => (
            <div class="sync-panel">
              <div class="sync-row">
                <span class="sync-label">This device</span>
                <span>
                  {s().deviceName} <span class="sync-mono">({s().deviceShort})</span>
                </span>
              </div>
              <Show when={s().keyStorage === "file"}>
                <div class="sync-row sync-error">Credential store unavailable — keys in a file.</div>
              </Show>
              <Show when={s().discoveryError}>
                <div class="sync-row sync-error">Discovery: {s().discoveryError}</div>
              </Show>

              <Show when={pairing()?.request}>
                <div class="pair-box pair-request">
                  <p class="settings-hint">
                    <b>“{pairing()?.request}”</b> wants to pair with this device.
                  </p>
                  <div class="pair-actions">
                    <button class="library-btn" onClick={() => void respond(true)}>
                      Accept
                    </button>
                    <button
                      class="library-btn library-btn-danger"
                      onClick={() => void respond(false)}
                    >
                      Decline
                    </button>
                  </div>
                </div>
              </Show>

              <Show
                when={pairing()?.active}
                fallback={
                  <Show when={!pairing()?.request}>
                    <button class="library-btn" onClick={() => void hostStart()}>
                      Add device
                    </button>
                  </Show>
                }
              >
                <div class="pair-box">
                  <Show when={pairing()?.status === "waiting" && pairing()?.code}>
                    <p class="settings-hint">Enter on your other device (tap Pair there):</p>
                    <div class="pair-code">{pairing()?.code}</div>
                  </Show>
                  <Show when={pairing()?.status === "requesting"}>
                    <p class="settings-hint">Waiting for the other device to accept…</p>
                  </Show>
                  <Show when={pairing()?.status === "connecting"}>
                    <p class="settings-hint">Connecting…</p>
                  </Show>
                  <Show when={pairing()?.status === "confirm"}>
                    <p class="settings-hint">
                      Same number on the other device? Confirm only if they match.
                    </p>
                    <div class="pair-safety">{pairing()?.safety}</div>
                    <div class="pair-actions">
                      <button class="library-btn" onClick={pairConfirm}>
                        Yes, they match
                      </button>
                      <button class="library-btn library-btn-danger" onClick={() => void pairCancel()}>
                        Cancel
                      </button>
                    </div>
                  </Show>
                  <Show when={pairing()?.status === "applying"}>
                    <p class="settings-hint">Waiting for the other device…</p>
                  </Show>
                  <Show when={pairing()?.status === "done"}>
                    <p class="pair-done">✓ Paired</p>
                    <button class="library-btn" onClick={() => void pairCancel()}>
                      Close
                    </button>
                  </Show>
                  <Show when={pairing()?.status === "failed"}>
                    <p class="sync-error">Pairing failed: {pairing()?.error}</p>
                    <button class="library-btn" onClick={() => void pairCancel()}>
                      Close
                    </button>
                  </Show>
                </div>
              </Show>

              <div class="sync-peers">
                <span class="sync-label">
                  Devices in this space ({devices().filter((d) => !d.revoked).length})
                </span>
                {/* <Index>, not <For>: the 1.5s poll rebuilds these arrays and
                    <For>'s referential keying would recreate every row. */}
                <Index each={devices()}>
                  {(d) => (
                    <div class="sync-peer" classList={{ "device-revoked": d().revoked }}>
                      <span>
                        {d().name} <span class="sync-mono">({d().short})</span>
                      </span>
                      <Show when={d().isSelf}>
                        <span class="sync-badge same">this device</span>
                      </Show>
                      <Show when={d().revoked}>
                        <span class="sync-badge">removed</span>
                      </Show>
                      <Show when={!d().isSelf && !d().revoked}>
                        <button
                          class="library-btn"
                          classList={{ "library-btn-danger": removeArm() === d().pk }}
                          onClick={() => void removeDevice(d().pk)}
                        >
                          {removeArm() === d().pk ? "Really remove?" : "Remove"}
                        </button>
                      </Show>
                    </div>
                  )}
                </Index>
                <p class="settings-hint">
                  After removing a device, rotate the key to fully lock it out.
                </p>
                <div class="pair-actions">
                  <button
                    class="library-btn"
                    classList={{ "library-btn-danger": rotateArm() }}
                    onClick={() => void rotateSpace()}
                  >
                    {rotateArm() ? "Really rotate?" : "Rotate space key…"}
                  </button>
                </div>
                <Show when={rotateMsg()}>
                  <div class="sync-status">{rotateMsg()}</div>
                </Show>
              </div>

              <div class="sync-peers">
                <div class="sync-peers-head">
                  <span class="sync-label">
                    Devices on this network ({s().peers.length})
                  </span>
                  <Show when={s().peers.some((p) => p.sameSpace)}>
                    <button class="library-btn" onClick={syncNow}>
                      Sync now
                    </button>
                  </Show>
                </div>
                {/* <Index>, not <For> — see the devices list above. */}
                <Index
                  each={s().peers}
                  fallback={<p class="settings-hint">None found yet.</p>}
                >
                  {(p) => (
                    <div class="sync-peer-block">
                      <div class="sync-peer">
                        <span>{p().deviceName}</span>
                        <span class="sync-mono">{p().addr}</span>
                        <span
                          class="sync-badge"
                          classList={{ same: p().sameSpace }}
                          title={p().sameSpace ? "Same space" : "Different space"}
                        >
                          {p().sameSpace ? "same space" : "other space"}
                        </span>
                        <Show when={!p().sameSpace && !pairing()?.active}>
                          <button
                            class="library-btn"
                            title="Ask this device to pair — its user accepts, then both confirm a matching number"
                            onClick={() => void requestPair(p().addr)}
                          >
                            Pair
                          </button>
                        </Show>
                      </div>
                      <Show when={p().sameSpace && p().lastSync}>
                        {(ls) => (
                          <div class="sync-status" classList={{ "sync-error": !ls().ok }}>
                            {ls().ok
                              ? `Synced ${agoLabel(ls().atMs)}` +
                                (ls().pulled || ls().pushed
                                  ? ` — ${ls().pulled} in / ${ls().pushed} out`
                                  : " — up to date")
                              : `Sync failed ${agoLabel(ls().atMs)}: ${ls().error}`}
                          </div>
                        )}
                      </Show>
                      <Show
                        when={
                          pairing()?.status === "requesting" && requestAddr() === p().addr
                        }
                      >
                        <div class="sync-status">Waiting for “{p().deviceName}” to accept…</div>
                      </Show>
                    </div>
                  )}
                </Index>
              </div>

              <div class="sync-peers">
                <span class="sync-label">Sync away from home</span>
                <p class="settings-hint">
                  Host a relay here — your other devices find it automatically.
                </p>
                <div class="seg">
                  <button
                    class="seg-btn"
                    classList={{ active: !settings().relayHost }}
                    onClick={() => void update({ relayHost: false })}
                  >
                    Off
                  </button>
                  <button
                    class="seg-btn"
                    classList={{ active: settings().relayHost }}
                    onClick={() => void update({ relayHost: true })}
                  >
                    Host relay on this device
                  </button>
                </div>
                <Show when={settings().relayHost && s().relayHost}>
                  {(h) => (
                    <div class="sync-status" classList={{ "sync-error": !!h().error }}>
                      {h().error
                        ? `Relay couldn't start: ${h().error}`
                        : `Hosting on port ${h().port}. From outside your network: ` +
                          `your-address:${h().port}#${h().id.slice(0, 16)}`}
                    </div>
                  )}
                </Show>
                <p class="settings-hint" style={{ "margin-top": "10px" }}>
                  Or use a relay elsewhere (ignored while hosting):
                </p>
                <div class="pair-join">
                  <input
                    class="settings-num relay-input"
                    placeholder="relay.example.com:47820"
                    value={settings().relayAddr}
                    onChange={(e) => void update({ relayAddr: e.currentTarget.value.trim() })}
                  />
                </div>
                <Show when={s().relayUsing}>
                  <div class="sync-status">Syncing via {s().relayUsing}</div>
                </Show>
                <Show when={s().relayUsing ? s().relay : null}>
                  {(r) => (
                    <div class="sync-status" classList={{ "sync-error": !r().ok }}>
                      {r().ok
                        ? `Relay synced ${agoLabel(r().atMs)}` +
                          (r().pulled || r().pushed
                            ? ` — ${r().pulled} in / ${r().pushed} out`
                            : " — up to date")
                        : `Relay failed ${agoLabel(r().atMs)}: ${r().error}`}
                    </div>
                  )}
                </Show>
              </div>
            </div>
          )}
        </Show>
      </section>
    </div>
  );
}
