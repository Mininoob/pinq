//! Signed device registry (spec §21, sync spike 4).
//!
//! Membership in a space is a fold over signed records: the first self-signed
//! "add" is the genesis device; every later add or revoke must be signed by a
//! device that is a member (and not revoked) at that point in the fold. The
//! fold order — (at_ms, device_pk, sig) — is total and identical on every
//! device, so replicas that hold the same record set agree on membership.
//!
//! Honest scope: the registry lets peers refuse sessions with REVOKED devices.
//! It cannot evict a device cryptographically — a revoked device still holds
//! the space key until the space is re-keyed (future work). Devices that
//! prove space-key possession but aren't in the registry yet (paired before
//! the registry existed) are auto-enrolled on first contact.

use crate::store::RegistryEntry;
use ed25519_dalek::{Signer, Verifier};
use std::collections::HashMap;

/// Registry records are scoped to a space by a keyed hash of the space key,
/// so records from a device's previous space are inert after it joins a new one.
pub fn space_tag(space_key: &[u8; 32]) -> String {
    blake3::keyed_hash(space_key, b"pinq-registry-space-v1").to_hex()[..16].to_string()
}

fn payload(space: &str, kind: &str, device_pk: &str, name: &str, at_ms: u64) -> Vec<u8> {
    format!("pinq-registry-v1|{space}|{kind}|{device_pk}|{name}|{at_ms}").into_bytes()
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn pk_of(secret: &[u8; 32]) -> String {
    hex(ed25519_dalek::SigningKey::from_bytes(secret).verifying_key().as_bytes())
}

/// Create a signed record with our device key.
pub fn sign_entry(
    secret: &[u8; 32],
    space: &str,
    kind: &str,
    device_pk: &str,
    name: &str,
    at_ms: u64,
) -> RegistryEntry {
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    let signer_pk = hex(signing.verifying_key().as_bytes());
    let sig = signing.sign(&payload(space, kind, device_pk, name, at_ms));
    RegistryEntry {
        kind: kind.into(),
        device_pk: device_pk.into(),
        name: name.into(),
        at_ms,
        signer_pk,
        sig: hex(&sig.to_bytes()),
    }
}

/// Does the signature actually cover this record, for this space?
pub fn verify_entry(space: &str, e: &RegistryEntry) -> bool {
    let Some(pk) = hex_bytes::<32>(&e.signer_pk) else { return false };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else { return false };
    let Some(sig) = hex_bytes::<64>(&e.sig) else { return false };
    let sig = ed25519_dalek::Signature::from_bytes(&sig);
    vk.verify(&payload(space, &e.kind, &e.device_pk, &e.name, e.at_ms), &sig).is_ok()
}

/// Verify a detached device signature (hex pk + hex sig) over arbitrary bytes
/// — used for sync-session auth challenges.
pub fn verify_device_sig(pk: &str, msg: &[u8], sig: &str) -> bool {
    let Some(pk) = hex_bytes::<32>(pk) else { return false };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else { return false };
    let Some(sig) = hex_bytes::<64>(sig) else { return false };
    vk.verify(msg, &ed25519_dalek::Signature::from_bytes(&sig)).is_ok()
}

/// Static-static X25519 over the device identity keys (Ed25519 converted to
/// Montgomery form). Both directions derive the same secret:
/// a·(b·B) == b·(a·B). Used to seal re-key grants to one specific device —
/// something the old space key can't unlock.
pub fn device_shared_secret(secret: &[u8; 32], peer_pk: &str) -> Option<[u8; 32]> {
    let pk = hex_bytes::<32>(peer_pk)?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk).ok()?;
    let signing = ed25519_dalek::SigningKey::from_bytes(secret);
    let shared = (&signing.to_scalar() * &vk.to_montgomery()).to_bytes();
    // A low-order peer point would make this predictable; refuse it.
    (shared != [0u8; 32]).then_some(shared)
}

