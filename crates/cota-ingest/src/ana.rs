//! ANA, HidroWebService: series from the telemetric stations (Brazil's national hydrometeorological network).
//!
//! River level **measured on a gauge**, unlike GloFAS discharge, which is modelled.
//! Requires registration: the login (`ANA_IDENTIFICADOR` / `ANA_SENHA`) returns a
//! short-lived token. The token never goes into the ledger, only the values.
//!
//! Row format (`HidroinfoanaSerieTelemetricaAdotada/v2`):
//! `{"codigoestacao":"87450004","Data_Hora_Medicao":"2026-11-03 23:00:00.0",
//!   "Cota_Adotada":"115.00","Chuva_Adotada":"0.20","Vazao_Adotada":null,…}`
//! - Brasília time (UTC−3, no daylight saving since 2019);
//! - stage in centimetres, relative to each station's gauge zero.
//!
//! Only on-the-hour readings are kept: the Cais Mauá gauge reports every hour and
//! the others every 15 min; a single cadence keeps availability comparable across stations.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate, NaiveDateTime, Timelike};
use cota_core::{Reading, Source, Variable};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::StationConfig;
use crate::decimal::parse_milli;

pub const API_URL: &str = "https://www.ana.gov.br/hidrowebservice/EstacoesTelemetricas";
/// Seconds to add to Brasília time to get UTC.
const BRT_TO_UTC: i64 = 3 * 3600;
/// The API accepts at most 10 stations per request.
const MAX_CODES: usize = 10;
/// Attempts per request and the wait between them: the 3 nodes query at the same time with the
/// same account, and a node that loses ANA signs a different root and weakens the quorum.
const ATTEMPTS: u32 = 3;
const RETRY_WAIT_SECS: u64 = 15;
const ID_PREFIX: &str = "ANA-";

#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub codigoestacao: String,
    #[serde(rename = "Data_Hora_Medicao")]
    pub data_hora: String,
    #[serde(rename = "Cota_Adotada")]
    pub cota: Option<String>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    items: Option<T>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct Auth {
    tokenautenticacao: Option<String>,
}

/// The station's ANA code: the catalog `id` is `ANA-<code>`.
pub fn code_of(station: &StationConfig) -> Result<&str> {
    station
        .id
        .strip_prefix(ID_PREFIX)
        .filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
        .with_context(|| format!("{}: estação da ANA deve ter id \"ANA-<código numérico>\"", station.id))
}

/// HidroWebService login. In GitHub Actions an unset secret arrives as an empty
/// string, so empty counts as missing.
pub struct Credentials {
    id: String,
    senha: String,
}

pub fn credentials() -> Result<Credentials> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match (get("ANA_IDENTIFICADOR"), get("ANA_SENHA")) {
        (Some(id), Some(senha)) => Ok(Credentials { id, senha }),
        _ => bail!("defina ANA_IDENTIFICADOR e ANA_SENHA (cadastro no HidroWebService)"),
    }
}

