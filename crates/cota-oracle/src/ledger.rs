//! Registro local de cada lote ancorado: o que o dashboard lê junto com a cadeia.

use std::path::Path;

use anyhow::Result;
use chrono::NaiveDate;
use cota_core::{BatchHeader, Reading};
use serde::{Deserialize, Serialize};

use crate::aggregate::{Aggregate, MemberReport};
use crate::quality::Outcome;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSignature {
    pub member: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerEntry<'a> {
    pub batch_id: u64,
    pub day: NaiveDate,
    pub header: &'a BatchHeader,
    pub batch_account: String,
    pub tx_signature: String,
    pub cluster: String,
    pub explorer: String,
    pub members: &'a [MemberReport],
    pub signatures: Vec<LedgerSignature>,
    pub quality: &'a Outcome,
    pub readings: &'a [Reading],
}

/// Grava `{dir}/batches/{batch_id:05}.json`.
pub fn write_entry(
    dir: &Path,
    agg: &Aggregate,
    batch_account: String,
    tx_signature: String,
    cluster: &str,
    quality: &Outcome,
) -> Result<()> {
    let entry = LedgerEntry {
        batch_id: agg.header.batch_id,
        day: agg.batch.day,
        header: &agg.header,
        batch_account,
        explorer: crate::rpc::explorer_tx(&tx_signature, cluster),
        tx_signature,
        cluster: cluster.to_string(),
        members: &agg.members,
        signatures: agg
            .signatures
            .iter()
            .map(|(pk, sig)| LedgerSignature { member: bs58::encode(pk).into_string(), signature: hex::encode(sig) })
            .collect(),
        quality,
        readings: &agg.batch.readings,
    };
    let dir = dir.join("batches");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{:05}.json", agg.header.batch_id)), serde_json::to_string_pretty(&entry)?)?;
    Ok(())
}
