//! The ledger's rules, entry by entry, and its continuity with v2 on the real records.

use cota_ledger::*;
use ed25519_dalek::{Signer as _, SigningKey};

const DAY: i64 = 86_400;
/// Cutover in these tests: 2026-11-20T00:00Z.
const D: i64 = 1_795_132_800;
const GUAIBA: &str = "guaiba";
const URUGUAI: &str = "uruguai";

fn key(i: u8) -> SigningKey {
    SigningKey::from_bytes(&[i; 32])
}

/// Signs `body` with the keys at `positions` of `keys` (the committee of `epoch`).
fn sign(body: Body, keys: &[SigningKey], positions: &[usize], epoch: u32) -> Entry {
    let msg = body.signing_message();
    let signatures = positions
        .iter()
        .map(|&p| cota_ledger::Signature { member: p as u16, signature: keys[p].sign(&msg).to_bytes() })
        .collect();
    Entry { body, quorum: Quorum { committee: MAIN_COMMITTEE, epoch, signatures } }
}

struct Env {
    ledger: Ledger,
    keys: Vec<SigningKey>,
    epoch: u32,
}

impl Env {
    /// A ledger after Genesis (3 nodes, 2 of 3, no v2 records) with the `guaiba` and `uruguai` streams from D.
    fn new() -> Self {
        let keys: Vec<SigningKey> = (1..=3).map(key).collect();
        let mut env = Self { ledger: Ledger::new(), keys, epoch: 0 };
        env.apply(env.genesis(2)).unwrap();
        env.ok(Body::StreamCreate(StreamCreate { id: GUAIBA.into(), starts_at: D }));
        env.ok(Body::StreamCreate(StreamCreate { id: URUGUAI.into(), starts_at: D }));
        env
    }

    fn members(&self) -> Vec<Key> {
        self.keys.iter().map(|k| k.verifying_key().to_bytes()).collect()
    }

    fn genesis(&self, threshold: u16) -> Entry {
        let body = Body::Genesis(Genesis {
            v2_program: [1; 32],
            v2_registry: [2; 32],
            v2_leaf_count: self.ledger.size(),
            v2_root: self.ledger.root().unwrap_or_default(),
            v1_archive_sha256: [3; 32],
            members: self.members(),
            threshold,
            cutover_at: D,
        });
        let positions: Vec<usize> = (0..threshold as usize).collect();
        sign(body, &self.keys, &positions, 0)
    }

    /// Signed by the first `threshold` members of the epoch in force.
    fn entry(&self, body: Body) -> Entry {
        let k = self.ledger.committee().map(|c| c.threshold as usize).unwrap_or(2);
        sign(body, &self.keys, &(0..k).collect::<Vec<_>>(), self.epoch)
    }

    fn apply(&mut self, entry: Entry) -> Result<Applied, Refusal> {
        let root_before = self.ledger.root();
        let size_before = self.ledger.size();
        let r = self.ledger.apply(&entry);
        match &r {
            Ok(a) => {
                assert_eq!(a.index, size_before);
                if let Some(e) = a.activated_epoch {
                    self.epoch = e;
                }
            }
            // A refused entry leaves the ledger exactly as it was.
            Err(_) => assert_eq!((self.ledger.root(), self.ledger.size()), (root_before, size_before)),
        }
        r
    }

    fn try_body(&mut self, body: Body) -> Result<Applied, Refusal> {
        let e = self.entry(body);
        self.apply(e)
    }

    fn ok(&mut self, body: Body) -> Applied {
        self.try_body(body).unwrap()
    }

    fn next_batch(&self, stream: &str) -> Batch {
        let s = self.ledger.stream(stream).unwrap();
        Batch {
            stream: stream.into(),
            batch_id: s.next_batch_id,
            t_start: s.last_t_end,
            t_end: s.last_t_end + DAY,
            leaf_count: 14,
            merkle_root: [s.next_batch_id as u8 + stream.len() as u8; 32],
            station_set_hash: [0; 32],
            prev_batch_root: s.last_batch_root,
        }
    }

    fn day(&mut self, stream: &str) -> Batch {
        let b = self.next_batch(stream);
        self.ok(Body::Batch(b.clone()));
        b
    }

    fn forecast(&self, station: &str, target_ts: i64, p90: i64) -> Forecast {
        let s = self.ledger.stream(GUAIBA).unwrap();
        Forecast {
            stream: GUAIBA.into(),
            station_id: station.into(),
            based_on_batch_id: s.next_batch_id - 1,
            based_on_root: s.last_batch_root,
            target_ts,
            horizon_hours: 24,
            p10: p90 / 2,
            p50: p90 * 3 / 4,
            p90,
            model_sha256: [7; 32],
            features_root: [8; 32],
        }
    }

