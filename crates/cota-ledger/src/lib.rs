//! Cota's own ledger (v3). Specification: `docs/ledger-v3.md`.
//!
//! - [`Entry`]: what the committee signs (a [`Body`]) and the signatures ([`Quorum`]). Its bytes, prefixed with
//!   [`ENTRY_PREFIX`], are a leaf of the accumulator that continues v2's.
//! - [`Ledger`]: the validity rules, as a state machine. The builder applies each new entry before publishing it;
//!   a verifier applies every published entry and must reach the same state, the same root and the same checkpoints.
//! - [`Checkpoint`]: the committee's signed statement of the ledger's size and root, chained and anchored in Bitcoin.

mod ledger;
mod refusal;

pub use ledger::{replay, Anchor, Applied, Check, Committee, Ledger, Outcome, PendingChange, Replay, ReplayError, StreamState};
pub use refusal::Refusal;

use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];
/// An Ed25519 public key.
pub type Key = [u8; 32];

/// First byte of every v3 leaf. v2 records start with their kind tag (0–5), so v2 and v3 leaves never collide.
pub const ENTRY_PREFIX: u8 = 0xC3;
/// A committee change takes effect this long after it is announced, on the ledger clock.
pub const COMMITTEE_DELAY_SECS: i64 = 7 * 86_400;
/// The only committee in this version; the field exists so regional committees can come later.
pub const MAIN_COMMITTEE: u8 = 0;

pub const MAX_ID_LEN: usize = 16;
pub const MAX_STATION_ID_LEN: usize = 32;
pub const MAX_STATION_NAME_LEN: usize = 64;
pub const MAX_DATUM_ID_LEN: usize = 24;
pub const MAX_ISSUER_LEN: usize = 16;
pub const MAX_ALERT_ID_LEN: usize = 64;
/// `Degradation` reasons: 0 unavailable, 1 stuck, 2 jump, 3 disagreement between nodes, 4 no recent data.
pub const MAX_DEGRADATION_REASON: u8 = 4;
/// `Correction` reasons: 0 other (explained in the corrected data), 1 the source revised its data, 2 wrong
/// metadata, 3 the entry should not have been made (e.g. a faulty sensor).
pub const MAX_CORRECTION_REASON: u8 = 3;

pub const CHECKPOINT_DOMAIN: &[u8] = b"COTA-CHECKPOINT-v3";
/// How far after the verifier's "now" a batch window may end before it counts as dated in the future (clock skew of
/// whoever runs the check).
pub const FUTURE_TOLERANCE_SECS: i64 = 86_400;

/// One member's signature; `member` is its position in the committee of the quorum's epoch.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub member: u16,
    pub signature: [u8; 64],
}

/// Who vouched for an entry: signatures of distinct members of committee `committee` in epoch `epoch`, sorted by
/// position, at least the threshold of that epoch.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Quorum {
    pub committee: u8,
    pub epoch: u32,
    pub signatures: Vec<Signature>,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub body: Body,
    pub quorum: Quorum,
}

impl Entry {
    /// The leaf's data: `ENTRY_PREFIX ‖ borsh(entry)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![ENTRY_PREFIX];
        self.serialize(&mut out).expect("Borsh into a Vec does not fail");
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Refusal> {
        match bytes.split_first() {
            Some((&ENTRY_PREFIX, rest)) => Entry::try_from_slice(rest).map_err(|_| Refusal::Malformed),
            _ => Err(Refusal::Malformed),
        }
    }

    /// Leaf hash, as in v1/v2: `H(0x00 ‖ data)`.
    pub fn leaf(&self) -> Hash {
        cota_core::merkle::hash_leaf(&self.to_bytes())
    }
}

/// What the committee signs. Each kind has its own signing domain (see [`Body::signing_message`]).
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Genesis(Genesis),
    CommitteeChange(CommitteeChange),
    StreamCreate(StreamCreate),
    RuleSet(RuleSet),
    Station(Station),
    Batch(Batch),
    Degradation(Degradation),
    Forecast(Forecast),
    PublicAlert(PublicAlert),
    Outlook(Outlook),
    ExternalForecast(ExternalForecast),
    Correction(Correction),
}

impl Body {
    /// `domain ‖ borsh(body)`: a signature for one kind never counts as another, and v1/v2 signatures (other
    /// domains) never count here.
    pub fn signing_message(&self) -> Vec<u8> {
        fn msg<T: BorshSerialize>(domain: &[u8], value: &T) -> Vec<u8> {
            let mut out = domain.to_vec();
            value.serialize(&mut out).expect("Borsh into a Vec does not fail");
            out
        }
        match self {
            Body::Genesis(b) => msg(b"COTA-GENESIS-v3", b),
            Body::CommitteeChange(b) => msg(b"COTA-COMMITTEE-v3", b),
            Body::StreamCreate(b) => msg(b"COTA-STREAM-v3", b),
            Body::RuleSet(b) => msg(b"COTA-RULE-v3", b),
            Body::Station(b) => msg(b"COTA-STATION-v3", b),
            Body::Batch(b) => msg(b"COTA-BATCH-v3", b),
            Body::Degradation(b) => msg(b"COTA-DEGRADATION-v3", b),
            Body::Forecast(b) => msg(b"COTA-FORECAST-v3", b),
            Body::PublicAlert(b) => msg(b"COTA-PUBALERT-v3", b),
            Body::Outlook(b) => msg(b"COTA-OUTLOOK-v3", b),
            Body::ExternalForecast(b) => msg(b"COTA-EXTFC-v3", b),
            Body::Correction(b) => msg(b"COTA-CORRECTION-v3", b),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Body::Genesis(_) => "genesis",
            Body::CommitteeChange(_) => "committee_change",
            Body::StreamCreate(_) => "stream_create",
            Body::RuleSet(_) => "rule_set",
            Body::Station(_) => "station",
            Body::Batch(_) => "batch",
            Body::Degradation(_) => "degradation",
            Body::Forecast(_) => "forecast",
            Body::PublicAlert(_) => "public_alert",
            Body::Outlook(_) => "outlook",
            Body::ExternalForecast(_) => "external_forecast",
            Body::Correction(_) => "correction",
        }
    }
}

