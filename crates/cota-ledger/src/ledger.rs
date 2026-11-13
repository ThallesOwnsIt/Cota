//! The validity rules of the ledger as a state machine (`docs/ledger-v3.md` §4–§7).
//!
//! Every check of an entry happens before any change to the state, so a refused entry leaves the ledger exactly as it
//! was.

use std::collections::{BTreeMap, BTreeSet};

use cota_core::mmr::{self, MAX_PEAKS};
use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};

use crate::*;

/// A committee: its keys, in position order, and its threshold (always a majority).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committee {
    pub members: Vec<Key>,
    pub threshold: u16,
}

impl Committee {
    /// At least one member, distinct valid Ed25519 keys (no weak, small-order key, whose signatures anyone can forge),
    /// positions that fit in a u16, threshold > members/2 and ≤ members: two disjoint groups of members can never
    /// both reach it.
    pub fn new(members: Vec<Key>, threshold: u16) -> Result<Self, Refusal> {
        let (n, k) = (members.len(), threshold as usize);
        if n == 0 || n > u16::MAX as usize + 1 || 2 * k <= n || k > n {
            return Err(Refusal::InvalidCommittee);
        }
        let distinct: BTreeSet<&Key> = members.iter().collect();
        if distinct.len() != n || members.iter().any(|m| VerifyingKey::from_bytes(m).map_or(true, |k| k.is_weak())) {
            return Err(Refusal::InvalidCommittee);
        }
        Ok(Self { members, threshold })
    }

    /// Every signature valid over `message`, from distinct members in increasing position order, and at least the
    /// threshold of them. A signature from outside the committee, or an invalid one, refuses the whole set.
    pub fn verify(&self, message: &[u8], signatures: &[Signature]) -> Result<(), Refusal> {
        if signatures.len() < self.threshold as usize {
            return Err(Refusal::ThresholdNotMet { have: signatures.len(), need: self.threshold });
        }
        let mut last: Option<u16> = None;
        for s in signatures {
            if last.is_some_and(|l| s.member <= l) {
                return Err(Refusal::SignaturesNotSorted);
            }
            last = Some(s.member);
            let key = self.members.get(s.member as usize).ok_or(Refusal::UnknownMember(s.member))?;
            let key = VerifyingKey::from_bytes(key).map_err(|_| Refusal::BadSignature(s.member))?;
            key.verify_strict(message, &DalekSignature::from_bytes(&s.signature))
                .map_err(|_| Refusal::BadSignature(s.member))?;
        }
        Ok(())
    }
}

/// How much `apply_checked` and [`replay`] check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Every rule and every signature: what the builder and a full verification do.
    Full,
    /// Every rule, the signatures of Genesis and committee changes (who may sign), and the last checkpoint's; the
    /// other entries are trusted to carry the signatures that checkpoint vouches for. Fast enough to run on every page
    /// load; a full check confirms it.
    Light,
}

/// A replay's outcome: the ledger, what the rules decided, and the checkpoints checked.
#[derive(Clone, Debug)]
pub struct Replay {
    pub ledger: Ledger,
    /// Entries by kind.
    pub kinds: BTreeMap<&'static str, u64>,
    /// Flow forecasts whose alert rule fired, and outlooks that triggered an early warning.
    pub alerts_fired: u64,
    pub outlooks_triggered: u64,
    /// Checkpoints whose signatures were verified (all of them in a full check, the last one in a light check), and
    /// the last checkpoint with its signatures.
    pub checkpoints_signed: u64,
    pub checkpoints: u64,
    pub last_checkpoint: Option<(Checkpoint, Vec<Signature>)>,
}

/// Why a replay stopped: the entry (leaf index) or checkpoint (its size), and the refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    Entry { index: u64, kind: &'static str, refusal: Refusal },
    Checkpoint { size: u64, refusal: Refusal },
    /// A checkpoint whose size no state of the ledger had.
    OrphanCheckpoint { size: u64 },
}

