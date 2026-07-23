//! Sync foundation (spec §19/§21).
//!
//! Spike 1 — device identity + pairing-blind LAN discovery (mDNS).
//! Spike 2 — device pairing: host shows a one-time code; joiner enters it;
//! both run SPAKE2 (so the short code can't be brute-forced offline and one
//! wrong guess burns the session); both display a safety number derived from
//! the shared key; host confirmation gates SENDING the space root key
//! (ChaCha20Poly1305 under a PAKE-derived key), joiner confirmation gates
//! APPLYING it. After pairing the joiner re-keys, recomputes its LAN tag, and
//! re-advertises — both machines then see each other as "same space".
//!
//! Spike 3 — note data moves: op-log exchange with version vectors. Every op
//! is stamped (device, ctr) in the store; a sync session is a TCP connection
//! where both sides swap version vectors, send each other exactly the ops the
//! other lacks, then fetch any attachment blobs referenced but missing. The
//! whole session (after a plaintext hello that only carries random nonces) is
//! encrypted with ChaCha20Poly1305 under a key derived from the shared space
//! key — possession of the space key IS the authentication; a device that was
//! never paired can't produce a single valid frame. Convergence comes from the
//! store's canonical replay, not from message ordering.

use crate::registry;
use crate::store::{RegistryEntry, Store, SyncOp};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const SYNC_PORT: u16 = 47814;
const SERVICE_TYPE: &str = "_pinq._udp.local.";
const PAIRING_TTL_MS: u64 = 180_000;
const MAX_FRAME: u32 = 256 * 1024;
/// Raw bytes per blob frame (fits MAX_FRAME after base64 + JSON overhead).
const BLOB_CHUNK: usize = 128 * 1024;
/// Target serialized size per ops frame.
const OPS_BATCH_BYTES: usize = 96 * 1024;
/// How often the loop looks for something to do.
// 1s: the per-tick cost is one MAX(ctr) query + a hash, and content edits
// should feel live on the other device (~1-2s with the 400ms edit debounce).
const SYNC_TICK: Duration = Duration::from_secs(1);
/// Full sync at least this often even with no local changes (pulls remote-only edits).
const SYNC_REFRESH_MS: u64 = 60_000;
/// Back off failing peers.
// Short: a transiently failed session (peer restarting, dropped socket)
// shouldn't make edits feel stuck; truly-offline peers cost one bounded
// connect attempt per window.
const SYNC_RETRY_MS: u64 = 5_000;
/// Presence: how often the open-note set is re-broadcast unchanged, and how
/// long a peer's set is believed without a refresh (crash/offline expiry).
const PRESENCE_RESEND_MS: u64 = 10_000;
const PRESENCE_TTL_MS: u64 = 25_000;
/// Reject presence frames older than this (replay bound / clock skew).
const PRESENCE_MAX_AGE_MS: u64 = 30_000;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fingerprint(bytes: &[u8], groups: usize) -> String {
    hex(bytes)
        .as_bytes()
        .chunks(4)
        .take(groups)
        .map(|c| String::from_utf8_lossy(c).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn lan_tag(space_key: &[u8; 32]) -> String {
    hex(&blake3::keyed_hash(space_key, b"pinq-lan-tag-v1").as_bytes()[..8])
}

fn space_fp(space_key: &[u8; 32]) -> String {
    fingerprint(blake3::keyed_hash(space_key, b"pinq-space-fingerprint-v1").as_bytes(), 4)
}

// ---------- state ----------

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub device_name: String,
    pub device_short: String,
    /// Best single candidate, for display and as a fallback.
    pub addr: String,
    /// All ranked connect candidates ("ip:port"). A peer advertises every
    /// interface it has (Wi-Fi, VPN tunnels, IPv6…); only some are reachable
    /// from here, so connecting tries them in order.
    #[serde(skip)]
    pub addrs: Vec<String>,
    #[serde(skip)]
    pub tag: String,
    pub same_space: bool, // recomputed at read time
    pub last_seen_ms: u64,
    /// Outcome of the most recent sync session with this peer (joined at read).
    pub last_sync: Option<SyncStatus>,
    /// "ip:port#id" if this peer hosts a relay (from its mDNS advertisement).
    pub relay_addr: Option<String>,
}

/// Rank a peer's advertised IPs into connect candidates: private IPv4 first
/// (the LAN address that almost always works), then public IPv4, then global
/// IPv6. Loopback and link-local are dropped — loopback is never the peer,
/// and link-local needs scope ids we don't have. Picking ONE arbitrary
/// address here (the old behavior) broke pairing whenever mDNS resolved a
/// VPN/virtual interface first.
fn rank_addrs(ips: impl IntoIterator<Item = std::net::IpAddr>, port: u16) -> Vec<String> {
    let mut ranked: Vec<(u8, String)> = ips
        .into_iter()
        .filter_map(|ip| match ip {
            std::net::IpAddr::V4(v4) => {
                if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
                    return None;
                }
                Some((if v4.is_private() { 0 } else { 1 }, format!("{v4}:{port}")))
            }
            std::net::IpAddr::V6(v6) => {
                if v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80 {
                    return None;
                }
                Some((2, format!("[{v6}]:{port}")))
            }
        })
        .collect();
    ranked.sort();
    ranked.dedup();
    ranked.into_iter().map(|(_, a)| a).collect()
}

/// Try each candidate in order; the first that accepts wins. Returns the
/// stream plus the address that worked (so follow-up connections reuse it).
fn connect_peer(addrs: &[String]) -> Result<(TcpStream, String), String> {
    let mut last = "peer has no reachable address".to_string();
    for addr in addrs {
        let Ok(sock) = addr.parse::<std::net::SocketAddr>() else { continue };
        match TcpStream::connect_timeout(&sock, Duration::from_secs(4)) {
            Ok(s) => return Ok((s, addr.clone())),
            Err(e) => last = format!("connect {addr}: {e}"),
        }
    }
    Err(last)
}

/// A peer's connect candidates, with the display addr as a last resort.
fn peer_candidates(p: &PeerInfo) -> Vec<String> {
    if p.addrs.is_empty() {
        vec![p.addr.clone()]
    } else {
        p.addrs.clone()
    }
}

/// What happened the last time we synced with a peer.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub at_ms: u64,
    pub ok: bool,
    pub error: Option<String>,
    /// Ops received / sent in that session.
    pub pulled: usize,
    pub pushed: usize,
}

/// An incoming click-to-pair request awaiting the user's Accept/Decline.
pub struct IncomingPair {
    pub name: String,
    pub reply_tx: std::sync::mpsc::Sender<bool>,
}

pub struct PairingSession {
    pub role: &'static str, // "host" | "joiner"
    pub code: Option<String>,
    pub safety: Option<String>,
    pub status: String, // waiting | connecting | confirm | applying | done | failed
    pub error: Option<String>,
    pub confirm_tx: Option<std::sync::mpsc::Sender<bool>>,
    pub started_ms: u64,
}

#[derive(Default)]
pub struct SyncState {
    pub device_name: String,
    pub device_fingerprint: String,
    pub device_short: String,
    pub key_storage: String,
    /// Signs registry records and sync-session auth challenges.
    pub device_secret: Mutex<[u8; 32]>,
    pub space_key: Mutex<[u8; 32]>,
    pub current_tag: Mutex<String>,
    pub space_fingerprint: Mutex<String>,
    pub mdns: Mutex<Option<mdns_sd::ServiceDaemon>>,
    pub discovery_error: Mutex<Option<String>>,
    pub peers: Mutex<HashMap<String, PeerInfo>>,
    pub pairing: Mutex<Option<PairingSession>>,
    /// An unanswered "wants to pair with you" from another device; the
    /// listener thread blocks on `reply_tx`'s channel until the user answers.
    pub pair_req: Mutex<Option<IncomingPair>>,
    /// Peers (by device_short) with a sync session in flight — one at a time each.
    pub syncing: Mutex<std::collections::HashSet<String>>,
    /// Last session outcome per peer.
    pub last_sync: Mutex<HashMap<String, SyncStatus>>,
    /// Set by `sync_now` to make the loop run immediately.
    pub sync_asap: std::sync::atomic::AtomicBool,
    /// Same, for the relay loop (separate flag so both loops see one click).
    pub relay_asap: std::sync::atomic::AtomicBool,
    /// Last relay exchange outcome.
    pub relay_status: Mutex<Option<SyncStatus>>,
    /// The in-app hosted relay, when the toggle is on.
    pub relay_host_info: Mutex<Option<RelayHostInfo>>,
    /// Kill switch for the hosted relay's accept loop.
    pub relay_host_flag: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    /// Live co-editing presence: device_short -> (peer's open note ids, received_at_ms).
    pub peer_open_notes: Mutex<HashMap<String, (std::collections::HashSet<String>, u64)>>,
    /// Union of peer open-sets at the last recompute (drives pinq-live events).
    pub last_live: Mutex<std::collections::HashSet<String>>,
    /// Peers to sync with immediately (a live note was edited); drained per tick.
    pub live_push: Mutex<std::collections::HashSet<String>>,
    /// Wakes run_sync_loop out of its 1s tick. Set once by init().
    pub sync_wake: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayHostInfo {
    pub port: u16,
    pub id: String,
    pub error: Option<String>,
}

impl SyncState {
    fn set_pairing_status(&self, status: &str, error: Option<String>) {
        if let Some(s) = self.pairing.lock().unwrap().as_mut() {
            s.status = status.into();
            s.error = error;
        }
    }

    fn set_pairing_safety(&self, safety: String) -> std::sync::mpsc::Receiver<bool> {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(s) = self.pairing.lock().unwrap().as_mut() {
            s.safety = Some(safety);
            s.status = "confirm".into();
            s.confirm_tx = Some(tx);
        }
        rx
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncInfo {
    device_name: String,
    device_short: String,
    device_fingerprint: String,
    space_fingerprint: String,
    key_storage: String,
    discovery_error: Option<String>,
    peers: Vec<PeerInfo>,
    relay: Option<SyncStatus>,
    relay_host: Option<RelayHostInfo>,
    /// The relay this device is currently syncing through (pin stripped).
    relay_using: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingUi {
    active: bool,
    role: Option<String>,
    code: Option<String>,
    safety: Option<String>,
    status: Option<String>,
    error: Option<String>,
    /// Name of a device asking to pair with THIS one (Accept/Decline prompt).
    request: Option<String>,
}

// ---------- commands ----------

/// Async on purpose (runs off the main thread), and every lock below is taken
/// in its OWN `let` statement so its guard drops at that semicolon.
///
/// THE BUG THIS GUARDS AGAINST (froze the whole app): a `.lock()` inside a
/// struct literal is a temporary, and Rust keeps temporaries alive until the
/// END OF THE STATEMENT — so `SyncInfo { relay_host: x.lock()…, relay_using:
/// f() }` still held the `relay_host_info` guard while `f` =
/// `effective_relay_addr` re-locked the same mutex: a same-thread deadlock on
/// the main thread. Never call a locking function from a statement that still
/// holds a guard temporary.
#[tauri::command]
pub async fn get_sync_info(app: AppHandle, state: State<'_, SyncState>) -> Result<SyncInfo, ()> {
    let tag = state.current_tag.lock().unwrap().clone();
    let last_sync = state.last_sync.lock().unwrap().clone();
    let mut peers: Vec<PeerInfo> = state
        .peers
        .lock()
        .unwrap()
        .values()
        .cloned()
        .map(|mut p| {
            p.same_space = !tag.is_empty() && p.tag == tag;
            p.last_sync = last_sync.get(&p.device_short).cloned();
            p
        })
        .collect();
    peers.sort_by(|a, b| a.device_name.cmp(&b.device_name));
    let space_fingerprint = state.space_fingerprint.lock().unwrap().clone();
    let discovery_error = state.discovery_error.lock().unwrap().clone();
    let relay = state.relay_status.lock().unwrap().clone();
    let relay_host = state.relay_host_info.lock().unwrap().clone();
    let relay_using = app
        .try_state::<crate::settings::SettingsStore>()
        .and_then(|s| effective_relay_addr(&state, &s.current()))
        .map(|a| a.split('#').next().unwrap_or(&a).to_string());
    Ok(SyncInfo {
        device_name: state.device_name.clone(),
        device_short: state.device_short.clone(),
        device_fingerprint: state.device_fingerprint.clone(),
        space_fingerprint,
        key_storage: state.key_storage.clone(),
        discovery_error,
        peers,
        relay,
        relay_host,
        relay_using,
    })
}

#[tauri::command]
pub async fn pairing_state(state: State<'_, SyncState>) -> Result<PairingUi, ()> {
    let mut guard = state.pairing.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        // Lazy expiry.
        if pinq_core::now_ms().saturating_sub(s.started_ms) > PAIRING_TTL_MS
            && s.status != "done"
            && s.status != "failed"
        {
            guard.as_mut().unwrap().status = "failed".into();
            guard.as_mut().unwrap().error = Some("pairing timed out".into());
        }
    }
    drop(guard);
    let request = state.pair_req.lock().unwrap().as_ref().map(|r| r.name.clone());
    let guard = state.pairing.lock().unwrap();
    Ok(match guard.as_ref() {
        None => PairingUi {
            active: false,
            role: None,
            code: None,
            safety: None,
            status: None,
            error: None,
            request,
        },
        Some(s) => PairingUi {
            active: true,
            role: Some(s.role.into()),
            code: s.code.clone(),
            safety: s.safety.clone(),
            status: Some(s.status.clone()),
            error: s.error.clone(),
            request,
        },
    })
}

/// Mint a one-time code and arm a host pairing session around it.
fn arm_host_session(state: &SyncState) -> Result<String, String> {
    let mut code_bytes = [0u8; 6];
    getrandom::fill(&mut code_bytes).map_err(|e| e.to_string())?;
    let raw = hex(&code_bytes);
    let code = format!("{}-{}-{}", &raw[..4], &raw[4..8], &raw[8..12]);
    *state.pairing.lock().unwrap() = Some(PairingSession {
        role: "host",
        code: Some(code.clone()),
        safety: None,
        status: "waiting".into(),
        error: None,
        confirm_tx: None,
        started_ms: pinq_core::now_ms(),
    });
    Ok(code)
}

/// Host side: arm a one-time pairing session and show the code.
#[tauri::command]
pub fn pairing_host_start(state: State<'_, SyncState>) -> Result<String, String> {
    arm_host_session(&state)
}

/// Click-to-pair, requester side: ask the device at `addr` to pair with us.
/// If its user accepts, it arms a host session and sends back the session
/// code, and we join automatically. The code travels in the clear, so the
/// SECURITY GATE is the 6-digit safety-number comparison both users must
/// confirm before any key material moves — an interceptor produces different
/// numbers on the two screens.
#[tauri::command]
pub fn pair_request(
    app: AppHandle,
    state: State<'_, SyncState>,
    addr: String,
) -> Result<(), String> {
    {
        let guard = state.pairing.lock().unwrap();
        if matches!(guard.as_ref(), Some(s) if s.status != "done" && s.status != "failed") {
            return Err("a pairing is already in progress".into());
        }
    }
    *state.pairing.lock().unwrap() = Some(PairingSession {
        role: "joiner",
        code: None,
        safety: None,
        status: "requesting".into(),
        error: None,
        confirm_tx: None,
        started_ms: pinq_core::now_ms(),
    });
    let name = state.device_name.clone();
    // Try every address this peer advertises, not just the display one.
    let candidates = {
        let peers = state.peers.lock().unwrap();
        peers
            .values()
            .find(|p| p.addr == addr || p.addrs.contains(&addr))
            .map(peer_candidates)
            .unwrap_or_else(|| vec![addr.clone()])
    };
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let (mut stream, good_addr) = connect_peer(&candidates)?;
            stream.set_write_timeout(Some(Duration::from_secs(20))).ok();
            // The other user gets 60s to answer; give the reply a little more.
            stream.set_read_timeout(Some(Duration::from_secs(75))).ok();
            write_frame(
                &mut stream,
                &serde_json::json!({ "type": "pair_request", "name": name }),
            )
            .map_err(|e| e.to_string())?;
            let reply = read_frame(&mut stream).map_err(|_| "no answer".to_string())?;
            match reply.get("type").and_then(|t| t.as_str()) {
                Some("pair_accept") => {
                    let code = reply
                        .get("code")
                        .and_then(|c| c.as_str())
                        .ok_or("malformed accept")?
                        .replace('-', "")
                        .to_lowercase();
                    let s = app.state::<SyncState>();
                    s.set_pairing_status("connecting", None);
                    drop(stream); // the SPAKE2 join opens its own connection
                    run_joiner(&app, &good_addr, &code)
                }
                Some("pair_decline") => Err("the other device declined".into()),
                _ => Err("unexpected reply".into()),
            }
        })();
        if let Err(e) = result {
            if let Some(s) = app.try_state::<SyncState>() {
                s.set_pairing_status("failed", Some(e));
            }
        }
    });
    Ok(())
}