/// One device's standing in the space after the fold.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    pub pk: String,
    pub name: String,
    pub added_ms: u64,
    pub added_by: String,
    pub revoked_ms: Option<u64>,
    pub revoked_by: Option<String>,
}

/// Fold records into membership. Invalid signatures are skipped; adds require
/// a member signer (or self-signature while the space is empty — genesis);
/// revokes require a member signer and aren't retroactive (a revoked device's
/// earlier signatures stand). Deterministic for any permutation of `entries`.
pub fn fold(space: &str, entries: &[RegistryEntry]) -> HashMap<String, DeviceRecord> {
    let mut valid: Vec<&RegistryEntry> =
        entries.iter().filter(|e| verify_entry(space, e)).collect();
    valid.sort_by(|a, b| {
        (a.at_ms, &a.device_pk, &a.sig).cmp(&(b.at_ms, &b.device_pk, &b.sig))
    });

    let mut members: HashMap<String, DeviceRecord> = HashMap::new();
    for e in valid {
        let signer_active = members
            .get(&e.signer_pk)
            .map(|r| r.revoked_ms.is_none())
            .unwrap_or(false);
        match e.kind.as_str() {
            "add" => {
                let genesis = e.signer_pk == e.device_pk && members.is_empty();
                if (genesis || signer_active) && !members.contains_key(&e.device_pk) {
                    members.insert(
                        e.device_pk.clone(),
                        DeviceRecord {
                            pk: e.device_pk.clone(),
                            name: e.name.clone(),
                            added_ms: e.at_ms,
                            added_by: e.signer_pk.clone(),
                            revoked_ms: None,
                            revoked_by: None,
                        },
                    );
                }
            }
            "revoke" => {
                if signer_active {
                    if let Some(r) = members.get_mut(&e.device_pk) {
                        if r.revoked_ms.is_none() {
                            r.revoked_ms = Some(e.at_ms);
                            r.revoked_by = Some(e.signer_pk.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    members
}

/// Is this device currently allowed to sync? None = unknown (auto-enroll),
/// Some(true) = member, Some(false) = revoked.
pub fn standing(members: &HashMap<String, DeviceRecord>, pk: &str) -> Option<bool> {
    members.get(pk).map(|r| r.revoked_ms.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: &str = "test-space";

    fn secret(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn genesis_add_revoke_fold() {
        let (a, b, c) = (secret(1), secret(2), secret(3));
        let (pa, pb, pc) = (pk_of(&a), pk_of(&b), pk_of(&c));

        let entries = vec![
            sign_entry(&a, SPACE, "add", &pa, "alpha", 100), // genesis
            sign_entry(&a, SPACE, "add", &pb, "bravo", 200), // member adds member
            sign_entry(&b, SPACE, "add", &pc, "charlie", 300),
            sign_entry(&a, SPACE, "revoke", &pc, "charlie", 400),
        ];
        let m = fold(SPACE, &entries);
        assert_eq!(m.len(), 3);
        assert_eq!(standing(&m, &pa), Some(true));
        assert_eq!(standing(&m, &pb), Some(true));
        assert_eq!(standing(&m, &pc), Some(false), "revoked");
        assert_eq!(standing(&m, "ffff"), None, "unknown");
        assert_eq!(m[&pc].revoked_by.as_deref(), Some(pa.as_str()));
    }

    #[test]
    fn non_member_and_late_self_signs_are_ignored() {
        let (a, b, mallory) = (secret(1), secret(2), secret(9));
        let (pa, pb, pm) = (pk_of(&a), pk_of(&b), pk_of(&mallory));

        let entries = vec![
            sign_entry(&a, SPACE, "add", &pa, "alpha", 100),
            // Mallory self-signs after genesis exists: not a member.
            sign_entry(&mallory, SPACE, "add", &pm, "mallory", 200),
            // Mallory tries to add B and revoke A: signer not a member.
            sign_entry(&mallory, SPACE, "add", &pb, "bravo", 300),
            sign_entry(&mallory, SPACE, "revoke", &pa, "alpha", 400),
        ];
        let m = fold(SPACE, &entries);
        assert_eq!(m.len(), 1);
        assert_eq!(standing(&m, &pa), Some(true));
        assert_eq!(standing(&m, &pm), None);
    }

    #[test]
    fn tampered_and_cross_space_records_fail_verification() {
        let a = secret(1);
        let pa = pk_of(&a);
        let good = sign_entry(&a, SPACE, "add", &pa, "alpha", 100);
        assert!(verify_entry(SPACE, &good));
        assert!(!verify_entry("other-space", &good), "space is bound into the signature");
        let mut renamed = good.clone();
        renamed.name = "evil".into();
        assert!(!verify_entry(SPACE, &renamed));
    }

    #[test]
    fn fold_is_order_independent_with_competing_geneses() {
        // Two devices each self-genesised (legacy pairs), then met: both must
        // fold to the same membership regardless of record order.
        let (a, b) = (secret(1), secret(2));
        let (pa, pb) = (pk_of(&a), pk_of(&b));
        let entries = vec![
            sign_entry(&b, SPACE, "add", &pb, "bravo", 150), // later self-genesis loses
            sign_entry(&a, SPACE, "add", &pa, "alpha", 100),
            sign_entry(&a, SPACE, "add", &pb, "bravo", 500), // but A vouched for B
            sign_entry(&b, SPACE, "add", &pa, "alpha", 500), // and vice versa (no-op)
        ];
        let forward = fold(SPACE, &entries);
        let mut reversed = entries.clone();
        reversed.reverse();
        let backward = fold(SPACE, &reversed);
        assert_eq!(forward.len(), 2);
        assert_eq!(standing(&forward, &pb), Some(true));
        assert_eq!(
            serde_json::to_string(&{
                let mut v: Vec<_> = forward.values().map(|r| format!("{r:?}")).collect();
                v.sort();
                v
            })
            .unwrap(),
            serde_json::to_string(&{
                let mut v: Vec<_> = backward.values().map(|r| format!("{r:?}")).collect();
                v.sort();
                v
            })
            .unwrap()
        );
    }

    #[test]
    fn shared_secret_is_symmetric_and_pairwise_unique() {
        let (a, b, c) = (secret(1), secret(2), secret(3));
        let ab = device_shared_secret(&a, &pk_of(&b)).unwrap();
        let ba = device_shared_secret(&b, &pk_of(&a)).unwrap();
        assert_eq!(ab, ba, "both directions must agree");
        let ac = device_shared_secret(&a, &pk_of(&c)).unwrap();
        assert_ne!(ab, ac, "different peers, different secrets");
        assert!(device_shared_secret(&a, "not-hex").is_none());
    }

    #[test]
    fn revoked_member_cannot_act_afterwards() {
        let (a, b, c) = (secret(1), secret(2), secret(3));
        let (pa, pb, pc) = (pk_of(&a), pk_of(&b), pk_of(&c));
        let entries = vec![
            sign_entry(&a, SPACE, "add", &pa, "alpha", 100),
            sign_entry(&a, SPACE, "add", &pb, "bravo", 200),
            sign_entry(&a, SPACE, "revoke", &pb, "bravo", 300),
            // B, already revoked, tries to add C and un-revoke itself via a friend it adds.
            sign_entry(&b, SPACE, "add", &pc, "charlie", 400),
            sign_entry(&b, SPACE, "revoke", &pa, "alpha", 500),
        ];
        let m = fold(SPACE, &entries);
        assert_eq!(standing(&m, &pb), Some(false));
        assert_eq!(standing(&m, &pc), None, "revoked device's later adds are void");
        assert_eq!(standing(&m, &pa), Some(true));
    }
}
