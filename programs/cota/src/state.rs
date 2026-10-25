use anchor_lang::prelude::*;

use crate::constants::*;

/// Comitê de oráculos. Singleton (PDA `["committee"]`).
#[account]
#[derive(InitSpace)]
pub struct Committee {
    /// Quem pode registrar estações e trocar membros. Não pode ancorar lotes sozinho.
    pub authority: Pubkey,
    #[max_len(MAX_MEMBERS)]
    pub members: Vec<Pubkey>,
    /// Assinaturas distintas exigidas por lote (k de n).
    pub threshold: u8,
    /// Próximo `batch_id` aceito (começa em 1).
    pub next_batch_id: u64,
    /// Raiz Merkle do último lote; zeros antes do primeiro.
    pub last_batch_root: [u8; 32],
    /// Fim da janela do último lote; o próximo não pode começar antes.
    pub last_t_end: i64,
    pub bump: u8,
}

/// Estação ou ponto de grade monitorado. PDA `["station", station_id]`.
#[account]
#[derive(InitSpace)]
pub struct Station {
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    #[max_len(MAX_STATION_NAME_LEN)]
    pub name: String,
    /// Mesmo discriminante de `cota_core::Source`.
    pub source: u8,
    /// Mesmo discriminante de `cota_core::Variable`.
    pub variable: u8,
    /// Coordenadas efetivamente usadas, em microgrus.
    pub lat_e6: i32,
    pub lon_e6: i32,
    #[max_len(MAX_DATUM_ID_LEN)]
    pub datum_id: Option<String>,
    pub registered_at: i64,
    pub bump: u8,
}

/// Lote ancorado. PDA `["batch", batch_id (u64 LE)]`; `init` impede reescrever um lote.
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
    /// Slot e horário de consenso em que o lote foi aceito: o "quando" auditável.
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// Cabeçalho que os oráculos assinam. Layout Borsh idêntico a `cota_core::BatchHeader`
/// (há um teste que compara os bytes).
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

/// Previsão quantílica registrada antes de a observação existir.
/// PDA `["forecast", station_id, based_on_batch_id (u64 LE), target_ts (i64 LE)]`.
#[account]
#[derive(InitSpace)]
pub struct ForecastCommitment {
    #[max_len(MAX_STATION_ID_LEN)]
    pub station_id: String,
    pub variable: u8,
    /// Último lote ancorado quando a previsão foi aceita: os dados que ela podia conhecer.
    pub based_on_batch_id: u64,
    pub based_on_root: [u8; 32],
    /// Instante de emissão no tempo do replay (= fim da janela de `based_on_batch_id`).
    pub issued_at: i64,
    /// `ts_utc` da leitura prevista; estava fora de qualquer lote ancorado na emissão.
    pub target_ts: i64,
    pub horizon_hours: u16,
    /// Quantis em milésimos da unidade da variável, com p10 ≤ p50 ≤ p90.
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: [u8; 32],
    /// Raiz Merkle (RFC 9162) das features usadas, para reproduzir a previsão.
    pub features_root: [u8; 32],
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}

/// Limiar de alerta de uma estação. Imutável depois de criado. PDA `["alert_rule", station_id]`.
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

/// Recibo de alerta: a P90 de uma previsão já ancorada cruzou o limiar da estação
/// antes de a observação existir. PDA `["alert", forecast]`.
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

/// O que os oráculos assinam ao atestar um aviso oficial (ex.: INMET). O conteúdo completo
/// fica fora da cadeia; `content_sha256` é o hash do JSON canônico dele.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PublicAlertHeader {
    pub issuer: String,
    pub alert_id: String,
    pub content_sha256: [u8; 32],
    /// 1 perigo potencial (amarelo), 2 perigo (laranja), 3 grande perigo (vermelho); 0 outro.
    pub severity: u8,
    /// Vigência declarada pelo emissor, em segundos UTC.
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

/// Aviso oficial atestado pelo comitê: a versão com este conteúdo existia, no máximo,
/// no slot `slot`. Imutável. PDA `["public_alert", content_sha256]`.
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

/// Regra do alerta antecipado de uma região. Imutável. PDA `["outlook_rule", region]`.
#[account]
#[derive(InitSpace)]
pub struct OutlookRule {
    #[max_len(MAX_REGION_LEN)]
    pub region: String,
    /// Chuva média prevista na janela (milésimos de mm) a partir da qual o alerta dispara.
    pub threshold_milli: i64,
    pub window_secs: i64,
    pub min_models: u8,
    /// SHA-256 da especificação completa (pontos, modelos, calibração), publicada fora da cadeia.
    pub rule_sha256: [u8; 32],
    pub set_at: i64,
    pub bump: u8,
}

/// O que os oráculos assinam sobre a chuva prevista de uma região. O conteúdo completo (por
/// modelo, ponto e dia) fica fora da cadeia; `content_sha256` é o hash do JSON canônico.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct OutlookHeader {
    pub region: String,
    /// Fim do último dia ancorado: a perspectiva só usa o que existia até ali.
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    /// Chuva acumulada na janela, média dos pontos, em milésimos de mm: média, mínimo e
    /// máximo entre os modelos.
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

/// Chuva prevista atestada pelo comitê e avaliada pela regra no próprio programa.
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
    /// Limiar da regra no momento do registro e o resultado: o alerta antecipado.
    pub threshold_milli: i64,
    pub triggered: bool,
    pub signers: u8,
    pub signer_count: u8,
    pub slot: u64,
    pub committed_at: i64,
    pub submitter: Pubkey,
    pub bump: u8,
}