/// Accepting side: answer the pending "wants to pair" prompt.
#[tauri::command]
pub fn pair_respond(state: State<'_, SyncState>, accept: bool) {
    let req = state.pair_req.lock().unwrap().take();
    if let Some(r) = req {
        let _ = r.reply_tx.send(accept);
    }
}

/// Joiner side: connect to a discovered peer and pair with its code.
#[tauri::command]
pub fn pairing_join(
    app: AppHandle,
    state: State<'_, SyncState>,
    addr: String,
    code: String,
) -> Result<(), String> {
    *state.pairing.lock().unwrap() = Some(PairingSession {
        role: "joiner",
        code: None,
        safety: None,
        status: "connecting".into(),
        error: None,
        confirm_tx: None,
        started_ms: pinq_core::now_ms(),
    });
    let code = code.trim().replace('-', "").to_lowercase();
    std::thread::spawn(move || {
        if let Err(e) = run_joiner(&app, &addr, &code) {
            if let Some(s) = app.try_state::<SyncState>() {
                s.set_pairing_status("failed", Some(e));
            }
        }
    });
    Ok(())
}

/// Both sides call this after visually comparing safety numbers.
#[tauri::command]
pub fn pairing_confirm(state: State<'_, SyncState>) {
    if let Some(s) = state.pairing.lock().unwrap().as_mut() {
        if let Some(tx) = s.confirm_tx.take() {
            let _ = tx.send(true);
        }
    }
}

/// Kick both sync loops immediately (Settings "Sync now" button).
#[tauri::command]
pub fn sync_now(state: State<'_, SyncState>) {
    state.sync_asap.store(true, std::sync::atomic::Ordering::Relaxed);
    state.relay_asap.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub pk: String,
    pub short: String,
    pub name: String,
    pub added_ms: u64,
    pub revoked: bool,
    pub is_self: bool,
}

/// The signed device registry for this space, folded into current membership.
/// Async: runs off the main thread (it reads the database).
#[tauri::command]
pub async fn get_registry(app: AppHandle, state: State<'_, SyncState>) -> Result<Vec<DeviceView>, ()> {
    let Some(store) = app.try_state::<Arc<Store>>() else { return Ok(Vec::new()) };
    let space = registry::space_tag(&state.space_key.lock().unwrap());
    let entries = store.registry_entries(&space).unwrap_or_default();
    let my_pk = registry::pk_of(&state.device_secret.lock().unwrap());
    let mut devices: Vec<DeviceView> = registry::fold(&space, &entries)
        .into_values()
        .map(|r| DeviceView {
            short: r.pk[..8].to_string(),
            is_self: r.pk == my_pk,
            revoked: r.revoked_ms.is_some(),
            name: r.name,
            added_ms: r.added_ms,
            pk: r.pk,
        })
        .collect();
    devices.sort_by(|a, b| a.added_ms.cmp(&b.added_ms).then(a.pk.cmp(&b.pk)));
    Ok(devices)
}

/// Sign a revocation for a device. It propagates on the next sync, after which
/// members refuse sessions with it. Honest limit (also shown in the UI): the
/// revoked device still holds the space key until the space is re-keyed.
#[tauri::command]
pub fn revoke_device(
    app: AppHandle,
    state: State<'_, SyncState>,
    pk: String,
) -> Result<(), String> {
    let secret = *state.device_secret.lock().unwrap();
    if registry::pk_of(&secret) == pk {
        return Err("a device can't remove itself".into());
    }
    let store = app.state::<Arc<Store>>();
    let space = registry::space_tag(&state.space_key.lock().unwrap());
    let entries = store.registry_entries(&space).map_err(|e| e.to_string())?;
    let name = registry::fold(&space, &entries)
        .get(&pk)
        .map(|r| r.name.clone())
        .ok_or("unknown device")?;
    let e = registry::sign_entry(&secret, &space, "revoke", &pk, &name, pinq_core::now_ms());
    store.add_registry_entries(&space, &[e]).map_err(|e| e.to_string())?;
    // Push the revocation to the remaining devices right away.
    state.sync_asap.store(true, std::sync::atomic::Ordering::Relaxed);
    state.relay_asap.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Rotate the space key: deliver sealed grants to every reachable survivor
/// (online LAN peers now, the relay for everyone else), then switch this
/// device. The revoked device sees the deliveries but can open none of them.
/// Async command: blocking network work must stay off the main thread.
#[tauri::command]
pub async fn rotate_space(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<SyncState>();
        let store = app.state::<Arc<Store>>();
        let blobs = crate::blobs::blobs_dir(&app).map_err(|e| e.to_string())?;
        let old_key = *state.space_key.lock().unwrap();
        let secret = *state.device_secret.lock().unwrap();
        let device_name = state.device_name.clone();
        let space = registry::space_tag(&old_key);

        let (msg, new_key) = build_rekey(&store, &space, &secret, &device_name)?;
        let survivors = msg.grants.len();

        // Hand-deliver to every same-space peer that's online right now.
        let tag = state.current_tag.lock().unwrap().clone();
        let peers: Vec<PeerInfo> = state
            .peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.tag == tag && !p.addr.is_empty())
            .cloned()
            .collect();
        let mut delivered = 0usize;
        for peer in &peers {
            let ok = connect_peer(&peer_candidates(peer))
                .and_then(|(s, _)| {
                    sync_as_initiator(&store, &blobs, &old_key, &secret, &device_name, Some(&msg), s)
                })
                .is_ok();
            if ok {
                delivered += 1;
            }
        }

        // Leave the rotation at the relay for survivors that are offline.
        let relay_addr = app
            .try_state::<crate::settings::SettingsStore>()
            .and_then(|s| effective_relay_addr(&state, &s.current()));
        let mut relayed = false;
        if let Some(relay_addr) = relay_addr {
            match relay_rekey_post(&store, &old_key, &secret, &device_name, &relay_addr, &msg) {
                Ok(()) => relayed = true,
                Err(e) => eprintln!("pinq: rekey relay post failed: {e}"),
            }
        }

        // Now switch ourselves.
        let new_registry = msg.new_registry.clone();
        apply_rekey(&app, &AcceptedRekey { new_key, new_registry });

        if survivors == 0 {
            return Ok("Space key rotated. This was the only device — nothing to deliver.".into());
        }
        let mut parts = vec![format!(
            "Space key rotated. Delivered to {delivered} of {survivors} device(s) directly"
        )];
        if relayed {
            parts.push("the rest will pick it up from the relay".into());
        } else if delivered < survivors {
            parts.push(
                "devices that were offline have no relay to fetch it from — re-pair them".into(),
            );
        }
        Ok(parts.join("; ") + ".")
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn pairing_cancel(state: State<'_, SyncState>) {
    let mut guard = state.pairing.lock().unwrap();
    if let Some(s) = guard.as_mut() {
        if let Some(tx) = s.confirm_tx.take() {
            let _ = tx.send(false);
        }
    }
    *guard = None;
}

// ---------- framing ----------

fn write_frame(stream: &mut TcpStream, value: &serde_json::Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn read_frame_cap(stream: &mut TcpStream, cap: u32) -> std::io::Result<serde_json::Value> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > cap {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(std::io::Error::other)
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<serde_json::Value> {
    read_frame_cap(stream, MAX_FRAME)
}

fn b64f(v: &serde_json::Value, key: &str) -> std::io::Result<Vec<u8>> {
    use base64::Engine;
    let s = v.get(key).and_then(|x| x.as_str()).ok_or_else(|| {
        std::io::Error::other(format!("missing field {key}"))
    })?;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(std::io::Error::other)
}

fn safety_number(key: &[u8]) -> String {
    let h = blake3::keyed_hash(
        key.try_into().unwrap_or(&[0u8; 32]),
        b"pinq-pair-safety-v1",
    );
    let n = u32::from_le_bytes(h.as_bytes()[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

fn seal_key(shared: &[u8], plaintext: &[u8; 32]) -> std::io::Result<Vec<u8>> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
    let k = blake3::keyed_hash(shared.try_into().map_err(|_| std::io::Error::other("bad key len"))?, b"pinq-pair-enc-v1");
    let cipher = ChaCha20Poly1305::new(k.as_bytes().into());
    // Single message under a fresh PAKE-derived key: zero nonce is safe.
    cipher
        .encrypt(&Nonce::default(), plaintext.as_slice())
        .map_err(|_| std::io::Error::other("encrypt failed"))
}

fn open_key(shared: &[u8], ciphertext: &[u8]) -> std::io::Result<[u8; 32]> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
    let k = blake3::keyed_hash(shared.try_into().map_err(|_| std::io::Error::other("bad key len"))?, b"pinq-pair-enc-v1");
    let cipher = ChaCha20Poly1305::new(k.as_bytes().into());
    let pt = cipher
        .decrypt(&Nonce::default(), ciphertext)
        .map_err(|_| std::io::Error::other("decrypt failed (wrong key?)"))?;
    pt.try_into().map_err(|_| std::io::Error::other("bad key size"))
}

// ---------- key storage ----------

fn get_or_create_secret(app: &AppHandle, name: &str) -> std::io::Result<([u8; 32], &'static str)> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;

    // A key file from an earlier credential-store denial IS this device's
    // identity — prefer it, and never let a permission prompt decide which
    // keys we run with (on macOS, every rebuilt binary re-prompts to read
    // the keychain, and answering "allow" would resurrect older keys and
    // silently unpair the device).
    let dir = app.path().app_data_dir().map_err(std::io::Error::other)?.join("keys");
    let path = dir.join(format!("{name}.key"));
    if let Ok(existing) = std::fs::read(&path) {
        if existing.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&existing);
            return Ok((key, "file"));
        }
    }

    if let Ok(entry) = keyring::Entry::new("Pinq", name) {
        match entry.get_password() {
            Ok(stored) => {
                if let Ok(bytes) = b64.decode(&stored) {
                    if bytes.len() == 32 {
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&bytes);
                        return Ok((key, "credential-store"));
                    }
                }
            }
            Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(std::io::Error::other)?;
                if entry.set_password(&b64.encode(key)).is_ok() {
                    return Ok((key, "credential-store"));
                }
            }
            Err(_) => {}
        }
    }

    eprintln!("pinq: credential store unavailable for '{name}', using key file");
    let dir = app.path().app_data_dir().map_err(std::io::Error::other)?.join("keys");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{name}.key"));
    if let Ok(existing) = std::fs::read(&path) {
        if existing.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&existing);
            return Ok((key, "file"));
        }
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(std::io::Error::other)?;
    std::fs::write(&path, key)?;
    Ok((key, "file"))
}

fn set_secret(app: &AppHandle, name: &str, key: &[u8; 32]) -> std::io::Result<()> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    // Mirror get_or_create_secret: a device whose identity lives in a key
    // file must keep writing there — a rotation stored in the keychain would
    // be invisible to the file-first reads.
    let dir = app.path().app_data_dir().map_err(std::io::Error::other)?.join("keys");
    let path = dir.join(format!("{name}.key"));
    if path.exists() {
        return std::fs::write(&path, key);
    }
    if let Ok(entry) = keyring::Entry::new("Pinq", name) {
        if entry.set_password(&b64.encode(key)).is_ok() {
            return Ok(());
        }
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&path, key)
}

// ---------- init ----------

pub fn init(app: &AppHandle) {
    let (device_secret, storage_a) = match get_or_create_secret(app, "device-key") {
        Ok(v) => v,
        Err(e) => {
            app.manage(SyncState { key_storage: format!("unavailable: {e}"), ..Default::default() });
            return;
        }
    };
    let (space_key, storage_b) = match get_or_create_secret(app, "space-key") {
        Ok(v) => v,
        Err(e) => {
            app.manage(SyncState { key_storage: format!("unavailable: {e}"), ..Default::default() });
            return;
        }
    };

    let signing = ed25519_dalek::SigningKey::from_bytes(&device_secret);
    let pubkey = signing.verifying_key();
    let device_short = hex(pubkey.as_bytes())[..8].to_string();
    let device_name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "this-device".into());

    let state = SyncState {
        device_name: device_name.clone(),
        device_fingerprint: fingerprint(pubkey.as_bytes(), 6),
        device_short: device_short.clone(),
        key_storage: if storage_a == "file" || storage_b == "file" { "file" } else { "credential-store" }
            .to_string(),
        device_secret: Mutex::new(device_secret),
        space_key: Mutex::new(space_key),
        current_tag: Mutex::new(lan_tag(&space_key)),
        space_fingerprint: Mutex::new(space_fp(&space_key)),
        mdns: Mutex::new(None),
        discovery_error: Mutex::new(None),
        peers: Mutex::new(HashMap::new()),
        pairing: Mutex::new(None),
        pair_req: Mutex::new(None),
        syncing: Mutex::new(Default::default()),
        last_sync: Mutex::new(HashMap::new()),
        sync_asap: std::sync::atomic::AtomicBool::new(false),
        relay_asap: std::sync::atomic::AtomicBool::new(false),
        relay_status: Mutex::new(None),
        relay_host_info: Mutex::new(None),
        relay_host_flag: Mutex::new(None),
        peer_open_notes: Mutex::new(HashMap::new()),
        last_live: Mutex::new(Default::default()),
        live_push: Mutex::new(Default::default()),
        sync_wake: Mutex::new(None),
    };
    app.manage(state);

    // First device in a space declares itself the registry genesis.
    if let Some(store) = app.try_state::<Arc<Store>>() {
        ensure_genesis(&store, &registry::space_tag(&space_key), &device_secret, &device_name);
    }

    // Start the hosted relay before discovery registers, so the very first
    // mDNS advertisement already carries it.
    update_relay_host(app);

    {
        let app = app.clone();
        std::thread::spawn(move || run_discovery(app));
    }
    {
        let app = app.clone();
        std::thread::spawn(move || run_listener(app));
    }
    {
        let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
        *app.state::<SyncState>().sync_wake.lock().unwrap() = Some(wake_tx);
        let app = app.clone();
        std::thread::spawn(move || run_sync_loop(app, wake_rx));
    }
    {
        let app = app.clone();
        std::thread::spawn(move || run_relay_loop(app));
    }
    {
        let app = app.clone();
        std::thread::spawn(move || run_presence_loop(app));
    }
}

// ---------- discovery ----------

fn service_info(
    device_name: &str,
    device_short: &str,
    tag: &str,
    relay: Option<(u16, String)>,
) -> Result<mdns_sd::ServiceInfo, mdns_sd::Error> {
    let mut props: Vec<(String, String)> = vec![
        ("name".into(), device_name.into()),
        ("tag".into(), tag.into()),
        ("dev".into(), device_short.into()),
    ];
    if let Some((port, id)) = relay {
        props.push(("relayPort".into(), port.to_string()));
        props.push(("relayId".into(), id));
    }
    mdns_sd::ServiceInfo::new(
        SERVICE_TYPE,
        &format!("pinq-{device_short}"),
        &format!("pinq-{device_short}.local."),
        "",
        SYNC_PORT,
        &props[..],
    )
    .map(|s| s.enable_addr_auto())
}

fn hosted_relay_props(state: &SyncState) -> Option<(u16, String)> {
    state
        .relay_host_info
        .lock()
        .unwrap()
        .as_ref()
        .filter(|i| i.error.is_none())
        .map(|i| (i.port, i.id.clone()))
}

/// Re-advertise with the current tag (after joining a new space, or when the
/// hosted relay starts/stops).
fn re_register(state: &SyncState) {
    let guard = state.mdns.lock().unwrap();
    let Some(daemon) = guard.as_ref() else { return };
    let fullname = format!("pinq-{}.{}", state.device_short, SERVICE_TYPE);
    let _ = daemon.unregister(&fullname);
    let tag = state.current_tag.lock().unwrap().clone();
    if let Ok(si) =
        service_info(&state.device_name, &state.device_short, &tag, hosted_relay_props(state))
    {
        let _ = daemon.register(si);
    }
}

