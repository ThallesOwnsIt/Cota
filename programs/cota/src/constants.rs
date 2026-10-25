use anchor_lang::prelude::*;

#[constant]
pub const COMMITTEE_SEED: &[u8] = b"committee";

#[constant]
pub const STATION_SEED: &[u8] = b"station";

#[constant]
pub const BATCH_SEED: &[u8] = b"batch";

/// Prefixo da mensagem que os oráculos assinam. Igual a `cota_core::BATCH_DOMAIN`.
#[constant]
pub const BATCH_DOMAIN: &[u8] = b"COTA-BATCH-v1";

/// Limite de membros do comitê; o bitmap de assinantes é um `u8`.
pub const MAX_MEMBERS: usize = 8;

pub const MAX_STATION_ID_LEN: usize = 32;
pub const MAX_STATION_NAME_LEN: usize = 64;
pub const MAX_DATUM_ID_LEN: usize = 24;

#[constant]
pub const FORECAST_SEED: &[u8] = b"forecast";

#[constant]
pub const ALERT_RULE_SEED: &[u8] = b"alert_rule";

#[constant]
pub const ALERT_SEED: &[u8] = b"alert";

/// Motivos de `SourceDegraded` (mesmos valores de `cota_quality::Reason`).
pub const MAX_DEGRADATION_REASON: u8 = 4;

#[constant]
pub const PUBLIC_ALERT_SEED: &[u8] = b"public_alert";

/// Prefixo da mensagem que os oráculos assinam ao atestar um aviso oficial.
#[constant]
pub const PUBLIC_ALERT_DOMAIN: &[u8] = b"COTA-PUBALERT-v1";

pub const MAX_ISSUER_LEN: usize = 16;
pub const MAX_ALERT_ID_LEN: usize = 64;

#[constant]
pub const OUTLOOK_RULE_SEED: &[u8] = b"outlook_rule";

#[constant]
pub const OUTLOOK_SEED: &[u8] = b"outlook";

/// Prefixo da mensagem que os oráculos assinam ao atestar a chuva prevista.
#[constant]
pub const OUTLOOK_DOMAIN: &[u8] = b"COTA-OUTLOOK-v1";

pub const MAX_REGION_LEN: usize = 16;

#[constant]
pub const VAULT_SEED: &[u8] = b"vault";

#[constant]
pub const PAYOUT_SEED: &[u8] = b"payout";

/// Gatilhos de cofre: alerta antecipado (`OutlookRecord` com `triggered`, assunto = região)
/// ou alerta do modelo de vazão (`AlertReceipt`, assunto = estação).
pub const TRIGGER_EARLY_WARNING: u8 = 0;
pub const TRIGGER_FLOW_ALERT: u8 = 1;
