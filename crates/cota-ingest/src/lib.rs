//! Independent ingestion of the public sources, producing canonical `Reading`s.
//!
//! Each oracle node runs this on its own; honest nodes with the same sources
//! reach the same readings and therefore the same Merkle root.

pub mod ana;
pub mod config;
pub mod decimal;
pub mod glofas;
pub mod inmet;

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Days, NaiveDate};
use cota_core::{Reading, Source};
use serde::{Deserialize, Serialize};

use config::StationConfig;

/// Per-station summary, written to the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationReport {
    pub station_id: String,
    pub source: Source,
    pub readings: usize,
    pub missing: usize,
    pub expected: usize,
}

pub fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("cota-oracle/0.1")
        .timeout(std::time::Duration::from_secs(300))
        .build()?)
}

/// Reads every configured station for the window [from, to) in UTC seconds.
///
/// With `lenient`, an INMET station missing from a year's zip counts as missing hours
/// (useful for the training history); without it, it is an error (what the nodes use).
pub fn ingest(
    http: &reqwest::blocking::Client,
    stations: &[StationConfig],
    cache_dir: &Path,
    from: i64,
    to: i64,
    lenient: bool,
) -> Result<(Vec<Reading>, Vec<StationReport>)> {
    let mut all = Vec::new();
    let mut reports = Vec::new();
    let ana_rows = fetch_ana(http, stations, from, to)?;

    for st in stations {
        let (readings, missing, expected) = match st.source {
            Source::Inmet => ingest_inmet(http, st, &cache_dir.join("inmet"), from, to, lenient)?,
            Source::OpenMeteoGlofas => {
                let (start, end) = (date_of(from)?, date_of(to - 1)?);
                let resp = glofas::fetch(http, st, start, end)?;
                let (r, stats) = glofas::to_readings(st, &resp)?;
                (r, stats.missing, stats.days)
            }
            Source::Ana => ana::to_readings(st, &ana_rows, from, to)?,
            other => anyhow::bail!("{}: fonte {other:?} ainda não suportada", st.id),
        };
        reports.push(StationReport {
            station_id: st.id.clone(),
            source: st.source,
            readings: readings.len(),
            missing,
            expected,
        });
        all.extend(readings);
    }
    all.sort();
    Ok((all, reports))
}

/// Rows for all of the catalog's ANA stations, in a single request.
///
/// An ANA failure that persists after the retries does not bring the batch down: the hours
/// count as missing and quality records the station as unavailable, as happened
/// with the Cais Mauá gauge in 2024. If only one node loses ANA, it signs a different root and the
/// other two keep the quorum. Without credentials (or with an empty secret), it is a configuration
/// error.
fn fetch_ana(http: &reqwest::blocking::Client, stations: &[StationConfig], from: i64, to: i64) -> Result<Vec<ana::Row>> {
    let codes = stations.iter().filter(|s| s.source == Source::Ana).map(ana::code_of).collect::<Result<Vec<_>>>()?;
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let cred = ana::credentials()?;
    let (first, last) = ana::local_days(from, to)?;
    match ana::login(http, &cred).and_then(|token| ana::fetch(http, &token, &codes, first, last)) {
        Ok(rows) => Ok(rows),
        Err(e) => {
            eprintln!("aviso: ANA indisponível, estações {codes:?} ficam sem leituras: {e:#}");
            Ok(Vec::new())
        }
    }
}

/// A window may span a year boundary; reads the zip of each year involved.
fn ingest_inmet(
    http: &reqwest::blocking::Client,
    st: &StationConfig,
    cache: &Path,
    from: i64,
    to: i64,
    lenient: bool,
) -> Result<(Vec<Reading>, usize, usize)> {
    let (mut readings, mut missing, mut rows) = (Vec::new(), 0, 0);
    for year in date_of(from)?.year()..=date_of(to - 1)?.year() {
        let zip = inmet::ensure_zip(http, cache, year)?;
        let Some(csv) = inmet::read_station_csv(&zip, &st.id)? else {
            anyhow::ensure!(lenient, "estação {} não encontrada em {}", st.id, zip.display());
            continue;
        };
        let (r, stats) = inmet::parse_csv(&csv, &st.id, from, to).with_context(|| format!("{} em {year}", st.id))?;
        readings.extend(r);
        missing += stats.missing;
        rows += stats.rows_in_window;
    }
    let expected = ((to - from) / 3600) as usize;
    // Hours that don't even appear in the CSV are missing too.
    missing += expected.saturating_sub(rows);
    Ok((readings, missing, expected))
}

fn date_of(ts: i64) -> Result<NaiveDate> {
    DateTime::from_timestamp(ts, 0).map(|d| d.date_naive()).context("timestamp fora do intervalo")
}

/// First instant (00:00 UTC) of the day after `date`.
pub fn end_of_day(date: NaiveDate) -> i64 {
    start_of_day(date.checked_add_days(Days::new(1)).expect("data válida"))
}

pub fn start_of_day(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0).expect("meia-noite existe").and_utc().timestamp()
}
