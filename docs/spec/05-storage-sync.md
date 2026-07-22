# 05 — Storage, Synchronization, Conflicts, Encryption

## 18. Local storage architecture

### Layout (per profile / "space")
```
%APPDATA%/Pinq/spaces/<spaceId>/
├── store.db          SQLite, WAL mode — objects, blocks, props, ops, index
├── store.db-wal/-shm
├── blobs/aa/bb/<blake3>          content-addressed originals (encrypted at rest opt.)
├── derived/thumbs/<blake3>-<size>.webp   regenerable thumbnails/posters
├── derived/previews/…                     link-preview cache
├── keys/             OS-keystore-wrapped key material (never plaintext on disk)
└── backups/          rotating local snapshot archives
```

### SQLite schema (principal tables)
- `objects(id PK, type_id, title_snapshot, flags, pinning_json, created_hlc,
  modified_hlc, created_by, head_op)` — current-state projection.
- `blocks(object_id, block_id, parent_id, order_key, kind, payload_json)` — projection
  of the CRDT for query/render; the CRDT source of truth is `crdt_state`.
- `crdt_state(object_id, field, snapshot BLOB, snapshot_op)` — compacted Yjs update
  blobs per text field.
- `props(object_id, prop_id, value_json)` + `prop_index(object_id, prop_id,
  value_type, value_text, value_num, value_date)` with indexes
  `(prop_id, value_date)`, `(prop_id, value_num)`, `(prop_id, value_text)`.
- `oplog(op_id PK, object_id, device_id, hlc, kind, payload BLOB, applied)` — the
  append-only history; also the sync unit and the note-history source.
- `fts` — FTS5 (unicode61, remove_diacritics) over title, block text, tag names, file
  display names, bookmark titles; external-content table synced by triggers on the
  projections.
- `blobs(hash PK, size, mime, refcount, present, sync_state)`;
  `attachments(object_id, block_id, blob_hash, display_name, meta_json)`.
- `devices`, `peers`, `sync_cursor(peer_id, version_vector)`, `tombstones`.

### Transactions, migrations, recovery
- **Transaction boundary**: one user action = one transaction = (append op(s) → apply
  to projections → update FTS). Autosave batches keystrokes into a CRDT update at
  most every 500 ms or on blur/idle 1 s.
- **Migrations**: numbered forward-only SQL migrations run at startup inside a
  transaction, after an automatic pre-migration backup snapshot; app refuses to open a
  newer-schema DB (message names the required app version).
- **Crash recovery**: WAL replay is automatic; on open, verify `PRAGMA quick_check`,
  then re-project any ops after the projection watermark (projections are always
  rebuildable from `oplog` + `crdt_state`). Editor keeps a per-note journal of the
  last unsaved CRDT delta in a sidecar file, replayed on restart (S18 dialog).
- **Corruption**: `PRAGMA integrity_check` weekly + on unclean shutdown; on failure,
  auto-restore newest valid snapshot from `backups/` (daily, 7 rotations, plus
  pre-migration) and re-sync deltas from peers if paired.
- **Content-addressed blobs**: BLAKE3 hash = identity ⇒ dedup is automatic across
  notes and devices. `refcount` maintained transactionally; GC deletes blobs with
  refcount 0 **and** age > 7 days **and** no un-synced references (mark-and-sweep,
  interruption-safe).
- **Search indexing** off the write path beyond FTS triggers: attachment metadata
  extraction (EXIF, media duration, PDF title) runs in a background queue at idle
  priority, writing `attachments.meta_json` + FTS terms.
- **Cache management**: `derived/` is fully regenerable; LRU-capped (default 2 GB,
  setting); storage manager (S19) shows usage by category with purge buttons.
- **Encryption at rest** (optional, on by default when a keystore exists): SQLCipher-
  compatible page encryption for `store.db` + XChaCha20-Poly1305 per-blob; keys
  wrapped by the OS keystore (DPAPI / Keychain / libsecret). Documented honestly:
  protects a stolen disk, not a compromised logged-in session.