    fn outlook(&self, region: &str, issued_at: i64, mean: i64) -> Outlook {
        Outlook {
            region: region.into(),
            issued_at,
            window_end: issued_at + 5 * DAY,
            models: 3,
            rain_mean_milli: mean,
            rain_min_milli: mean / 2,
            rain_max_milli: mean * 2,
            content_sha256: [3; 32],
        }
    }

    fn outlook_rule(&mut self, region: &str) {
        self.ok(Body::RuleSet(RuleSet::Outlook {
            region: region.into(),
            stream: GUAIBA.into(),
            threshold_milli: 90_000,
            window_secs: 5 * DAY,
            min_models: 2,
            spec_sha256: [9; 32],
        }));
    }
}

// ---- continuity with v2 -----------------------------------------------------------------------------

fn v2_records() -> Vec<Vec<u8>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/ledger/records.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let start = line.find("\"record\":\"").unwrap() + 10;
            let end = start + line[start..].find('"').unwrap();
            hex::decode(&line[start..end]).unwrap()
        })
        .collect()
}

/// The latest root the v2 round stamped in Bitcoin (`data/ledger/timestamps/<n>.txt`): how many records it covers and
/// the root.
fn latest_stamp() -> (usize, [u8; 32]) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/ledger/timestamps");
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "txt")).collect();
    files.sort();
    let text = std::fs::read_to_string(files.last().unwrap()).unwrap();
    let field = |name: &str| text.lines().find(|l| l.starts_with(name)).unwrap().split_whitespace().last().unwrap().to_string();
    (field("records").parse().unwrap(), hex::decode(field("root")).unwrap().try_into().unwrap())
}

#[test]
fn genesis_continues_the_real_v2_accumulator() {
    let (stamped, root) = latest_stamp();
    let records = &v2_records()[..stamped];
    let mut env = Env { ledger: Ledger::new(), keys: (1..=3).map(key).collect(), epoch: 0 };
    for r in records {
        env.ledger.push_v2_record(r).unwrap();
    }
    // The same root the v2 round stamped in Bitcoin for these records.
    assert_eq!(env.ledger.root(), Some(root));

    // A Genesis that does not match v2's last state is refused: wrong count, wrong root.
    let mut wrong = env.genesis(2);
    if let Body::Genesis(g) = &mut wrong.body {
        g.v2_leaf_count -= 1;
    }
    let wrong = sign(wrong.body, &env.keys, &[0, 1], 0);
    assert_eq!(env.apply(wrong), Err(Refusal::GenesisMismatch));
    // Nor a first committee without a majority threshold (1 of 3).
    let minority = sign(env.genesis(2).body, &env.keys, &[0], 0);
    assert_eq!(env.apply(minority), Err(Refusal::ThresholdNotMet { have: 1, need: 2 }));

    let g = env.apply(env.genesis(2)).unwrap();
    assert_eq!(g.index, records.len() as u64, "Genesis comes right after the last v2 record");
    assert_eq!(env.ledger.genesis_index(), Some(records.len() as u64));
    assert_eq!(env.ledger.clock(), D);
    // v2 is frozen: no more v2 records, no second Genesis.
    assert_eq!(env.ledger.push_v2_record(&records[0]), Err(Refusal::GenesisAgain));
    assert_eq!(env.apply(env.genesis(2)), Err(Refusal::GenesisAgain));
}

#[test]
fn nothing_enters_before_genesis_and_committees_need_a_majority() {
    let mut ledger = Ledger::new();
    let keys: Vec<SigningKey> = (1..=3).map(key).collect();
    let stream = sign(Body::StreamCreate(StreamCreate { id: GUAIBA.into(), starts_at: D }), &keys, &[0, 1], 0);
    assert_eq!(ledger.apply(&stream), Err(Refusal::NoGenesis));
    let members: Vec<Key> = keys.iter().map(|k| k.verifying_key().to_bytes()).collect();
    assert_eq!(Committee::new(members.clone(), 1), Err(Refusal::InvalidCommittee), "1 of 3 is not a majority");
    assert_eq!(Committee::new(members.clone(), 4), Err(Refusal::InvalidCommittee));
    assert_eq!(Committee::new(vec![members[0], members[0], members[1]], 2), Err(Refusal::InvalidCommittee), "repeated key");
    let mut identity = [0u8; 32];
    identity[0] = 1;
    assert_eq!(Committee::new(vec![identity], 1), Err(Refusal::InvalidCommittee), "weak key (the identity point)");
    assert!(Committee::new(members, 2).is_ok());
}

// ---- encoding and signatures -------------------------------------------------------------------------