/// Rebuilds the ledger from v2's leaves (only the prefix Genesis cites is used), the v3 entries' bytes, and the
/// published checkpoints (sorted by size), checking each checkpoint when the replay reaches its size.
pub fn replay(
    v2_leaves: &[Hash],
    entries: &[Vec<u8>],
    checkpoints: &[(Checkpoint, Vec<Signature>)],
    check: Check,
) -> Result<Replay, ReplayError> {
    let mut ledger = Ledger::new();
    let first = entries.first().map(|b| Entry::from_bytes(b));
    let n = match &first {
        Some(Ok(Entry { body: Body::Genesis(g), .. })) => (g.v2_leaf_count as usize).min(v2_leaves.len()),
        _ => v2_leaves.len(),
    };
    for leaf in &v2_leaves[..n] {
        ledger.push_v2_leaf(*leaf).expect("before Genesis");
    }
    let mut out = Replay {
        ledger,
        kinds: BTreeMap::new(),
        alerts_fired: 0,
        outlooks_triggered: 0,
        checkpoints_signed: 0,
        checkpoints: 0,
        last_checkpoint: None,
    };
    let mut pending = checkpoints.iter().peekable();
    let mut prev: Option<&Checkpoint> = None;
    for bytes in entries {
        let index = out.ledger.size();
        let entry = Entry::from_bytes(bytes).map_err(|refusal| ReplayError::Entry { index, kind: "?", refusal })?;
        let kind = entry.body.kind();
        let applied = if entry.to_bytes() != *bytes {
            Err(Refusal::Malformed)
        } else {
            out.ledger.apply_checked(&entry, check)
        }
        .map_err(|refusal| ReplayError::Entry { index, kind, refusal })?;
        *out.kinds.entry(kind).or_default() += 1;
        match applied.outcome {
            Outcome::Forecast { alert: Some(_) } => out.alerts_fired += 1,
            Outcome::Outlook { triggered: true, .. } => out.outlooks_triggered += 1,
            _ => {}
        }
        while let Some((cp, sigs)) = pending.next_if(|(cp, _)| cp.size == out.ledger.size()) {
            let last = pending.peek().is_none();
            let size = cp.size;
            if check == Check::Full || last {
                out.ledger.verify_checkpoint(cp, sigs, prev).map_err(|refusal| ReplayError::Checkpoint { size, refusal })?;
                out.checkpoints_signed += 1;
            } else {
                // Size, root, epoch and chain, without the signatures.
                let expected_prev = prev.map(Checkpoint::hash).unwrap_or_default();
                if cp.log != Checkpoint::LOG
                    || Some(cp.root) != out.ledger.root()
                    || Some(cp.epoch) != out.ledger.epoch()
                    || cp.prev != expected_prev
                {
                    return Err(ReplayError::Checkpoint { size, refusal: Refusal::CheckpointMismatch });
                }
            }
            out.checkpoints += 1;
            prev = Some(cp);
            out.last_checkpoint = Some((cp.clone(), sigs.clone()));
        }
    }
    if let Some((cp, _)) = pending.next() {
        return Err(ReplayError::OrphanCheckpoint { size: cp.size });
    }
    Ok(out)
}

/// A committee announced and not yet in force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingChange {
    pub epoch: u32,
    pub committee: Committee,
    /// On the ledger clock.
    pub effective_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamState {
    pub next_batch_id: u64,
    pub last_batch_root: Hash,
    /// End of the latest batch's window (the stream's start before the first one).
    pub last_t_end: i64,
}

#[derive(Clone, Debug)]
struct AlertRule {
    stream: String,
    threshold_milli: i64,
    level: u8,
}

#[derive(Clone, Debug)]
struct OutlookRule {
    stream: String,
    threshold_milli: i64,
    window_secs: i64,
    min_models: u8,
    last_issued_at: i64,
}

#[derive(Clone, Debug)]
struct Source {
    stream: String,
    last_issued_at: i64,
}

/// What the rules decided about an accepted entry, beyond accepting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// The station's flow-alert rule applied to the forecast's P90: `(threshold_milli, level)` when it fired.
    Forecast { alert: Option<(i64, u8)> },
    /// The region's early-warning rule applied to the forecast rain.
    Outlook { triggered: bool, threshold_milli: i64 },
    CommitteeAnnounced { effective_at: i64 },
    CommitteeWithdrawn,
}

/// An accepted entry: its position, its leaf, what the rules decided, and the epoch that came into force right after
/// it (a pending committee change whose time had come).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub index: u64,
    pub leaf: Hash,
    pub outcome: Outcome,
    pub activated_epoch: Option<u32>,
}

