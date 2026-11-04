//! Official warning (e.g. INMET) attested by the committee.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::merkle::Hash;

/// Domain prefix of the signed message: a warning signature is not valid as a batch, and vice versa.
pub const PUBLIC_ALERT_DOMAIN: &[u8] = b"COTA-PUBALERT-v1";

/// What the nodes sign. Borsh layout identical to `cota::PublicAlertHeader` in the program.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct PublicAlertHeader {
    pub issuer: String,
    /// Issuer's identifier (for INMET, the CAP `codigo` URN, which includes the sequence number).
    pub alert_id: String,
    /// SHA-256 of the canonical content (see [`content_hash`]).
    #[serde(with = "crate::reading::hex32")]
    pub content_sha256: Hash,
    /// 1 perigo potencial (amarelo), 2 perigo (laranja), 3 grande perigo (vermelho); 0 outro.
    pub severity: u8,
    /// Validity period declared by the issuer, in UTC seconds.
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

/// SHA-256 of the exact bytes of the canonical content (which are kept in the ledger).
pub fn content_hash(canonical: &str) -> Hash {
    Sha256::digest(canonical.as_bytes()).into()
}
