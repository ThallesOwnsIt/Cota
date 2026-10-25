//! Nó-oráculo: ingere um dia de dados por conta própria, monta o lote e assina o cabeçalho.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use cota_core::{Batch, BatchHeader, Hash, Reading, Variable};
use cota_ingest::{config::StationConfig, end_of_day, ingest, start_of_day, StationReport};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::keys::pubkey_base58;

/// Falhas simuladas para demonstrar a degradação graciosa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Funciona normalmente.
    None,
    /// Não produz assinatura (nó caído ou sem acesso às fontes).
    Silent,
    /// Soma 10 mm à maior chuva do dia antes de assinar (nó comprometido ou fonte adulterada).
    Tamper,
}

/// O que um nó publica para um dia: cabeçalho assinado e as leituras que o compõem.
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

/// Executa uma rodada: ingere `day`, monta o lote `batch_id` encadeado em `prev_root` e assina.
/// Retorna `None` se o nó estiver configurado como mudo.
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
    let signature = key.sign(&batch.header.signing_message()).to_bytes();
    Ok(Some(SignedBatch {
        node: pubkey_base58(key),
        day,
        header: batch.header,
        signature,
        readings: batch.readings,
        ingest: ingest_reports,
    }))
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
