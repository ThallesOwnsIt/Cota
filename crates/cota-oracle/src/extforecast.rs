//! Third-party forecasts attested by the committee, until program v3 has an on-chain record for them (Etapa 3).
//!
//! Each node downloads the same forecast and signs its canonical content; the aggregator keeps in the ledger
//! (`data/ledger/external_forecasts/<source>/<day>.json`) the version the most members signed, saying whether
//! that reached the committee's threshold. Until v3 the ledger copy is what proves it: it is stamped in Bitcoin
//! (OpenTimestamps) and copied to Arweave, and the signatures can be checked against the committee on-chain.
//!
//! First source: GloFAS river discharge (Copernicus) through Open-Meteo's flood API, the ensemble's median,
//! quartiles and range for the 30 days after the day that has just closed, at the catalog's GloFAS points. The API's
//! response carries a timing field that changes on every request; only the forecast itself is signed, as integers
//! (thousandths of m³/s), so every node reaches the same bytes.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate};
use cota_core::{public_alert::content_hash, Source};
use cota_ingest::config::StationConfig;
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};

use crate::{attest::Attestation, keys::pubkey_base58};

/// Signing domain of third-party forecasts: such a signature can never be taken for a batch, a warning or an outlook.
pub const DOMAIN: &[u8] = b"COTA-EXTFC-v1";
pub const GLOFAS: &str = "glofas";
pub const DAYS_AHEAD: u64 = 30;
pub const FLOOD_API: &str = "https://flood-api.open-meteo.com/v1/flood";
const FIELDS: [&str; 5] = ["river_discharge_median", "river_discharge_p25", "river_discharge_p75", "river_discharge_min", "river_discharge_max"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub source: String,
    /// The day that has just closed (UTC); the forecast covers the days after it.
    pub day: NaiveDate,
    /// SHA-256 (hex) of `content`.
    pub content_sha256: String,
}

impl Header {
    pub fn message(&self) -> Vec<u8> {
        let mut m = DOMAIN.to_vec();
        for part in [self.source.as_bytes(), self.day.to_string().as_bytes(), self.content_sha256.as_bytes()] {
            m.push(0);
            m.extend_from_slice(part);
        }
        m
    }
}

/// One day of the ensemble at one point, in thousandths of m³/s (`None` where the API has no value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayForecast {
    pub date: String,
    pub median: Option<i64>,
    pub p25: Option<i64>,
    pub p75: Option<i64>,
    pub min: Option<i64>,
    pub max: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointForecast {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub days: Vec<DayForecast>,
}

/// The signed content. Serialized with fields in declaration order, so the same forecast gives the same bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub source: String,
    pub provider: String,
    pub day: NaiveDate,
    pub unit: String,
    pub points: Vec<PointForecast>,
}

#[derive(Deserialize)]
struct FloodResponse {
    daily: BTreeMap<String, serde_json::Value>,
}

fn milli(v: &serde_json::Value) -> Option<i64> {
    v.as_f64().map(|x| (x * 1000.0).round() as i64)
}

/// The forecast at one point, from the API's JSON response.
pub fn parse_point(station: &StationConfig, body: &str) -> Result<PointForecast> {
    let resp: FloodResponse = serde_json::from_str(body).context("resposta inválida da API de cheias")?;
    let times = resp.daily.get("time").and_then(|t| t.as_array()).context("resposta sem datas")?;
    let col = |name: &str| resp.daily.get(name).and_then(|c| c.as_array()).cloned().unwrap_or_default();
    let cols: Vec<Vec<serde_json::Value>> = FIELDS.iter().map(|f| col(f)).collect();
    let at = |c: usize, i: usize| cols[c].get(i).and_then(milli);
    let days = times
        .iter()
        .enumerate()
        .map(|(i, t)| DayForecast {
            date: t.as_str().unwrap_or_default().to_string(),
            median: at(0, i),
            p25: at(1, i),
            p75: at(2, i),
            min: at(3, i),
            max: at(4, i),
        })
        .collect::<Vec<_>>();
    if days.iter().all(|d| d.median.is_none()) {
        bail!("{}: previsão sem valores", station.id);
    }
    Ok(PointForecast { id: station.id.clone(), lat: station.lat, lon: station.lon, days })
}

