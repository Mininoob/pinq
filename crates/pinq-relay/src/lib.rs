//! Pinq relay — a deliberately dumb, self-hostable store-and-forward server
//! (spec §19: "optional self-hostable E2EE relay").
//!
//! The relay never sees plaintext notes. Devices post *envelopes*
//! (ChaCha20Poly1305 ciphertext, sealed with a key derived from the space key)
//! to a *channel* — an unguessable hex id also derived from the space key.
//! Knowing the channel id is the capability to use it; the relay has no
//! accounts, no device list, and nothing worth stealing.
//!
//! The connection itself is encrypted too: an ephemeral X25519 handshake
//! (forward secrecy — recorded traffic stays dark even if keys leak later)
//! under a persistent relay identity key that signs every handshake. Clients
//! pin that identity on first use, SSH-style, so channel ids and envelope
//! traffic are invisible to network observers and a machine-in-the-middle is
//! detected. No TLS certificates to manage — one key file in the data dir.
//!
//! Storage is just files: `<data>/<channel>/<seq>.env`, atomic tmp+rename,
//! pruned by age. One binary, zero config required:
//!
//! ```text
//! pinq-relay --bind 0.0.0.0:47820 --data ./pinq-relay-data --keep-days 30
//! ```

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Envelopes are opaque blobs; this only bounds abuse, not content.
pub const MAX_ENVELOPE: usize = 32 * 1024 * 1024;
/// Frame cap: one envelope plus base64 + JSON overhead.
pub const MAX_FRAME: u32 = 48 * 1024 * 1024;
/// A poll response stops growing past this many payload bytes.
const POLL_BYTE_BUDGET: usize = 16 * 1024 * 1024;

pub struct RelayConfig {
    pub data_dir: PathBuf,
    /// Envelopes older than this are pruned on write. 0 = keep forever.
    pub keep_days: u64,
}

pub struct Relay {
    cfg: RelayConfig,
    identity: ed25519_dalek::SigningKey,
    /// Next sequence number per channel (lazily seeded from the directory).
    counters: Mutex<HashMap<String, u64>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Static/ephemeral X25519 via Ed25519 keys in Montgomery form — the same
/// construction the app uses for rekey grants.
fn ecdh(secret: &[u8; 32], peer_pk_hex: &str) -> Option<[u8; 32]> {
    let pk = hex_bytes::<32>(peer_pk_hex)?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk).ok()?;
    let sk = ed25519_dalek::SigningKey::from_bytes(secret);
    let shared = (&sk.to_scalar() * &vk.to_montgomery()).to_bytes();
    (shared != [0u8; 32]).then_some(shared)
}

fn auth_msg(client_eph: &str, relay_eph: &str) -> Vec<u8> {
    format!("pinq-relay-auth-v1|{client_eph}|{relay_eph}").into_bytes()
}

fn channel_key(shared: &[u8; 32], client_eph: &str, relay_eph: &str) -> blake3::Hash {
    blake3::keyed_hash(shared, format!("pinq-relay-chan-v1|{client_eph}|{relay_eph}").as_bytes())
}

