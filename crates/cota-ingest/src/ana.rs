//! ANA, HidroWebService: séries das estações telemétricas (rede hidrometeorológica nacional).
//!
//! Nível do rio **medido em régua**, ao contrário da vazão do GloFAS, que é modelada.
//! Exige cadastro: o login (`ANA_IDENTIFICADOR` / `ANA_SENHA`) devolve um token de
//! curta duração. O token nunca entra no ledger, só os valores.
//!
//! Formato das linhas (`HidroinfoanaSerieTelemetricaAdotada/v2`):
//! `{"codigoestacao":"87450004","Data_Hora_Medicao":"2026-11-03 23:00:00.0",
//!   "Cota_Adotada":"115.00","Chuva_Adotada":"0.20","Vazao_Adotada":null,…}`
//! - horário de Brasília (UTC−3, sem horário de verão desde 2019);
//! - cota em centímetros, relativa ao zero da régua de cada estação.
//!
//! Só entram as leituras da hora cheia: a régua do Cais Mauá transmite de hora em hora e
//! as outras a cada 15 min; uma cadência só deixa a disponibilidade comparável entre estações.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate, NaiveDateTime, Timelike};
use cota_core::{Reading, Source, Variable};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::StationConfig;
use crate::decimal::parse_milli;

pub const API_URL: &str = "https://www.ana.gov.br/hidrowebservice/EstacoesTelemetricas";
/// Horário de Brasília em segundos a somar para chegar ao UTC.
const BRT_TO_UTC: i64 = 3 * 3600;
/// A API aceita no máximo 10 estações por consulta.
const MAX_CODES: usize = 10;
/// Tentativas por requisição e espera entre elas: os 3 nós consultam ao mesmo tempo com a
/// mesma conta, e um nó que perde a ANA assina outra raiz e enfraquece o quórum.
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

/// Código ANA da estação: o `id` do catálogo é `ANA-<código>`.
pub fn code_of(station: &StationConfig) -> Result<&str> {
    station
        .id
        .strip_prefix(ID_PREFIX)
        .filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
        .with_context(|| format!("{}: estação da ANA deve ter id \"ANA-<código numérico>\"", station.id))
}

/// Login do HidroWebService. No GitHub Actions um secret não cadastrado chega como string
/// vazia, então vazio conta como ausente.
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

/// Repete `f` até `ATTEMPTS` vezes, esperando entre as tentativas.
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

/// Todas as linhas das estações `codes` entre os dias locais `first` e `last` (inclusive).
pub fn fetch(http: &reqwest::blocking::Client, token: &str, codes: &[&str], first: NaiveDate, last: NaiveDate) -> Result<Vec<Row>> {
    // Um lote diário cobre 2 dias locais: DIAS_2 basta. Janelas maiores (backfill) vão de 7 em 7.
    let range_days: u64 = if (last - first).num_days() < 2 { 2 } else { 7 };
    let mut rows = Vec::new();
    for chunk in codes.chunks(MAX_CODES) {
        let joined = chunk.join(",");
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
    }
    Ok(rows)
}

/// String canônica cujo SHA-256 vira `raw_payload_sha256`: código, instante local e cota
/// exatamente como a ANA os publica. `Data_Atualizacao` fica de fora porque muda sozinha.
pub fn canonical_payload(code: &str, data_hora: &str, cota: &str) -> String {
    format!("ana-telemetrica|{code}|{data_hora}|{cota}")
}

pub fn datum_id(code: &str) -> String {
    format!("ANA-{code}-regua")
}

/// Leituras de nível da estação na janela [from, to) UTC, uma por hora cheia.
/// Devolve também quantas horas da janela ficaram sem leitura.
///
/// Linha com horário ou cota ilegível conta como hora faltante, em vez de derrubar o lote
/// de todas as estações (o que aconteceria nos 3 nós ao mesmo tempo).
pub fn to_readings(station: &StationConfig, rows: &[Row], from: i64, to: i64) -> Result<(Vec<Reading>, usize, usize)> {
    let code = code_of(station)?;
    let datum = station.datum_id.clone().unwrap_or_else(|| datum_id(code));
    // Uma linha por instante. Se a ANA repetir um instante com valores diferentes, fica o
    // maior texto de cota: a escolha não depende da ordem em que as linhas chegaram.
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
            // centímetros x1000 -> metros x1000, arredondando meio para longe de zero.
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

/// Dias locais (Brasília) que cobrem a janela UTC [from, to).
pub fn local_days(from: i64, to: i64) -> Result<(NaiveDate, NaiveDate)> {
    let day = |ts: i64| {
        chrono::DateTime::from_timestamp(ts - BRT_TO_UTC, 0).map(|d| d.date_naive()).context("timestamp fora do intervalo")
    };
    Ok((day(from)?, day(to - 1)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(id: &str) -> StationConfig {
        StationConfig {
            id: id.into(),
            name: "Guaíba no Cais Mauá".into(),
            source: Source::Ana,
            variable: Variable::RiverStage,
            lat: -30.0211,
            lon: -51.2211,
            datum_id: Some("ANA-87450004-regua".into()),
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
        // Janela de 2024-05-02 23:00Z a 2024-05-03 02:00Z: 20h e 21h de Brasília.
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
        // Dia UTC 2026-11-03 = 02/11 21h a 03/11 21h em Brasília.
        let from = 1_793_664_000;
        let (a, b) = local_days(from, from + 86_400).unwrap();
        assert_eq!((a.to_string(), b.to_string()), ("2026-11-02".into(), "2026-11-03".into()));
    }
}