#[test]
fn entries_are_prefixed_and_canonical() {
    let mut env = Env::new();
    let e = env.entry(Body::Batch(env.next_batch(GUAIBA)));
    let bytes = e.to_bytes();
    assert_eq!(bytes[0], ENTRY_PREFIX, "v3 leaves start with 0xC3; v2 records start with their kind (0–5)");
    assert_eq!(Entry::from_bytes(&bytes).unwrap(), e);
    assert_eq!(e.leaf(), cota_core::merkle::hash_leaf(&bytes));
    // A v2 record, or trailing bytes, are not v3 entries.
    assert_eq!(env.ledger.clone().apply_bytes(&v2_records()[0]), Err(Refusal::Malformed));
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(env.ledger.clone().apply_bytes(&longer), Err(Refusal::Malformed));
    let a = env.ledger.apply_bytes(&bytes).unwrap();
    assert_eq!(a.leaf, e.leaf());
}

#[test]
fn signatures_must_be_a_sorted_majority_of_the_epoch_in_force_over_this_exact_body() {
    let mut env = Env::new();
    let b = Body::Batch(env.next_batch(GUAIBA));
    let keys = env.keys.clone();
    assert_eq!(env.apply(sign(b.clone(), &keys, &[1], 0)), Err(Refusal::ThresholdNotMet { have: 1, need: 2 }));
    assert_eq!(env.apply(sign(b.clone(), &keys, &[1, 0], 0)), Err(Refusal::SignaturesNotSorted));
    assert_eq!(env.apply(sign(b.clone(), &keys, &[1, 1], 0)), Err(Refusal::SignaturesNotSorted));
    assert_eq!(env.apply(sign(b.clone(), &keys, &[0, 1], 1)), Err(Refusal::WrongEpoch { expected: 0, got: 1 }));
    // A key outside the committee, at a position that does not exist.
    let mut outsider = sign(b.clone(), &keys, &[0, 1], 0);
    outsider.quorum.signatures[1].member = 7;
    assert_eq!(env.apply(outsider), Err(Refusal::UnknownMember(7)));
    // A signature over other bytes: the v2 domain, or another kind with the same fields.
    let Body::Batch(batch) = &b else { unreachable!() };
    let mut v2_msg = b"COTA-BATCH-v2".to_vec();
    borsh::to_writer(&mut v2_msg, batch).unwrap();
    let mut forged = sign(b.clone(), &keys, &[0, 1], 0);
    forged.quorum.signatures[1].signature = keys[1].sign(&v2_msg).to_bytes();
    assert_eq!(env.apply(forged), Err(Refusal::BadSignature(1)));
    // Three valid signatures are fine (more than the threshold).
    env.apply(sign(b.clone(), &keys, &[0, 1, 2], 0)).unwrap();
    // The same body again, even with other signers: a copy.
    assert_eq!(env.apply(sign(b, &keys, &[1, 2], 0)), Err(Refusal::Duplicate));
}

// ---- batches and streams -----------------------------------------------------------------------------

#[test]
fn batches_follow_their_streams_independently() {
    let mut env = Env::new();
    let b1 = env.day(GUAIBA);
    let b2 = env.next_batch(GUAIBA);
    assert_eq!(env.try_body(Body::Batch(Batch { batch_id: 3, ..b2.clone() })), Err(Refusal::BatchOutOfSequence));
    assert_eq!(env.try_body(Body::Batch(Batch { prev_batch_root: [9; 32], ..b2.clone() })), Err(Refusal::BrokenHashChain));
    assert_eq!(env.try_body(Body::Batch(Batch { t_start: b1.t_end - 1, ..b2.clone() })), Err(Refusal::InvalidWindow));
    assert_eq!(env.try_body(Body::Batch(Batch { t_end: b2.t_start, ..b2.clone() })), Err(Refusal::InvalidWindow));
    assert_eq!(env.try_body(Body::Batch(Batch { stream: "litoranea".into(), ..b2.clone() })), Err(Refusal::UnknownStream));
    // A gap is allowed and stays visible.
    env.ok(Body::Batch(Batch { t_start: b2.t_start + DAY, t_end: b2.t_end + DAY, ..b2 }));
    // uruguai did not move; it goes on its own day 1.
    assert_eq!(env.ledger.stream(URUGUAI).unwrap().next_batch_id, 1);
    env.day(URUGUAI);
    assert_eq!((env.ledger.stream(GUAIBA).unwrap().next_batch_id, env.ledger.stream(URUGUAI).unwrap().next_batch_id), (3, 2));
    assert_eq!(env.ledger.clock(), D + 3 * DAY, "the ledger clock is the latest batch end of any stream");
    assert_eq!(env.try_body(Body::StreamCreate(StreamCreate { id: GUAIBA.into(), starts_at: D + DAY })), Err(Refusal::StreamExists));
}

