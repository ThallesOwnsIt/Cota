use anchor_lang::prelude::*;

#[event]
pub struct CommitteeInitialized {
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
pub struct StationRegistered {
    pub station_id: String,
    pub source: u8,
    pub variable: u8,
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub datum_id: Option<String>,
}

#[event]
pub struct BatchCommitted {
    pub batch_id: u64,
    pub merkle_root: [u8; 32],
    pub leaf_count: u32,
    pub t_start: i64,
    pub t_end: i64,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
}

/// Lote aceito pelo limiar, mas com membros do comitê que não assinaram (nó mudo ou discordante).
#[event]
pub struct OracleAbsent {
    pub batch_id: u64,
    /// Bit i ligado = `committee.members[i]` não assinou.
    pub missing: u8,
}

/// Uma fonte perdeu confiança no lote `batch_id` (índice C_s abaixo do mínimo).
#[event]
pub struct SourceDegraded {
    pub batch_id: u64,
    pub station_id: String,
    /// Índice de confiança C_s × 1000.
    pub score_milli: u16,
    /// 0 indisponível, 1 travado, 2 salto, 3 discordância entre nós, 4 sem dados recentes.
    pub reason: u8,
}

#[event]
pub struct ForecastCommitted {
    pub forecast: Pubkey,
    pub station_id: String,
    pub based_on_batch_id: u64,
    pub issued_at: i64,
    pub target_ts: i64,
    pub horizon_hours: u16,
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: [u8; 32],
    pub features_root: [u8; 32],
}

#[event]
pub struct AlertRuleSet {
    pub station_id: String,
    pub threshold_milli: i64,
    pub level: u8,
}

#[event]
pub struct AlertIssued {
    pub alert: Pubkey,
    pub forecast: Pubkey,
    pub station_id: String,
    pub target_ts: i64,
    pub p90: i64,
    pub threshold_milli: i64,
    pub level: u8,
}

#[event]
pub struct PublicAlertRecorded {
    pub record: Pubkey,
    pub issuer: String,
    pub alert_id: String,
    pub content_sha256: [u8; 32],
    pub severity: u8,
    pub onset: i64,
    pub expires: i64,
    pub signers: u8,
    pub slot: u64,
}

#[event]
pub struct AuthorityChanged {
    pub old: Pubkey,
    pub new: Pubkey,
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
pub struct OutlookCommitted {
    pub outlook: Pubkey,
    pub region: String,
    pub issued_at: i64,
    pub window_end: i64,
    pub rain_mean_milli: i64,
    pub content_sha256: [u8; 32],
    pub signers: u8,
}

/// A chuva prevista cruzou o limiar da regra: alerta antecipado.
#[event]
pub struct EarlyWarning {
    pub outlook: Pubkey,
    pub region: String,
    pub issued_at: i64,
    pub window_end: i64,
    pub rain_mean_milli: i64,
    pub threshold_milli: i64,
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
    pub trigger: Pubkey,
    pub beneficiary: Pubkey,
    pub amount: u64,
    pub event_at: i64,
}

#[event]
pub struct VaultClosed {
    pub vault: Pubkey,
    pub returned: u64,
}