fn run_discovery(app: AppHandle) {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let fail = |app: &AppHandle, e: String| {
        if let Some(s) = app.try_state::<SyncState>() {
            *s.discovery_error.lock().unwrap() = Some(e);
        }
    };

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => return fail(&app, format!("mDNS unavailable: {e}")),
    };

    let (device_name, device_short, tag, relay) = {
        let s = app.state::<SyncState>();
        let d = (
            s.device_name.clone(),
            s.device_short.clone(),
            s.current_tag.lock().unwrap().clone(),
            hosted_relay_props(&s),
        );
        *s.mdns.lock().unwrap() = Some(daemon.clone());
        d
    };

    match service_info(&device_name, &device_short, &tag, relay) {
        Ok(si) => {
            if let Err(e) = daemon.register(si) {
                return fail(&app, format!("advertise failed: {e}"));
            }
        }
        Err(e) => return fail(&app, format!("service info: {e}")),
    }

    let receiver = match daemon.browse(SERVICE_TYPE) {
        Ok(r) => r,
        Err(e) => return fail(&app, format!("browse failed: {e}")),
    };

    while let Ok(event) = receiver.recv() {
        let Some(state) = app.try_state::<SyncState>() else { continue };
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let dev = info.get_property_val_str("dev").unwrap_or_default().to_string();
                if dev.is_empty() || dev == state.device_short {
                    continue;
                }
                let peer_tag = info.get_property_val_str("tag").unwrap_or_default().to_string();
                let name = info
                    .get_property_val_str("name")
                    .unwrap_or("unknown device")
                    .to_string();
                let addrs = rank_addrs(
                    info.get_addresses().iter().map(|a| a.to_ip_addr()),
                    info.get_port(),
                );
                let addr = addrs.first().cloned().unwrap_or_default();
                // A peer hosting a relay advertises it; the id doubles as the
                // pin, so adopted relays are identity-verified from the start.
                let relay_addr = match (
                    info.get_property_val_str("relayPort"),
                    info.get_property_val_str("relayId"),
                ) {
                    (Some(port), Some(id)) if !port.is_empty() && !id.is_empty() => addrs
                        .first()
                        .and_then(|a| a.parse::<std::net::SocketAddr>().ok())
                        .map(|s| match s.ip() {
                            std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}#{id}"),
                            v4 => format!("{v4}:{port}#{id}"),
                        }),
                    _ => None,
                };
                state.peers.lock().unwrap().insert(
                    dev.clone(),
                    PeerInfo {
                        device_name: name,
                        device_short: dev,
                        addr,
                        addrs,
                        tag: peer_tag,
                        same_space: false,
                        last_seen_ms: pinq_core::now_ms(),
                        last_sync: None,
                        relay_addr,
                    },
                );
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                state
                    .peers
                    .lock()
                    .unwrap()
                    .retain(|dev, _| !fullname.contains(&format!("pinq-{dev}")));
            }
            _ => {}
        }
    }
}

// ---------- pairing protocol ----------

/// One listener serves both protocols on :47814. The first frame decides:
/// a SPAKE2 `msg` routes to pairing (only while a host session is armed);
/// a `sync_hello` routes to a sync session (gated by space-key possession —
/// the AEAD handshake, not this router, is the security boundary).
///
/// Dual-stack: discovery advertises every address of every family, and IPv4
/// and IPv6 are separate sockets on macOS/Windows — listening on only one
/// family answers "connection refused" to peers dialing the other. Bind both;
/// one family failing is fine (e.g. Linux dual-stack sockets collide, and the
/// v4 bind already covers both there).
fn run_listener(app: AppHandle) {
    let v4 = TcpListener::bind(("0.0.0.0", SYNC_PORT));
    let v6 = TcpListener::bind(("::", SYNC_PORT));
    if let (Err(e), Err(_)) = (&v4, &v6) {
        if let Some(s) = app.try_state::<SyncState>() {
            *s.discovery_error.lock().unwrap() = Some(format!("sync listener failed: {e}"));
        }
        return;
    }
    for listener in [v4, v6].into_iter().flatten() {
        let app = app.clone();
        std::thread::spawn(move || accept_loop(app, listener));
    }
}

fn accept_loop(app: AppHandle, listener: TcpListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let app = app.clone();
        std::thread::spawn(move || {
            stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
            let Ok(first) = read_frame(&mut stream) else { return };

            if first.get("type").and_then(|t| t.as_str()) == Some("sync_hello") {
                run_sync_responder_app(&app, stream, first);
                return;
            }

            if first.get("type").and_then(|t| t.as_str()) == Some("pair_request") {
                handle_pair_request(&app, stream, &first);
                return;
            }

            if first.get("type").and_then(|t| t.as_str()) == Some("presence") {
                handle_presence(&app, &first);
                return;
            }

            if first.get("msg").is_some() {
                let armed = {
                    let Some(s) = app.try_state::<SyncState>() else { return };
                    let guard = s.pairing.lock().unwrap();
                    matches!(guard.as_ref(), Some(p) if p.role == "host" && p.status == "waiting")
                };
                if !armed {
                    return;
                }
                if let Err(e) = run_host(&app, stream, first) {
                    if let Some(s) = app.try_state::<SyncState>() {
                        s.set_pairing_status("failed", Some(e));
                    }
                }
            }
        });
    }
}

/// Another device clicked "Pair" on us: surface an Accept/Decline prompt and
/// hold the connection open until the user answers (or 60s passes). On accept
/// we arm a normal host session and hand its code back — the requester joins
/// with it automatically and the flow becomes the ordinary SPAKE2 →
/// safety-number → confirm path.
fn handle_pair_request(app: &AppHandle, mut stream: TcpStream, first: &serde_json::Value) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let name = first
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("Another device")
        .chars()
        .take(40)
        .collect::<String>();

    // Busy: one pairing (or one prompt) at a time.
    let busy = {
        let pairing = state.pairing.lock().unwrap();
        matches!(pairing.as_ref(), Some(s) if s.status != "done" && s.status != "failed")
    } || state.pair_req.lock().unwrap().is_some();
    if busy {
        let _ = write_frame(&mut stream, &serde_json::json!({ "type": "pair_decline" }));
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    *state.pair_req.lock().unwrap() = Some(IncomingPair { name: name.clone(), reply_tx: tx });
    let _ = app.emit("pinq-pair-request", &name);
    {
        use tauri_plugin_notification::NotificationExt;
        let _ = app
            .notification()
            .builder()
            .title("Pinq pairing")
            .body(format!("“{name}” wants to pair with this device — open Settings"))
            .show();
    }

    stream.set_write_timeout(Some(Duration::from_secs(20))).ok();
    let accepted = rx.recv_timeout(Duration::from_secs(60)).unwrap_or(false);
    // Clear the prompt whichever way this went (pair_respond may have taken it
    // already; a timeout leaves it behind for us to clean up).
    let _ = state.pair_req.lock().unwrap().take();

    if !accepted {
        let _ = write_frame(&mut stream, &serde_json::json!({ "type": "pair_decline" }));
        return;
    }
    match arm_host_session(&state) {
        Ok(code) => {
            let _ = write_frame(
                &mut stream,
                &serde_json::json!({ "type": "pair_accept", "code": code }),
            );
        }
        Err(_) => {
            let _ = write_frame(&mut stream, &serde_json::json!({ "type": "pair_decline" }));
        }
    }
}

fn run_host(app: &AppHandle, mut stream: TcpStream, hello: serde_json::Value) -> Result<(), String> {
    use base64::Engine;
    use spake2::{Ed25519Group, Identity, Password, Spake2};
    let b64 = base64::engine::general_purpose::STANDARD;

    stream.set_read_timeout(Some(Duration::from_secs(150))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(20))).ok();

    let state = app.state::<SyncState>();
    let code = {
        let mut guard = state.pairing.lock().unwrap();
        let Some(s) = guard.as_mut() else { return Err("no session".into()) };
        s.status = "connecting".into();
        s.code.clone().ok_or("no code")?.replace('-', "").to_lowercase()
    };

    // One round: joiner's message arrived (routed here by the listener), ours
    // goes back, both derive the key.
    let msg_joiner = b64f(&hello, "msg").map_err(|e| e.to_string())?;
    let (spake, msg_host) = Spake2::<Ed25519Group>::start_a(
        &Password::new(code.as_bytes()),
        &Identity::new(b"pinq-host"),
        &Identity::new(b"pinq-joiner"),
    );
    write_frame(&mut stream, &serde_json::json!({ "v": 1, "msg": b64.encode(&msg_host) }))
        .map_err(|e| e.to_string())?;
    let shared = spake
        .finish(&msg_joiner)
        .map_err(|_| "code mismatch — pairing aborted (start again with a new code)".to_string())?;

    // Both screens now show this; the user must compare before anything moves.
    let rx = state.set_pairing_safety(safety_number(&shared));
    let confirmed = rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false);
    if !confirmed {
        return Err("cancelled".into());
    }

    state.set_pairing_status("applying", None);

    // The joiner introduces its device key so we can enroll it in the signed
    // registry before it ever syncs. (pk + name aren't secrets — both are
    // already broadcast over mDNS; the registry record is what's new.)
    let ident = read_frame(&mut stream).map_err(|e| e.to_string())?;
    if ident.get("type").and_then(|t| t.as_str()) != Some("ident") {
        return Err("peer did not introduce itself".into());
    }
    let joiner_pk = ident.get("pk").and_then(|p| p.as_str()).unwrap_or_default().to_string();
    let joiner_name = ident.get("name").and_then(|p| p.as_str()).unwrap_or("device").to_string();

    let space_key = *state.space_key.lock().unwrap();
    let secret = *state.device_secret.lock().unwrap();
    let space = registry::space_tag(&space_key);
    let mut entries = Vec::new();
    if let Some(store) = app.try_state::<Arc<Store>>() {
        ensure_genesis(&store, &space, &secret, &state.device_name);
        if joiner_pk.len() == 64 {
            let existing = store.registry_entries(&space).unwrap_or_default();
            if registry::standing(&registry::fold(&space, &existing), &joiner_pk).is_none() {
                let e = registry::sign_entry(
                    &secret,
                    &space,
                    "add",
                    &joiner_pk,
                    &joiner_name,
                    pinq_core::now_ms(),
                );
                let _ = store.add_registry_entries(&space, &[e]);
            }
        }
        entries = store.registry_entries(&space).unwrap_or_default();
    }

    let sealed = seal_key(&shared, &space_key).map_err(|e| e.to_string())?;
    write_frame(
        &mut stream,
        &serde_json::json!({ "type": "space_key", "ct": b64.encode(&sealed), "registry": entries }),
    )
    .map_err(|e| e.to_string())?;

    // Wait for the joiner to confirm + apply.
    let done = read_frame(&mut stream).map_err(|e| e.to_string())?;
    if done.get("type").and_then(|t| t.as_str()) != Some("done") {
        return Err("peer did not complete".into());
    }
    state.set_pairing_status("done", None);
    Ok(())
}

fn run_joiner(app: &AppHandle, addr: &str, code: &str) -> Result<(), String> {
    use base64::Engine;
    use spake2::{Ed25519Group, Identity, Password, Spake2};
    let b64 = base64::engine::general_purpose::STANDARD;

    let state = app.state::<SyncState>();
    let (mut stream, _) = connect_peer(&[addr.to_string()])?;
    stream.set_read_timeout(Some(Duration::from_secs(150))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(20))).ok();

    let (spake, msg_joiner) = Spake2::<Ed25519Group>::start_b(
        &Password::new(code.as_bytes()),
        &Identity::new(b"pinq-host"),
        &Identity::new(b"pinq-joiner"),
    );
    write_frame(&mut stream, &serde_json::json!({ "v": 1, "msg": b64.encode(&msg_joiner) }))
        .map_err(|e| e.to_string())?;
    let reply = read_frame(&mut stream).map_err(|e| e.to_string())?;
    let msg_host = b64f(&reply, "msg").map_err(|e| e.to_string())?;
    let shared = spake
        .finish(&msg_host)
        .map_err(|_| "code mismatch — check the code and try again".to_string())?;

    let rx = state.set_pairing_safety(safety_number(&shared));
    let confirmed = rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false);
    if !confirmed {
        return Err("cancelled".into());
    }

    state.set_pairing_status("applying", None);

    // Introduce this device so the host can enroll it in the signed registry.
    let secret = *state.device_secret.lock().unwrap();
    write_frame(
        &mut stream,
        &serde_json::json!({
            "type": "ident",
            "pk": registry::pk_of(&secret),
            "name": state.device_name,
        }),
    )
    .map_err(|e| e.to_string())?;

    let frame = read_frame(&mut stream).map_err(|e| e.to_string())?;
    if frame.get("type").and_then(|t| t.as_str()) != Some("space_key") {
        return Err("unexpected message".into());
    }
    let sealed = b64f(&frame, "ct").map_err(|e| e.to_string())?;
    let new_space_key = open_key(&shared, &sealed).map_err(|e| e.to_string())?;

    // Adopt the host's space: persist, refresh tag + fingerprint, re-advertise.
    set_secret(app, "space-key", &new_space_key).map_err(|e| e.to_string())?;
    *state.space_key.lock().unwrap() = new_space_key;
    *state.current_tag.lock().unwrap() = lan_tag(&new_space_key);
    *state.space_fingerprint.lock().unwrap() = space_fp(&new_space_key);
    re_register(&state);

    // Take the host's registry along with the key (our own membership record
    // is in there, signed by the host). Records for our OLD space stay inert.
    if let Some(store) = app.try_state::<Arc<Store>>() {
        let space = registry::space_tag(&new_space_key);
        if let Ok(entries) =
            serde_json::from_value::<Vec<RegistryEntry>>(frame.get("registry").cloned().unwrap_or_default())
        {
            let valid: Vec<RegistryEntry> =
                entries.into_iter().filter(|e| registry::verify_entry(&space, e)).collect();
            let _ = store.add_registry_entries(&space, &valid);
        }
    }

    write_frame(&mut stream, &serde_json::json!({ "type": "done" })).map_err(|e| e.to_string())?;
    state.set_pairing_status("done", None);
    Ok(())
}

// ---------- sync protocol (spike 3) ----------

/// Encrypted JSON framing over TCP. Every frame is sealed with ChaCha20Poly1305
/// under a per-session key; nonces are direction-tagged counters, so frames
/// can't be dropped, reordered, or replayed without failing authentication.
struct SecureChan {
    stream: TcpStream,
    cipher: chacha20poly1305::ChaCha20Poly1305,
    send_dir: u8,
    send_ctr: u64,
    recv_ctr: u64,
}

fn nonce_for(dir: u8, ctr: u64) -> chacha20poly1305::Nonce {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[4..].copy_from_slice(&ctr.to_le_bytes());
    n.into()
}

impl SecureChan {
    /// dir: 1 = initiator, 2 = responder. Session key = space key (proves
    /// membership) ⊕ hello nonces (uniqueness) ⊕ ephemeral ECDH (forward
    /// secrecy: a recorded session stays dark even if the space key leaks
    /// later — the ephemeral secrets die with the session).
    fn new(
        stream: TcpStream,
        space_key: &[u8; 32],
        ni: &[u8],
        nr: &[u8],
        eph_shared: &[u8; 32],
        initiator: bool,
    ) -> Self {
        use chacha20poly1305::KeyInit;
        let mut material = b"pinq-sync-sess-v2".to_vec();
        material.extend_from_slice(ni);
        material.extend_from_slice(nr);
        material.extend_from_slice(eph_shared);
        let key = blake3::keyed_hash(space_key, &material);
        Self {
            stream,
            cipher: chacha20poly1305::ChaCha20Poly1305::new(key.as_bytes().into()),
            send_dir: if initiator { 1 } else { 2 },
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    fn send(&mut self, v: &serde_json::Value) -> std::io::Result<()> {
        use chacha20poly1305::aead::Aead;
        let plain = serde_json::to_vec(v)?;
        let sealed = self
            .cipher
            .encrypt(&nonce_for(self.send_dir, self.send_ctr), plain.as_slice())
            .map_err(|_| std::io::Error::other("encrypt failed"))?;
        self.send_ctr += 1;
        self.stream.write_all(&(sealed.len() as u32).to_le_bytes())?;
        self.stream.write_all(&sealed)?;
        self.stream.flush()
    }

    fn recv(&mut self) -> std::io::Result<serde_json::Value> {
        use chacha20poly1305::aead::Aead;
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len);
        if len > MAX_FRAME {
            return Err(std::io::Error::other("frame too large"));
        }
        let mut buf = vec![0u8; len as usize];
        self.stream.read_exact(&mut buf)?;
        let their_dir = if self.send_dir == 1 { 2 } else { 1 };
        let plain = self
            .cipher
            .decrypt(&nonce_for(their_dir, self.recv_ctr), buf.as_slice())
            .map_err(|_| std::io::Error::other("peer is not in this space (decrypt failed)"))?;
        self.recv_ctr += 1;
        serde_json::from_slice(&plain).map_err(std::io::Error::other)
    }
}

// ---------- encrypted envelopes (relay wire format) ----------
//
// The relay stores-and-forwards envelopes for devices that aren't online
// together. The carrier sees only a version byte, a random nonce, and
// ciphertext — no note text, no op kinds, no device names, no blob bytes.

/// What travels inside an envelope: ops, the (small) attachment blobs those
/// ops reference so images survive the relay hop, and the sender's current
/// registry records so device adds/revocations propagate off-LAN too. The ops
/// are signed by the sender's device key — a store-and-forward carrier can't
/// do the LAN handshake, so the signature is what ties an envelope to a
/// registry identity (and lets receivers refuse envelopes from revoked devices).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct EnvelopePayload {
    pub ops: Vec<SyncOp>,
    /// (hash, base64 bytes) pairs; oversized blobs are left for LAN sessions.
    #[serde(default)]
    pub blobs: Vec<(String, String)>,
    #[serde(default)]
    pub registry: Vec<RegistryEntry>,
    #[serde(default)]
    pub sender_pk: String,
    #[serde(default)]
    pub sender_name: String,
    /// Device signature over the serialized ops (domain-separated).
    #[serde(default)]
    pub sig: String,
    /// A space rotation riding the old channel (grants are sealed to device
    /// identities, so the old space key can't open them).
    #[serde(default)]
    pub rekey: Option<RekeyMsg>,
}

