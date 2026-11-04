use anchor_lang::prelude::*;

use crate::constants::*;

/// Oracle committee. Singleton (PDA `["committee"]`).
#[account]
#[derive(InitSpace)]
pub struct Committee {
    /// Who can register stations and replace members. Cannot anchor batches alone.
    pub authority: Pubkey,
    #[max_len(MAX_MEMBERS)]
    pub members: Vec<Pubkey>,
    /// Distinct signatures required per batch (k of n).
    pub threshold: u8,
    /// Next accepted `batch_id` (starts at 1).
    pub next_batch_id: u64,
    /// Merkle root of the latest batch; zeros before the first one.
    pub last_batch_root: [u8; 32],
    /// End of the latest batch's window; the next one cannot start before it.
    pub last_t_end: i64,
    pub bump: u8,
}

/// Monitored station or grid point. PDA `["station", station_id]`.
#[account]
#[derive(InitSpace)]
pub struct Station {
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    #[max_len(MAX_STATION_NAME_LEN)]
    pub name: String,
    /// Same discriminant as `cota_core::Source`.
    pub source: u8,
    /// Same discriminant as `cota_core::Variable`.
    pub variable: u8,
    /// Coordinates actually used, in microdegrees.
    pub lat_e6: i32,
    pub lon_e6: i32,
    #[max_len(MAX_DATUM_ID_LEN)]
    pub datum_id: Option<String>,
    pub registered_at: i64,
    pub bump: u8,
}

/// Anchored batch. PDA `["batch", batch_id (u64 LE)]`; `init` prevents rewriting a batch.
#[account]
#[derive(InitSpace)]
pub struct BatchCommitment {
    pub batch_id: u64,
    pub t_start: i64,
    pub t_end: i64,
    pub leaf_count: u32,
    pub merkle_root: [u8; 32],
    pub station_set_hash: [u8; 32],
    pub prev_batch_root: [u8; 32],
    /// Bit i ligado = `committee.members[i]` assinou.
    pub signers: u8,
    pub signer_count: u8,
    /// Slot and consensus time at which the batch was accepted: the auditable "when".
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// Header the oracles sign. Borsh layout identical to `cota_core::BatchHeader`
/// (a test compares the bytes).
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

impl BatchHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = BATCH_DOMAIN.to_vec();
        self.serialize(&mut msg).expect("serialização de BatchHeader em Vec não falha");
        msg
    }
}

/// Quantile forecast recorded before the observation exists.
/// PDA `["forecast", station_id, based_on_batch_id (u64 LE), target_ts (i64 LE)]`.
#[account]
#[derive(InitSpace)]
pub struct ForecastCommitment {
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    pub variable: u8,
    /// Latest anchored batch when the forecast was accepted: the data it could have known.
    pub based_on_batch_id: u64,
    pub based_on_root: [u8; 32],
    /// Issue time in replay time (= end of the `based_on_batch_id` window).
    pub issued_at: i64,
    /// `ts_utc` of the forecast reading; it was outside every anchored batch at issue time.
    pub target_ts: i64,
    pub horizon_hours: u16,
    /// Quantiles in thousandths of the variable's unit, with p10 ≤ p50 ≤ p90.
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: [u8; 32],
    /// Merkle root (RFC 9162) of the features used, to reproduce the forecast.
    pub features_root: [u8; 32],
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// A station's alert threshold. Immutable once created. PDA `["alert_rule", station_id]`.
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

/// Alert receipt: the P90 of an already-anchored forecast crossed the station's threshold
/// before the observation existed. PDA `["alert", forecast]`.
#[account]
#[derive(InitSpace)]
pub struct AlertReceipt {
    pub forecast: Pubkey,
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    pub issued_at: i64,
    pub target_ts: i64,
    pub p90: i64,
    pub threshold_milli: i64,
    pub level: u8,
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// What the oracles sign when attesting an official warning (e.g. INMET). The full content
/// stays off-chain; `content_sha256` is the hash of its canonical JSON.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublicAlertHeader {
    pub issuer: String,
    pub alert_id: String,
    pub content_sha256: [u8; 32],
    /// 1 perigo potencial (amarelo), 2 perigo (laranja), 3 grande perigo (vermelho); 0 outro.
    pub severity: u8,
    /// Validity period declared by the issuer, in UTC seconds.
    pub onset: i64,
    pub expires: i64,
}

impl PublicAlertHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = PUBLIC_ALERT_DOMAIN.to_vec();
        self.serialize(&mut msg).expect("serialização de PublicAlertHeader em Vec não falha");
        msg
    }
}

/// Official warning attested by the committee: the version with this content existed by
/// slot `slot` at the latest. Immutable. PDA `["public_alert", content_sha256]`.
#[account]
#[derive(InitSpace)]
pub struct PublicAlertRecord {
    #[max_len(MAX_ISSUER_LEN)]
    pub issuer: String,
    #[max_len(MAX_ALERT_ID_LEN)]
    pub alert_id: String,
    pub content_sha256: [u8; 32],
    pub severity: u8,
    pub onset: i64,
    pub expires: i64,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
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

/// What the oracles sign about a region's forecast rain. The full content (per
/// model, point and day) stays off-chain; `content_sha256` is the hash of the canonical JSON.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct OutlookHeader {
    pub region: String,
    /// End of the latest anchored day: the outlook only uses what existed up to then.
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    /// Rain accumulated over the window, averaged over the points, in thousandths of a mm: mean, minimum and
    /// maximum across the models.
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    pub content_sha256: [u8; 32],
}

impl OutlookHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = OUTLOOK_DOMAIN.to_vec();
        self.serialize(&mut msg).expect("serialização de OutlookHeader em Vec não falha");
        msg
    }
}

/// Forecast rain attested by the committee and evaluated against the rule inside the program.
/// PDA `["outlook", region, issued_at (i64 LE)]`.
#[account]
#[derive(InitSpace)]
pub struct OutlookRecord {
    #[max_len(MAX_REGION_LEN)]
    pub region: String,
    pub based_on_batch_id: u64,
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    pub content_sha256: [u8; 32],
    /// The rule's threshold at recording time and the result: the early warning.
    pub threshold_milli: i64,
    pub triggered: bool,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// Parametric vault: holds SOL and pays `payout_lamports` to the beneficiary when Cota
/// records a trigger, with nobody's approval. PDA `["vault", creator, vault_id (u64 LE)]`.
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

/// Payout receipt: one per (vault, trigger). PDA `["payout", vault, trigger]`.
#[account]
#[derive(InitSpace)]
pub struct Payout {
    pub vault: Pubkey,
    pub trigger: Pubkey,
    pub event_at: i64,
    pub amount: u64,
    pub beneficiary: Pubkey,
    pub slot: u64,
    pub paid_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}
