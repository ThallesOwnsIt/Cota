use anchor_lang::prelude::*;

use crate::state::Record;

/// Every record the program accepts, with its position and leaf hash. Anyone can rebuild the whole
/// accumulator from these events and must reach the peaks stored in `Registry`.
#[event]
pub struct RecordAppended {
    pub index: u64,
    pub leaf: [u8; 32],
    pub record: Record,
}

#[event]
pub struct RegistryInitialized {
    pub authority: Pubkey,
    pub members: Vec<Pubkey>,
    pub threshold: u8,
}

#[event]
pub struct MemberRotated {
    pub index: u8,
    pub old: Pubkey,
    pub new: Pubkey,
}

#[event]
pub struct AuthorityChanged {
    pub old: Pubkey,
    pub new: Pubkey,
}

#[event]
pub struct AlertRuleSet {
    pub station_id: String,
    pub threshold_milli: i64,
    pub level: u8,
}

#[event]
pub struct OutlookRuleSet {
    pub region: String,
    pub threshold_milli: i64,
    pub window_secs: i64,
    pub min_models: u8,
    pub rule_sha256: [u8; 32],
}

#[event]
pub struct VaultCreated {
    pub vault: Pubkey,
    pub creator: Pubkey,
    pub beneficiary: Pubkey,
    pub kind: u8,
    pub subject: String,
    pub payout_lamports: u64,
    pub cooldown_secs: i64,
    pub accept_from: i64,
    pub expires_at: i64,
}

#[event]
pub struct PayoutReleased {
    pub vault: Pubkey,
    pub trigger_leaf: [u8; 32],
    pub trigger_index: u64,
    pub beneficiary: Pubkey,
    pub amount: u64,
    pub event_at: i64,
}

#[event]
pub struct VaultClosed {
    pub vault: Pubkey,
    pub returned: u64,
}