fn envelope_sig_msg(ops: &[SyncOp]) -> Result<Vec<u8>, String> {
    Ok([b"pinq-envelope-sig-v1".as_slice(), &serde_json::to_vec(ops).map_err(|e| e.to_string())?]
        .concat())
}

/// Seal a payload for an untrusted carrier.
pub fn seal_envelope(space_key: &[u8; 32], payload: &EnvelopePayload) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    let key = blake3::keyed_hash(space_key, b"pinq-envelope-v1");
    let cipher = ChaCha20Poly1305::new(key.as_bytes().into());
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(&nonce.into(), plain.as_slice())
        .map_err(|_| "envelope encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(13 + ct.len());
    out.push(1); // envelope format version
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open an envelope; fails on the wrong space key or any tampering.
pub fn open_envelope(space_key: &[u8; 32], bytes: &[u8]) -> Result<EnvelopePayload, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    if bytes.len() < 14 || bytes[0] != 1 {
        return Err("not a v1 envelope".into());
    }
    let nonce: [u8; 12] = bytes[1..13].try_into().unwrap();
    let key = blake3::keyed_hash(space_key, b"pinq-envelope-v1");
    let cipher = ChaCha20Poly1305::new(key.as_bytes().into());
    let plain = cipher
        .decrypt(&nonce.into(), &bytes[13..])
        .map_err(|_| "envelope decrypt failed (wrong space key or tampered)".to_string())?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

// ---------- live-editing presence ----------
//
// Each device broadcasts which notes it has open; a note open on both ends of
// a pair is "live" — the frontend tightens its edit debounce and commits wake
// the sync loop immediately (see note_edited). Presence is advisory: a forged
// or replayed frame can only cause extra (correct, vv-gated) sync sessions
// and a tighter debounce, and frames are sealed under a space-key-derived key
// so non-space peers can neither read nor forge them.

#[derive(serde::Serialize, serde::Deserialize)]
struct PresencePayload {
    device_short: String,
    open: Vec<String>,
    at_ms: u64,
}

fn presence_key(space_key: &[u8; 32]) -> blake3::Hash {
    blake3::keyed_hash(space_key, b"pinq-presence-v1")
}

fn seal_presence(space_key: &[u8; 32], payload: &PresencePayload) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    let cipher = ChaCha20Poly1305::new(presence_key(space_key).as_bytes().into());
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(&nonce.into(), plain.as_slice())
        .map_err(|_| "presence encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(13 + ct.len());
    out.push(1); // presence format version
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_presence(space_key: &[u8; 32], bytes: &[u8]) -> Result<PresencePayload, String> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
    if bytes.len() < 14 || bytes[0] != 1 {
        return Err("not a v1 presence frame".into());
    }
    let nonce: [u8; 12] = bytes[1..13].try_into().unwrap();
    let cipher = ChaCha20Poly1305::new(presence_key(space_key).as_bytes().into());
    let plain = cipher
        .decrypt(&nonce.into(), &bytes[13..])
        .map_err(|_| "presence decrypt failed".to_string())?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

/// Incoming "presence" frame: decode, drop silently unless it's from our
/// space and fresh, then record the peer's open set.
fn handle_presence(app: &AppHandle, first: &serde_json::Value) {
    use base64::Engine;
    let Some(state) = app.try_state::<SyncState>() else { return };
    let Some(data) = first.get("data").and_then(|d| d.as_str()) else { return };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else { return };
    let space_key = *state.space_key.lock().unwrap();
    let Ok(payload) = open_presence(&space_key, &bytes) else { return };
    let now = pinq_core::now_ms();
    if now.abs_diff(payload.at_ms) > PRESENCE_MAX_AGE_MS {
        return;
    }
    state
        .peer_open_notes
        .lock()
        .unwrap()
        .insert(payload.device_short, (payload.open.into_iter().collect(), now));
    recompute_live(app);
}

/// Diff the union of live peer open-sets against the last one and tell each
/// affected note window whether it's now co-open on another device.
fn recompute_live(app: &AppHandle) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let now = pinq_core::now_ms();
    let union: std::collections::HashSet<String> = state
        .peer_open_notes
        .lock()
        .unwrap()
        .values()
        .filter(|(_, at)| now.saturating_sub(*at) <= PRESENCE_TTL_MS)
        .flat_map(|(set, _)| set.iter().cloned())
        .collect();
    let prev = std::mem::replace(&mut *state.last_live.lock().unwrap(), union.clone());
    for id in union.symmetric_difference(&prev) {
        let _ = app.emit(
            "pinq-live",
            serde_json::json!({ "noteId": id, "live": union.contains(id) }),
        );
    }
}

/// True while any live (non-expired) peer has this note open.
#[tauri::command]
pub fn note_is_live(state: State<'_, SyncState>, id: String) -> bool {
    let now = pinq_core::now_ms();
    state
        .peer_open_notes
        .lock()
        .unwrap()
        .values()
        .any(|(set, at)| now.saturating_sub(*at) <= PRESENCE_TTL_MS && set.contains(&id))
}

/// Nudge the sync loop to run now instead of at the next tick. Wired to the
/// store's commit hook, so every local content commit (new note, edit, tag,
/// drop, trash) starts propagating immediately.
pub fn wake(app: &AppHandle) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let tx = state.sync_wake.lock().unwrap().clone();
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
}

/// A content op just committed for `note_id`. If any live peer has that note
/// open, target it for an immediate session and wake the sync loop — the
/// session itself is still the ordinary vv-delta exchange.
pub fn note_edited(app: &AppHandle, note_id: &str) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let now = pinq_core::now_ms();
    let targets: Vec<String> = state
        .peer_open_notes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, (set, at))| {
            now.saturating_sub(*at) <= PRESENCE_TTL_MS && set.contains(note_id)
        })
        .map(|(dev, _)| dev.clone())
        .collect();
    if targets.is_empty() {
        return;
    }
    state.live_push.lock().unwrap().extend(targets);
    let tx = state.sync_wake.lock().unwrap().clone();
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
}

/// Every second: expire stale peer presence, and (re)send our own open-note
/// set to same-space peers when it changes or every PRESENCE_RESEND_MS.
fn run_presence_loop(app: AppHandle) {
    use base64::Engine;
    let mut last_sent: Option<(std::collections::BTreeSet<String>, u64)> = None;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let Some(state) = app.try_state::<SyncState>() else { continue };

        // Expire peers that stopped refreshing (crash, offline, note closed
        // forever) so live mode falls back to the normal cadence.
        let now = pinq_core::now_ms();
        let expired = {
            let mut map = state.peer_open_notes.lock().unwrap();
            let before = map.len();
            map.retain(|_, (_, at)| now.saturating_sub(*at) <= PRESENCE_TTL_MS);
            map.len() != before
        };
        if expired {
            recompute_live(&app);
        }

        let open: std::collections::BTreeSet<String> = app
            .webview_windows()
            .keys()
            .filter_map(|l| crate::notes::label_to_id(l).map(str::to_string))
            .collect();
        let due = match &last_sent {
            Some((set, at)) => *set != open || now.saturating_sub(*at) >= PRESENCE_RESEND_MS,
            None => true,
        };
        if !due {
            continue;
        }

        let tag = state.current_tag.lock().unwrap().clone();
        let peers: Vec<PeerInfo> = state
            .peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| !tag.is_empty() && p.tag == tag && !p.addr.is_empty())
            .cloned()
            .collect();
        if peers.is_empty() {
            // Nothing to tell; still remember the set so a peer arriving later
            // gets it via the RESEND window.
            last_sent = Some((open, now));
            continue;
        }

        let space_key = *state.space_key.lock().unwrap();
        let payload = PresencePayload {
            device_short: state.device_short.clone(),
            open: open.iter().cloned().collect(),
            at_ms: now,
        };
        let Ok(sealed) = seal_presence(&space_key, &payload) else { continue };
        let frame = serde_json::json!({
            "type": "presence",
            "data": base64::engine::general_purpose::STANDARD.encode(sealed),
        });
        for peer in peers {
            if let Ok((mut stream, _)) = connect_peer(&peer_candidates(&peer)) {
                let _ = write_frame(&mut stream, &frame);
            }
        }
        last_sent = Some((open, now));
    }
}

// ---------- space re-key (cryptographic eviction) ----------
//
// Revocation stops members from *talking* to a device; re-keying stops the
// device from ever *reading* again. The rotator mints a fresh space key and
// seals it individually to each remaining member's device identity (X25519
// over the Ed25519 keys) — the revoked device can watch every delivery, on
// LAN or relay, and open none of them. The new space gets a fresh registry
// with the rotator as genesis, vouching for every surviving member.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RekeyMsg {
    pub rotator_pk: String,
    pub at_ms: u64,
    /// (recipient device pk, base64 sealed new space key).
    pub grants: Vec<(String, String)>,
    /// Registry for the NEW space: rotator genesis + adds for each survivor.
    pub new_registry: Vec<RegistryEntry>,
    pub sig: String,
}

/// What a device holds after validating a rekey addressed to it.
#[derive(Debug, Clone)]
pub struct AcceptedRekey {
    pub new_key: [u8; 32],
    pub new_registry: Vec<RegistryEntry>,
}

fn rekey_sig_msg(msg: &RekeyMsg) -> Vec<u8> {
    let grants = serde_json::to_vec(&msg.grants).unwrap_or_default();
    let reg = serde_json::to_vec(&msg.new_registry).unwrap_or_default();
    [
        format!("pinq-rekey-v1|{}|{}|", msg.at_ms, msg.rotator_pk).as_bytes(),
        &grants,
        b"|",
        &reg,
    ]
    .concat()
}

/// Per-(rotation, pair) grant key: the shared secret alone would repeat across
/// rotations for the same pair, so the context binds timestamp and identities.
fn grant_key(shared: &[u8; 32], at_ms: u64, rotator_pk: &str, recipient_pk: &str) -> blake3::Hash {
    blake3::keyed_hash(
        shared,
        format!("pinq-rekey-grant-v1|{at_ms}|{rotator_pk}|{recipient_pk}").as_bytes(),
    )
}

fn seal_grant(
    secret: &[u8; 32],
    at_ms: u64,
    rotator_pk: &str,
    recipient_pk: &str,
    new_key: &[u8; 32],
) -> Option<Vec<u8>> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
    let shared = registry::device_shared_secret(secret, recipient_pk)?;
    let k = grant_key(&shared, at_ms, rotator_pk, recipient_pk);
    let cipher = ChaCha20Poly1305::new(k.as_bytes().into());
    cipher.encrypt(&Nonce::default(), new_key.as_slice()).ok()
}

fn open_grant(
    secret: &[u8; 32],
    at_ms: u64,
    rotator_pk: &str,
    recipient_pk: &str,
    sealed: &[u8],
) -> Option<[u8; 32]> {
    use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
    let shared = registry::device_shared_secret(secret, rotator_pk)?;
    let k = grant_key(&shared, at_ms, rotator_pk, recipient_pk);
    let cipher = ChaCha20Poly1305::new(k.as_bytes().into());
    let pt = cipher.decrypt(&Nonce::default(), sealed).ok()?;
    pt.try_into().ok()
}

/// Mint a new space key and the signed message that carries it to survivors.
/// `space` is the CURRENT space tag; grants go to every active member except
/// the rotator itself (revoked devices simply get no grant).
pub fn build_rekey(
    store: &Store,
    space: &str,
    secret: &[u8; 32],
    device_name: &str,
) -> Result<(RekeyMsg, [u8; 32]), String> {
    use ed25519_dalek::Signer;
    let mut new_key = [0u8; 32];
    getrandom::fill(&mut new_key).map_err(|e| e.to_string())?;

    let rotator_pk = registry::pk_of(secret);
    let at_ms = pinq_core::now_ms();
    let entries = store.registry_entries(space).map_err(ioerr)?;
    let members = registry::fold(space, &entries);
    if registry::standing(&members, &rotator_pk) != Some(true) {
        return Err("this device is not an active member of the space".into());
    }

    let new_space = registry::space_tag(&new_key);
    let mut new_registry =
        vec![registry::sign_entry(secret, &new_space, "add", &rotator_pk, device_name, at_ms)];
    let mut grants = Vec::new();
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    for rec in members.values() {
        if rec.pk == rotator_pk || rec.revoked_ms.is_some() {
            continue;
        }
        let Some(sealed) = seal_grant(secret, at_ms, &rotator_pk, &rec.pk, &new_key) else {
            return Err(format!("could not seal grant for {}", &rec.pk[..8]));
        };
        grants.push((rec.pk.clone(), b64.encode(sealed)));
        // Strictly after genesis in the fold's total order — same-timestamp
        // entries sort by pk, which could put a survivor before the rotator.
        new_registry.push(registry::sign_entry(
            secret,
            &new_space,
            "add",
            &rec.pk,
            &rec.name,
            at_ms + 1,
        ));
    }

    let mut msg = RekeyMsg { rotator_pk, at_ms, grants, new_registry, sig: String::new() };
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    msg.sig = hex(&signing.sign(&rekey_sig_msg(&msg)).to_bytes());
    Ok((msg, new_key))
}

/// Validate a rekey message against the CURRENT space and open our grant.
/// Ok(None) = valid-shaped but not for us (no grant / already applied);
/// Err = invalid or from someone with no right to rotate.
pub fn accept_rekey(
    store: &Store,
    space: &str,
    secret: &[u8; 32],
    msg: &RekeyMsg,
) -> Result<Option<AcceptedRekey>, String> {
    if !registry::verify_device_sig(&msg.rotator_pk, &rekey_sig_msg(msg), &msg.sig) {
        return Err("rekey signature invalid".into());
    }
    // Only an active member of the space we're currently in may rotate it.
    let entries = store.registry_entries(space).map_err(ioerr)?;
    if registry::standing(&registry::fold(space, &entries), &msg.rotator_pk) != Some(true) {
        return Err("rekey from a device that isn't an active member".into());
    }
    let my_pk = registry::pk_of(secret);
    if msg.rotator_pk == my_pk {
        return Ok(None); // our own rotation echoing back
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let Some((_, sealed)) = msg.grants.iter().find(|(pk, _)| pk == &my_pk) else {
        return Ok(None); // no grant for us — we were not invited along
    };
    let sealed = b64.decode(sealed).map_err(|e| e.to_string())?;
    let new_key = open_grant(secret, msg.at_ms, &msg.rotator_pk, &my_pk, &sealed)
        .ok_or("could not open rekey grant")?;

    // Sanity: the new registry must be valid for the new space and include us.
    let new_space = registry::space_tag(&new_key);
    if new_space == space {
        return Ok(None); // already on this key
    }
    let valid: Vec<RegistryEntry> = msg
        .new_registry
        .iter()
        .filter(|e| registry::verify_entry(&new_space, e))
        .cloned()
        .collect();
    if registry::standing(&registry::fold(&new_space, &valid), &my_pk) != Some(true) {
        return Err("rekey registry does not include this device".into());
    }
    Ok(Some(AcceptedRekey { new_key, new_registry: valid }))
}

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub applied: Vec<String>,
    pub pulled: usize,
    pub pushed: usize,
    /// A validated re-key addressed to this device, to be applied by the caller.
    pub rekey: Option<AcceptedRekey>,
}

fn ioerr(e: std::io::Error) -> String {
    e.to_string()
}