// ---- committee ---------------------------------------------------------------------------------------

#[test]
fn a_committee_change_waits_seven_days_of_ledger_time_then_takes_over() {
    let mut env = Env::new();
    let mut members = env.members();
    let fourth = key(4);
    members.push(fourth.verifying_key().to_bytes());
    let change = |threshold, epoch, announced_at| {
        Body::CommitteeChange(CommitteeChange { epoch, members: members.clone(), threshold, announced_at })
    };
    // 2 of 4 is not a majority; the epoch must be the next one; the announcement must carry the ledger clock.
    assert_eq!(env.try_body(change(2, 1, D)), Err(Refusal::InvalidCommittee));
    assert_eq!(env.try_body(change(3, 2, D)), Err(Refusal::WrongAnnouncement));
    assert_eq!(env.try_body(change(3, 1, D - DAY)), Err(Refusal::WrongAnnouncement), "no backdating");
    let a = env.ok(change(3, 1, D));
    assert_eq!(a.outcome, Outcome::CommitteeAnnounced { effective_at: D + 7 * DAY });
    assert!(env.ledger.pending().is_some());
    // Only one change at a time.
    let other: Vec<Key> = env.members()[..2].to_vec();
    let second = Body::CommitteeChange(CommitteeChange { epoch: 1, members: other, threshold: 2, announced_at: D });
    assert_eq!(env.try_body(second), Err(Refusal::ChangePending));

    // Six days later the old committee still signs; the fourth key does not count yet.
    for _ in 0..6 {
        env.day(GUAIBA);
    }
    assert_eq!(env.ledger.epoch(), Some(0));
    // The seventh day's batch moves the clock to the effective time: the change takes over right after it.
    let b = env.next_batch(GUAIBA);
    let a = env.apply(env.entry(Body::Batch(b))).unwrap();
    assert_eq!((a.activated_epoch, env.ledger.epoch()), (Some(1), Some(1)));
    assert_eq!(env.ledger.committee().unwrap().threshold, 3);
    env.keys.push(fourth);

    // Now: the old epoch is refused, two signatures are not enough, three of the new committee are.
    let b = Body::Batch(env.next_batch(GUAIBA));
    let keys = env.keys.clone();
    assert_eq!(env.apply(sign(b.clone(), &keys, &[0, 1], 0)), Err(Refusal::WrongEpoch { expected: 1, got: 0 }));
    assert_eq!(env.apply(sign(b.clone(), &keys, &[0, 1], 1)), Err(Refusal::ThresholdNotMet { have: 2, need: 3 }));
    env.apply(sign(b, &keys, &[0, 2, 3], 1)).unwrap();
    // The committee of each epoch stays known, for old checkpoints.
    assert_eq!(env.ledger.committee_of(0).unwrap().members.len(), 3);
}

#[test]
fn a_pending_change_can_be_withdrawn_by_the_current_majority() {
    let mut env = Env::new();
    let current = env.members();
    let mut members = current.clone();
    members.push(key(4).verifying_key().to_bytes());
    env.ok(Body::CommitteeChange(CommitteeChange { epoch: 1, members, threshold: 3, announced_at: D }));
    // Restating the current committee under the pending epoch withdraws it.
    let a = env.ok(Body::CommitteeChange(CommitteeChange { epoch: 1, members: current.clone(), threshold: 2, announced_at: D }));
    assert_eq!(a.outcome, Outcome::CommitteeWithdrawn);
    assert!(env.ledger.pending().is_none());
    for _ in 0..8 {
        env.day(GUAIBA);
    }
    assert_eq!(env.ledger.epoch(), Some(0), "the withdrawn change never takes effect");
    // Without a pending change, restating the current committee is not a change.
    let clock = env.ledger.clock();
    assert_eq!(
        env.try_body(Body::CommitteeChange(CommitteeChange { epoch: 1, members: current, threshold: 2, announced_at: clock })),
        Err(Refusal::NoChange)
    );
}

#[test]
fn removing_a_member_makes_its_signature_worthless_and_shifts_positions() {
    let mut env = Env::new();
    // Node 3 leaves: two members, threshold 2.
    let remaining = env.members()[..2].to_vec();
    env.ok(Body::CommitteeChange(CommitteeChange { epoch: 1, members: remaining, threshold: 2, announced_at: D }));
    for _ in 0..7 {
        env.day(GUAIBA);
    }
    assert_eq!(env.ledger.epoch(), Some(1));
    let b = Body::Batch(env.next_batch(GUAIBA));
    let keys = env.keys.clone();
    // Position 2 no longer exists.
    assert_eq!(env.apply(sign(b.clone(), &keys, &[0, 2], 1)), Err(Refusal::UnknownMember(2)));
    env.apply(sign(b, &keys, &[0, 1], 1)).unwrap();
}

