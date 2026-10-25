//! Chuva prevista para uma região, atestada pelo comitê (camada de alerta antecipado).

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use crate::merkle::Hash;

/// Prefixo de domínio: uma assinatura de perspectiva não vale como lote nem como aviso.
pub const OUTLOOK_DOMAIN: &[u8] = b"COTA-OUTLOOK-v1";

/// O que os nós assinam. Layout Borsh idêntico a `cota::OutlookHeader` no programa.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct OutlookHeader {
    pub region: String,
    /// Fim do dia D (UTC), que precisa ser o fim do último lote ancorado.
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    /// Chuva acumulada na janela, média dos pontos, em milésimos de mm (média, mínimo e
    /// máximo entre os modelos).
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    /// SHA-256 do conteúdo canônico (ver `public_alert::content_hash`).
    #[serde(with = "crate::reading::hex32")]
    pub content_sha256: Hash,
}

impl OutlookHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = OUTLOOK_DOMAIN.to_vec();
        msg.extend(borsh::to_vec(self).expect("Borsh de OutlookHeader não falha"));
        msg
    }
}