/// Retries `f` up to `ATTEMPTS` times, waiting between attempts.
fn retry<T>(what: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if attempt < ATTEMPTS => {
                eprintln!("aviso: {what} falhou (tentativa {attempt} de {ATTEMPTS}): {e:#}");
                std::thread::sleep(std::time::Duration::from_secs(RETRY_WAIT_SECS * attempt as u64));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn login(http: &reqwest::blocking::Client, cred: &Credentials) -> Result<String> {
    retry("login na ANA", || {
        let env: Envelope<Auth> = http
            .get(format!("{API_URL}/OAUth/v1"))
            .header("Identificador", &cred.id)
            .header("Senha", &cred.senha)
            .send()
            .and_then(|r| r.error_for_status())
            .context("login na ANA")?
            .json()
            .context("resposta inválida do login na ANA")?;
        env.items
            .and_then(|a| a.tokenautenticacao)
            .with_context(|| format!("login na ANA recusado: {}", env.message.unwrap_or_default()))
    })
}

/// All rows for stations `codes` between local days `first` and `last` (inclusive), in blocks of `MAX_CODES`.
/// A block that still fails after its retries leaves only its own stations without readings (missing hours, recorded
/// by quality); the other blocks go on. With the whole state (~44 blocks), one bad answer must not empty the batch.
pub fn fetch(http: &reqwest::blocking::Client, token: &str, codes: &[&str], first: NaiveDate, last: NaiveDate) -> Result<Vec<Row>> {
    // A daily batch covers 2 local days: DIAS_2 is enough. Larger windows (backfill) go 7 days at a
    // time.
    let range_days: u64 = if (last - first).num_days() < 2 { 2 } else { 7 };
    let (rows, _failed) = by_blocks(codes, |chunk| {
        let joined = chunk.join(",");
        let mut rows = Vec::new();
        let mut end = last;
        loop {
            let url = format!(
                "{API_URL}/HidroinfoanaSerieTelemetricaAdotada/v2?Codigos_Estacoes={joined}\
                 &Tipo%20Filtro%20Data=DATA_LEITURA&Data%20de%20Busca%20%28yyyy-MM-dd%29={end}\
                 &Range%20Intervalo%20de%20busca=DIAS_{range_days}"
            );
            let env: Envelope<Vec<Row>> = retry("consulta à ANA", || {
                http.get(&url)
                    .bearer_auth(token)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .with_context(|| format!("consultando a ANA ({joined}, até {end})"))?
                    .json()
                    .with_context(|| format!("resposta inválida da ANA ({joined}, até {end})"))
            })?;
            rows.extend(env.items.unwrap_or_default());
            match end.checked_sub_days(Days::new(range_days)) {
                Some(prev) if prev >= first => end = prev,
                _ => break,
            }
        }
        Ok(rows)
    });
    Ok(rows)
}

/// Runs `fetch_block` on each block of up to `MAX_CODES` codes and keeps what answered; returns the rows and the codes
/// of the blocks that failed.
pub fn by_blocks(codes: &[&str], mut fetch_block: impl FnMut(&[&str]) -> Result<Vec<Row>>) -> (Vec<Row>, Vec<String>) {
    let (mut rows, mut failed) = (Vec::new(), Vec::new());
    for chunk in codes.chunks(MAX_CODES) {
        match fetch_block(chunk) {
            Ok(r) => rows.extend(r),
            Err(e) => {
                eprintln!("aviso: a ANA não respondeu para {chunk:?}; essas estações ficam sem leituras neste lote: {e:#}");
                failed.extend(chunk.iter().map(|c| c.to_string()));
            }
        }
    }
    (rows, failed)
}

/// Canonical string whose SHA-256 becomes `raw_payload_sha256`: code, local instant and stage
/// exactly as ANA publishes them. `Data_Atualizacao` is left out because it changes on its own.
pub fn canonical_payload(code: &str, data_hora: &str, cota: &str) -> String {
    format!("ana-telemetrica|{code}|{data_hora}|{cota}")
}

pub fn datum_id(code: &str) -> String {
    format!("ANA-{code}-regua")
}

/// The station's level readings in the UTC window [from, to), one per full hour.
/// Also returns how many hours of the window had no reading.
///
/// A row with an unreadable time or stage counts as a missing hour, instead of bringing down the batch for
/// every station (which would happen on all 3 nodes at once).
pub fn to_readings(station: &StationConfig, rows: &[Row], from: i64, to: i64) -> Result<(Vec<Reading>, usize, usize)> {
    let code = code_of(station)?;
    let datum = station.datum_id.clone().unwrap_or_else(|| datum_id(code));
    // One row per instant. If ANA repeats an instant with different values, the
    // largest stage text wins: the choice does not depend on the order the rows arrived in.
    let mut by_ts: BTreeMap<i64, (&str, &str)> = BTreeMap::new();
    let mut unreadable = 0;
    for row in rows.iter().filter(|r| r.codigoestacao == code) {
        let Some(cota) = row.cota.as_deref() else { continue };
        let Ok(local) = NaiveDateTime::parse_from_str(&row.data_hora, "%Y-%m-%d %H:%M:%S%.f") else {
            unreadable += 1;
            continue;
        };
        if local.minute() != 0 || local.second() != 0 {
            continue;
        }
        let ts = local.and_utc().timestamp() + BRT_TO_UTC;
        if ts < from || ts >= to {
            continue;
        }
        if parse_milli(cota).is_none() {
            unreadable += 1;
            continue;
        }
        let slot = by_ts.entry(ts).or_insert((row.data_hora.as_str(), cota));
        if cota > slot.1 {
            *slot = (row.data_hora.as_str(), cota);
        }
    }
    if unreadable > 0 {
        eprintln!("aviso: {}: {unreadable} linha(s) ilegível(is) da ANA ignoradas", station.id);
    }
    let readings: Vec<Reading> = by_ts
        .into_iter()
        .map(|(ts, (data_hora, cota))| {
            let cm_milli = parse_milli(cota).expect("validada acima");
            // centimetres x1000 -> metres x1000, rounding half away from zero.
            let value_milli = (cm_milli + 50 * cm_milli.signum()) / 100;
            let payload = canonical_payload(code, data_hora, cota);
            Reading {
                station_id: station.id.clone(),
                variable: Variable::RiverStage,
                ts_utc: ts,
                source: Source::Ana,
                value_milli,
                datum_id: Some(datum.clone()),
                raw_payload_sha256: Sha256::digest(payload.as_bytes()).into(),
            }
        })
        .collect();
    let expected = ((to - from) / 3600) as usize;
    let missing = expected.saturating_sub(readings.len());
    Ok((readings, missing, expected))
}

/// Local (Brasília) days covering the UTC window [from, to).
pub fn local_days(from: i64, to: i64) -> Result<(NaiveDate, NaiveDate)> {
    let day = |ts: i64| {
        chrono::DateTime::from_timestamp(ts - BRT_TO_UTC, 0).map(|d| d.date_naive()).context("timestamp fora do intervalo")
    };
    Ok((day(from)?, day(to - 1)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_block_leaves_only_its_own_stations_without_readings() {
        let codes: Vec<String> = (0..25).map(|i| format!("{:08}", 87000000 + i)).collect();
        let refs: Vec<&str> = codes.iter().map(String::as_str).collect();
        let mut calls = 0;
        let (rows, failed) = by_blocks(&refs, |chunk| {
            calls += 1;
            assert!(chunk.len() <= MAX_CODES);
            if calls == 2 {
                anyhow::bail!("503");
            }
            Ok(chunk
                .iter()
                .map(|c| Row { codigoestacao: c.to_string(), data_hora: "2026-11-12 00:00:00.0".into(), cota: Some("100".into()) })
                .collect())
        });
        assert_eq!(calls, 3);
        assert_eq!(rows.len(), 15);
        assert_eq!(failed, refs[10..20].iter().map(|c| c.to_string()).collect::<Vec<_>>());
    }

    fn station(id: &str) -> StationConfig {
        StationConfig {
            id: id.into(),
            name: "Guaíba no Cais Mauá".into(),
            source: Source::Ana,
            variable: Variable::RiverStage,
            lat: -30.0211,
            lon: -51.2211,
            datum_id: Some("ANA-87450004-regua".into()),
            stream: None,
            kind: None,
        }
    }

    fn rows() -> Vec<Row> {
        serde_json::from_str(
            r#"[
              {"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 20:00:00.0","Cota_Adotada":"369.00","Data_Atualizacao":"x"},
              {"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 20:15:00.0","Cota_Adotada":"369.00"},
              {"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 21:00:00.0","Cota_Adotada":"368.37"},
              {"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 21:00:00.0","Cota_Adotada":"368.37"},
              {"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 22:00:00.0","Cota_Adotada":null},
              {"codigoestacao":"86879300","Data_Hora_Medicao":"2024-05-02 21:00:00.0","Cota_Adotada":"2900.00"}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn converts_hourly_brt_centimeters_to_utc_meters() {
        // Window from 2024-05-02 23:00Z to 2024-05-03 02:00Z: 20:00 and 21:00 Brasília time.
        let from = 1_714_690_800;
        let (r, missing, expected) = to_readings(&station("ANA-87450004"), &rows(), from, from + 3 * 3600).unwrap();
        assert_eq!((r.len(), missing, expected), (2, 1, 3));
        assert_eq!((r[0].ts_utc, r[0].value_milli), (from, 3_690));
        assert_eq!((r[1].ts_utc, r[1].value_milli), (from + 3600, 3_684));
        assert_eq!(r[0].datum_id.as_deref(), Some("ANA-87450004-regua"));
        let expected_hash: [u8; 32] = Sha256::digest(b"ana-telemetrica|87450004|2024-05-02 20:00:00.0|369.00").into();
        assert_eq!(r[0].raw_payload_sha256, expected_hash);
    }

    #[test]
    fn skips_unreadable_rows_and_resolves_duplicates_by_value() {
        let from = 1_714_690_800;
        let mut rs = rows();
        rs.push(serde_json::from_str(r#"{"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 22:00:00.0","Cota_Adotada":"-"}"#).unwrap());
        rs.push(serde_json::from_str(r#"{"codigoestacao":"87450004","Data_Hora_Medicao":"lixo","Cota_Adotada":"1.00"}"#).unwrap());
        rs.push(serde_json::from_str(r#"{"codigoestacao":"87450004","Data_Hora_Medicao":"2024-05-02 21:00:00.0","Cota_Adotada":"368.40"}"#).unwrap());
        let (a, missing, _) = to_readings(&station("ANA-87450004"), &rs, from, from + 3 * 3600).unwrap();
        rs.reverse();
        let (b, _, _) = to_readings(&station("ANA-87450004"), &rs, from, from + 3 * 3600).unwrap();
        assert_eq!(a, b);
        assert_eq!((a.len(), missing, a[1].value_milli), (2, 1, 3_684));
    }

    #[test]
    fn rejects_ids_without_code() {
        assert!(code_of(&station("CAIS-MAUA")).is_err());
        assert!(code_of(&station("ANA-")).is_err());
    }

    #[test]
    fn local_days_shift_three_hours() {
        // UTC day 2026-11-03 = Nov 2 21:00 to Nov 3 21:00 Brasília time.
        let from = 1_793_664_000;
        let (a, b) = local_days(from, from + 86_400).unwrap();
        assert_eq!((a.to_string(), b.to_string()), ("2026-11-02".into(), "2026-11-03".into()));
    }
}