#[test]
fn a_committee_of_forty_has_no_limit() {
    let keys: Vec<SigningKey> = (1..=40).map(key).collect();
    let mut env = Env { ledger: Ledger::new(), keys, epoch: 0 };
    env.apply(env.genesis(21)).unwrap();
    env.ok(Body::StreamCreate(StreamCreate { id: GUAIBA.into(), starts_at: D }));
    let b = Body::Batch(env.next_batch(GUAIBA));
    let keys = env.keys.clone();
    assert_eq!(env.apply(sign(b.clone(), &keys, &(0..20).collect::<Vec<_>>(), 0)), Err(Refusal::ThresholdNotMet { have: 20, need: 21 }));
    env.apply(sign(b, &keys, &(19..40).collect::<Vec<_>>(), 0)).unwrap();
}

// ---- forecasts, outlooks, third-party forecasts, warnings, corrections --------------------------------

#[test]
fn forecasts_come_before_the_observation_and_carry_their_rule() {
    let mut env = Env::new();
    assert_eq!(env.try_body(Body::Forecast(env.forecast("GLOFAS-GUAIBA", D + DAY, 1))), Err(Refusal::NoBatchYet));
    env.ok(Body::RuleSet(RuleSet::FlowAlert { station_id: "GLOFAS-GUAIBA".into(), stream: GUAIBA.into(), threshold_milli: 9_353_060, level: 1 }));
    let b = env.day(GUAIBA);
    assert_eq!(env.try_body(Body::Forecast(env.forecast("GLOFAS-GUAIBA", b.t_end - 1, 1))), Err(Refusal::TargetAlreadyAnchored));
    let bad = Forecast { p10: 3, p50: 2, ..env.forecast("GLOFAS-GUAIBA", b.t_end + DAY, 1) };
    assert_eq!(env.try_body(Body::Forecast(bad)), Err(Refusal::InvalidForecast));

    let below = env.ok(Body::Forecast(env.forecast("GLOFAS-GUAIBA", b.t_end + DAY, 5_000_000)));
    assert_eq!(below.outcome, Outcome::Forecast { alert: None });
    let above = env.ok(Body::Forecast(env.forecast("GLOFAS-GUAIBA", b.t_end + 2 * DAY, 9_400_000)));
    assert_eq!(above.outcome, Outcome::Forecast { alert: Some((9_353_060, 1)) });
    let no_rule = env.ok(Body::Forecast(env.forecast("GLOFAS-JACUI", b.t_end + DAY, 1_000_000_000_000)));
    assert_eq!(no_rule.outcome, Outcome::Forecast { alert: None });
    // The same forecast twice is a copy.
    assert_eq!(env.try_body(Body::Forecast(env.forecast("GLOFAS-GUAIBA", b.t_end + DAY, 5_000_000))), Err(Refusal::Duplicate));

    // Signed on yesterday's batch: stale once a new batch is in.
    let old = env.forecast("GLOFAS-GUAIBA", b.t_end + 3 * DAY, 1);
    env.day(GUAIBA);
    assert_eq!(env.try_body(Body::Forecast(old)), Err(Refusal::StaleForecastBase));
    // A guaiba station's rule does not apply on another stream.
    env.day(URUGUAI);
    let s = env.ledger.stream(URUGUAI).unwrap().clone();
    let other = Forecast {
        stream: URUGUAI.into(),
        based_on_batch_id: s.next_batch_id - 1,
        based_on_root: s.last_batch_root,
        target_ts: s.last_t_end + DAY,
        ..env.forecast("GLOFAS-GUAIBA", 0, 1)
    };
    assert_eq!(env.try_body(Body::Forecast(other)), Err(Refusal::StreamMismatch));
}