/// If this space has no registry yet (fresh install, or a pre-registry space),
/// this device declares itself the genesis member.
fn ensure_genesis(store: &Store, space: &str, secret: &[u8; 32], device_name: &str) {
    let entries = store.registry_entries(space).unwrap_or_default();
    if registry::fold(space, &entries).is_empty() {
        let pk = registry::pk_of(secret);
        let e = registry::sign_entry(secret, space, "add", &pk, device_name, pinq_core::now_ms());
        let _ = store.add_registry_entries(space, &[e]);
    }
}

/// Mutual device authentication, right after the channel comes up. Each side
/// signs the session nonces with its device key; the peer checks the registry:
/// revoked → session refused; unknown (paired before the registry existed, or
/// enrolled by another member we haven't synced with) → auto-enrolled, since
/// the encrypted channel already proved space-key possession. Registry
/// membership is therefore an audit trail plus a revocation gate — actually
/// evicting a device also needs a space re-key (future spike).
fn session_auth(
    chan: &mut SecureChan,
    store: &Store,
    space: &str,
    secret: &[u8; 32],
    device_name: &str,
    ni: &[u8],
    nr: &[u8],
    initiator: bool,
) -> Result<(), String> {
    use ed25519_dalek::Signer;
    ensure_genesis(store, space, secret, device_name);

    let msg = [b"pinq-sync-auth-v1".as_slice(), ni, nr].concat();
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    let mine = serde_json::json!({
        "type": "auth",
        "pk": registry::pk_of(secret),
        "name": device_name,
        "sig": hex(&signing.sign(&msg).to_bytes()),
    });

    let handle_peer = |frame: &serde_json::Value| -> Result<(), String> {
        match frame.get("type").and_then(|t| t.as_str()) {
            Some("auth") => {}
            Some("auth_reject") => {
                return Err(frame
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("peer refused the session")
                    .to_string())
            }
            other => return Err(format!("unexpected frame {other:?} during auth")),
        }
        let pk = frame.get("pk").and_then(|p| p.as_str()).unwrap_or_default();
        let name = frame.get("name").and_then(|p| p.as_str()).unwrap_or("device");
        let sig = frame.get("sig").and_then(|p| p.as_str()).unwrap_or_default();
        if !registry::verify_device_sig(pk, &msg, sig) {
            return Err("peer device signature invalid".into());
        }
        let entries = store.registry_entries(space).unwrap_or_default();
        match registry::standing(&registry::fold(space, &entries), pk) {
            Some(true) => Ok(()),
            Some(false) => Err(format!("device \"{name}\" was removed from this space")),
            None => {
                let e = registry::sign_entry(secret, space, "add", pk, name, pinq_core::now_ms());
                let _ = store.add_registry_entries(space, &[e]);
                Ok(())
            }
        }
    };

    let outcome = if initiator {
        chan.send(&mine).map_err(ioerr)?;
        handle_peer(&chan.recv().map_err(ioerr)?)
    } else {
        let theirs = chan.recv().map_err(ioerr)?;
        let r = handle_peer(&theirs);
        if r.is_ok() {
            chan.send(&mine).map_err(ioerr)?;
        }
        r
    };
    if let Err(reason) = &outcome {
        let _ = chan.send(&serde_json::json!({ "type": "auth_reject", "reason": reason }));
    }
    outcome
}

/// Swap and union-merge registry records (signature-checked before storing).
fn exchange_registry(
    chan: &mut SecureChan,
    store: &Store,
    space: &str,
    initiator: bool,
) -> Result<(), String> {
    let mine = store.registry_entries(space).unwrap_or_default();
    let frame = serde_json::json!({ "type": "registry", "entries": mine });
    let theirs = if initiator {
        chan.send(&frame).map_err(ioerr)?;
        chan.recv().map_err(ioerr)?
    } else {
        let t = chan.recv().map_err(ioerr)?;
        chan.send(&frame).map_err(ioerr)?;
        t
    };
    if theirs.get("type").and_then(|t| t.as_str()) != Some("registry") {
        return Err("unexpected frame during registry exchange".into());
    }
    let entries: Vec<RegistryEntry> =
        serde_json::from_value(theirs.get("entries").cloned().unwrap_or_default())
            .map_err(|e| e.to_string())?;
    let valid: Vec<RegistryEntry> = entries
        .into_iter()
        .filter(|e| registry::verify_entry(space, e))
        .collect();
    store.add_registry_entries(space, &valid).map_err(ioerr)?;
    Ok(())
}

/// Send ops in size-bounded batches, then an end marker.
fn send_ops(chan: &mut SecureChan, ops: &[SyncOp]) -> Result<(), String> {
    let mut batch: Vec<&SyncOp> = Vec::new();
    let mut batch_bytes = 0usize;
    for op in ops {
        let sz = op.action.len() + op.device.len() + 48;
        if !batch.is_empty() && batch_bytes + sz > OPS_BATCH_BYTES {
            chan.send(&serde_json::json!({ "type": "ops", "ops": batch })).map_err(ioerr)?;
            batch.clear();
            batch_bytes = 0;
        }
        batch.push(op);
        batch_bytes += sz;
    }
    if !batch.is_empty() {
        chan.send(&serde_json::json!({ "type": "ops", "ops": batch })).map_err(ioerr)?;
    }
    chan.send(&serde_json::json!({ "type": "ops_end" })).map_err(ioerr)
}

/// Receive op batches until the end marker, applying each as it lands.
fn recv_ops(chan: &mut SecureChan, store: &Store) -> Result<(Vec<String>, usize), String> {
    let mut applied = Vec::new();
    let mut pulled = 0usize;
    loop {
        let frame = chan.recv().map_err(ioerr)?;
        match frame.get("type").and_then(|t| t.as_str()) {
            Some("ops") => {
                let ops: Vec<SyncOp> =
                    serde_json::from_value(frame.get("ops").cloned().unwrap_or_default())
                        .map_err(|e| e.to_string())?;
                pulled += ops.len();
                applied.extend(store.apply_remote_ops(&ops).map_err(ioerr)?);
            }
            Some("ops_end") => break,
            other => return Err(format!("unexpected frame {other:?} during op exchange")),
        }
    }
    applied.sort();
    applied.dedup();
    Ok((applied, pulled))
}

/// Attachment hashes referenced by any note (trash included) whose bytes we
/// don't have locally.
fn missing_blob_hashes(store: &Store, blobs: &Path) -> Vec<String> {
    let mut hashes: Vec<String> = store
        .all_notes()
        .iter()
        .flat_map(|n| n.attachments.iter().map(|a| a.hash.clone()))
        .collect();
    hashes.sort();
    hashes.dedup();
    hashes.retain(|h| crate::blobs::blob_path_in(blobs, h).is_none());
    hashes
}

/// Serve requested blobs in chunks, streaming from disk so multi-GB blobs
/// never sit in memory; unknown hashes are skipped silently (the requester
/// just stays missing until another peer has them).
fn send_blobs(chan: &mut SecureChan, blobs: &Path, hashes: &[String]) -> Result<(), String> {
    use base64::Engine;
    use std::io::Read;
    let b64 = base64::engine::general_purpose::STANDARD;
    for hash in hashes {
        let Some(path) = crate::blobs::blob_path_in(blobs, hash) else { continue };
        let Ok(mut file) = std::fs::File::open(&path) else { continue };
        let mut buf = vec![0u8; BLOB_CHUNK];
        loop {
            let mut n = 0;
            // Fill the chunk fully (plain read may return short counts).
            while n < BLOB_CHUNK {
                match file.read(&mut buf[n..]).map_err(|e| e.to_string())? {
                    0 => break,
                    k => n += k,
                }
            }
            // A short (even empty) chunk is the final one — always sent, so
            // exact-multiple sizes still deliver their `last` marker.
            let last = n < BLOB_CHUNK;
            chan.send(&serde_json::json!({
                "type": "blob",
                "hash": hash,
                "last": last,
                "data": b64.encode(&buf[..n]),
            }))
            .map_err(ioerr)?;
            if last {
                break;
            }
        }
    }
    chan.send(&serde_json::json!({ "type": "blob_end" })).map_err(ioerr)
}

/// Receive blob chunks until the end marker, streaming each blob straight to
/// a temp file (content-addressed, so a corrupt or mismatched transfer simply
/// doesn't land under the requested hash).
fn recv_blobs(chan: &mut SecureChan, blobs: &Path) -> Result<usize, String> {
    let mut pending: HashMap<String, crate::blobs::IncomingBlob> = HashMap::new();
    // Blobs that went bad mid-stream (oversized, disk error): keep draining
    // their frames so ONE bad blob never aborts the whole session — a failed
    // session used to put the peer on retry cooldown and stall all sync.
    let mut skipped: std::collections::HashSet<String> = Default::default();
    let mut stored = 0usize;
    let result = (|| loop {
        let frame = chan.recv().map_err(ioerr)?;
        match frame.get("type").and_then(|t| t.as_str()) {
            Some("blob") => {
                let hash = frame.get("hash").and_then(|h| h.as_str()).unwrap_or_default().to_string();
                let data = b64f(&frame, "data").map_err(ioerr)?;
                let last = frame.get("last").and_then(|l| l.as_bool()).unwrap_or(false);
                if !skipped.contains(&hash) {
                    let incoming = match pending.entry(hash.clone()) {
                        std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            match crate::blobs::IncomingBlob::new(blobs) {
                                Ok(b) => Some(e.insert(b)),
                                Err(e) => {
                                    eprintln!("pinq: can't receive blob: {e}");
                                    skipped.insert(hash.clone());
                                    None
                                }
                            }
                        }
                    };
                    if let Some(incoming) = incoming {
                        let too_big = incoming.size + data.len() as u64
                            > crate::attachments::MAX_FILE_BYTES + BLOB_CHUNK as u64;
                        let failed = !too_big && incoming.write(&data).is_err();
                        if too_big || failed {
                            eprintln!(
                                "pinq: skipping blob from peer ({})",
                                if too_big { "too large" } else { "write failed" }
                            );
                            if let Some(b) = pending.remove(&hash) {
                                b.discard();
                            }
                            skipped.insert(hash.clone());
                        }
                    }
                }
                if last {
                    skipped.remove(&hash);
                    if let Some(incoming) = pending.remove(&hash) {
                        match incoming.finish() {
                            Ok(actual) if actual == hash => stored += 1,
                            Ok(actual) => {
                                // Landed under its true hash but nothing references
                                // it — drop it rather than leave junk for GC.
                                if let Some(p) = crate::blobs::blob_path_in(blobs, &actual) {
                                    let _ = std::fs::remove_file(p);
                                }
                                eprintln!("pinq: discarded blob with mismatched hash from peer");
                            }
                            Err(e) => eprintln!("pinq: failed to store synced blob: {e}"),
                        }
                    }
                }
            }
            Some("blob_end") => break Ok(stored),
            other => return Err(format!("unexpected frame {other:?} during blob exchange")),
        }
    })();
    // Half-received blobs from an aborted session don't linger as tmp files.
    for (_, incoming) in pending {
        incoming.discard();
    }
    result
}

/// Initiator half of a sync session. Fixed sequence (no interleaving needed):
/// hellos → vv swap → responder's ops → our ops → our blob wants → their
/// blob wants. Pure over (store, blob dir, space key, stream) so tests can run
/// it without a Tauri app.
pub fn sync_as_initiator(
    store: &Store,
    blobs: &Path,
    space_key: &[u8; 32],
    secret: &[u8; 32],
    device_name: &str,
    rekey: Option<&RekeyMsg>,
    mut stream: TcpStream,
) -> Result<SyncOutcome, String> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    let mut ni = [0u8; 16];
    getrandom::fill(&mut ni).map_err(|e| e.to_string())?;
    let mut eph = [0u8; 32];
    getrandom::fill(&mut eph).map_err(|e| e.to_string())?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    write_frame(
        &mut stream,
        &serde_json::json!({
            "type": "sync_hello", "v": 2,
            "nonce": b64.encode(ni),
            "eph": registry::pk_of(&eph),
        }),
    )
    .map_err(ioerr)?;
    let reply = read_frame(&mut stream).map_err(ioerr)?;
    let nr = b64f(&reply, "nonce").map_err(ioerr)?;
    let their_eph = reply
        .get("eph")
        .and_then(|e| e.as_str())
        .ok_or("peer speaks an older protocol (no forward secrecy) — update it")?;
    let eph_shared =
        registry::device_shared_secret(&eph, their_eph).ok_or("peer sent a bad ephemeral key")?;
    let mut chan = SecureChan::new(stream, space_key, &ni, &nr, &eph_shared, true);

    let space = registry::space_tag(space_key);
    session_auth(&mut chan, store, &space, secret, device_name, &ni, &nr, true)?;
    exchange_registry(&mut chan, store, &space, true)?;

    // Rekey delivery slot (usually empty). The rotator uses this to hand the
    // sealed new key to each online peer before switching itself.
    chan.send(&serde_json::json!({ "type": "rekey", "msg": rekey })).map_err(ioerr)?;
    let ack = chan.recv().map_err(ioerr)?;
    if ack.get("type").and_then(|t| t.as_str()) != Some("rekey_ack") {
        return Err("peer did not acknowledge the rekey slot".into());
    }

    chan.send(&serde_json::json!({ "type": "vv", "vv": store.version_vector().map_err(ioerr)? }))
        .map_err(ioerr)?;
    let their_vv_frame = chan.recv().map_err(ioerr)?;
    let their_vv: HashMap<String, u64> =
        serde_json::from_value(their_vv_frame.get("vv").cloned().unwrap_or_default())
            .map_err(|e| e.to_string())?;

    let (applied, pulled) = recv_ops(&mut chan, store)?;
    let to_send = store.ops_missing_from(&their_vv).map_err(ioerr)?;
    let pushed = to_send.len();
    send_ops(&mut chan, &to_send)?;

    let wants = missing_blob_hashes(store, blobs);
    chan.send(&serde_json::json!({ "type": "blob_req", "hashes": wants })).map_err(ioerr)?;
    recv_blobs(&mut chan, blobs)?;
    let their_req = chan.recv().map_err(ioerr)?;
    let their_wants: Vec<String> =
        serde_json::from_value(their_req.get("hashes").cloned().unwrap_or_default())
            .unwrap_or_default();
    send_blobs(&mut chan, blobs, &their_wants)?;

    Ok(SyncOutcome { applied, pulled, pushed, rekey: None })
}

/// Responder half — the exact mirror of `sync_as_initiator`.
pub fn sync_as_responder(
    store: &Store,
    blobs: &Path,
    space_key: &[u8; 32],
    secret: &[u8; 32],
    device_name: &str,
    mut stream: TcpStream,
    hello: &serde_json::Value,
) -> Result<SyncOutcome, String> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    let ni = b64f(hello, "nonce").map_err(ioerr)?;
    let their_eph = hello
        .get("eph")
        .and_then(|e| e.as_str())
        .ok_or("peer speaks an older protocol (no forward secrecy) — update it")?
        .to_string();
    let mut nr = [0u8; 16];
    getrandom::fill(&mut nr).map_err(|e| e.to_string())?;
    let mut eph = [0u8; 32];
    getrandom::fill(&mut eph).map_err(|e| e.to_string())?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    write_frame(
        &mut stream,
        &serde_json::json!({
            "type": "sync_hello", "v": 2,
            "nonce": b64.encode(nr),
            "eph": registry::pk_of(&eph),
        }),
    )
    .map_err(ioerr)?;
    let eph_shared =
        registry::device_shared_secret(&eph, &their_eph).ok_or("peer sent a bad ephemeral key")?;
    let mut chan = SecureChan::new(stream, space_key, &ni, &nr, &eph_shared, false);

    // The first encrypted frame (the peer's auth) doubles as space-key proof:
    // if the peer doesn't hold the space key, decrypt fails and nothing leaked.
    let space = registry::space_tag(space_key);
    session_auth(&mut chan, store, &space, secret, device_name, &ni, &nr, false)?;
    exchange_registry(&mut chan, store, &space, false)?;

    // Rekey slot: validate anything the initiator delivered, ack either way.
    let rk_frame = chan.recv().map_err(ioerr)?;
    if rk_frame.get("type").and_then(|t| t.as_str()) != Some("rekey") {
        return Err("expected the rekey slot".into());
    }
    let mut accepted_rekey = None;
    if let Some(m) = rk_frame.get("msg").filter(|m| !m.is_null()) {
        if let Ok(msg) = serde_json::from_value::<RekeyMsg>(m.clone()) {
            match accept_rekey(store, &space, secret, &msg) {
                Ok(r) => accepted_rekey = r,
                Err(e) => eprintln!("pinq: ignoring invalid rekey: {e}"),
            }
        }
    }
    chan.send(&serde_json::json!({ "type": "rekey_ack" })).map_err(ioerr)?;

    let their_vv_frame = chan.recv().map_err(ioerr)?;
    let their_vv: HashMap<String, u64> =
        serde_json::from_value(their_vv_frame.get("vv").cloned().unwrap_or_default())
            .map_err(|e| e.to_string())?;
    chan.send(&serde_json::json!({ "type": "vv", "vv": store.version_vector().map_err(ioerr)? }))
        .map_err(ioerr)?;

    let to_send = store.ops_missing_from(&their_vv).map_err(ioerr)?;
    let pushed = to_send.len();
    send_ops(&mut chan, &to_send)?;
    let (applied, pulled) = recv_ops(&mut chan, store)?;

    let their_req = chan.recv().map_err(ioerr)?;
    let their_wants: Vec<String> =
        serde_json::from_value(their_req.get("hashes").cloned().unwrap_or_default())
            .unwrap_or_default();
    send_blobs(&mut chan, blobs, &their_wants)?;
    let wants = missing_blob_hashes(store, blobs);
    chan.send(&serde_json::json!({ "type": "blob_req", "hashes": wants })).map_err(ioerr)?;
    recv_blobs(&mut chan, blobs)?;

    Ok(SyncOutcome { applied, pulled, pushed, rekey: accepted_rekey })
}

