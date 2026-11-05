//! Forecast rain for a region, attested by the committee (early-warning layer).

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use crate::merkle::Hash;

/// Domain prefix: an outlook signature is valid neither as a batch nor as a warning.
pub const OUTLOOK_DOMAIN: &[u8] = b"COTA-OUTLOOK-v1";

/// What the nodes sign. Borsh layout identical to `cota::OutlookHeader` in the program.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct OutlookHeader {
    pub region: String,
    /// End of day D (UTC), which must be the end of the latest anchored batch.
    pub issued_at: i64,
    pub window_end: i64,
    pub models: u8,
    /// Rain accumulated over the window, averaged over the points, in thousandths of a mm (mean, minimum and
    /// maximum across the models).
    pub rain_mean_milli: i64,
    pub rain_min_milli: i64,
    pub rain_max_milli: i64,
    /// SHA-256 of the canonical content (see `public_alert::content_hash`).
    #[serde(with = "crate::reading::hex32")]
    pub content_sha256: Hash,
}

impl OutlookHeader {
    /// Exact bytes each node signs with ed25519 for program v1: `OUTLOOK_DOMAIN || borsh(header)`.
    pub fn signing_message(&self) -> Vec<u8> {
        self.signing_message_for(crate::Protocol::V1)
    }

    /// Same, under the signing domain of `protocol`.
    pub fn signing_message_for(&self, protocol: crate::Protocol) -> Vec<u8> {
        let mut msg = protocol.outlook_domain().to_vec();
        msg.extend(borsh::to_vec(self).expect("Borsh de OutlookHeader não falha"));
        msg
    }
}