#[test]
fn one_outlook_per_rule_for_the_day_just_closed() {
    let mut env = Env::new();
    assert_eq!(env.try_body(Body::Outlook(env.outlook(GUAIBA, D, 1))), Err(Refusal::UnknownRule));
    env.outlook_rule(GUAIBA);
    env.outlook_rule("taquari");
    assert_eq!(env.try_body(Body::Outlook(env.outlook(GUAIBA, D, 1))), Err(Refusal::NoBatchYet));
    let b = env.day(GUAIBA);
    assert_eq!(env.try_body(Body::Outlook(env.outlook(GUAIBA, b.t_end + DAY, 1))), Err(Refusal::InvalidOutlook), "ahead of its day");
    let short = Outlook { window_end: b.t_end + DAY, ..env.outlook(GUAIBA, b.t_end, 1) };
    assert_eq!(env.try_body(Body::Outlook(short)), Err(Refusal::InvalidOutlook), "window other than the rule's");

    let fired = env.ok(Body::Outlook(env.outlook(GUAIBA, b.t_end, 95_000)));
    assert_eq!(fired.outcome, Outcome::Outlook { triggered: true, threshold_milli: 90_000 });
    // Another version for the same issue time (other numbers): refused, one per rule per issue time.
    assert_eq!(env.try_body(Body::Outlook(env.outlook(GUAIBA, b.t_end, 10_000))), Err(Refusal::AlreadyIssued));
    // A sub-basin rides on the same stream.
    let quiet = env.ok(Body::Outlook(env.outlook("taquari", b.t_end, 14_400)));
    assert_eq!(quiet.outcome, Outcome::Outlook { triggered: false, threshold_milli: 90_000 });
    // After the next batch, yesterday's issue time is late.
    env.day(GUAIBA);
    assert_eq!(env.try_body(Body::Outlook(env.outlook("taquari", b.t_end, 2))), Err(Refusal::InvalidOutlook));
    // Rules never change.
    assert_eq!(
        env.try_body(Body::RuleSet(RuleSet::Outlook { region: GUAIBA.into(), stream: GUAIBA.into(), threshold_milli: 1, window_secs: DAY, min_models: 1, spec_sha256: [0; 32] })),
        Err(Refusal::RuleExists)
    );
}

#[test]
fn third_party_forecasts_once_per_source_and_day() {
    let mut env = Env::new();
    env.ok(Body::RuleSet(RuleSet::ExternalSource { source: "glofas".into(), stream: GUAIBA.into() }));
    let ext = |env: &Env, stream: &str| ExternalForecast {
        source: "glofas".into(),
        stream: stream.into(),
        issued_at: env.ledger.stream(GUAIBA).unwrap().last_t_end,
        horizon_days: 30,
        content_sha256: [4; 32],
    };
    assert_eq!(env.try_body(Body::ExternalForecast(ext(&env, GUAIBA))), Err(Refusal::NoBatchYet));
    env.day(GUAIBA);
    let e = ext(&env, GUAIBA);
    assert_eq!(env.try_body(Body::ExternalForecast(ExternalForecast { horizon_days: 0, ..e.clone() })), Err(Refusal::InvalidExternalForecast));
    assert_eq!(env.try_body(Body::ExternalForecast(ExternalForecast { issued_at: e.issued_at - DAY, ..e.clone() })), Err(Refusal::InvalidExternalForecast));
    assert_eq!(env.try_body(Body::ExternalForecast(ext(&env, URUGUAI))), Err(Refusal::StreamMismatch));
    assert_eq!(env.try_body(Body::ExternalForecast(ExternalForecast { source: "flood-hub".into(), ..e.clone() })), Err(Refusal::UnknownSource));
    env.ok(Body::ExternalForecast(e.clone()));
    // Another version for the same day: refused.
    assert_eq!(env.try_body(Body::ExternalForecast(ExternalForecast { content_sha256: [5; 32], ..e })), Err(Refusal::AlreadyIssued));
    env.day(GUAIBA);
    env.ok(Body::ExternalForecast(ext(&env, GUAIBA)));
}

#[test]
fn warnings_once_per_version_and_corrections_point_to_existing_entries() {
    let mut env = Env::new();
    let w = PublicAlert {
        issuer: "INMET".into(),
        alert_id: "urn:oid:2.49.0.1.76.0.2026.11.20.1".into(),
        content_sha256: [5; 32],
        severity: 3,
        onset: D,
        expires: D + DAY,
    };
    assert_eq!(env.try_body(Body::PublicAlert(PublicAlert { expires: D - 1, ..w.clone() })), Err(Refusal::InvalidPublicAlert));
    let first = env.ok(Body::PublicAlert(w.clone()));
    // The same version under another id: still the same version.
    assert_eq!(env.try_body(Body::PublicAlert(PublicAlert { alert_id: "outro".into(), ..w.clone() })), Err(Refusal::Duplicate));
    env.ok(Body::PublicAlert(PublicAlert { content_sha256: [6; 32], ..w }));

    let size = env.ledger.size();
    let c = Correction { corrects: first.index, reason: 1, content_sha256: [0xC0; 32] };
    assert_eq!(env.try_body(Body::Correction(Correction { corrects: size, ..c.clone() })), Err(Refusal::InvalidCorrection));
    assert_eq!(env.try_body(Body::Correction(Correction { reason: 9, ..c.clone() })), Err(Refusal::InvalidCorrection));
    env.ok(Body::Correction(c.clone()));
    // The same pair with another reason is still the same correction.
    assert_eq!(env.try_body(Body::Correction(Correction { reason: 0, ..c.clone() })), Err(Refusal::InvalidCorrection));
    env.ok(Body::Correction(Correction { content_sha256: [0xC1; 32], ..c }));
}

