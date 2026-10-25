//! Open-Meteo Flood API: vazão diária modelada pelo GloFAS (sem chave, CC BY 4.0).
//!
//! Atribuição obrigatória: "Weather data by Open-Meteo.com". A vazão é **modelada**
//! (grade de ~5 km), não medida em régua.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use cota_core::{Reading, Source, Variable};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::StationConfig;
use crate::decimal::parse_milli;

pub const API_URL: &str = "https://flood-api.open-meteo.com/v1/flood";
/// Tolerância entre a coordenada configurada e o centro de célula devolvido.
const CELL_TOLERANCE_DEG: f64 = 0.001;

#[derive(Debug, Deserialize)]
pub struct FloodResponse {
    pub latitude: f64,
    pub longitude: f64,
    pub daily: Daily,
}

#[derive(Debug, Deserialize)]
pub struct Daily {
    pub time: Vec<String>,
    pub river_discharge: Vec<Option<f64>>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FetchStats {
    pub days: usize,
    pub missing: usize,
}

/// Baixa a série diária de `start` a `end` (inclusive) para a célula da estação.
pub fn fetch(http: &reqwest::blocking::Client, station: &StationConfig, start: NaiveDate, end: NaiveDate) -> Result<FloodResponse> {
    let url = format!(
        "{API_URL}?latitude={}&longitude={}&daily=river_discharge&start_date={start}&end_date={end}",
        station.lat, station.lon
    );
    http.get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("consultando {url}"))?
        .json()
        .with_context(|| format!("resposta inválida de {url}"))
}

/// String canônica cujo SHA-256 vira `raw_payload_sha256`. O corpo JSON da API não serve,
/// porque muda a cada requisição (`generationtime_ms`).
pub fn canonical_payload(lat: f64, lon: f64, date: &str, value_milli: i64) -> String {
    format!("open-meteo-flood|{lat:.3}|{lon:.3}|{date}|{value_milli}")
}

pub fn to_readings(station: &StationConfig, resp: &FloodResponse) -> Result<(Vec<Reading>, FetchStats)> {
    if (resp.latitude - station.lat).abs() > CELL_TOLERANCE_DEG || (resp.longitude - station.lon).abs() > CELL_TOLERANCE_DEG {
        bail!(
            "{}: API devolveu a célula ({}, {}), configurada ({}, {})",
            station.id, resp.latitude, resp.longitude, station.lat, station.lon
        );
    }
    if resp.daily.time.len() != resp.daily.river_discharge.len() {
        bail!("{}: séries de tempo e vazão com tamanhos diferentes", station.id);
    }

    let mut readings = Vec::new();
    let mut stats = FetchStats::default();
    for (date, q) in resp.daily.time.iter().zip(&resp.daily.river_discharge) {
        stats.days += 1;
        let Some(q) = q else {
            stats.missing += 1;
            continue;
        };
        let ts = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .with_context(|| format!("data inválida {date:?}"))?
            .and_hms_opt(0, 0, 0)
            .expect("meia-noite existe")
            .and_utc()
            .timestamp();
        // Formatar com 3 casas e reler evita depender de arredondamento de f64 entre linguagens.
        let value_milli = parse_milli(&format!("{q:.3}")).context("vazão fora do intervalo")?;
        let payload = canonical_payload(resp.latitude, resp.longitude, date, value_milli);
        readings.push(Reading {
            station_id: station.id.clone(),
            variable: Variable::RiverDischarge,
            ts_utc: ts,
            source: Source::OpenMeteoGlofas,
            value_milli,
            datum_id: None,
            raw_payload_sha256: Sha256::digest(payload.as_bytes()).into(),
        });
    }
    Ok((readings, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station() -> StationConfig {
        StationConfig {
            id: "GLOFAS-GUAIBA".into(),
            name: "Guaíba".into(),
            source: Source::OpenMeteoGlofas,
            variable: Variable::RiverDischarge,
            lat: -30.075,
            lon: -51.325,
            datum_id: None,
        }
    }

    fn response(lat: f64) -> FloodResponse {
        serde_json::from_str(&format!(
            r#"{{"latitude":{lat},"longitude":-51.325,"generationtime_ms":0.3,
               "daily":{{"time":["2024-05-05","2024-05-06"],"river_discharge":[15558.65,null]}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn converts_daily_values_and_skips_nulls() {
        let (r, stats) = to_readings(&station(), &response(-30.075)).unwrap();
        assert_eq!((stats.days, stats.missing), (2, 1));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].ts_utc, 1_714_867_200); // 2024-05-05T00:00:00Z
        assert_eq!(r[0].value_milli, 15_558_650);
        let expected: [u8; 32] = Sha256::digest(b"open-meteo-flood|-30.075|-51.325|2024-05-05|15558650").into();
        assert_eq!(r[0].raw_payload_sha256, expected);
    }

    #[test]
    fn rejects_unexpected_cell() {
        assert!(to_readings(&station(), &response(-30.025)).is_err());
    }
}