/// Inbound sync session (spawned by the listener). Runs on its own thread, so
/// window reconciliation here is safe (never the main thread).
fn run_sync_responder_app(app: &AppHandle, stream: TcpStream, hello: serde_json::Value) {
    let Some(store) = app.try_state::<Arc<Store>>() else { return };
    let Some(state) = app.try_state::<SyncState>() else { return };
    let Ok(blobs) = crate::blobs::blobs_dir(app) else { return };
    let space_key = *state.space_key.lock().unwrap();
    let secret = *state.device_secret.lock().unwrap();
    match sync_as_responder(&store, &blobs, &space_key, &secret, &state.device_name, stream, &hello) {
        Ok(outcome) => finish_sync(app, outcome),
        Err(e) => eprintln!("pinq: inbound sync failed: {e}"),
    }
}

/// Ops landed: rebuild note windows and tell every open webview to refresh.
fn finish_sync(app: &AppHandle, outcome: SyncOutcome) {
    if let Some(acc) = &outcome.rekey {
        apply_rekey(app, acc);
    }
    if outcome.applied.is_empty() {
        return;
    }
    crate::notes::reconcile_after_sync(app, &outcome.applied);
    let _ = app.emit("pinq-sync-applied", &outcome.applied);
}

/// Switch this device onto a rotated space: persist the key, adopt the new
/// registry, refresh tags/fingerprints, re-advertise, and sync immediately.
fn apply_rekey(app: &AppHandle, acc: &AcceptedRekey) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let Some(store) = app.try_state::<Arc<Store>>() else { return };
    if set_secret(app, "space-key", &acc.new_key).is_err() {
        eprintln!("pinq: could not persist rotated space key");
        return;
    }
    let new_space = registry::space_tag(&acc.new_key);
    let _ = store.add_registry_entries(&new_space, &acc.new_registry);
    *state.space_key.lock().unwrap() = acc.new_key;
    *state.current_tag.lock().unwrap() = lan_tag(&acc.new_key);
    *state.space_fingerprint.lock().unwrap() = space_fp(&acc.new_key);
    re_register(&state);
    state.sync_asap.store(true, std::sync::atomic::Ordering::Relaxed);
    state.relay_asap.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("pinq: space key rotated; now in space {}", &new_space[..8]);
}

/// Stable fingerprint of the local version vector — "has anything changed
/// since the last session with this peer?" without storing whole vectors.
fn vv_fingerprint(store: &Store) -> String {
    let mut entries: Vec<(String, u64)> =
        store.version_vector().unwrap_or_default().into_iter().collect();
    entries.sort();
    hex(&blake3::hash(format!("{entries:?}").as_bytes()).as_bytes()[..8])
}

/// The pump: every few seconds, sync with each same-space peer whose view of
/// us is stale — after local edits, on a "Sync now" click, or at least once a
/// minute to pick up remote-only changes. Peers that fail back off briefly.
fn run_sync_loop(app: AppHandle, wake_rx: std::sync::mpsc::Receiver<()>) {
    let mut memo: HashMap<String, (String, u64)> = HashMap::new(); // dev -> (vv fp at last success, last attempt)
    loop {
        // Tick every second, or immediately when a live-note commit wakes us.
        match wake_rx.recv_timeout(SYNC_TICK) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(SYNC_TICK),
        }
        while wake_rx.try_recv().is_ok() {} // coalesce queued wakes
        let Some(state) = app.try_state::<SyncState>() else { continue };
        let Some(store) = app.try_state::<Arc<Store>>() else { continue };
        let Ok(blobs) = crate::blobs::blobs_dir(&app) else { continue };
        let asap = state.sync_asap.swap(false, std::sync::atomic::Ordering::Relaxed);
        let live_targets = std::mem::take(&mut *state.live_push.lock().unwrap());

        let tag = state.current_tag.lock().unwrap().clone();
        let peers: Vec<PeerInfo> = state
            .peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| !tag.is_empty() && p.tag == tag && !p.addr.is_empty())
            .cloned()
            .collect();
        if peers.is_empty() {
            continue;
        }

        let local_fp = vv_fingerprint(&store);
        for peer in peers {
            let now = pinq_core::now_ms();
            let (last_fp, last_at) =
                memo.get(&peer.device_short).cloned().unwrap_or_default();
            // Live peers (they have the just-edited note open) bypass the
            // retry cooldown so keystrokes propagate at debounce + RTT speed;
            // everyone else keeps the polite cadence.
            let is_live = live_targets.contains(&peer.device_short);
            let due = asap
                || is_live
                || last_fp != local_fp
                || now.saturating_sub(last_at) > SYNC_REFRESH_MS;
            if !due || (!asap && !is_live && now.saturating_sub(last_at) < SYNC_RETRY_MS) {
                continue;
            }
            {
                let mut busy = state.syncing.lock().unwrap();
                if !busy.insert(peer.device_short.clone()) {
                    continue;
                }
            }
            let space_key = *state.space_key.lock().unwrap();
            let secret = *state.device_secret.lock().unwrap();
            let result = connect_peer(&peer_candidates(&peer)).and_then(|(stream, _)| {
                sync_as_initiator(
                    &store,
                    &blobs,
                    &space_key,
                    &secret,
                    &state.device_name,
                    None,
                    stream,
                )
            });
            state.syncing.lock().unwrap().remove(&peer.device_short);

            let done = pinq_core::now_ms();
            match result {
                Ok(outcome) => {
                    state.last_sync.lock().unwrap().insert(
                        peer.device_short.clone(),
                        SyncStatus {
                            at_ms: done,
                            ok: true,
                            error: None,
                            pulled: outcome.pulled,
                            pushed: outcome.pushed,
                        },
                    );
                    // Success marker uses the *post-session* vector, so ops we
                    // just pulled don't immediately re-trigger a session.
                    memo.insert(peer.device_short.clone(), (vv_fingerprint(&store), done));
                    finish_sync(&app, outcome);
                }
                Err(e) => {
                    state.last_sync.lock().unwrap().insert(
                        peer.device_short.clone(),
                        SyncStatus { at_ms: done, ok: false, error: Some(e), pulled: 0, pushed: 0 },
                    );
                    memo.insert(peer.device_short.clone(), (last_fp, done));
                }
            }
        }
    }
}

// ---------- relay client ----------

/// How often the relay is contacted when nothing forces it sooner.
const RELAY_EVERY_MS: u64 = 30_000;
/// Blobs above this ride only on LAN sessions, not through the relay.
const RELAY_BLOB_CAP: u64 = 8 * 1024 * 1024;
const RELAY_OPS_PER_ENVELOPE: usize = 500;

/// The mailbox id on the relay: unguessable, derived from the space key.
/// Knowing it is the capability to use the channel; the relay has no accounts.
fn relay_channel(space_key: &[u8; 32]) -> String {
    blake3::keyed_hash(space_key, b"pinq-relay-space-v1").to_hex().to_string()
}

/// Port the in-app hosted relay listens on (the CLI default, so docs match).
const HOSTED_RELAY_PORT: u16 = 47820;

/// Start or stop the in-app relay to match the settings toggle. Runs off the
/// main thread (network bind + mDNS). Safe to call repeatedly.
pub fn update_relay_host(app: &AppHandle) {
    let Some(state) = app.try_state::<SyncState>() else { return };
    let Some(settings) = app.try_state::<crate::settings::SettingsStore>() else { return };
    let want = settings.current().relay_host;

    if !want {
        let was_on = state.relay_host_flag.lock().unwrap().take().map(|flag| {
            flag.store(false, std::sync::atomic::Ordering::Relaxed);
        });
        let had_info = state.relay_host_info.lock().unwrap().take().is_some();
        if was_on.is_some() || had_info {
            re_register(&state);
        }
        return;
    }
    if state.relay_host_flag.lock().unwrap().is_some() {
        return; // already running
    }

    let started = (|| -> Result<(Arc<pinq_relay::Relay>, TcpListener), String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("relay-data");
        let relay =
            pinq_relay::Relay::new(pinq_relay::RelayConfig { data_dir: dir, keep_days: 30 })
                .map_err(|e| e.to_string())?;
        let listener = TcpListener::bind(("0.0.0.0", HOSTED_RELAY_PORT))
            .map_err(|e| format!("port {HOSTED_RELAY_PORT}: {e}"))?;
        Ok((Arc::new(relay), listener))
    })();

    match started {
        Ok((relay, listener)) => {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
            *state.relay_host_flag.lock().unwrap() = Some(flag.clone());
            *state.relay_host_info.lock().unwrap() = Some(RelayHostInfo {
                port: HOSTED_RELAY_PORT,
                id: relay.public_key_hex(),
                error: None,
            });
            std::thread::spawn(move || pinq_relay::serve_until(listener, relay, flag));
        }
        Err(e) => {
            *state.relay_host_info.lock().unwrap() =
                Some(RelayHostInfo { port: HOSTED_RELAY_PORT, id: String::new(), error: Some(e) });
        }
    }
    re_register(&state);
    state.relay_asap.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Which relay should this device talk to?
/// 1. Its own hosted relay (loopback, pinned to its identity).
/// 2. An explicitly configured address.
/// 3. A relay some same-space peer is hosting (auto-adopted from mDNS,
///    pinned via the advertised identity; lowest device id wins so every
///    device converges on the same choice).
fn effective_relay_addr(state: &SyncState, settings: &crate::settings::Settings) -> Option<String> {
    if settings.relay_host {
        if let Some(info) = state.relay_host_info.lock().unwrap().as_ref() {
            if info.error.is_none() {
                return Some(format!("127.0.0.1:{}#{}", info.port, info.id));
            }
        }
    }
    let manual = settings.relay_addr.trim();
    if !manual.is_empty() {
        return Some(manual.to_string());
    }
    let tag = state.current_tag.lock().unwrap().clone();
    state
        .peers
        .lock()
        .unwrap()
        .values()
        .filter(|p| !tag.is_empty() && p.tag == tag)
        .filter_map(|p| p.relay_addr.clone().map(|a| (p.device_short.clone(), a)))
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, a)| a)
}

/// Encrypted, identity-verified connection to the relay. The whole protocol
/// rides an ephemeral handshake (forward secrecy; channel ids invisible on
/// the wire) signed by the relay's persistent identity. That identity is
/// pinned SSH-style: remembered on first contact (TOFU), or supplied
/// explicitly as `host:port#relayid`. A changed identity is an error, not a
/// shrug — that's what a machine-in-the-middle looks like.
struct RelayConn {
    chan: pinq_relay::SecureStream,
}

impl RelayConn {
    fn connect(store: &Store, addr: &str) -> Result<Self, String> {
        use std::net::ToSocketAddrs;
        let (host, pin) = match addr.split_once('#') {
            Some((h, p)) => (h.trim(), Some(p.trim().to_lowercase())),
            None => (addr.trim(), None),
        };
        let sock = host
            .to_socket_addrs()
            .map_err(|e| format!("relay address: {e}"))?
            .next()
            .ok_or("relay address did not resolve")?;
        let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(5))
            .map_err(|e| format!("relay connect: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(60))).ok();
        let (chan, relay_pk) =
            pinq_relay::client_handshake(stream).map_err(|e| format!("relay handshake: {e}"))?;

        match pin {
            Some(pin) => {
                if pin.len() < 16 || !relay_pk.starts_with(&pin) {
                    return Err(format!(
                        "relay identity mismatch: pinned {}…, server is {}…",
                        &pin[..pin.len().min(16)],
                        &relay_pk[..16]
                    ));
                }
            }
            None => {
                let tofu_key =
                    format!("relay-id-{}", hex(&blake3::hash(addr.as_bytes()).as_bytes()[..4]));
                match store.meta_get(&tofu_key) {
                    None => {
                        let _ = store.meta_set(&tofu_key, &relay_pk);
                    }
                    Some(known) if known == relay_pk => {}
                    Some(known) => {
                        return Err(format!(
                            "relay identity CHANGED (was {}…, now {}…). If you migrated the \
                             relay on purpose, re-enter the address as {host}#{} to accept it; \
                             otherwise someone may be intercepting the connection",
                            &known[..16],
                            &relay_pk[..16],
                            &relay_pk[..16]
                        ))
                    }
                }
            }
        }
        Ok(Self { chan })
    }

    fn request(&mut self, req: &serde_json::Value) -> Result<serde_json::Value, String> {
        self.chan.send(req).map_err(|e| format!("relay send: {e}"))?;
        let resp = self.chan.recv().map_err(|e| format!("relay recv: {e}"))?;
        if resp.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            return Err(format!(
                "relay error: {}",
                resp.get("error").and_then(|e| e.as_str()).unwrap_or("unknown")
            ));
        }
        Ok(resp)
    }
}