#[test]
fn degradations_and_stations_belong_to_a_stream() {
    let mut env = Env::new();
    let station = Station {
        station_id: "ANA-87450004".into(),
        name: "Guaíba no Cais Mauá".into(),
        source: 2,
        variable: 1,
        lat_e6: -30_021_100,
        lon_e6: -51_221_100,
        datum_id: Some("ANA-87450004-regua".into()),
        stream: GUAIBA.into(),
    };
    assert_eq!(env.try_body(Body::Station(Station { stream: "litoranea".into(), ..station.clone() })), Err(Refusal::UnknownStream));
    env.ok(Body::Station(station.clone()));
    // A new datum is a new registration; the same one again is a copy.
    env.ok(Body::Station(Station { datum_id: Some("ANA-87450004-2027".into()), ..station.clone() }));
    assert_eq!(env.try_body(Body::Station(station)), Err(Refusal::Duplicate));

    env.day(GUAIBA);
    let deg = |batch_id, reason| Body::Degradation(Degradation { stream: GUAIBA.into(), batch_id, station_id: "ANA-87450004".into(), score_milli: 200, reason });
    assert_eq!(env.try_body(deg(0, 0)), Err(Refusal::StaleBatch));
    assert_eq!(env.try_body(deg(1, 9)), Err(Refusal::InvalidDegradation));
    env.ok(deg(1, 2));
}

// ---- checkpoints and replay ---------------------------------------------------------------------------

#[test]
fn checkpoints_are_chained_signed_and_rebuilt_by_any_verifier() {
    let mut env = Env::new();
    env.outlook_rule(GUAIBA);
    let mut published: Vec<Vec<u8>> = Vec::new();
    let mut checkpoints: Vec<(Checkpoint, Vec<cota_ledger::Signature>)> = Vec::new();
    let sign_cp = |cp: &Checkpoint, keys: &[SigningKey], positions: &[usize]| -> Vec<cota_ledger::Signature> {
        positions.iter().map(|&p| cota_ledger::Signature { member: p as u16, signature: keys[p].sign(&cp.signing_message()).to_bytes() }).collect()
    };
    // Three rounds, each appending entries and publishing a checkpoint.
    for round in 0..3 {
        let b = env.next_batch(GUAIBA);
        let e = env.entry(Body::Batch(b.clone()));
        env.apply(e.clone()).unwrap();
        published.push(e.to_bytes());
        let o = env.entry(Body::Outlook(env.outlook(GUAIBA, b.t_end, 50_000 + round)));
        env.apply(o.clone()).unwrap();
        published.push(o.to_bytes());
        let prev = checkpoints.last().map(|(c, _)| c.clone());
        let cp = env.ledger.checkpoint(prev.as_ref(), b.t_end + 3600).unwrap();
        let sigs = sign_cp(&cp, &env.keys, &[0, 1, 2]);
        env.ledger.verify_checkpoint(&cp, &sigs, prev.as_ref()).unwrap();
        checkpoints.push((cp, sigs));
    }

    // A verifier starting from nothing: Genesis and the streams (re-signed here, as published), then every entry.
    let mut verifier = Env::new();
    verifier.outlook_rule(GUAIBA);
    assert_eq!(verifier.ledger.root(), {
        let mut fresh = Env::new();
        fresh.outlook_rule(GUAIBA);
        fresh.ledger.root()
    });
    let mut next_cp = 0;
    let mut prev: Option<Checkpoint> = None;
    for bytes in &published {
        verifier.ledger.apply_bytes(bytes).unwrap();
        if let Some((cp, sigs)) = checkpoints.get(next_cp).filter(|(cp, _)| cp.size == verifier.ledger.size()) {
            verifier.ledger.verify_checkpoint(cp, sigs, prev.as_ref()).unwrap();
            prev = Some(cp.clone());
            next_cp += 1;
        }
    }
    assert_eq!(next_cp, 3, "every checkpoint verified during the replay");
    assert_eq!(verifier.ledger.root(), env.ledger.root());

    // Tampering: another root, a broken chain, too few signatures.
    let (last, sigs) = checkpoints.last().unwrap().clone();
    let before = &checkpoints[1].0;
    assert_eq!(env.ledger.verify_checkpoint(&Checkpoint { root: [0; 32], ..last.clone() }, &sigs, Some(before)), Err(Refusal::CheckpointMismatch));
    assert_eq!(env.ledger.verify_checkpoint(&last, &sigs, Some(&checkpoints[0].0)), Err(Refusal::CheckpointMismatch));
    assert_eq!(env.ledger.verify_checkpoint(&last, &sigs[..1], Some(before)), Err(Refusal::ThresholdNotMet { have: 1, need: 2 }));
    // Signatures over the checkpoint are not valid for anything else.
    let other = Checkpoint { signed_at: last.signed_at + 1, ..last.clone() };
    assert_eq!(env.ledger.verify_checkpoint(&other, &sigs, Some(before)), Err(Refusal::BadSignature(0)));
}