/// The first v3 entry: pins v2's final state and names the first committee (epoch 0), which signs it.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Genesis {
    pub v2_program: Key,
    pub v2_registry: Key,
    /// v2's record count at the cutover: the index of this entry.
    pub v2_leaf_count: u64,
    /// v2's bagged accumulator root at that count.
    pub v2_root: Hash,
    pub v1_archive_sha256: Hash,
    pub members: Vec<Key>,
    pub threshold: u16,
    /// 00:00 UTC of the cutover day; the ledger clock starts here.
    pub cutover_at: i64,
}

/// A new committee, announced now and in force [`COMMITTEE_DELAY_SECS`] later. Restating the current committee
/// while a change is pending withdraws it.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct CommitteeChange {
    /// The current epoch + 1.
    pub epoch: u32,
    pub members: Vec<Key>,
    pub threshold: u16,
    /// Must be the ledger clock when the entry is applied (no backdating).
    pub announced_at: i64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct StreamCreate {
    pub id: String,
    /// The first batch cannot start before this.
    pub starts_at: i64,
}

/// Rules and sources: each created once, never changed.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum RuleSet {
    /// A forecast whose P90 reaches `threshold_milli` carries an alert of `level`.
    FlowAlert { station_id: String, stream: String, threshold_milli: i64, level: u8 },
    /// Early warning: mean forecast rain over `window_secs` ≥ `threshold_milli`.
    Outlook { region: String, stream: String, threshold_milli: i64, window_secs: i64, min_models: u8, spec_sha256: Hash },
    /// A third-party forecast source the committee attests.
    ExternalSource { source: String, stream: String },
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Station {
    pub station_id: String,
    pub name: String,
    pub source: u8,
    pub variable: u8,
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub datum_id: Option<String>,
    pub stream: String,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub stream: String,
    pub batch_id: u64,
    pub t_start: i64,
    pub t_end: i64,
    /// Readings in the batch; their Merkle root (RFC 9162) is `merkle_root`.
    pub leaf_count: u32,
    pub merkle_root: Hash,
    pub station_set_hash: Hash,
    pub prev_batch_root: Hash,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Degradation {
    pub stream: String,
    pub batch_id: u64,
    pub station_id: String,
    /// Confidence index × 1000.
    pub score_milli: u16,
    pub reason: u8,
}

/// A flow forecast every node computes with the public model, rounded so that machines agree on the bytes.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Forecast {
    pub stream: String,
    pub station_id: String,
    pub based_on_batch_id: u64,
    pub based_on_root: Hash,
    pub target_ts: i64,
    pub horizon_hours: u16,
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: Hash,
    pub features_root: Hash,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublicAlert {
    pub issuer: String,
    pub alert_id: String,
    pub content_sha256: Hash,
    /// 1 potential danger, 2 danger, 3 great danger; 0 other.
    pub severity: u8,
    pub onset: i64,
    pub expires: i64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Outlook {
    pub region: String,
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    pub content_sha256: Hash,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExternalForecast {
    pub source: String,
    pub stream: String,
    pub issued_at: i64,
    pub horizon_days: u16,
    pub content_sha256: Hash,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    /// Leaf index of the entry (or v2 record) being corrected.
    pub corrects: u64,
    pub reason: u8,
    pub content_sha256: Hash,
}

/// The committee's statement "the ledger has `size` leaves and this root", chained to the previous one.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub log: String,
    pub size: u64,
    pub root: Hash,
    /// SHA-256 of the previous checkpoint's signing message; zeros for the first.
    pub prev: Hash,
    pub epoch: u32,
    pub signed_at: i64,
}

impl Checkpoint {
    pub const LOG: &'static str = "cota";

    pub fn signing_message(&self) -> Vec<u8> {
        let mut out = CHECKPOINT_DOMAIN.to_vec();
        self.serialize(&mut out).expect("Borsh into a Vec does not fail");
        out
    }

    /// What the next checkpoint's `prev` must be.
    pub fn hash(&self) -> Hash {
        Sha256::digest(self.signing_message()).into()
    }
}

pub(crate) fn sha256(data: &[u8]) -> Hash {
    Sha256::digest(data).into()
}
