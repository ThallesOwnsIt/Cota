//! Oracle node: ingests a day of data on its own, builds the batch and signs the header.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use cota_core::{Batch, BatchHeader, Hash, Reading, Variable};
use cota_ingest::{config::StationConfig, end_of_day, ingest, start_of_day, StationReport};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::keys::pubkey_base58;

/// Simulated faults to demonstrate graceful degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Funciona normalmente.
    None,
    /// Produces no signature (node down or without access to the sources).
    Silent,
    /// Adds 10 mm to the day's largest rain before signing (compromised node or tampered source).
    Tamper,
}

/// What a node publishes for a day: the signed header and the readings that make it up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedBatch {
    pub node: String,
    pub day: NaiveDate,
    pub header: BatchHeader,
    #[serde(with = "hex64")]
    pub signature: [u8; 64],
    pub readings: Vec<Reading>,
    pub ingest: Vec<StationReport>,
}

/// Runs one round: ingests `day`, builds batch `batch_id` chained onto `prev_root` and signs it.
/// Returns `None` if the node is configured as silent.
pub fn sign_day(
    key: &SigningKey,
    stations: &[StationConfig],
    cache: &Path,
    day: NaiveDate,
    batch_id: u64,
    prev_root: Hash,
    fault: Fault,
) -> Result<Option<SignedBatch>> {
    if fault == Fault::Silent {
        return Ok(None);
    }
    let (from, to) = (start_of_day(day), end_of_day(day));
    let http = cota_ingest::http_client()?;
    let (mut readings, ingest_reports) = ingest(&http, stations, cache, from, to, false).with_context(|| format!("ingestão de {day}"))?;

    if fault == Fault::Tamper {
        if let Some(r) = readings.iter_mut().filter(|r| r.variable == Variable::Precipitation).max_by_key(|r| r.value_milli) {
            r.value_milli += 10_000;
        }
    }

    let batch = Batch::build(batch_id, from, to, prev_root, readings)?;
    let signature = key.sign(&batch.header.signing_message_for(crate::PROTOCOL)).to_bytes();
    Ok(Some(SignedBatch {
        node: pubkey_base58(key),
        day,
        header: batch.header,
        signature,
        readings: batch.readings,
        ingest: ingest_reports,
    }))
}

/// Hours after a day closes during which a node waits for a source that is down instead of signing the day without
/// it. Past that, it signs what it has (the missing stations stay on record), so the ledger never stalls.
pub const COMPLETE_BY_HOURS: i64 = 4;

fn source_label(s: cota_core::Source) -> &'static str {
    match s {
        cota_core::Source::Ana => "ANA",
        cota_core::Source::Inmet => "INMET",
        cota_core::Source::OpenMeteoGlofas => "GloFAS",
        _ => "outra fonte",
    }
}

/// Sources that are down, not just a gauge out: fewer than half of their stations returned any reading for the day.
/// Each with how many stations answered and how many there are.
pub fn sources_down(reports: &[StationReport]) -> Vec<(&'static str, usize, usize)> {
    let mut by_source: std::collections::BTreeMap<&'static str, (usize, usize)> = std::collections::BTreeMap::new();
    for r in reports.iter().filter(|r| r.expected > 0) {
        let e = by_source.entry(source_label(r.source)).or_default();
        e.1 += 1;
        if r.readings > 0 {
            e.0 += 1;
        }
    }
    by_source.into_iter().filter(|(_, (with, total))| with * 2 < *total).map(|(s, (w, t))| (s, w, t)).collect()
}

/// Why the node should not sign `day` yet: a source is down and the day closed less than `complete_by_hours` ago.
/// `None` means sign (everything answered, or the wait is over).
pub fn wait_reason(reports: &[StationReport], day: NaiveDate, now: i64, complete_by_hours: i64) -> Option<String> {
    let down = sources_down(reports);
    if down.is_empty() || now >= end_of_day(day) + complete_by_hours * 3600 {
        return None;
    }
    Some(down.iter().map(|(s, w, t)| format!("{s}: {w} de {t} estações responderam")).collect::<Vec<_>>().join("; "))
}

pub mod hex64 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(d)?;
        let mut out = [0u8; 64];
        hex::decode_to_slice(&text, &mut out).map_err(D::Error::custom)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cota_core::Source;

    fn report(id: &str, source: Source, readings: usize) -> StationReport {
        StationReport { station_id: id.into(), source, readings, missing: 24 - readings.min(24), expected: 24 }
    }

    #[test]
    fn a_source_is_down_when_most_of_its_stations_are_silent() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        // 2026-11-12: the ANA login failed, its 14 gauges came back empty, only GloFAS answered.
        let mut reports: Vec<StationReport> = (0..14).map(|i| report(&format!("ANA-{i}"), Source::Ana, 0)).collect();
        reports.extend((0..6).map(|i| report(&format!("GLOFAS-{i}"), Source::OpenMeteoGlofas, 1)));
        assert_eq!(sources_down(&reports), vec![("ANA", 0, 14)]);
        let closed = end_of_day(day);
        // Right after the day closed: wait and try again next round.
        assert_eq!(wait_reason(&reports, day, closed + 3900, COMPLETE_BY_HOURS).as_deref(), Some("ANA: 0 de 14 estações responderam"));
        // Past the deadline: sign what there is.
        assert_eq!(wait_reason(&reports, day, closed + COMPLETE_BY_HOURS * 3600, COMPLETE_BY_HOURS), None);
        // One gauge out (13 of 14) is not a source down.
        reports.iter_mut().filter(|r| r.source == Source::Ana).skip(1).for_each(|r| r.readings = 20);
        assert!(sources_down(&reports).is_empty());
        assert_eq!(wait_reason(&reports, day, closed + 3900, COMPLETE_BY_HOURS), None);
    }
}
