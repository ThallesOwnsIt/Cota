//! Cota's oracle nodes, k-of-n aggregator and on-chain client.

pub mod aggregate;
pub mod attest;
pub mod chain;
pub mod extforecast;
pub mod inmet_alerts;
pub mod keys;
pub mod ledger;
pub mod ledger3;
pub mod node;
pub mod outlook;
pub mod quality;
pub mod records;
pub mod rpc;

/// Which program the committee signs for. v1 (`4FCh…`) is a frozen archive; see `docs/v2-accumulator.md`.
pub const PROTOCOL: cota_core::Protocol = cota_core::Protocol::V2;
