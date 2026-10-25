//! INMET: zips anuais de dados históricos das estações automáticas.
//!
//! Formato (um CSV por estação dentro do zip, Latin-1, separador `;`, decimal com vírgula):
//! 8 linhas de metadados (`CODIGO (WMO):;A801`, `LATITUDE:;-30,05`…), 1 linha de
//! cabeçalho e uma linha por hora UTC. Desde 2019: `2024/05/02;0300 UTC;17,2;…`.
//! Antes: `2018-05-02;03:00;17.2;…`. Faltantes: campo vazio ou `-9999`.
//! A chuva de uma linha é o acumulado da hora que termina no horário indicado.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveTime};
use cota_core::{Reading, Source, Variable};
use sha2::{Digest, Sha256};

use crate::decimal::parse_milli;

const METADATA_LINES: usize = 8;
const MISSING_SENTINEL: &str = "-9999";
pub const ZIP_URL: &str = "https://portal.inmet.gov.br/uploads/dadoshistoricos";

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ParseStats {
    pub rows_in_window: usize,
    pub missing: usize,
}

/// Baixa `{ano}.zip` para `cache_dir` se ainda não existir; retorna o caminho.
pub fn ensure_zip(http: &reqwest::blocking::Client, cache_dir: &Path, year: i32) -> Result<PathBuf> {
    let path = cache_dir.join(format!("{year}.zip"));
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(cache_dir)?;
    let url = format!("{ZIP_URL}/{year}.zip");
    // O portal do INMET derruba conexões sem User-Agent de navegador.
    let bytes = http
        .get(&url)
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (cota-oracle)")
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("baixando {url}"))?
        .bytes()?;
    let tmp = path.with_extension("zip.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Bytes crus do CSV da estação dentro do zip anual.
/// `None` se a estação não está no zip daquele ano (ainda não existia ou foi desativada).
pub fn read_station_csv(zip_path: &Path, station_id: &str) -> Result<Option<Vec<u8>>> {
    let file = std::fs::File::open(zip_path).with_context(|| format!("abrindo {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)?;
    let needle = format!("_{station_id}_");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.name()?.contains(&needle) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            return Ok(Some(bytes));
        }
    }
    Ok(None)
}

/// Converte o CSV em leituras de chuva dentro de [from, to) (segundos UTC).
///
/// `raw_payload_sha256` = SHA-256 da linha exatamente como está no arquivo (bytes Latin-1,
/// sem o terminador de linha). Qualquer pessoa com o zip oficial refaz o hash.
pub fn parse_csv(bytes: &[u8], station_id: &str, from: i64, to: i64) -> Result<(Vec<Reading>, ParseStats)> {
    let lines: Vec<&[u8]> = bytes
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .collect();
    if lines.len() <= METADATA_LINES {
        bail!("CSV do INMET curto demais");
    }

    let code = lines[..METADATA_LINES]
        .iter()
        .find_map(|l| l.strip_prefix(b"CODIGO (WMO):;"))
        .map(|c| String::from_utf8_lossy(c).trim().to_string());
    if code.as_deref() != Some(station_id) {
        bail!("CSV pertence a {code:?}, esperado {station_id}");
    }

    let mut readings = Vec::new();
    let mut stats = ParseStats::default();
    for (n, line) in lines.iter().enumerate().skip(METADATA_LINES + 1) {
        if line.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(line).with_context(|| format!("linha {} não é ASCII", n + 1))?;
        let mut fields = text.split(';');
        let (Some(date), Some(hour), Some(rain)) = (fields.next(), fields.next(), fields.next()) else {
            bail!("linha {} com colunas insuficientes", n + 1);
        };
        let ts = parse_timestamp(date, hour).with_context(|| format!("linha {}: data/hora inválida", n + 1))?;
        if ts < from || ts >= to {
            continue;
        }
        stats.rows_in_window += 1;

        let rain = rain.trim();
        if rain.is_empty() || rain == MISSING_SENTINEL {
            stats.missing += 1;
            continue;
        }
        let value_milli = parse_milli(rain).with_context(|| format!("linha {}: chuva inválida {rain:?}", n + 1))?;
        readings.push(Reading {
            station_id: station_id.to_string(),
            variable: Variable::Precipitation,
            ts_utc: ts,
            source: Source::Inmet,
            value_milli,
            datum_id: None,
            raw_payload_sha256: Sha256::digest(line).into(),
        });
    }
    Ok((readings, stats))
}

/// Aceita `2024/05/02` + `0300 UTC` e `2018-05-02` + `03:00`.
fn parse_timestamp(date: &str, hour: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(&date.replace('-', "/"), "%Y/%m/%d").ok()?;
    let hhmm: String = hour.trim().trim_end_matches(" UTC").chars().filter(|c| c.is_ascii_digit()).collect();
    let time = NaiveTime::parse_from_str(&hhmm, "%H%M").ok()?;
    Some(date.and_time(time).and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"REGIAO:;S\r\nUF:;RS\r\nESTACAO:;PORTO ALEGRE\r\nCODIGO (WMO):;A801\r\nLATITUDE:;-30,05\r\n\
LONGITUDE:;-51,17\r\nALTITUDE:;41,18\r\nDATA DE FUNDACAO:;22/09/00\r\nData;Hora UTC;PRECIPITA\xc7\xc3O TOTAL\r\n\
2024/05/02;0200 UTC;17,2;1002,3\r\n2024/05/02;0300 UTC;;1000,4\r\n2024/05/02;0400 UTC;-9999;1001\r\n\
2024/05/02;0500 UTC;,8;1001\r\n";

    const T0: i64 = 1_714_615_200; // 2024-05-02T02:00:00Z

    #[test]
    fn parses_values_timestamps_and_gaps() {
        let (r, stats) = parse_csv(SAMPLE, "A801", T0, T0 + 4 * 3600).unwrap();
        assert_eq!(stats.rows_in_window, 4);
        assert_eq!(stats.missing, 2);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].ts_utc, r[0].value_milli), (T0, 17_200));
        assert_eq!((r[1].ts_utc, r[1].value_milli), (T0 + 3 * 3600, 800));
        let expected: [u8; 32] = Sha256::digest(b"2024/05/02;0200 UTC;17,2;1002,3").into();
        assert_eq!(r[0].raw_payload_sha256, expected);
    }

    #[test]
    fn window_is_half_open() {
        let (r, _) = parse_csv(SAMPLE, "A801", T0 + 3600, T0 + 3 * 3600).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn rejects_csv_of_another_station() {
        assert!(parse_csv(SAMPLE, "B807", T0, T0 + 3600).is_err());
    }

    #[test]
    fn accepts_pre_2019_format() {
        assert_eq!(parse_timestamp("2018-05-02", "03:00"), Some(1_525_230_000));
        assert_eq!(parse_timestamp("2024/05/02", "0200 UTC"), Some(T0));
    }
}