/// The ledger's state, rebuilt from v2's records and the v3 entries alone.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    leaves: Vec<Hash>,
    peaks: [Hash; MAX_PEAKS],
    genesis_index: Option<u64>,
    /// By epoch: `committees[e]` is the committee of epoch `e`.
    committees: Vec<Committee>,
    pending: Option<PendingChange>,
    clock: i64,
    streams: BTreeMap<String, StreamState>,
    alert_rules: BTreeMap<String, AlertRule>,
    outlook_rules: BTreeMap<String, OutlookRule>,
    sources: BTreeMap<String, Source>,
    alert_versions: BTreeSet<Hash>,
    corrections: BTreeSet<(u64, Hash)>,
    /// SHA-256 of the signing message of every accepted entry: the same signed body never enters twice.
    seen: BTreeSet<Hash>,
}

fn text(s: &str, max: usize) -> Result<(), Refusal> {
    if s.is_empty() || s.len() > max {
        return Err(Refusal::TextTooLong);
    }
    Ok(())
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one of v2's records (its bytes as the program emitted them). Only before Genesis: v2 is frozen at the
    /// cutover. v2's records are checked against the program's on-chain peaks by the verifier, not here.
    pub fn push_v2_record(&mut self, record: &[u8]) -> Result<u64, Refusal> {
        self.push_v2_leaf(cota_core::merkle::hash_leaf(record))
    }

    /// Like `push_v2_record`, from the leaf hash (what the site publishes for v2).
    pub fn push_v2_leaf(&mut self, leaf: Hash) -> Result<u64, Refusal> {
        if self.genesis_index.is_some() {
            return Err(Refusal::GenesisAgain);
        }
        Ok(self.append(leaf))
    }

    pub fn size(&self) -> u64 {
        self.leaves.len() as u64
    }

    /// The bagged accumulator root (`cota_core::mmr::bag`); `None` when empty.
    pub fn root(&self) -> Option<Hash> {
        mmr::bag(&self.peaks, self.size())
    }

    pub fn peaks(&self) -> &[Hash; MAX_PEAKS] {
        &self.peaks
    }

    pub fn leaves(&self) -> &[Hash] {
        &self.leaves
    }

    pub fn genesis_index(&self) -> Option<u64> {
        self.genesis_index
    }

    /// The epoch in force (`None` before Genesis).
    pub fn epoch(&self) -> Option<u32> {
        self.committees.len().checked_sub(1).map(|e| e as u32)
    }

    pub fn committee(&self) -> Option<&Committee> {
        self.committees.last()
    }

    pub fn committee_of(&self, epoch: u32) -> Option<&Committee> {
        self.committees.get(epoch as usize)
    }

    pub fn pending(&self) -> Option<&PendingChange> {
        self.pending.as_ref()
    }

    /// The largest batch `t_end` so far (the cutover time right after Genesis).
    pub fn clock(&self) -> i64 {
        self.clock
    }

    pub fn stream(&self, id: &str) -> Option<&StreamState> {
        self.streams.get(id)
    }

    /// Decodes a leaf's data and applies it. The bytes must be exactly the canonical encoding.
    pub fn apply_bytes(&mut self, bytes: &[u8]) -> Result<Applied, Refusal> {
        let entry = Entry::from_bytes(bytes)?;
        if entry.to_bytes() != bytes {
            return Err(Refusal::Malformed);
        }
        self.apply(&entry)
    }

    /// Checks `entry` against the rules and, if it passes, appends it.
    pub fn apply(&mut self, entry: &Entry) -> Result<Applied, Refusal> {
        self.apply_checked(entry, Check::Full)
    }

    /// `apply`, choosing how much to check: [`Check::Light`] skips the signatures of ordinary entries.
    pub fn apply_checked(&mut self, entry: &Entry, check: Check) -> Result<Applied, Refusal> {
        let message = entry.body.signing_message();
        // 1. Who signed: a majority of the committee in force (Genesis: of the committee it names).
        if entry.quorum.committee != MAIN_COMMITTEE {
            return Err(Refusal::UnknownCommittee);
        }
        let (committee, epoch) = match &entry.body {
            Body::Genesis(g) => {
                if self.genesis_index.is_some() {
                    return Err(Refusal::GenesisAgain);
                }
                (Committee::new(g.members.clone(), g.threshold)?, 0)
            }
            _ => {
                let epoch = self.epoch().ok_or(Refusal::NoGenesis)?;
                (self.committees[epoch as usize].clone(), epoch)
            }
        };
        if entry.quorum.epoch != epoch {
            return Err(Refusal::WrongEpoch { expected: epoch, got: entry.quorum.epoch });
        }
        // Who may sign (Genesis, committee changes) is always checked; the rest only in a full check.
        if check == Check::Full || matches!(entry.body, Body::Genesis(_) | Body::CommitteeChange(_)) {
            committee.verify(&message, &entry.quorum.signatures)?;
        }
        let digest = sha256(&message);
        if self.seen.contains(&digest) {
            return Err(Refusal::Duplicate);
        }

        // 2. The rules of its kind (they change the state only once every check passed).
        let outcome = self.apply_body(&entry.body)?;

        // 3. The leaf.
        self.seen.insert(digest);
        let leaf = entry.leaf();
        let index = self.append(leaf);

        // 4. A pending committee change whose time has come takes effect right after this entry.
        let activated_epoch = match &self.pending {
            Some(p) if self.clock >= p.effective_at => {
                let p = self.pending.take().expect("matched Some");
                self.committees.push(p.committee);
                Some(p.epoch)
            }
            _ => None,
        };
        Ok(Applied { index, leaf, outcome, activated_epoch })
    }

    fn append(&mut self, leaf: Hash) -> u64 {
        let index = self.size();
        mmr::append(&mut self.peaks, index, leaf);
        self.leaves.push(leaf);
        index
    }

    fn apply_body(&mut self, body: &Body) -> Result<Outcome, Refusal> {
        match body {
            Body::Genesis(g) => {
                if g.v2_leaf_count != self.size() || g.v2_root != self.root().unwrap_or_default() {
                    return Err(Refusal::GenesisMismatch);
                }
                let committee = Committee::new(g.members.clone(), g.threshold)?;
                self.genesis_index = Some(self.size());
                self.committees = vec![committee];
                self.clock = g.cutover_at;
                Ok(Outcome::None)
            }
            Body::CommitteeChange(c) => self.committee_change(c),
            Body::StreamCreate(s) => {
                text(&s.id, MAX_ID_LEN)?;
                if self.streams.contains_key(&s.id) {
                    return Err(Refusal::StreamExists);
                }
                let state = StreamState { next_batch_id: 1, last_batch_root: [0; 32], last_t_end: s.starts_at };
                self.streams.insert(s.id.clone(), state);
                Ok(Outcome::None)
            }
            Body::RuleSet(r) => self.rule_set(r),
            Body::Station(s) => {
                text(&s.station_id, MAX_STATION_ID_LEN)?;
                if s.name.len() > MAX_STATION_NAME_LEN || s.datum_id.as_ref().is_some_and(|d| d.len() > MAX_DATUM_ID_LEN) {
                    return Err(Refusal::TextTooLong);
                }
                self.streams.get(&s.stream).ok_or(Refusal::UnknownStream)?;
                Ok(Outcome::None)
            }
            Body::Batch(b) => {
                let s = self.streams.get(&b.stream).ok_or(Refusal::UnknownStream)?;
                // Prevents skipping, repeating or inserting batches in the past. A gap stays visible; an overlap is
                // refused.
                if b.batch_id != s.next_batch_id {
                    return Err(Refusal::BatchOutOfSequence);
                }
                if b.prev_batch_root != s.last_batch_root {
                    return Err(Refusal::BrokenHashChain);
                }
                if b.t_end <= b.t_start || b.t_start < s.last_t_end {
                    return Err(Refusal::InvalidWindow);
                }
                let s = self.streams.get_mut(&b.stream).expect("checked above");
                s.next_batch_id += 1;
                s.last_batch_root = b.merkle_root;
                s.last_t_end = b.t_end;
                self.clock = self.clock.max(b.t_end);
                Ok(Outcome::None)
            }
            Body::Degradation(d) => {
                let s = self.streams.get(&d.stream).ok_or(Refusal::UnknownStream)?;
                // Only the batch just anchored: recorded "on the spot", not rewritten later.
                if d.batch_id + 1 != s.next_batch_id {
                    return Err(Refusal::StaleBatch);
                }
                if d.score_milli > 1000 || d.reason > MAX_DEGRADATION_REASON {
                    return Err(Refusal::InvalidDegradation);
                }
                text(&d.station_id, MAX_STATION_ID_LEN)?;
                Ok(Outcome::None)
            }
            Body::Forecast(f) => self.forecast(f),
            Body::PublicAlert(a) => {
                text(&a.issuer, MAX_ISSUER_LEN)?;
                text(&a.alert_id, MAX_ALERT_ID_LEN)?;
                if a.expires < a.onset || a.severity > 3 {
                    return Err(Refusal::InvalidPublicAlert);
                }
                // One entry per warning version.
                if !self.alert_versions.insert(a.content_sha256) {
                    return Err(Refusal::Duplicate);
                }
                Ok(Outcome::None)
            }
            Body::Outlook(o) => self.outlook(o),
            Body::ExternalForecast(e) => {
                let source = self.sources.get(&e.source).ok_or(Refusal::UnknownSource)?;
                if source.stream != e.stream {
                    return Err(Refusal::StreamMismatch);
                }
                let s = self.streams.get(&e.stream).ok_or(Refusal::UnknownStream)?;
                if s.next_batch_id == 1 {
                    return Err(Refusal::NoBatchYet);
                }
                // Tied to the day that has just closed; one per source per issue time.
                if e.issued_at != s.last_t_end || e.horizon_days == 0 {
                    return Err(Refusal::InvalidExternalForecast);
                }
                if e.issued_at <= source.last_issued_at {
                    return Err(Refusal::AlreadyIssued);
                }
                self.sources.get_mut(&e.source).expect("checked above").last_issued_at = e.issued_at;
                Ok(Outcome::None)
            }
            Body::Correction(c) => {
                if c.corrects >= self.size()
                    || c.reason > MAX_CORRECTION_REASON
                    || self.corrections.contains(&(c.corrects, c.content_sha256))
                {
                    return Err(Refusal::InvalidCorrection);
                }
                self.corrections.insert((c.corrects, c.content_sha256));
                Ok(Outcome::None)
            }
        }
    }

    fn committee_change(&mut self, c: &CommitteeChange) -> Result<Outcome, Refusal> {
        let epoch = self.epoch().ok_or(Refusal::NoGenesis)?;
        let current = self.committees[epoch as usize].clone();
        let proposed = Committee::new(c.members.clone(), c.threshold)?;
        if let Some(p) = &self.pending {
            // Restating the current committee, under the pending epoch, withdraws the change.
            if c.epoch == p.epoch && proposed == current {
                self.pending = None;
                return Ok(Outcome::CommitteeWithdrawn);
            }
            return Err(Refusal::ChangePending);
        }
        if proposed == current {
            return Err(Refusal::NoChange);
        }
        if c.epoch != epoch + 1 || c.announced_at != self.clock {
            return Err(Refusal::WrongAnnouncement);
        }
        let effective_at = c.announced_at + COMMITTEE_DELAY_SECS;
        self.pending = Some(PendingChange { epoch: c.epoch, committee: proposed, effective_at });
        Ok(Outcome::CommitteeAnnounced { effective_at })
    }

    fn rule_set(&mut self, r: &RuleSet) -> Result<Outcome, Refusal> {
        match r {
            RuleSet::FlowAlert { station_id, stream, threshold_milli, level } => {
                text(station_id, MAX_STATION_ID_LEN)?;
                self.streams.get(stream).ok_or(Refusal::UnknownStream)?;
                if self.alert_rules.contains_key(station_id) {
                    return Err(Refusal::RuleExists);
                }
                let rule = AlertRule { stream: stream.clone(), threshold_milli: *threshold_milli, level: *level };
                self.alert_rules.insert(station_id.clone(), rule);
            }
            RuleSet::Outlook { region, stream, threshold_milli, window_secs, min_models, .. } => {
                text(region, MAX_ID_LEN)?;
                self.streams.get(stream).ok_or(Refusal::UnknownStream)?;
                if *threshold_milli <= 0 || *window_secs <= 0 || *min_models == 0 {
                    return Err(Refusal::InvalidRule);
                }
                if self.outlook_rules.contains_key(region) {
                    return Err(Refusal::RuleExists);
                }
                let rule = OutlookRule {
                    stream: stream.clone(),
                    threshold_milli: *threshold_milli,
                    window_secs: *window_secs,
                    min_models: *min_models,
                    last_issued_at: i64::MIN,
                };
                self.outlook_rules.insert(region.clone(), rule);
            }
            RuleSet::ExternalSource { source, stream } => {
                text(source, MAX_ID_LEN)?;
                self.streams.get(stream).ok_or(Refusal::UnknownStream)?;
                if self.sources.contains_key(source) {
                    return Err(Refusal::RuleExists);
                }
                self.sources.insert(source.clone(), Source { stream: stream.clone(), last_issued_at: i64::MIN });
            }
        }
        Ok(Outcome::None)
    }

    fn forecast(&mut self, f: &Forecast) -> Result<Outcome, Refusal> {
        text(&f.station_id, MAX_STATION_ID_LEN)?;
        let s = self.streams.get(&f.stream).ok_or(Refusal::UnknownStream)?;
        if s.next_batch_id == 1 {
            return Err(Refusal::NoBatchYet);
        }
        // Based on the data the stream had: its latest batch.
        if f.based_on_batch_id != s.next_batch_id - 1 || f.based_on_root != s.last_batch_root {
            return Err(Refusal::StaleForecastBase);
        }
        // The forecast reading is outside every anchored batch: the forecast was not adjusted after seeing it.
        if f.target_ts < s.last_t_end {
            return Err(Refusal::TargetAlreadyAnchored);
        }
        if f.p10 > f.p50 || f.p50 > f.p90 || f.horizon_hours == 0 {
            return Err(Refusal::InvalidForecast);
        }
        let alert = match self.alert_rules.get(&f.station_id) {
            None => None,
            Some(r) if r.stream != f.stream => return Err(Refusal::StreamMismatch),
            Some(r) => (f.p90 >= r.threshold_milli).then_some((r.threshold_milli, r.level)),
        };
        Ok(Outcome::Forecast { alert })
    }

    fn outlook(&mut self, o: &Outlook) -> Result<Outcome, Refusal> {
        let rule = self.outlook_rules.get(&o.region).ok_or(Refusal::UnknownRule)?;
        let s = self.streams.get(&rule.stream).ok_or(Refusal::UnknownStream)?;
        if s.next_batch_id == 1 {
            return Err(Refusal::NoBatchYet);
        }
        // Issued on the day that has just closed, with the rule's window and enough models.
        if o.issued_at != s.last_t_end
            || o.window_end - o.issued_at != rule.window_secs
            || o.models < rule.min_models
            || o.rain_min_milli < 0
            || o.rain_min_milli > o.rain_mean_milli
            || o.rain_mean_milli > o.rain_max_milli
        {
            return Err(Refusal::InvalidOutlook);
        }
        if o.issued_at <= rule.last_issued_at {
            return Err(Refusal::AlreadyIssued);
        }
        let outcome = Outcome::Outlook { triggered: o.rain_mean_milli >= rule.threshold_milli, threshold_milli: rule.threshold_milli };
        self.outlook_rules.get_mut(&o.region).expect("checked above").last_issued_at = o.issued_at;
        Ok(outcome)
    }

    /// The checkpoint of the current state, for the committee to sign.
    pub fn checkpoint(&self, prev: Option<&Checkpoint>, signed_at: i64) -> Result<Checkpoint, Refusal> {
        Ok(Checkpoint {
            log: Checkpoint::LOG.into(),
            size: self.size(),
            root: self.root().ok_or(Refusal::CheckpointMismatch)?,
            prev: prev.map(Checkpoint::hash).unwrap_or_default(),
            epoch: self.epoch().ok_or(Refusal::NoGenesis)?,
            signed_at,
        })
    }

    /// Checks a checkpoint against the current state (call it when the replay reaches `checkpoint.size`): same size,
    /// same root, the epoch in force, chained to `prev`, signed by a majority of that epoch's committee.
    pub fn verify_checkpoint(&self, checkpoint: &Checkpoint, signatures: &[Signature], prev: Option<&Checkpoint>) -> Result<(), Refusal> {
        let expected_prev = prev.map(Checkpoint::hash).unwrap_or_default();
        if checkpoint.log != Checkpoint::LOG
            || checkpoint.size != self.size()
            || Some(checkpoint.root) != self.root()
            || Some(checkpoint.epoch) != self.epoch()
            || checkpoint.prev != expected_prev
        {
            return Err(Refusal::CheckpointMismatch);
        }
        let committee = self.committee_of(checkpoint.epoch).ok_or(Refusal::CheckpointMismatch)?;
        committee.verify(&checkpoint.signing_message(), signatures)
    }
}