// ---- replay (what the browser and check.mjs run) ---------------------------------------------------------------

#[test]
fn a_light_replay_checks_rules_and_the_last_checkpoint_and_a_full_one_every_signature() {
    let mut entries: Vec<Vec<u8>> = Vec::new();
    // The published entries, as the builder appended them; `replay` rebuilds the ledger from their bytes.
    let mut published = Env { ledger: Ledger::new(), keys: (1..=3).map(key).collect(), epoch: 0 };
    let opening = vec![
        published.genesis(2),
        published.entry(Body::StreamCreate(StreamCreate { id: GUAIBA.into(), starts_at: D })),
    ];
    let mut checkpoints = Vec::new();
    for e in opening {
        published.apply(e.clone()).unwrap();
        entries.push(e.to_bytes());
    }
    let sign_cp = |cp: &Checkpoint, keys: &[SigningKey]| -> Vec<cota_ledger::Signature> {
        (0..3).map(|p| cota_ledger::Signature { member: p as u16, signature: keys[p].sign(&cp.signing_message()).to_bytes() }).collect()
    };
    for _ in 0..3 {
        let b = published.entry(Body::Batch(published.next_batch(GUAIBA)));
        published.apply(b.clone()).unwrap();
        entries.push(b.to_bytes());
        let prev = checkpoints.last().map(|(c, _): &(Checkpoint, Vec<cota_ledger::Signature>)| c.clone());
        let cp = published.ledger.checkpoint(prev.as_ref(), published.ledger.clock()).unwrap();
        let sigs = sign_cp(&cp, &published.keys);
        checkpoints.push((cp, sigs));
    }
    for check in [Check::Full, Check::Light] {
        let r = replay(&[], &entries, &checkpoints, check, &Anchor::default()).unwrap();
        assert_eq!(r.ledger.root(), published.ledger.root());
        assert_eq!((r.checkpoints, r.kinds["batch"]), (3, 3));
        assert_eq!(r.checkpoints_signed, if check == Check::Full { 3 } else { 1 });
    }
    // A forged signature on an ordinary entry. With no checkpoint vouching for it, both checks refuse it (a light
    // check verifies in full what no checkpoint covers).
    let mut forged = Entry::from_bytes(&entries[2]).unwrap();
    forged.quorum.signatures[0].signature[0] ^= 1;
    let mut tampered = entries.clone();
    tampered[2] = forged.to_bytes();
    assert!(matches!(replay(&[], &tampered, &[], Check::Full, &Anchor::default()), Err(ReplayError::Entry { index: 2, refusal: Refusal::BadSignature(0), .. })));
    assert!(replay(&[], &tampered, &[], Check::Light, &Anchor::default()).is_err());
    // Under a checkpoint the committee signed over it (only a colluding majority would), the light check trusts the
    // checkpoint and the full one still finds it: why the page offers "check everything".
    let mut colluded = Ledger::new();
    for b in &tampered {
        colluded.apply_checked(&Entry::from_bytes(b).unwrap(), Check::Light).unwrap();
    }
    let cp = colluded.checkpoint(None, colluded.clock()).unwrap();
    let over = vec![(cp.clone(), sign_cp(&cp, &published.keys))];
    assert!(replay(&[], &tampered, &over, Check::Light, &Anchor::default()).is_ok());
    assert!(replay(&[], &tampered, &over, Check::Full, &Anchor::default()).is_err());
    // But a forged last checkpoint is caught by both, and so is a changed entry (the root moves).
    let mut bad = checkpoints.clone();
    bad.last_mut().unwrap().1[0].signature[0] ^= 1;
    assert!(matches!(replay(&[], &entries, &bad, Check::Light, &Anchor::default()), Err(ReplayError::Checkpoint { size: 5, .. })));
    let mut other = Entry::from_bytes(&entries[4]).unwrap();
    if let Body::Batch(b) = &mut other.body {
        b.leaf_count += 1;
    }
    let mut changed = entries.clone();
    changed[4] = other.to_bytes();
    assert!(replay(&[], &changed, &checkpoints, Check::Light, &Anchor::default()).is_err());
    // A checkpoint for a size the ledger never had.
    let mut orphan = checkpoints.clone();
    orphan[2].0.size = 99;
    assert_eq!(replay(&[], &entries, &orphan, Check::Light, &Anchor::default()).unwrap_err(), ReplayError::OrphanCheckpoint { size: 99 });
}