/// Attachment blob hashes referenced by these ops.
fn hashes_in_ops(ops: &[SyncOp]) -> Vec<String> {
    let mut out = Vec::new();
    for op in ops {
        match serde_json::from_str::<pinq_core::Action>(&op.action) {
            Ok(pinq_core::Action::ItemAdded { item, .. }) => {
                if let pinq_core::ItemContent::Attachment { attachment } = item.content {
                    out.push(attachment.hash);
                }
            }
            Ok(pinq_core::Action::AttachmentAdded { attachment, .. }) => out.push(attachment.hash),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Default)]
pub struct RelayOutcome {
    pub applied: Vec<String>,
    pub pulled: usize,
    pub pushed: usize,
    /// Newest validated re-key found during the pull, if any.
    pub rekey: Option<AcceptedRekey>,
}

/// One full relay exchange: pull everything after our cursor, apply it, then
/// push whatever the relay hasn't seen. Cursors and the pushed version vector
/// live in the store's meta table, keyed by channel AND relay address, so
/// changing relays or re-pairing into a new space starts clean.
pub fn relay_exchange(
    store: &Store,
    blobs: &Path,
    space_key: &[u8; 32],
    secret: &[u8; 32],
    device_name: &str,
    addr: &str,
) -> Result<RelayOutcome, String> {
    use base64::Engine;
    use ed25519_dalek::Signer;
    let b64 = base64::engine::general_purpose::STANDARD;

    let channel = relay_channel(space_key);
    let reg_space = registry::space_tag(space_key);
    let addr_h = hex(&blake3::hash(addr.as_bytes()).as_bytes()[..4]);
    let cursor_key = format!("relay-cursor-{}-{}", &channel[..16], addr_h);
    let pushed_key = format!("relay-pushed-{}-{}", &channel[..16], addr_h);
    let reg_key = format!("relay-reg-{}-{}", &channel[..16], addr_h);

    ensure_genesis(store, &reg_space, secret, device_name);
    let mut conn = RelayConn::connect(store, addr)?;

    // ---- pull ----
    let mut cursor: u64 = store.meta_get(&cursor_key).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut pushed_vv: HashMap<String, u64> = store
        .meta_get(&pushed_key)
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    let mut applied = Vec::new();
    let mut pulled = 0usize;
    let mut rekey: Option<AcceptedRekey> = None;
    loop {
        let resp = conn.request(&serde_json::json!({
            "type": "poll", "channel": channel, "since": cursor, "max": 20,
        }))?;
        let envs = resp.get("envelopes").and_then(|e| e.as_array()).cloned().unwrap_or_default();
        if envs.is_empty() {
            break;
        }
        for env in &envs {
            let seq = env.get("seq").and_then(|s| s.as_u64()).unwrap_or(cursor);
            cursor = cursor.max(seq);
            let Some(bytes) =
                env.get("data").and_then(|d| d.as_str()).and_then(|d| b64.decode(d).ok())
            else {
                continue;
            };
            let payload = match open_envelope(space_key, &bytes) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("pinq: skipping unreadable relay envelope {seq}: {e}");
                    continue;
                }
            };

            // Registry records are individually signed — safe to merge from
            // any carrier. This is how revocations reach off-LAN devices.
            let valid_reg: Vec<RegistryEntry> = payload
                .registry
                .iter()
                .filter(|e| registry::verify_entry(&reg_space, e))
                .cloned()
                .collect();
            let _ = store.add_registry_entries(&reg_space, &valid_reg);

            // Every envelope (registry-only ones included) is signed by its
            // sender's device key; that identity gates everything below.
            let msg = envelope_sig_msg(&payload.ops)?;
            if !registry::verify_device_sig(&payload.sender_pk, &msg, &payload.sig) {
                eprintln!("pinq: relay envelope {seq} has a bad signature, skipped");
                continue;
            }
            let entries = store.registry_entries(&reg_space).unwrap_or_default();
            match registry::standing(&registry::fold(&reg_space, &entries), &payload.sender_pk) {
                Some(false) => {
                    eprintln!("pinq: relay envelope {seq} is from a removed device, skipped");
                    continue;
                }
                Some(true) => {}
                None => {
                    // Same policy as LAN sessions: a valid envelope proves
                    // space-key possession, so enroll the sender.
                    let e = registry::sign_entry(
                        secret,
                        &reg_space,
                        "add",
                        &payload.sender_pk,
                        &payload.sender_name,
                        pinq_core::now_ms(),
                    );
                    let _ = store.add_registry_entries(&reg_space, &[e]);
                }
            }

            // A rotation riding this envelope? Keep the last valid one seen.
            if let Some(msg) = &payload.rekey {
                match accept_rekey(store, &reg_space, secret, msg) {
                    Ok(Some(acc)) => rekey = Some(acc),
                    Ok(None) => {}
                    Err(e) => eprintln!("pinq: ignoring invalid relay rekey: {e}"),
                }
            }
            if payload.ops.is_empty() {
                continue;
            }

            for (hash, data) in &payload.blobs {
                if let Ok(raw) = b64.decode(data) {
                    match crate::blobs::store_blob_in(blobs, &raw) {
                        Ok(actual) if &actual == hash => {}
                        _ => eprintln!("pinq: discarded relay blob with mismatched hash"),
                    }
                }
            }
            pulled += payload.ops.len();
            // These ops are on the relay already — never echo them back up.
            for op in &payload.ops {
                let e = pushed_vv.entry(op.device.clone()).or_insert(0);
                *e = (*e).max(op.ctr);
            }
            applied.extend(store.apply_remote_ops(&payload.ops).map_err(ioerr)?);
        }
        store.meta_set(&cursor_key, &cursor.to_string()).map_err(ioerr)?;
        if resp.get("more").and_then(|m| m.as_bool()) != Some(true) {
            break;
        }
    }

    // ---- push ----
    let reg_entries = store.registry_entries(&reg_space).map_err(ioerr)?;
    let reg_fp = hex(
        &blake3::hash(&serde_json::to_vec(&reg_entries).map_err(|e| e.to_string())?).as_bytes()
            [..8],
    );
    let to_push = store.ops_missing_from(&pushed_vv).map_err(ioerr)?;
    let registry_dirty = store.meta_get(&reg_key).as_deref() != Some(reg_fp.as_str());

    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    let sender_pk = registry::pk_of(secret);
    let mut pushed = 0usize;
    let mut posted_seqs: Vec<u64> = Vec::new();

    // Registry-only envelope: a revocation must propagate even with no note edits.
    let chunks: Vec<Vec<SyncOp>> = if to_push.is_empty() && registry_dirty {
        vec![Vec::new()]
    } else {
        to_push.chunks(RELAY_OPS_PER_ENVELOPE).map(|c| c.to_vec()).collect()
    };

    for chunk in &chunks {
        let sig = hex(&signing.sign(&envelope_sig_msg(chunk)?).to_bytes());
        let mut payload = EnvelopePayload {
            ops: chunk.clone(),
            blobs: Vec::new(),
            registry: reg_entries.clone(),
            sender_pk: sender_pk.clone(),
            sender_name: device_name.into(),
            sig,
            rekey: None,
        };
        for hash in hashes_in_ops(chunk) {
            let Some(path) = crate::blobs::blob_path_in(blobs, &hash) else { continue };
            let small =
                std::fs::metadata(&path).map(|m| m.len() <= RELAY_BLOB_CAP).unwrap_or(false);
            if !small {
                continue; // too big for the mailbox; LAN sessions carry it
            }
            if let Ok(raw) = std::fs::read(&path) {
                payload.blobs.push((hash, b64.encode(raw)));
            }
        }
        let sealed = seal_envelope(space_key, &payload)?;
        let resp = conn.request(&serde_json::json!({
            "type": "post", "channel": channel, "data": b64.encode(&sealed),
        }))?;
        if let Some(seq) = resp.get("seq").and_then(|s| s.as_u64()) {
            posted_seqs.push(seq);
        }
        pushed += chunk.len();
    }

    if !chunks.is_empty() {
        let vv = store.version_vector().map_err(ioerr)?;
        store
            .meta_set(&pushed_key, &serde_json::to_string(&vv).map_err(|e| e.to_string())?)
            .map_err(ioerr)?;
        store.meta_set(&reg_key, &reg_fp).map_err(ioerr)?;
    } else {
        // Persist the echo-suppression bumps learned during the pull.
        store
            .meta_set(&pushed_key, &serde_json::to_string(&pushed_vv).map_err(|e| e.to_string())?)
            .map_err(ioerr)?;
    }

    // Skip past our own envelopes next time — but only if nothing landed in
    // between (a gap would hide another device's envelope).
    if let (Some(first), Some(last)) = (posted_seqs.first(), posted_seqs.last()) {
        let contiguous = *first == cursor + 1 && last - first + 1 == posted_seqs.len() as u64;
        if contiguous {
            store.meta_set(&cursor_key, &last.to_string()).map_err(ioerr)?;
        }
    }

    applied.sort();
    applied.dedup();
    Ok(RelayOutcome { applied, pulled, pushed, rekey })
}

/// Post a rotation to the relay's OLD channel so offline survivors pick it up.
fn relay_rekey_post(
    store: &Store,
    space_key: &[u8; 32],
    secret: &[u8; 32],
    device_name: &str,
    addr: &str,
    msg: &RekeyMsg,
) -> Result<(), String> {
    use base64::Engine;
    use ed25519_dalek::Signer;
    let b64 = base64::engine::general_purpose::STANDARD;
    let reg_space = registry::space_tag(space_key);
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    let payload = EnvelopePayload {
        ops: Vec::new(),
        blobs: Vec::new(),
        registry: store.registry_entries(&reg_space).unwrap_or_default(),
        sender_pk: registry::pk_of(secret),
        sender_name: device_name.into(),
        sig: hex(&signing.sign(&envelope_sig_msg(&[])?).to_bytes()),
        rekey: Some(msg.clone()),
    };
    let sealed = seal_envelope(space_key, &payload)?;
    let mut conn = RelayConn::connect(store, addr)?;
    conn.request(&serde_json::json!({
        "type": "post", "channel": relay_channel(space_key), "data": b64.encode(&sealed),
    }))?;
    Ok(())
}

