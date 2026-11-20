//! Station catalog (`config/stations.toml`).

use std::path::Path;

use anyhow::{Context, Result};
use cota_core::{Source, Variable};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct StationConfig {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub variable: Variable,
    pub lat: f64,
    pub lon: f64,
    /// Vertical reference, river level only.
    #[serde(default)]
    pub datum_id: Option<String>,
    /// The ledger stream (region) whose daily batch carries the station; none: the main stream.
    #[serde(default)]
    pub stream: Option<String>,
    /// River gauges: `rio`, `jusante` (right below a dam) or `reservatorio` (the dam's lake). Informative: the
    /// station's name in the ledger says it in words; the page shows it.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Catalog {
    station: Vec<StationConfig>,
}

pub fn load(path: &Path) -> Result<Vec<StationConfig>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?;
    let catalog: Catalog = toml::from_str(&text).with_context(|| format!("interpretando {}", path.display()))?;
    Ok(catalog.station)
}
