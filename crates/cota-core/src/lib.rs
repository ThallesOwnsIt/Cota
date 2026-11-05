//! Cota's shared core: canonical reading, signable batch and Merkle tree.
//!
//! Everything here is deterministic: the same readings produce the same bytes, the same
//! root and the same signed message on any node.

pub mod batch;
pub mod merkle;
pub mod mmr;
pub mod outlook;
pub mod precompile;
pub mod public_alert;
pub mod reading;

pub use batch::{Batch, BatchError, BatchHeader, BATCH_DOMAIN};
pub use merkle::Hash;
pub use outlook::{OutlookHeader, OUTLOOK_DOMAIN};
pub use public_alert::{PublicAlertHeader, PUBLIC_ALERT_DOMAIN};
pub use reading::{Reading, Source, Unit, Variable};

/// Which program a signature is for. Each version signs under its own domains, so a committee signature
/// made for one program can never be replayed against the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// Program `4FCh…` (one account per record), kept as the archive of the 2024 replay.
    #[default]
    V1,
    /// One accumulator (MMR) for every record; see `docs/v2-accumulator.md`.
    V2,
}

impl Protocol {
    pub fn batch_domain(self) -> &'static [u8] {
        match self {
            Protocol::V1 => BATCH_DOMAIN,
            Protocol::V2 => b"COTA-BATCH-v2",
        }
    }
    pub fn outlook_domain(self) -> &'static [u8] {
        match self {
            Protocol::V1 => OUTLOOK_DOMAIN,
            Protocol::V2 => b"COTA-OUTLOOK-v2",
        }
    }
    pub fn public_alert_domain(self) -> &'static [u8] {
        match self {
            Protocol::V1 => PUBLIC_ALERT_DOMAIN,
            Protocol::V2 => b"COTA-PUBALERT-v2",
        }
    }
}