/// Channel ids are client-derived hex — enforced strictly since they become
/// directory names.
fn valid_channel(s: &str) -> bool {
    (16..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn seq_of(path: &std::path::Path) -> Option<u64> {
    path.file_stem()?.to_str()?.parse().ok()
}

impl Relay {
    /// Load (or mint) the relay identity key and set up the mailbox.
    pub fn new(cfg: RelayConfig) -> io::Result<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let key_path = cfg.data_dir.join("relay.key");
        let identity = match std::fs::read(&key_path) {
            Ok(bytes) if bytes.len() == 32 => {
                ed25519_dalek::SigningKey::from_bytes(&bytes.try_into().unwrap())
            }
            _ => {
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(io::Error::other)?;
                std::fs::write(&key_path, secret)?;
                ed25519_dalek::SigningKey::from_bytes(&secret)
            }
        };
        Ok(Self { cfg, identity, counters: Mutex::new(HashMap::new()) })
    }

    /// The identity clients pin (shown at startup; paste as `addr#id` to
    /// pre-pin, or let trust-on-first-use record it automatically).
    pub fn public_key_hex(&self) -> String {
        hex(self.identity.verifying_key().as_bytes())
    }

    fn channel_dir(&self, channel: &str) -> PathBuf {
        self.cfg.data_dir.join(channel)
    }

    /// Append an envelope; returns its sequence number.
    pub fn post(&self, channel: &str, data: &[u8]) -> io::Result<u64> {
        if !valid_channel(channel) {
            return Err(io::Error::other("bad channel id"));
        }
        if data.len() > MAX_ENVELOPE {
            return Err(io::Error::other("envelope too large"));
        }
        let dir = self.channel_dir(channel);
        std::fs::create_dir_all(&dir)?;

        let mut counters = self.counters.lock().unwrap();
        let next = match counters.get(channel) {
            Some(n) => n + 1,
            None => {
                std::fs::read_dir(&dir)?
                    .filter_map(|e| seq_of(&e.ok()?.path()))
                    .max()
                    .unwrap_or(0)
                    + 1
            }
        };
        let path = dir.join(format!("{next:016}.env"));
        let tmp = dir.join(format!("{next:016}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, &path)?;
        counters.insert(channel.to_string(), next);
        drop(counters);

        self.prune(channel);
        Ok(next)
    }

    /// Envelopes with seq > `since`, oldest first, bounded by `max` count and
    /// a byte budget. The second return value says whether more remain.
    pub fn poll(&self, channel: &str, since: u64, max: usize) -> io::Result<(Vec<(u64, Vec<u8>)>, bool)> {
        if !valid_channel(channel) {
            return Err(io::Error::other("bad channel id"));
        }
        let dir = self.channel_dir(channel);
        let mut seqs: Vec<u64> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| {
                    let p = e.ok()?.path();
                    (p.extension()? == "env").then(|| seq_of(&p))?
                })
                .filter(|s| *s > since)
                .collect(),
            Err(_) => return Ok((Vec::new(), false)), // channel never written
        };
        seqs.sort();
        let total = seqs.len();
        let mut out = Vec::new();
        let mut bytes = 0usize;
        for seq in seqs.into_iter().take(max) {
            let Ok(data) = std::fs::read(dir.join(format!("{seq:016}.env"))) else { continue };
            bytes += data.len();
            out.push((seq, data));
            if bytes > POLL_BYTE_BUDGET {
                break;
            }
        }
        let more = out.len() < total;
        Ok((out, more))
    }

    /// Drop envelopes older than the retention window (and stray tmp files).
    fn prune(&self, channel: &str) {
        if self.cfg.keep_days == 0 {
            return;
        }
        let cutoff = Duration::from_secs(self.cfg.keep_days * 24 * 60 * 60);
        let Ok(rd) = std::fs::read_dir(self.channel_dir(channel)) else { return };
        for entry in rd.filter_map(|e| e.ok()) {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|age| age > cutoff)
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

// ---------- plaintext framing (handshake only) ----------

fn write_frame(stream: &mut TcpStream, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn read_frame(stream: &mut TcpStream) -> io::Result<serde_json::Value> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > 4096 {
        return Err(io::Error::other("handshake frame too large"));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(io::Error::other)
}

// ---------- encrypted stream (everything after the handshake) ----------

/// AEAD-framed JSON over TCP under an ephemeral session key. Nonces are
/// direction-tagged counters — no frame can be dropped, reordered, or
/// replayed without failing authentication.
pub struct SecureStream {
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

impl SecureStream {
    fn new(stream: TcpStream, key: &blake3::Hash, is_client: bool) -> Self {
        use chacha20poly1305::KeyInit;
        Self {
            stream,
            cipher: chacha20poly1305::ChaCha20Poly1305::new(key.as_bytes().into()),
            send_dir: if is_client { 1 } else { 2 },
            send_ctr: 0,
            recv_ctr: 0,
        }
    }

    pub fn send(&mut self, v: &serde_json::Value) -> io::Result<()> {
        use chacha20poly1305::aead::Aead;
        let plain = serde_json::to_vec(v)?;
        let sealed = self
            .cipher
            .encrypt(&nonce_for(self.send_dir, self.send_ctr), plain.as_slice())
            .map_err(|_| io::Error::other("encrypt failed"))?;
        self.send_ctr += 1;
        self.stream.write_all(&(sealed.len() as u32).to_le_bytes())?;
        self.stream.write_all(&sealed)?;
        self.stream.flush()
    }

    pub fn recv(&mut self) -> io::Result<serde_json::Value> {
        use chacha20poly1305::aead::Aead;
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len);
        if len > MAX_FRAME {
            return Err(io::Error::other("frame too large"));
        }
        let mut buf = vec![0u8; len as usize];
        self.stream.read_exact(&mut buf)?;
        let their_dir = if self.send_dir == 1 { 2 } else { 1 };
        let plain = self
            .cipher
            .decrypt(&nonce_for(their_dir, self.recv_ctr), buf.as_slice())
            .map_err(|_| io::Error::other("transport decrypt failed"))?;
        self.recv_ctr += 1;
        serde_json::from_slice(&plain).map_err(io::Error::other)
    }
}

/// Client half of the handshake. Returns the encrypted stream plus the
/// relay's identity (hex) for the caller to pin / verify. The signature check
/// always runs — an unpinned first contact still proves the relay holds the
/// key it claims, and the caller decides whether to trust it.
pub fn client_handshake(mut stream: TcpStream) -> io::Result<(SecureStream, String)> {
    let mut eph = [0u8; 32];
    getrandom::fill(&mut eph).map_err(io::Error::other)?;
    let eph_pk = hex(ed25519_dalek::SigningKey::from_bytes(&eph).verifying_key().as_bytes());
    write_frame(&mut stream, &serde_json::json!({ "type": "hello", "eph": eph_pk }))?;

    let reply = read_frame(&mut stream)?;
    let relay_eph = reply.get("eph").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let relay_pk = reply.get("pk").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let sig = reply.get("sig").and_then(|v| v.as_str()).unwrap_or_default();

    let pk_bytes = hex_bytes::<32>(&relay_pk).ok_or_else(|| io::Error::other("bad relay pk"))?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|_| io::Error::other("bad relay pk"))?;
    let sig_bytes = hex_bytes::<64>(sig).ok_or_else(|| io::Error::other("bad relay sig"))?;
    use ed25519_dalek::Verifier;
    vk.verify(&auth_msg(&eph_pk, &relay_eph), &ed25519_dalek::Signature::from_bytes(&sig_bytes))
        .map_err(|_| io::Error::other("relay identity signature invalid"))?;

    let shared = ecdh(&eph, &relay_eph).ok_or_else(|| io::Error::other("bad relay ephemeral"))?;
    let key = channel_key(&shared, &eph_pk, &relay_eph);
    Ok((SecureStream::new(stream, &key, true), relay_pk))
}

fn server_handshake(relay: &Relay, mut stream: TcpStream) -> io::Result<SecureStream> {
    use ed25519_dalek::Signer;
    let hello = read_frame(&mut stream)?;
    let client_eph = hello
        .get("eph")
        .and_then(|v| v.as_str())
        .ok_or_else(|| io::Error::other("expected encrypted-transport hello"))?
        .to_string();

    let mut eph = [0u8; 32];
    getrandom::fill(&mut eph).map_err(io::Error::other)?;
    let relay_eph = hex(ed25519_dalek::SigningKey::from_bytes(&eph).verifying_key().as_bytes());
    let sig = relay.identity.sign(&auth_msg(&client_eph, &relay_eph));
    write_frame(
        &mut stream,
        &serde_json::json!({
            "type": "hello",
            "eph": relay_eph,
            "pk": relay.public_key_hex(),
            "sig": hex(&sig.to_bytes()),
        }),
    )?;

    let shared = ecdh(&eph, &client_eph).ok_or_else(|| io::Error::other("bad client ephemeral"))?;
    let key = channel_key(&shared, &client_eph, &relay_eph);
    Ok(SecureStream::new(stream, &key, false))
}

fn handle_request(relay: &Relay, req: &serde_json::Value) -> serde_json::Value {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let err = |m: &str| serde_json::json!({ "ok": false, "error": m });

    match req.get("type").and_then(|t| t.as_str()) {
        Some("ping") => serde_json::json!({ "ok": true }),
        Some("post") => {
            let channel = req.get("channel").and_then(|c| c.as_str()).unwrap_or_default();
            let Some(data) = req
                .get("data")
                .and_then(|d| d.as_str())
                .and_then(|d| b64.decode(d).ok())
            else {
                return err("missing or invalid data");
            };
            match relay.post(channel, &data) {
                Ok(seq) => serde_json::json!({ "ok": true, "seq": seq }),
                Err(e) => err(&e.to_string()),
            }
        }
        Some("poll") => {
            let channel = req.get("channel").and_then(|c| c.as_str()).unwrap_or_default();
            let since = req.get("since").and_then(|s| s.as_u64()).unwrap_or(0);
            let max = req.get("max").and_then(|m| m.as_u64()).unwrap_or(100).min(500) as usize;
            match relay.poll(channel, since, max) {
                Ok((envelopes, more)) => serde_json::json!({
                    "ok": true,
                    "more": more,
                    "envelopes": envelopes
                        .iter()
                        .map(|(seq, data)| serde_json::json!({ "seq": seq, "data": b64.encode(data) }))
                        .collect::<Vec<_>>(),
                }),
                Err(e) => err(&e.to_string()),
            }
        }
        _ => err("unknown request type"),
    }
}

fn handle_conn(relay: Arc<Relay>, stream: TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(60))).ok();
    let Ok(mut chan) = server_handshake(&relay, stream) else { return };
    while let Ok(req) = chan.recv() {
        let resp = handle_request(&relay, &req);
        if chan.send(&resp).is_err() {
            break;
        }
    }
}

/// Accept loop — one thread per connection (mailbox workloads are tiny).
/// Runs until `running` goes false (checked a few times a second), so an
/// embedding app can host the relay behind a settings toggle.
pub fn serve_until(
    listener: TcpListener,
    relay: Arc<Relay>,
    running: Arc<std::sync::atomic::AtomicBool>,
) {
    listener.set_nonblocking(true).ok();
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).ok();
                let relay = relay.clone();
                std::thread::spawn(move || handle_conn(relay, stream));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Accept loop that never stops — the standalone binary's mode.
pub fn serve(listener: TcpListener, relay: Arc<Relay>) {
    serve_until(listener, relay, Arc::new(std::sync::atomic::AtomicBool::new(true)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_relay(keep_days: u64) -> (Arc<Relay>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pinq-relay-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (
            Arc::new(Relay::new(RelayConfig { data_dir: dir.clone(), keep_days }).unwrap()),
            dir,
        )
    }

    const CHAN: &str = "abcdef0123456789abcdef0123456789";

    #[test]
    fn post_assigns_monotonic_seqs_and_poll_returns_after_cursor() {
        let (relay, dir) = temp_relay(0);
        assert_eq!(relay.post(CHAN, b"one").unwrap(), 1);
        assert_eq!(relay.post(CHAN, b"two").unwrap(), 2);
        assert_eq!(relay.post(CHAN, b"three").unwrap(), 3);

        let (all, more) = relay.poll(CHAN, 0, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert!(!more);
        assert_eq!(all[0], (1, b"one".to_vec()));

        let (tail, _) = relay.poll(CHAN, 2, 100).unwrap();
        assert_eq!(tail, vec![(3, b"three".to_vec())]);

        // `max` caps a batch and signals more.
        let (page, more) = relay.poll(CHAN, 0, 2).unwrap();
        assert_eq!(page.len(), 2);
        assert!(more);

        // Sequence numbers AND identity survive a "restart".
        let relay2 = Relay::new(RelayConfig { data_dir: dir.clone(), keep_days: 0 }).unwrap();
        assert_eq!(relay2.post(CHAN, b"four").unwrap(), 4);
        assert_eq!(relay2.public_key_hex(), relay.public_key_hex());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn channels_are_isolated_and_ids_validated() {
        let (relay, dir) = temp_relay(0);
        let other = "0000000000000000";
        relay.post(CHAN, b"secret").unwrap();
        assert!(relay.poll(other, 0, 10).unwrap().0.is_empty());

        assert!(relay.post("../escape", b"x").is_err());
        assert!(relay.post("UPPER0123456789A", b"x").is_err());
        assert!(relay.post("short", b"x").is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn serves_encrypted_and_hides_the_channel_id_on_the_wire() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let (relay, dir) = temp_relay(0);
        let expected_pk = relay.public_key_hex();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, relay));

        let stream = TcpStream::connect(addr).unwrap();
        let (mut chan, relay_pk) = client_handshake(stream).unwrap();
        assert_eq!(relay_pk, expected_pk, "handshake reports the relay's real identity");

        chan.send(&serde_json::json!({ "type": "post", "channel": CHAN, "data": b64.encode(b"hello") }))
            .unwrap();
        let resp = chan.recv().unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["seq"], 1);

        // Same connection, second request.
        chan.send(&serde_json::json!({ "type": "poll", "channel": CHAN, "since": 0 })).unwrap();
        let resp = chan.recv().unwrap();
        assert_eq!(resp["ok"], true);
        let data = resp["envelopes"][0]["data"].as_str().unwrap();
        assert_eq!(b64.decode(data).unwrap(), b"hello");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn plaintext_clients_are_refused() {
        let (relay, dir) = temp_relay(0);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, relay));

        // Old-style plaintext request instead of a hello: connection dies,
        // nothing is stored.
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        write_frame(&mut stream, &serde_json::json!({ "type": "post", "channel": CHAN, "data": "aGk=" }))
            .unwrap();
        let mut buf = [0u8; 4];
        assert!(
            stream.read_exact(&mut buf).is_err(),
            "relay must hang up on plaintext protocol"
        );
        assert!(!dir.join(CHAN).exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