/// The relay pump: every 30s (or immediately after local changes / "Sync now"),
/// exchange with the configured relay. No relay configured = no traffic at all.
fn run_relay_loop(app: AppHandle) {
    loop {
        std::thread::sleep(SYNC_TICK);
        let Some(state) = app.try_state::<SyncState>() else { continue };
        let Some(store) = app.try_state::<Arc<Store>>() else { continue };
        let Some(settings) = app.try_state::<crate::settings::SettingsStore>() else { continue };
        let Ok(blobs) = crate::blobs::blobs_dir(&app) else { continue };

        let asap = state.relay_asap.swap(false, std::sync::atomic::Ordering::Relaxed);
        let last_at = state.relay_status.lock().unwrap().as_ref().map(|s| s.at_ms).unwrap_or(0);
        if !asap && pinq_core::now_ms().saturating_sub(last_at) < RELAY_EVERY_MS {
            continue;
        }
        let Some(addr) = effective_relay_addr(&state, &settings.current()) else {
            *state.relay_status.lock().unwrap() = None;
            continue;
        };

        let space_key = *state.space_key.lock().unwrap();
        let secret = *state.device_secret.lock().unwrap();
        match relay_exchange(&store, &blobs, &space_key, &secret, &state.device_name, &addr) {
            Ok(out) => {
                *state.relay_status.lock().unwrap() = Some(SyncStatus {
                    at_ms: pinq_core::now_ms(),
                    ok: true,
                    error: None,
                    pulled: out.pulled,
                    pushed: out.pushed,
                });
                finish_sync(
                    &app,
                    SyncOutcome {
                        applied: out.applied,
                        pulled: out.pulled,
                        pushed: out.pushed,
                        rekey: out.rekey,
                    },
                );
            }
            Err(e) => {
                *state.relay_status.lock().unwrap() = Some(SyncStatus {
                    at_ms: pinq_core::now_ms(),
                    ok: false,
                    error: Some(e),
                    pulled: 0,
                    pushed: 0,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinq_core::Rect;

    #[test]
    fn rank_addrs_prefers_private_v4_and_drops_unusable() {
        let ips: Vec<std::net::IpAddr> = vec![
            "fe80::1".parse().unwrap(),      // link-local v6 — dropped
            "2001:db8::7".parse().unwrap(),  // global v6 — last resort
            "127.0.0.1".parse().unwrap(),    // loopback — dropped
            "169.254.9.9".parse().unwrap(),  // link-local v4 — dropped
            "8.8.4.4".parse().unwrap(),      // public v4 — second
            "192.168.1.20".parse().unwrap(), // private v4 — first
        ];
        let ranked = rank_addrs(ips, 47814);
        assert_eq!(
            ranked,
            vec![
                "192.168.1.20:47814".to_string(),
                "8.8.4.4:47814".to_string(),
                "[2001:db8::7]:47814".to_string(),
            ]
        );
        // Every candidate must parse as a connectable socket address.
        for a in &ranked {
            a.parse::<std::net::SocketAddr>().expect("parseable");
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pinq-sync-{tag}-{}", pinq_core::new_note_id()));
        std::fs::create_dir_all(dir.join("blobs")).unwrap();
        dir
    }

    fn rect() -> Rect {
        Rect { x: 10, y: 10, w: 280, h: 220 }
    }

    const SEC_A: [u8; 32] = [11u8; 32];
    const SEC_B: [u8; 32] = [22u8; 32];

    /// Run one full session between two stores over a real localhost socket.
    /// A responds (device secret SEC_A, name "alpha"); B initiates (SEC_B, "bravo").
    fn run_session(
        a: Arc<Store>,
        blobs_a: std::path::PathBuf,
        key_a: [u8; 32],
        b: &Store,
        blobs_b: &Path,
        key_b: &[u8; 32],
    ) -> (Result<SyncOutcome, String>, Result<SyncOutcome, String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let hello = read_frame(&mut stream).map_err(ioerr)?;
            sync_as_responder(&a, &blobs_a, &key_a, &SEC_A, "alpha", stream, &hello)
        });
        let initiator = TcpStream::connect(addr)
            .map_err(|e| e.to_string())
            .and_then(|s| sync_as_initiator(b, blobs_b, key_b, &SEC_B, "bravo", None, s));
        (responder.join().unwrap(), initiator)
    }

    #[test]
    fn session_moves_notes_and_blobs_both_ways() {
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let (blobs_a, blobs_b) = (dir_a.join("blobs"), dir_b.join("blobs"));
        let a = Arc::new(Store::open(&dir_a).unwrap());
        let b = Store::open(&dir_b).unwrap();
        let key = [7u8; 32];

        // A: a note with text and an image whose bytes live only on A.
        let note_a = a.create_note(rect()).unwrap();
        a.set_text(&note_a.id, "made on A".into()).unwrap();
        let png = vec![42u8; 300 * 1024]; // 3 chunks worth
        let hash = crate::blobs::store_blob_in(&blobs_a, &png).unwrap();
        let att = pinq_core::AttachmentRef {
            hash: hash.clone(),
            name: "photo.png".into(),
            mime: "image/png".into(),
            size: png.len() as u64,
        };
        a.add_item(&note_a.id, pinq_core::CanvasItem::attachment_item(10.0, 10.0, 120.0, att))
            .unwrap();

        // B: its own note, so the exchange is two-way.
        let note_b = b.create_note(rect()).unwrap();
        b.set_text(&note_b.id, "made on B".into()).unwrap();

        let (ra, rb) = run_session(a.clone(), blobs_a.clone(), key, &b, &blobs_b, &key);
        let (oa, ob) = (ra.unwrap(), rb.unwrap());
        assert!(oa.pulled > 0 && oa.pushed > 0);
        assert!(ob.pulled > 0 && ob.pushed > 0);

        // Notes crossed, blob bytes arrived intact and content-addressed.
        assert_eq!(b.get(&note_a.id).unwrap().text, "made on A");
        assert_eq!(a.get(&note_b.id).unwrap().text, "made on B");
        let landed = crate::blobs::blob_path_in(&blobs_b, &hash).expect("blob synced");
        assert_eq!(std::fs::read(landed).unwrap(), png);

        // A second session has nothing to say.
        let (ra, rb) = run_session(a.clone(), blobs_a, key, &b, &blobs_b, &key);
        let (oa, ob) = (ra.unwrap(), rb.unwrap());
        assert_eq!((oa.pulled, oa.pushed, ob.pulled, ob.pushed), (0, 0, 0, 0));
        assert!(oa.applied.is_empty() && ob.applied.is_empty());

        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    #[test]
    fn wrong_space_key_syncs_nothing() {
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let a = Arc::new(Store::open(&dir_a).unwrap());
        let b = Store::open(&dir_b).unwrap();
        let note = a.create_note(rect()).unwrap();
        a.set_text(&note.id, "secret".into()).unwrap();

        let (ra, rb) = run_session(
            a.clone(),
            dir_a.join("blobs"),
            [1u8; 32],
            &b,
            &dir_b.join("blobs"),
            &[2u8; 32],
        );
        assert!(ra.is_err() && rb.is_err(), "mismatched keys must fail the session");
        assert!(b.get(&note.id).is_none(), "nothing may cross without the space key");

        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    #[test]
    fn sessions_build_a_shared_registry() {
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let a = Arc::new(Store::open(&dir_a).unwrap());
        let b = Store::open(&dir_b).unwrap();
        let key = [9u8; 32];
        let space = registry::space_tag(&key);

        let (ra, rb) = run_session(a.clone(), dir_a.join("blobs"), key, &b, &dir_b.join("blobs"), &key);
        ra.unwrap();
        rb.unwrap();

        // Both sides fold to the same two active members.
        for store in [&*a, &b] {
            let m = registry::fold(&space, &store.registry_entries(&space).unwrap());
            assert_eq!(
                registry::standing(&m, &registry::pk_of(&SEC_A)),
                Some(true),
                "alpha is a member on both"
            );
            assert_eq!(registry::standing(&m, &registry::pk_of(&SEC_B)), Some(true));
        }

        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    #[test]
    fn revoked_device_is_refused() {
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let a = Arc::new(Store::open(&dir_a).unwrap());
        let b = Store::open(&dir_b).unwrap();
        let key = [9u8; 32];
        let space = registry::space_tag(&key);

        // First session enrolls both devices.
        let (ra, rb) = run_session(a.clone(), dir_a.join("blobs"), key, &b, &dir_b.join("blobs"), &key);
        ra.unwrap();
        rb.unwrap();

        // Alpha revokes bravo, then bravo tries to sync a new note over.
        let revoke = registry::sign_entry(
            &SEC_A,
            &space,
            "revoke",
            &registry::pk_of(&SEC_B),
            "bravo",
            pinq_core::now_ms(),
        );
        a.add_registry_entries(&space, &[revoke]).unwrap();
        let note = b.create_note(rect()).unwrap();
        b.set_text(&note.id, "sneaky".into()).unwrap();

        let (ra, rb) = run_session(a.clone(), dir_a.join("blobs"), key, &b, &dir_b.join("blobs"), &key);
        assert!(ra.is_err(), "responder must refuse the revoked device");
        let err = rb.expect_err("initiator must see the rejection");
        assert!(err.contains("removed"), "got: {err}");
        assert!(a.get(&note.id).is_none(), "no ops accepted from a revoked device");

        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    const SEC_C: [u8; 32] = [33u8; 32];

    #[test]
    fn rekey_grants_survivors_and_excludes_the_revoked() {
        let dirs = [temp("a"), temp("b"), temp("c")];
        let a = Store::open(&dirs[0]).unwrap();
        let b = Store::open(&dirs[1]).unwrap();
        let c = Store::open(&dirs[2]).unwrap();
        let old_key = [9u8; 32];
        let space = registry::space_tag(&old_key);
        let (pa, pb, pc) =
            (registry::pk_of(&SEC_A), registry::pk_of(&SEC_B), registry::pk_of(&SEC_C));

        // Shared history: alpha genesis, adds bravo + charlie, revokes charlie.
        let entries = vec![
            registry::sign_entry(&SEC_A, &space, "add", &pa, "alpha", 100),
            registry::sign_entry(&SEC_A, &space, "add", &pb, "bravo", 200),
            registry::sign_entry(&SEC_A, &space, "add", &pc, "charlie", 300),
            registry::sign_entry(&SEC_A, &space, "revoke", &pc, "charlie", 400),
        ];
        for store in [&a, &b, &c] {
            store.add_registry_entries(&space, &entries).unwrap();
        }

        let (msg, new_key) = build_rekey(&a, &space, &SEC_A, "alpha").unwrap();
        assert_eq!(msg.grants.len(), 1, "only bravo gets a grant");
        assert_eq!(msg.grants[0].0, pb);

        // Bravo accepts and lands on the same key, a member of the new space.
        let acc = accept_rekey(&b, &space, &SEC_B, &msg).unwrap().expect("bravo accepted");
        assert_eq!(acc.new_key, new_key);
        let new_space = registry::space_tag(&new_key);
        let fold = registry::fold(&new_space, &acc.new_registry);
        assert_eq!(registry::standing(&fold, &pb), Some(true));
        assert_eq!(registry::standing(&fold, &pc), None, "charlie is not in the new space");

        // Charlie has no grant and can't open bravo's.
        assert!(accept_rekey(&c, &space, &SEC_C, &msg).unwrap().is_none());
        use base64::Engine;
        let sealed = base64::engine::general_purpose::STANDARD.decode(&msg.grants[0].1).unwrap();
        assert!(open_grant(&SEC_C, msg.at_ms, &msg.rotator_pk, &pb, &sealed).is_none());

        // Tampering breaks the signature; a revoked rotator is refused outright.
        let mut bent = msg.clone();
        bent.grants.push((pc.clone(), bent.grants[0].1.clone()));
        assert!(accept_rekey(&b, &space, &SEC_B, &bent).is_err());
        let (evil, _) = build_rekey(&c, &space, &SEC_C, "charlie")
            .map(|v| (Some(v.0), v.1))
            .unwrap_or((None, [0; 32]));
        if let Some(evil) = evil {
            assert!(accept_rekey(&b, &space, &SEC_B, &evil).is_err());
        } // (build_rekey may already refuse charlie — either failure mode is correct)

        for d in dirs {
            std::fs::remove_dir_all(d).ok();
        }
    }

    #[test]
    fn lan_session_delivers_a_rekey() {
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let (blobs_a, blobs_b) = (dir_a.join("blobs"), dir_b.join("blobs"));
        let a = Arc::new(Store::open(&dir_a).unwrap());
        let b = Store::open(&dir_b).unwrap();
        let old_key = [9u8; 32];

        // Enroll both devices in the old space.
        let (ra, rb) =
            run_session(a.clone(), blobs_a.clone(), old_key, &b, &blobs_b, &old_key);
        ra.unwrap();
        rb.unwrap();

        // Bravo rotates and hand-delivers to alpha over a session.
        let space = registry::space_tag(&old_key);
        let (msg, new_key) = build_rekey(&b, &space, &SEC_B, "bravo").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let a2 = a.clone();
        let blobs_a2 = blobs_a.clone();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let hello = read_frame(&mut stream).map_err(ioerr)?;
            sync_as_responder(&a2, &blobs_a2, &old_key, &SEC_A, "alpha", stream, &hello)
        });
        let s = TcpStream::connect(addr).unwrap();
        sync_as_initiator(&b, &blobs_b, &old_key, &SEC_B, "bravo", Some(&msg), s).unwrap();
        let acc = responder.join().unwrap().unwrap().rekey.expect("alpha received the rekey");
        assert_eq!(acc.new_key, new_key);

        // Both sides now sync happily on the new key.
        let (ra, rb) = run_session(a.clone(), blobs_a, new_key, &b, &blobs_b, &new_key);
        ra.unwrap();
        rb.unwrap();

        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    #[test]
    fn relay_delivers_a_rekey_to_offline_survivors() {
        let (relay_addr, relay_dir) = spawn_relay();
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let (blobs_a, blobs_b) = (dir_a.join("blobs"), dir_b.join("blobs"));
        let a = Store::open(&dir_a).unwrap();
        let b = Store::open(&dir_b).unwrap();
        let old_key = [9u8; 32];

        // Both devices known to each other via the relay.
        relay_exchange(&a, &blobs_a, &old_key, &SEC_A, "alpha", &relay_addr).unwrap();
        relay_exchange(&b, &blobs_b, &old_key, &SEC_B, "bravo", &relay_addr).unwrap();
        relay_exchange(&a, &blobs_a, &old_key, &SEC_A, "alpha", &relay_addr).unwrap();

        // Alpha rotates while bravo is "offline": the rotation waits at the relay.
        let space = registry::space_tag(&old_key);
        let (msg, new_key) = build_rekey(&a, &space, &SEC_A, "alpha").unwrap();
        relay_rekey_post(&a, &old_key, &SEC_A, "alpha", &relay_addr, &msg).unwrap();

        // Bravo comes online later and picks it up.
        let out = relay_exchange(&b, &blobs_b, &old_key, &SEC_B, "bravo", &relay_addr).unwrap();
        let acc = out.rekey.expect("bravo found the rekey at the relay");
        assert_eq!(acc.new_key, new_key);

        // Bravo lands as a member of the new space's registry.
        let new_space = registry::space_tag(&new_key);
        let fold = registry::fold(&new_space, &acc.new_registry);
        assert_eq!(registry::standing(&fold, &registry::pk_of(&SEC_B)), Some(true));

        for d in [dir_a, dir_b, relay_dir] {
            std::fs::remove_dir_all(d).ok();
        }
    }

    #[test]
    fn envelope_roundtrip_and_tamper_rejection() {
        let key = [5u8; 32];
        let payload = EnvelopePayload {
            ops: vec![
                SyncOp { device: "aaaa".into(), ctr: 1, at_ms: 100, action: r#"{"kind":"x"}"#.into() },
                SyncOp { device: "bbbb".into(), ctr: 7, at_ms: 200, action: r#"{"kind":"y"}"#.into() },
            ],
            blobs: vec![("h".repeat(64), "AAAA".into())],
            rekey: None,
            ..Default::default()
        };
        let sealed = seal_envelope(&key, &payload).unwrap();
        let opened = open_envelope(&key, &sealed).unwrap();
        assert_eq!(opened.ops.len(), 2);
        assert_eq!(opened.ops[1].ctr, 7);
        assert_eq!(opened.blobs.len(), 1);

        // The carrier sees no plaintext.
        let hay = String::from_utf8_lossy(&sealed).to_string();
        assert!(!hay.contains("kind") && !hay.contains("aaaa"));

        // Tampering and wrong keys are both detected.
        let mut bent = sealed.clone();
        let last = bent.len() - 1;
        bent[last] ^= 1;
        assert!(open_envelope(&key, &bent).is_err());
        assert!(open_envelope(&[6u8; 32], &sealed).is_err());
    }

    /// Spin a real relay on localhost, backed by a temp dir.
    fn spawn_relay() -> (String, std::path::PathBuf) {
        let dir = temp("relay");
        let relay = Arc::new(
            pinq_relay::Relay::new(pinq_relay::RelayConfig { data_dir: dir.clone(), keep_days: 0 })
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || pinq_relay::serve(listener, relay));
        (addr, dir)
    }

    #[test]
    fn relay_carries_notes_and_blobs_between_devices() {
        let (relay_addr, relay_dir) = spawn_relay();
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let (blobs_a, blobs_b) = (dir_a.join("blobs"), dir_b.join("blobs"));
        let a = Store::open(&dir_a).unwrap();
        let b = Store::open(&dir_b).unwrap();
        let key = [9u8; 32];

        // A: a note with text and a small image; push through the relay.
        let note = a.create_note(rect()).unwrap();
        a.set_text(&note.id, "via the relay".into()).unwrap();
        let png = vec![7u8; 64 * 1024];
        let hash = crate::blobs::store_blob_in(&blobs_a, &png).unwrap();
        let att = pinq_core::AttachmentRef {
            hash: hash.clone(),
            name: "pic.png".into(),
            mime: "image/png".into(),
            size: png.len() as u64,
        };
        a.add_item(&note.id, pinq_core::CanvasItem::attachment_item(5.0, 5.0, 100.0, att))
            .unwrap();

        let out = relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();
        assert!(out.pushed > 0);

        // B: pulls everything, including the blob bytes.
        let out = relay_exchange(&b, &blobs_b, &key, &SEC_B, "bravo", &relay_addr).unwrap();
        assert!(out.pulled > 0);
        assert_eq!(out.applied, vec![note.id.clone()]);
        assert_eq!(b.get(&note.id).unwrap().text, "via the relay");
        let landed = crate::blobs::blob_path_in(&blobs_b, &hash).expect("blob crossed the relay");
        assert_eq!(std::fs::read(landed).unwrap(), png);

        // The relay's own disk holds only ciphertext.
        let mut leaked = false;
        for entry in walk(&relay_dir) {
            let bytes = std::fs::read(entry).unwrap();
            let hay = String::from_utf8_lossy(&bytes).to_string();
            leaked |= hay.contains("via the relay") || hay.contains("pic.png");
        }
        assert!(!leaked, "relay must never see plaintext");

        // Registry crossed too: B now knows alpha.
        let space = registry::space_tag(&key);
        let m = registry::fold(&space, &b.registry_entries(&space).unwrap());
        assert_eq!(registry::standing(&m, &registry::pk_of(&SEC_A)), Some(true));

        // Steady state: another round each way moves nothing (no echo).
        let oa = relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();
        let ob = relay_exchange(&b, &blobs_b, &key, &SEC_B, "bravo", &relay_addr).unwrap();
        assert_eq!((oa.pushed, ob.pulled, ob.pushed), (0, 0, 0));
        assert!(oa.applied.is_empty() && ob.applied.is_empty());

        for d in [dir_a, dir_b, relay_dir] {
            std::fs::remove_dir_all(d).ok();
        }
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else { return out };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn effective_relay_prefers_own_then_manual_then_adopted() {
        let state = SyncState::default();
        let mut settings = crate::settings::Settings::default();
        assert_eq!(effective_relay_addr(&state, &settings), None);

        // A same-space peer hosting a relay gets adopted (lowest dev id wins).
        *state.current_tag.lock().unwrap() = "tag1".into();
        let peer = |dev: &str, relay: Option<&str>| PeerInfo {
            device_name: dev.into(),
            device_short: dev.into(),
            addr: "10.0.0.9:47814".into(),
            addrs: vec!["10.0.0.9:47814".into()],
            tag: "tag1".into(),
            same_space: false,
            last_seen_ms: 0,
            last_sync: None,
            relay_addr: relay.map(Into::into),
        };
        state.peers.lock().unwrap().insert("bb".into(), peer("bb", Some("10.0.0.2:47820#beef")));
        state.peers.lock().unwrap().insert("aa".into(), peer("aa", Some("10.0.0.1:47820#feed")));
        state.peers.lock().unwrap().insert("cc".into(), {
            let mut p = peer("cc", Some("10.0.0.3:47820#dead"));
            p.tag = "othertag".into(); // different space: never adopted
            p
        });
        assert_eq!(effective_relay_addr(&state, &settings).as_deref(), Some("10.0.0.1:47820#feed"));

        // An explicit address beats adoption…
        settings.relay_addr = "relay.example:47820".into();
        assert_eq!(effective_relay_addr(&state, &settings).as_deref(), Some("relay.example:47820"));

        // …and hosting our own beats everything.
        settings.relay_host = true;
        *state.relay_host_info.lock().unwrap() =
            Some(RelayHostInfo { port: 47820, id: "aa11".into(), error: None });
        assert_eq!(
            effective_relay_addr(&state, &settings).as_deref(),
            Some("127.0.0.1:47820#aa11")
        );

        // Unless it failed to start — then fall back down the ladder.
        *state.relay_host_info.lock().unwrap() =
            Some(RelayHostInfo { port: 47820, id: String::new(), error: Some("port busy".into()) });
        assert_eq!(effective_relay_addr(&state, &settings).as_deref(), Some("relay.example:47820"));
    }

    #[test]
    fn relay_identity_is_pinned_and_changes_are_refused() {
        let (relay_addr, relay_dir) = spawn_relay();
        let dir = temp("a");
        let blobs = dir.join("blobs");
        let a = Store::open(&dir).unwrap();
        let key = [9u8; 32];

        // First contact records the identity (TOFU)…
        relay_exchange(&a, &blobs, &key, &SEC_A, "alpha", &relay_addr).unwrap();
        let tofu_key =
            format!("relay-id-{}", hex(&blake3::hash(relay_addr.as_bytes()).as_bytes()[..4]));
        let pinned = a.meta_get(&tofu_key).expect("identity remembered on first use");

        // …and a changed identity at the same address is refused loudly.
        a.meta_set(&tofu_key, &"f".repeat(64)).unwrap();
        let err = relay_exchange(&a, &blobs, &key, &SEC_A, "alpha", &relay_addr)
            .expect_err("identity change must fail");
        assert!(err.contains("identity CHANGED"), "got: {err}");
        a.meta_set(&tofu_key, &pinned).unwrap();

        // Explicit pinning: right prefix works, wrong prefix is refused.
        let good = format!("{relay_addr}#{}", &pinned[..16]);
        relay_exchange(&a, &blobs, &key, &SEC_A, "alpha", &good).unwrap();
        let bad = format!("{relay_addr}#{}", "0".repeat(16));
        let err = relay_exchange(&a, &blobs, &key, &SEC_A, "alpha", &bad)
            .expect_err("wrong pin must fail");
        assert!(err.contains("identity mismatch"), "got: {err}");

        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(relay_dir).ok();
    }

    #[test]
    fn relay_rejects_ops_from_revoked_devices() {
        let (relay_addr, relay_dir) = spawn_relay();
        let (dir_a, dir_b) = (temp("a"), temp("b"));
        let (blobs_a, blobs_b) = (dir_a.join("blobs"), dir_b.join("blobs"));
        let a = Store::open(&dir_a).unwrap();
        let b = Store::open(&dir_b).unwrap();
        let key = [9u8; 32];
        let space = registry::space_tag(&key);

        // Both devices meet the relay once, so both are enrolled everywhere.
        relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();
        relay_exchange(&b, &blobs_b, &key, &SEC_B, "bravo", &relay_addr).unwrap();
        relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();

        // Alpha revokes bravo; the revocation rides a registry-only envelope.
        let revoke = registry::sign_entry(
            &SEC_A,
            &space,
            "revoke",
            &registry::pk_of(&SEC_B),
            "bravo",
            pinq_core::now_ms(),
        );
        a.add_registry_entries(&space, &[revoke]).unwrap();
        relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();

        // Bravo, unaware, posts a note through the relay.
        let note = b.create_note(rect()).unwrap();
        b.set_text(&note.id, "too late".into()).unwrap();
        relay_exchange(&b, &blobs_b, &key, &SEC_B, "bravo", &relay_addr).unwrap();

        // Alpha pulls: bravo's envelope is refused, and bravo has learned of
        // its own revocation from alpha's registry-only envelope.
        let out = relay_exchange(&a, &blobs_a, &key, &SEC_A, "alpha", &relay_addr).unwrap();
        assert!(out.applied.is_empty());
        assert!(a.get(&note.id).is_none(), "revoked device's ops must not apply");
        let mb = registry::fold(&space, &b.registry_entries(&space).unwrap());
        assert_eq!(registry::standing(&mb, &registry::pk_of(&SEC_B)), Some(false));

        for d in [dir_a, dir_b, relay_dir] {
            std::fs::remove_dir_all(d).ok();
        }
    }
}