## 19. Peer-to-peer synchronization architecture

**Vocabulary (used precisely):**
- **P2P sync** — devices exchanging ops/blobs directly (LAN or hole-punched).
- **Encrypted relay** — a dumb store-and-forward mailbox for ciphertext when devices
  aren't online simultaneously. It cannot read anything; it is not "the sync."
- **Backup** — an encrypted, versioned archive (local disk or a relay bucket) for
  disaster recovery. Not a sync mechanism.
- **Collaboration** — multiple *users* on one object. Out of scope until later; the op
  format carries authorship so it can be added without a data migration.

### What syncs
Two planes:
1. **Op plane** — the object op-log (small, ordered, CRDT-mergeable).
2. **Blob plane** — content-addressed files, fetched by hash on demand/policy.

### Protocol
- **Device identity**: Ed25519 keypair per device, generated locally.
- **Discovery (LAN)**: mDNS/DNS-SD `_pinq._udp` advertising a spaceId-derived
  *pairing-blind* service tag (HMAC of spaceId — non-members learn nothing).
- **Transport**: QUIC (quinn) with TLS 1.3, both peers authenticating raw Ed25519 keys
  pinned during pairing. Relay path: same QUIC to the relay, payloads additionally
  E2E-encrypted (relay terminates transport TLS but sees only ciphertext envelopes).
- **Reconciliation**: per-object **version vectors** (deviceId → last op counter) plus
  a space-level **Merkle search tree over op-ids** for set reconciliation — a
  returning device finds missing ranges in O(log n) round trips instead of comparing
  100k object vectors. Evaluation summary: pure op-log replay (simple, replays too
  much after long offline), pure state CRDT (large metadata), Merkle-DAG à la
  git (poor partial sync) → chosen hybrid: **CRDT for text fields (yrs), LWW-register
  ops for scalar fields, version vectors per object, Merkle tree for set diff**.
- **Order & causality**: ops carry HLC timestamps (physical + logical + deviceId) —
  immune to wrong wall clocks for correctness; wall-clock skew > 10 min triggers a
  user-visible warning ("Device clock looks wrong; timestamps may display oddly")
  without affecting convergence.
- **Sync queue & retry**: per-peer outbound queue, exponential backoff (1 s → 5 min
  cap), resumable blob transfers (ranged by chunk of 1 MiB), integrity-checked by
  hash on arrival.
- **Selective sync**: per device — full ops always; blob policy = all / on-demand /
  only-under-N-MB / only-on-LAN. A device without a blob shows a placeholder card with
  a fetch button ("available on: Workstation, Relay").
- **Network controls**: metered-connection detection (Windows/macOS APIs; manual
  toggle on Linux) pauses blob plane; bandwidth cap setting; battery: no periodic
  wake — sync is event-driven (local commit, peer hello, app focus) + a 15-min timer
  only while on AC power.
