//! Catálogo de estações (`config/stations.toml`).

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
    /// Referência vertical, só para nível de rio.
    #[serde(default)]
    pub datum_id: Option<String>,
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
