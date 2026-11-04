//! Cota's shared core: canonical reading, signable batch and Merkle tree.
//!
//! Everything here is deterministic: the same readings produce the same bytes, the same
//! root and the same signed message on any node.

pub mod batch;
pub mod merkle;
pub mod outlook;
pub mod precompile;
pub mod public_alert;
pub mod reading;

pub use batch::{Batch, BatchError, BatchHeader, BATCH_DOMAIN};
pub use merkle::Hash;
pub use outlook::{OutlookHeader, OUTLOOK_DOMAIN};
pub use public_alert::{PublicAlertHeader, PUBLIC_ALERT_DOMAIN};
pub use reading::{Reading, Source, Unit, Variable};