/// Downloads the GloFAS ensemble for the 30 days after `day` at every GloFAS point of the catalog.
pub fn fetch_glofas(http: &reqwest::blocking::Client, catalog: &[StationConfig], day: NaiveDate) -> Result<Content> {
    let (first, last) = (day + Days::new(1), day + Days::new(DAYS_AHEAD));
    let mut points = Vec::new();
    for st in catalog.iter().filter(|s| s.source == Source::OpenMeteoGlofas) {
        let url = format!(
            "{FLOOD_API}?latitude={}&longitude={}&daily={}&start_date={first}&end_date={last}",
            st.lat,
            st.lon,
            FIELDS.join(",")
        );
        let body = http.get(&url).send().and_then(|r| r.error_for_status()).and_then(|r| r.text()).with_context(|| format!("previsão GloFAS de {}", st.id))?;
        points.push(parse_point(st, &body)?);
    }
    if points.is_empty() {
        bail!("nenhum ponto GloFAS no catálogo");
    }
    Ok(Content {
        source: GLOFAS.into(),
        provider: "GloFAS (Copernicus EMS) via Open-Meteo flood API, CC BY 4.0".into(),
        day,
        unit: "milli m3/s".into(),
        points,
    })
}

/// What a node publishes about one forecast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedForecast {
    pub node: String,
    pub header: Header,
    #[serde(with = "crate::node::hex64")]
    pub signature: [u8; 64],
    /// Canonical content; `header.content_sha256` is the hash of these bytes.
    pub content: String,
    /// When the node fetched it (local clock, informational).
    pub seen_at: i64,
}

pub fn sign(key: &SigningKey, content: &Content, seen_at: i64) -> Result<SignedForecast> {
    let text = serde_json::to_string(content)?;
    let header = Header { source: content.source.clone(), day: content.day, content_sha256: hex::encode(content_hash(&text)) };
    Ok(SignedForecast { node: pubkey_base58(key), signature: key.sign(&header.message()).to_bytes(), header, content: text, seen_at })
}

impl Attestation for SignedForecast {
    type Header = Header;
    fn node(&self) -> &str {
        &self.node
    }
    fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
    fn header(&self) -> &Header {
        &self.header
    }
    fn message(&self) -> Vec<u8> {
        self.header.message()
    }
    fn content_matches(&self) -> bool {
        hex::encode(content_hash(&self.content)) == self.header.content_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::select;
    use cota_core::Variable;

    fn station() -> StationConfig {
        StationConfig {
            id: "GLOFAS-GUAIBA".into(),
            name: "Guaíba".into(),
            source: Source::OpenMeteoGlofas,
            variable: Variable::RiverDischarge,
            lat: -30.075,
            lon: -51.325,
            datum_id: None,
            stream: None,
            kind: None,
        }
    }

    // A response like the API's, including the timing field that changes on every request.
    fn body(generation_ms: f64) -> String {
        format!(
            r#"{{"latitude":-30.075,"longitude":-51.325,"generationtime_ms":{generation_ms},"daily":{{"time":["2026-11-13","2026-11-14"],
            "river_discharge_median":[2810.9,2700.12],"river_discharge_p25":[2810.9,2650.0],"river_discharge_p75":[2810.9,2750.5],
            "river_discharge_min":[2800.0,2600.0],"river_discharge_max":[2817.38,2900.0]}}}}"#
        )
    }

    fn content(generation_ms: f64) -> Content {
        Content {
            source: GLOFAS.into(),
            provider: "test".into(),
            day: NaiveDate::from_ymd_opt(2026, 11, 12).unwrap(),
            unit: "milli m3/s".into(),
            points: vec![parse_point(&station(), &body(generation_ms)).unwrap()],
        }
    }

    #[test]
    fn the_timing_field_does_not_change_the_signed_content() {
        assert_eq!(serde_json::to_string(&content(0.31)).unwrap(), serde_json::to_string(&content(17.9)).unwrap());
        let d = &content(0.31).points[0].days[0];
        assert_eq!((d.median, d.max), (Some(2_810_900), Some(2_817_380)));
    }

    #[test]
    fn two_members_signing_the_same_forecast_reach_the_threshold() {
        let keys: Vec<SigningKey> = (1u8..=3).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let members: Vec<[u8; 32]> = keys.iter().map(|k| k.verifying_key().to_bytes()).collect();
        let subs: Vec<SignedForecast> = keys[..2].iter().map(|k| sign(k, &content(1.0), 0).unwrap()).collect();
        let groups = select(&members, 2, &subs).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].signatures.len(), 2);
    }

    #[test]
    fn a_tampered_content_or_another_domain_is_refused() {
        let k = SigningKey::from_bytes(&[7; 32]);
        let members = [k.verifying_key().to_bytes()];
        let mut s = sign(&k, &content(1.0), 0).unwrap();
        s.content = s.content.replace("2810900", "9810900");
        assert!(select(&members, 1, &[s]).unwrap().is_empty());
        // A signature over the same bytes without the domain (as another kind of message) does not verify.
        let mut s = sign(&k, &content(1.0), 0).unwrap();
        s.signature = k.sign(s.header.content_sha256.as_bytes()).to_bytes();
        assert!(select(&members, 1, &[s]).unwrap().is_empty());
    }
}
