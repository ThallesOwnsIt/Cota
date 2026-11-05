use anchor_lang::prelude::*;

#[constant]
pub const REGISTRY_SEED: &[u8] = b"registry";

#[constant]
pub const ALERT_RULE_SEED: &[u8] = b"alert_rule";

#[constant]
pub const OUTLOOK_RULE_SEED: &[u8] = b"outlook_rule";

#[constant]
pub const VAULT_SEED: &[u8] = b"vault";

#[constant]
pub const PAYOUT_SEED: &[u8] = b"payout";

/// Signing domains of v2. Different from v1's, so a committee signature made for program v1 is never
/// valid here (and vice versa). Same values as `cota_core::Protocol::V2`.
#[constant]
pub const BATCH_DOMAIN: &[u8] = b"COTA-BATCH-v2";
#[constant]
pub const PUBLIC_ALERT_DOMAIN: &[u8] = b"COTA-PUBALERT-v2";
#[constant]
pub const OUTLOOK_DOMAIN: &[u8] = b"COTA-OUTLOOK-v2";

/// Maximum committee size; the signer bitmap is a `u8`.
pub const MAX_MEMBERS: usize = 8;
/// Heights of the accumulator: up to 2^32 − 1 records in a fixed-size account.
pub const MAX_PEAKS: usize = 32;

pub const MAX_STATION_ID_LEN: usize = 32;
pub const MAX_STATION_NAME_LEN: usize = 64;
pub const MAX_DATUM_ID_LEN: usize = 24;
pub const MAX_ISSUER_LEN: usize = 16;
pub const MAX_ALERT_ID_LEN: usize = 64;
pub const MAX_REGION_LEN: usize = 16;

/// `Degradation` reasons (same values as `cota_quality::Reason`).
pub const MAX_DEGRADATION_REASON: u8 = 4;

/// Vault triggers: early warning (an `Outlook` record with `triggered`, subject = region)
/// or flow-model alert (a `Forecast` record with `alert`, subject = station).
pub const TRIGGER_EARLY_WARNING: u8 = 0;
pub const TRIGGER_FLOW_ALERT: u8 = 1;