- **Status UX**: one chip (✓ synced / ↻ syncing n items / ⚠ attention) expanding to a
  per-device panel: last seen, pending counts, plain-language errors ("Laptop hasn't
  connected in 12 days — bring it online to receive 214 changes"), and the honest
  relay-visibility statement (see J3).

### Self-hosting
The relay is a small stateless-ish service (Rust, same repo) storing opaque ciphertext
envelopes with TTL + quota; official builds let users point at their own URL; the
managed relay is the same binary. Backup buckets are the same envelope store with
retention.

## 20. Conflict-resolution strategy

Baseline: all devices converge deterministically without user intervention; user-visible
conflict UI is reserved for cases where silent merging would lie.

| Situation | Rule |
|---|---|
| Simultaneous text edits (same block) | Yjs sequence CRDT character-level merge; both edits survive. |
| Block reorder vs. reorder | Order keys are CRDT list positions (fractional/YATA); deterministic interleave; no duplicates or loss. |
| Reorder vs. edit | Independent fields — both apply. |
| Checklist: check vs. uncheck same item | LWW by HLC on the `checked` register; loser recorded in history; note history makes it recoverable. |
| Checklist: concurrent adds | Both items appear (list CRDT). |
| Property value vs. value | LWW-register per property by HLC. If both changed **within 5 min of each other on different devices** and values differ semantically, flag in Conflict panel (S12) showing both, one-click keep-other. |
| Move/resize vs. move/resize | LWW per placement field group (rect as one register — no mixed x-from-A/y-from-B Frankenstein). |
| Pin vs. unpin | LWW; plus rule: an explicit user unpin beats a concurrent auto-restore. Pinning is per-monitor state; pinning on two different devices' monitors is not a conflict (each device shows its own placement — placement is **device-scoped state**, synced for backup but applied per-device). |
| Template edits (concurrent) | Templates are objects → same block/property rules. Instances already created never change. |
| File replaced on two devices | Attachment slot is a register pointing at a hash; LWW wins the slot; **both blobs are kept**, loser accessible via attachment version history (attachment versioning = the register's op history). |
| Delete (trash) vs. edit | Edit wins: concurrent edit un-trashes and the trash op is surfaced in history ("Restored because it was edited on Laptop"). |
| Purge vs. edit | Purge tombstone wins (it's explicit and confirmed); the edit is retained in an orphan-ops archive for the trash-retention window. |
| Restore vs. restore | Idempotent. |
| Wrong device clock | HLC keeps causality; convergence unaffected; display timestamps flagged if skew detected. |
| Device offline for months | Merkle reconciliation streams the delta; ops are never pruned before all *paired* devices ack **or** a device is revoked/expired (configurable 90-day stale-device policy prompts the user: revoke or keep waiting — prevents unbounded log growth). Compaction snapshots + retained tail let a revoked-then-restored device do a full re-baseline instead of op replay. |

## 21. Encryption and device-pairing model

- **User identity** = a space root key (32 bytes, CSPRNG) → derives (HKDF): space
  content key (data at rest + envelope encryption), search-safe key (FTS stays local
  and unencrypted-in-DB *only* when DB page encryption is on; otherwise FTS is inside
  the encrypted DB anyway), relay auth key. **No email, no account.**
- **Device identity** = per-device Ed25519 (sign) + X25519 (KEM) pair, stored in the
  OS keystore.
- **Pairing**: QR (or typed word-code) carries {spaceId, ephemeral pairing public key,
  LAN hints, 60 s expiry}. Devices run a PAKE (SPAKE2) over LAN/relay so a typed short
  code can't be offline-brute-forced; both screens show a 6-digit **safety number**
  (hash of session transcript); user confirms on both. Existing device then wraps the
  space root key to the new device's X25519 key and signs the new device into the
  space's **device registry** (an ordinary synced, signed object).
- **In transit**: QUIC-TLS peer channels + per-envelope XChaCha20-Poly1305 under the
  space content key (so relays see nothing).
- **Key rotation**: space content key re-derivable via lattice of key epochs; rotation
  triggered by device revocation — new epoch key wrapped to all remaining devices; old
  data is lazily re-encrypted for blobs on next write, DB re-key immediate.
  (Honest note: a revoked device keeps whatever it already had — revocation protects
  *future* data, stated plainly in UI.)
- **Revocation**: any device can sign a revocation record; other devices stop trusting
  the revoked key and rotate epoch. Lost device = revoke + rotate + (optionally)
  remote-wipe-on-next-connect best-effort (documented as best-effort).
- **Recovery**: 12-word BIP39-style mnemonic encoding the space root key, shown at
  space creation, confirmed once, nag until confirmed. Restoring = new device + phrase
  → derives keys → pulls from relay/backup or any peer. **Key loss with no phrase and
  no surviving device = data unrecoverable; the UI says exactly this at setup.**
- We do **not** market "zero knowledge"; we state the precise property: relay/backup
  operators store ciphertext and metadata (sizes, timing, device count) and cannot
  read content.
