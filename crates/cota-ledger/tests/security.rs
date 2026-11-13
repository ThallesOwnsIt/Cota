//! Security review of the v3 ledger (Etapa 4.5): each finding as a test that failed before its fix.

use cota_ledger::*;
use ed25519_dalek::{Signer as _, SigningKey};

const DAY: i64 = 86_400;
const D: i64 = 1_795_132_800; // 2026-11-20T00:00Z

fn keys() -> Vec<SigningKey> {
    (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect()
}

fn members(keys: &[SigningKey]) -> Vec<Key> {
    keys.iter().map(|k| k.verifying_key().to_bytes()).collect()
}

fn sign(body: Body, keys: &[SigningKey], positions: &[usize]) -> Entry {
    let msg = body.signing_message();
    let signatures = positions.iter().map(|&p| Signature { member: p as u16, signature: keys[p].sign(&msg).to_bytes() }).collect();
    Entry { body, quorum: Quorum { committee: MAIN_COMMITTEE, epoch: 0, signatures } }
}

fn genesis(keys: &[SigningKey]) -> Entry {
    sign(
        Body::Genesis(Genesis {
            v2_program: [1; 32],
            v2_registry: [2; 32],
            v2_leaf_count: 0,
            v2_root: [0; 32],
            v1_archive_sha256: [3; 32],
            members: members(keys),
            threshold: 2,
            cutover_at: D,
        }),
        keys,
        &[0, 1],
    )
}

fn stream(keys: &[SigningKey], id: &str) -> Entry {
    sign(Body::StreamCreate(StreamCreate { id: id.into(), starts_at: D }), keys, &[0, 1])
}

fn batch(keys: &[SigningKey], id: u64, t_start: i64, t_end: i64, prev: Hash) -> Entry {
    sign(
        Body::Batch(Batch {
            stream: "guaiba".into(),
            batch_id: id,
            t_start,
            t_end,
            leaf_count: 1,
            merkle_root: [id as u8; 32],
            station_set_hash: [0; 32],
            prev_batch_root: prev,
        }),
        keys,
        &[0, 1],
    )
}

fn bytes(entries: &[Entry]) -> Vec<Vec<u8>> {
    entries.iter().map(Entry::to_bytes).collect()
}

/// An entry whose signatures are garbage (what an attacker who controls the published files can add).
fn unsigned(body: Body) -> Entry {
    Entry { body, quorum: Quorum { committee: MAIN_COMMITTEE, epoch: 0, signatures: vec![Signature { member: 0, signature: [7; 64] }, Signature { member: 1, signature: [7; 64] }] } }
}

#[test]
fn ids_used_in_file_names_are_restricted() {
    // Finding: a stream id became part of a file name (batches/<stream>-<id>.json); "../x" wrote outside the ledger.
    let k = keys();
    let mut l = Ledger::new();
    l.apply(&genesis(&k)).unwrap();
    for bad in ["../x", "a/b", ".hidden", "Guaíba", "a b"] {
        assert_eq!(l.apply(&stream(&k, bad)).unwrap_err(), Refusal::InvalidId, "{bad}");
        let src = sign(Body::RuleSet(RuleSet::ExternalSource { source: bad.into(), stream: "guaiba".into() }), &k, &[0, 1]);
        assert!(l.apply(&src).is_err());
    }
    l.apply(&stream(&k, "guaiba")).unwrap();
    l.apply(&stream(&k, "rio-pardo_2")).unwrap();
    // Station ids keep their upper case (ANA-87450004) but no path characters either.
    let st = |id: &str| {
        sign(
            Body::Station(Station { station_id: id.into(), name: "x".into(), source: 0, variable: 0, lat_e6: 0, lon_e6: 0, datum_id: None, stream: "guaiba".into() }),
            &k,
            &[0, 1],
        )
    };
    assert_eq!(l.apply(&st("../ANA")).unwrap_err(), Refusal::InvalidId);
    l.apply(&st("ANA-87450004")).unwrap();
}

#[test]
fn a_light_check_verifies_the_entries_after_the_last_checkpoint() {
    // Finding: in a light check, entries after the last checkpoint were accepted without their signatures, so
    // anyone who controls the published files could append unsigned entries and the page still said "confere".
    let k = keys();
    let mut l = Ledger::new();
    let mut entries = vec![genesis(&k), stream(&k, "guaiba")];
    for e in &entries {
        l.apply(e).unwrap();
    }
    let cp = l.checkpoint(None, l.clock()).unwrap();
    let sigs: Vec<Signature> = (0..2).map(|p| Signature { member: p, signature: k[p as usize].sign(&cp.signing_message()).to_bytes() }).collect();
    entries.push(unsigned(Body::Batch(Batch {
        stream: "guaiba".into(),
        batch_id: 1,
        t_start: D,
        t_end: D + DAY,
        leaf_count: 9,
        merkle_root: [9; 32],
        station_set_hash: [0; 32],
        prev_batch_root: [0; 32],
    })));
    let r = replay(&[], &bytes(&entries), &[(cp, sigs)], Check::Light, &Anchor::default());
    assert!(matches!(r, Err(ReplayError::Entry { index: 2, refusal: Refusal::BadSignature(0), .. })), "{r:?}");
}

#[test]
fn extreme_numbers_are_refused_not_a_crash() {
    // Finding: arithmetic on signed fields could overflow and panic (the verifier crashed instead of refusing).
    let k = keys();
    let base = vec![genesis(&k), stream(&k, "guaiba"), batch(&k, 1, D, D + DAY, [0; 32])];
    let outlook_rule = sign(
        Body::RuleSet(RuleSet::Outlook { region: "guaiba".into(), stream: "guaiba".into(), threshold_milli: 1, window_secs: DAY, min_models: 1, spec_sha256: [0; 32] }),
        &k,
        &[0, 1],
    );
    let cases: Vec<Body> = vec![
        Body::Degradation(Degradation { stream: "guaiba".into(), batch_id: u64::MAX, station_id: "X".into(), score_milli: 0, reason: 0 }),
        Body::Outlook(Outlook {
            region: "guaiba".into(),
            issued_at: D + DAY,
            window_end: i64::MIN,
            models: 1,
            rain_mean_milli: 0,
            rain_min_milli: 0,
            rain_max_milli: 0,
            content_sha256: [0; 32],
        }),
    ];
    for body in cases {
        let mut entries = base.clone();
        entries.push(outlook_rule.clone());
        entries.push(sign(body, &k, &[0, 1]));
        let r = std::panic::catch_unwind(|| replay(&[], &bytes(&entries), &[], Check::Full, &Anchor::default()));
        assert!(matches!(r, Ok(Err(ReplayError::Entry { .. }))), "{r:?}");
    }
    // A batch ending at the end of time, then a committee change announced then: the delay must not overflow.
    let mut entries = vec![genesis(&k), stream(&k, "guaiba"), batch(&k, 1, D, i64::MAX - 1, [0; 32])];
    let mut more = members(&k);
    more.push(SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes());
    entries.push(sign(Body::CommitteeChange(CommitteeChange { epoch: 1, members: more, threshold: 3, announced_at: i64::MAX - 1 }), &k, &[0, 1]));
    let r = std::panic::catch_unwind(|| replay(&[], &bytes(&entries), &[], Check::Full, &Anchor::default()));
    assert!(matches!(r, Ok(Err(ReplayError::Entry { .. }))), "{r:?}");
}

#[test]
fn verifiers_pin_the_first_committee() {
    // Finding: any Genesis signed by its own committee was accepted, so a whole ledger made with other keys passed
    // the check. A verifier must compare Genesis with the committee it trusts (v2's, on-chain).
    let k = keys();
    let impostors: Vec<SigningKey> = (7..=9u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let fake = vec![genesis(&impostors), stream(&impostors, "guaiba")];
    let anchor = Anchor { genesis_members: Some(members(&k)), now: None };
    let r = replay(&[], &bytes(&fake), &[], Check::Full, &anchor);
    assert!(matches!(r, Err(ReplayError::Entry { index: 0, refusal: Refusal::UntrustedGenesis, .. })), "{r:?}");
    let real = vec![genesis(&k), stream(&k, "guaiba")];
    assert!(replay(&[], &bytes(&real), &[], Check::Full, &anchor).is_ok());
}

#[test]
fn a_ledger_clock_in_the_future_is_flagged() {
    // Finding: the 7-day committee delay runs on the ledger clock, which a colluding majority could push forward
    // with a batch dated in the future. A verifier that knows the real time refuses batches from the future.
    let k = keys();
    let entries = vec![genesis(&k), stream(&k, "guaiba"), batch(&k, 1, D, D + 30 * DAY, [0; 32])];
    let anchor = Anchor { genesis_members: Some(members(&k)), now: Some(D + 2 * DAY) };
    let r = replay(&[], &bytes(&entries), &[], Check::Full, &anchor);
    assert!(matches!(r, Err(ReplayError::Entry { index: 2, refusal: Refusal::FutureBatch, .. })), "{r:?}");
    let today = vec![genesis(&k), stream(&k, "guaiba"), batch(&k, 1, D, D + DAY, [0; 32])];
    assert!(replay(&[], &bytes(&today), &[], Check::Full, &anchor).is_ok());
}
