use anchor_lang::prelude::*;

use crate::constants::*;

/// The committee and the accumulator of every record. Singleton, PDA `["registry"]`.
///
/// This is the only account that grows in meaning over time; its size is fixed.
#[account]
#[derive(InitSpace)]
pub struct Registry {
    /// Who can register stations, set rules, record forecasts and replace members. Cannot anchor
    /// batches, warnings or outlooks alone: those need the committee's signatures.
    pub authority: Pubkey,
    #[max_len(MAX_MEMBERS)]
    pub members: Vec<Pubkey>,
    /// Distinct signatures required (k of n).
    pub threshold: u8,
    /// Next accepted `batch_id` (starts at 1).
    pub next_batch_id: u64,
    /// Merkle root of the latest batch; zeros before the first one.
    pub last_batch_root: [u8; 32],
    /// End of the latest batch's window; the next one cannot start before it.
    pub last_t_end: i64,
    /// Number of records in the accumulator.
    pub leaf_count: u64,
    /// `peaks[h]` is the root of the perfect tree of height `h`, valid when bit `h` of `leaf_count` is set.
    pub peaks: [[u8; 32]; MAX_PEAKS],
    pub bump: u8,
}

/// A station's alert threshold for the flow forecast. Immutable once created.
/// PDA `["alert_rule", station_id]`.
#[account]
#[derive(InitSpace)]
pub struct AlertRule {
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    pub threshold_milli: i64,
    pub level: u8,
    pub set_at: i64,
    pub bump: u8,
}

/// A region's early-warning rule. Immutable. PDA `["outlook_rule", region]`.
#[account]
#[derive(InitSpace)]
pub struct OutlookRule {
    #[max_len(MAX_REGION_LEN)]
    pub region: String,
    /// Mean forecast rain over the window (thousandths of a mm) from which the warning fires.
    pub threshold_milli: i64,
    pub window_secs: i64,
    pub min_models: u8,
    /// SHA-256 of the full specification (points, models, calibration), published off-chain.
    pub rule_sha256: [u8; 32],
    pub set_at: i64,
    pub bump: u8,
}

/// Parametric vault: holds SOL and pays `payout_lamports` to the beneficiary when Cota records a
/// trigger, with nobody's approval. PDA `["vault", creator, vault_id (u64 LE)]`.
#[account]
#[derive(InitSpace)]
pub struct Vault {
    pub creator: Pubkey,
    pub vault_id: u64,
    pub beneficiary: Pubkey,
    /// `TRIGGER_EARLY_WARNING` or `TRIGGER_FLOW_ALERT`.
    pub kind: u8,
    /// Region (early warning) or station (flow alert).
    #[max_len(MAX_STATION_ID_LEN)]
    pub subject: String,
    pub payout_lamports: u64,
    /// Minimum interval between paid events (by the trigger's `issued_at`): an event spanning
    /// several consecutive days pays once.
    pub cooldown_secs: i64,
    /// Only triggers recorded on-chain from this moment on count.
    pub accept_from: i64,
    pub expires_at: i64,
    pub last_event_at: i64,
    pub payouts: u32,
    pub total_paid: u64,
    pub created_at: i64,
    pub bump: u8,
}

/// Payout receipt: one per (vault, trigger record). PDA `["payout", vault, leaf_hash]`.
#[account]
#[derive(InitSpace)]
pub struct Payout {
    pub vault: Pubkey,
    /// Leaf hash of the trigger record and its position in the accumulator.
    pub trigger_leaf: [u8; 32],
    pub trigger_index: u64,
    pub event_at: i64,
    pub amount: u64,
    pub beneficiary: Pubkey,
    pub slot: u64,
    pub paid_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

// ---- what the oracles sign ------------------------------------------------------------------------
// Same Borsh layouts as `cota_core` (tests compare the bytes); only the signing domains differ from v1.

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchHeader {
    pub batch_id: u64,
    pub t_start: i64,
    pub t_end: i64,
    pub leaf_count: u32,
    pub merkle_root: [u8; 32],
    pub station_set_hash: [u8; 32],
    pub prev_batch_root: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublicAlertHeader {
    pub issuer: String,
    pub alert_id: String,
    pub content_sha256: [u8; 32],
    /// 1 potential danger (yellow), 2 danger (orange), 3 great danger (red); 0 other.
    pub severity: u8,
    /// Validity period declared by the issuer, in UTC seconds.
    pub onset: i64,
    pub expires: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct OutlookHeader {
    pub region: String,
    /// End of the latest anchored day: the outlook only uses what existed up to then.
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    /// Rain accumulated over the window, averaged over the points, in thousandths of a mm:
    /// mean, minimum and maximum across the models.
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    pub content_sha256: [u8; 32],
}

fn signing_message<T: AnchorSerialize>(domain: &[u8], value: &T) -> Vec<u8> {
    let mut msg = domain.to_vec();
    value.serialize(&mut msg).expect("Borsh into a Vec does not fail");
    msg
}

impl BatchHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        signing_message(BATCH_DOMAIN, self)
    }
}

impl PublicAlertHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        signing_message(PUBLIC_ALERT_DOMAIN, self)
    }
}

impl OutlookHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        signing_message(OUTLOOK_DOMAIN, self)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct StationArgs {
    pub station_id: String,
    pub name: String,
    pub source: u8,
    pub variable: u8,
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub datum_id: Option<String>,
}

// ---- records: the leaves of the accumulator ----------------------------------------------------------
// Each one is built by the program, with the `slot` and time it read from the `Clock`, so the "when" is
// never the submitter's word. Leaf = H(0x00 ‖ borsh(Record)).

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Station(StationRecord),
    Batch(BatchRecord),
    Degradation(DegradationRecord),
    Forecast(ForecastRecord),
    PublicAlert(PublicAlertRecord),
    Outlook(OutlookRecord),
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct StationRecord {
    pub station: StationArgs,
    pub slot: u64,
    pub registered_at: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchRecord {
    pub header: BatchHeader,
    /// Bit i set = `registry.members[i]` signed.
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct DegradationRecord {
    pub batch_id: u64,
    pub station_id: String,
    /// Confidence index C_s × 1000.
    pub score_milli: u16,
    /// 0 unavailable, 1 stuck, 2 jump, 3 disagreement between nodes, 4 no recent data.
    pub reason: u8,
    pub slot: u64,
}

/// An alert decided by the program in the same instruction as the forecast: P90 ≥ the station's rule.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ForecastAlert {
    pub threshold_milli: i64,
    pub level: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ForecastRecord {
    pub station_id: String,
    /// Latest anchored batch when the forecast was accepted: the data it could have known.
    pub based_on_batch_id: u64,
    pub based_on_root: [u8; 32],
    /// End of that batch's window.
    pub issued_at: i64,
    /// `ts_utc` of the forecast reading; outside every anchored batch at issue time.
    pub target_ts: i64,
    pub horizon_hours: u16,
    /// Quantiles in thousandths of the variable's unit, p10 ≤ p50 ≤ p90.
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: [u8; 32],
    /// Merkle root (RFC 9162) of the features used, to reproduce the forecast.
    pub features_root: [u8; 32],
    pub alert: Option<ForecastAlert>,
    pub slot: u64,
    pub committed_at: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublicAlertRecord {
    pub header: PublicAlertHeader,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct OutlookRecord {
    pub header: OutlookHeader,
    pub based_on_batch_id: u64,
    /// The rule's threshold at recording time and the result: the early warning.
    pub threshold_milli: i64,
    pub triggered: bool,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
}
