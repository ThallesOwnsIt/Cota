//! Aviso oficial (ex.: INMET) atestado pelo comitê.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::merkle::Hash;

/// Prefixo de domínio da mensagem assinada: uma assinatura de aviso não vale como lote, e vice-versa.
pub const PUBLIC_ALERT_DOMAIN: &[u8] = b"COTA-PUBALERT-v1";

/// O que os nós assinam. Layout Borsh idêntico a `cota::PublicAlertHeader` no programa.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct PublicAlertHeader {
    pub issuer: String,
    /// Identificador do emissor (no INMET, o `codigo` URN do CAP, que inclui a sequência).
    pub alert_id: String,
    /// SHA-256 do conteúdo canônico (ver [`content_hash`]).
    #[serde(with = "crate::reading::hex32")]
    pub content_sha256: Hash,
    /// 1 perigo potencial (amarelo), 2 perigo (laranja), 3 grande perigo (vermelho); 0 outro.
    pub severity: u8,
    /// Vigência declarada pelo emissor, em segundos UTC.
    pub onset: i64,
    pub expires: i64,
}

impl PublicAlertHeader {
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = PUBLIC_ALERT_DOMAIN.to_vec();
        msg.extend(borsh::to_vec(self).expect("Borsh de PublicAlertHeader não falha"));
        msg
    }
}

/// SHA-256 dos bytes exatos do conteúdo canônico (que ficam guardados no ledger).
pub fn content_hash(canonical: &str) -> Hash {
    Sha256::digest(canonical.as_bytes()).into()
}
