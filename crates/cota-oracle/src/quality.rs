//! Qualidade de cada lote ancorado (via `cota-quality`), com estado persistido entre lotes.

use std::path::Path;

use anyhow::{Context, Result};
use cota_core::Source;
use cota_quality::{assess, node_agreement, Dissent, Limits, Reason, State, StationQuality};
use serde::{Deserialize, Serialize};

use crate::aggregate::Aggregate;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DissentReport {
    pub member: String,
    #[serde(flatten)]
    pub dissent: Dissent,
}

/// `SourceDegraded` enviado on-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DegradationReport {
    pub station_id: String,
    pub reason: Reason,
    pub score_milli: u16,
    pub tx_signature: String,
    pub explorer: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub stations: Vec<StationQuality>,
    pub dissents: Vec<DissentReport>,
    pub degradations: Vec<DegradationReport>,
}

/// Compara os nós, avalia cada estação do catálogo e atualiza o estado em `state_path`.
pub fn evaluate(state_path: &Path, limits: &Limits, catalog: &[(String, Source)], agg: &Aggregate) -> Result<Outcome> {
    let mut state: State = match std::fs::read_to_string(state_path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("interpretando {}", state_path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(e) => return Err(e.into()),
    };

    let node_readings: Vec<&[cota_core::Reading]> = agg.valid.iter().map(|b| b.readings.as_slice()).collect();
    let agreement = node_agreement(&node_readings);
    let stations = assess(&mut state, limits, catalog, agg.header.t_start, agg.header.t_end, &agg.batch.readings, &agreement);

    if let Some(dir) = state_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(state_path, serde_json::to_string_pretty(&state)?)?;

    let dissents = agreement
        .dissents
        .into_iter()
        .map(|d| DissentReport { member: agg.valid[d.node].node.clone(), dissent: d })
        .collect();
    Ok(Outcome { stations, dissents, degradations: vec![] })
}
