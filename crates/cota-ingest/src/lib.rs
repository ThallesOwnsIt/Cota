//! Ingestão independente das fontes públicas, produzindo `Reading`s canônicos.
//!
//! Cada nó-oráculo roda isto por conta própria; nós honestos com as mesmas fontes
//! chegam às mesmas leituras e, portanto, à mesma raiz Merkle.

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

/// Resumo por estação, gravado no manifesto.
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

/// Lê todas as estações configuradas para a janela [from, to) em segundos UTC.
///
/// Com `lenient`, uma estação INMET ausente no zip de um ano conta como horas faltantes
/// (útil para o histórico de treino); sem ele, é erro (o que os nós usam).
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

    for st in stations {
        let (readings, missing, expected) = match st.source {
            Source::Inmet => ingest_inmet(http, st, &cache_dir.join("inmet"), from, to, lenient)?,
            Source::OpenMeteoGlofas => {
                let (start, end) = (date_of(from)?, date_of(to - 1)?);
                let resp = glofas::fetch(http, st, start, end)?;
                let (r, stats) = glofas::to_readings(st, &resp)?;
                (r, stats.missing, stats.days)
            }
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

/// Uma janela pode atravessar o ano; lê o zip de cada ano envolvido.
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
    // Horas que nem aparecem no CSV também são faltantes.
    missing += expected.saturating_sub(rows);
    Ok((readings, missing, expected))
}

fn date_of(ts: i64) -> Result<NaiveDate> {
    DateTime::from_timestamp(ts, 0).map(|d| d.date_naive()).context("timestamp fora do intervalo")
}

/// Primeiro instante (00:00 UTC) do dia seguinte a `date`.
pub fn end_of_day(date: NaiveDate) -> i64 {
    start_of_day(date.checked_add_days(Days::new(1)).expect("data válida"))
}

pub fn start_of_day(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0).expect("meia-noite existe").and_utc().timestamp()
}
