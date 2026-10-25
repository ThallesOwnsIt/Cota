//! Lote de leituras de uma janela de tempo e o cabeçalho que os oráculos assinam.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::merkle::{self, Hash};
use crate::reading::Reading;

/// Prefixo de domínio da mensagem assinada. Uma assinatura de lote não serve em outro contexto.
pub const BATCH_DOMAIN: &[u8] = b"COTA-BATCH-v1";

/// O que os nós assinam e o programa on-chain reconstrói para conferir as assinaturas.
/// A ordem dos campos é o layout Borsh: alterá-la quebra a compatibilidade com o programa.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize, Serialize, Deserialize)]
pub struct BatchHeader {
    /// Sequencial e estritamente crescente.
    pub batch_id: u64,
    /// Início da janela, inclusivo (segundos UTC).
    pub t_start: i64,
    /// Fim da janela, exclusivo (segundos UTC).
    pub t_end: i64,
    pub leaf_count: u32,
    #[serde(with = "crate::reading::hex32")]
    pub merkle_root: Hash,
    /// SHA-256 da lista ordenada de estações presentes no lote.
    #[serde(with = "crate::reading::hex32")]
    pub station_set_hash: Hash,
    /// Raiz do lote anterior (zeros no primeiro): forma uma hash-chain que impede inserir lotes no passado.
    #[serde(with = "crate::reading::hex32")]
    pub prev_batch_root: Hash,
}

impl BatchHeader {
    /// Bytes exatos que cada nó assina com ed25519: `BATCH_DOMAIN || borsh(header)`.
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = BATCH_DOMAIN.to_vec();
        msg.extend(borsh::to_vec(self).expect("Borsh de BatchHeader não falha"));
        msg
    }
}

/// Lote em ordem canônica, com as folhas já calculadas.
#[derive(Debug, Clone)]
pub struct Batch {
    pub header: BatchHeader,
    pub readings: Vec<Reading>,
    pub leaves: Vec<Hash>,
}

impl Batch {
    /// Monta o lote. As leituras são ordenadas e deduplicadas, então dois nós honestos
    /// que leem os mesmos dados em qualquer ordem chegam à mesma raiz.
    pub fn build(
        batch_id: u64,
        t_start: i64,
        t_end: i64,
        prev_batch_root: Hash,
        mut readings: Vec<Reading>,
    ) -> Result<Self, BatchError> {
        if t_end <= t_start {
            return Err(BatchError::EmptyWindow);
        }
        if let Some(r) = readings.iter().find(|r| r.ts_utc < t_start || r.ts_utc >= t_end) {
            return Err(BatchError::OutOfWindow { station_id: r.station_id.clone(), ts_utc: r.ts_utc });
        }
        readings.sort();
        readings.dedup();
        // Mesma (estação, variável, instante, fonte) com dois valores diferentes é conflito, não duplicata.
        if let Some(w) = readings.windows(2).find(|w| same_slot(&w[0], &w[1])) {
            return Err(BatchError::ConflictingValues { station_id: w[0].station_id.clone(), ts_utc: w[0].ts_utc });
        }

        let leaves: Vec<Hash> = readings.iter().map(|r| merkle::hash_leaf(&r.canonical_bytes())).collect();
        let header = BatchHeader {
            batch_id,
            t_start,
            t_end,
            leaf_count: leaves.len() as u32,
            merkle_root: merkle::root_from_leaves(&leaves),
            station_set_hash: station_set_hash(&readings),
            prev_batch_root,
        };
        Ok(Self { header, readings, leaves })
    }

    /// Prova de inclusão da leitura na posição `index` (posição na ordem canônica).
    pub fn proof(&self, index: usize) -> Vec<Hash> {
        merkle::proof(&self.leaves, index)
    }
}

fn same_slot(a: &Reading, b: &Reading) -> bool {
    a.station_id == b.station_id && a.variable == b.variable && a.ts_utc == b.ts_utc && a.source == b.source
}

/// SHA-256 de `borsh(Vec<String>)` com as estações únicas em ordem.
fn station_set_hash(sorted: &[Reading]) -> Hash {
    let mut ids: Vec<&str> = sorted.iter().map(|r| r.station_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let owned: Vec<String> = ids.into_iter().map(String::from).collect();
    Sha256::digest(borsh::to_vec(&owned).expect("Borsh de Vec<String> não falha")).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    EmptyWindow,
    OutOfWindow { station_id: String, ts_utc: i64 },
    ConflictingValues { station_id: String, ts_utc: i64 },
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::EmptyWindow => write!(f, "janela vazia (t_end <= t_start)"),
            BatchError::OutOfWindow { station_id, ts_utc } => {
                write!(f, "leitura de {station_id} em {ts_utc} fora da janela do lote")
            }
            BatchError::ConflictingValues { station_id, ts_utc } => {
                write!(f, "valores conflitantes para {station_id} em {ts_utc}")
            }
        }
    }
}

impl std::error::Error for BatchError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reading::{Source, Variable};

    fn reading(station: &str, ts: i64, mm: f64) -> Reading {
        Reading {
            station_id: station.into(),
            variable: Variable::Precipitation,
            ts_utc: ts,
            source: Source::Inmet,
            value_milli: Reading::to_milli(mm),
            datum_id: None,
            raw_payload_sha256: [7; 32],
        }
    }

    fn sample() -> Vec<Reading> {
        vec![reading("A801", 3600, 12.4), reading("B807", 0, 0.0), reading("A801", 0, 3.2)]
    }

    #[test]
    fn order_of_input_does_not_change_root() {
        let a = Batch::build(1, 0, 7200, [0; 32], sample()).unwrap();
        let mut rev = sample();
        rev.reverse();
        let b = Batch::build(1, 0, 7200, [0; 32], rev).unwrap();
        assert_eq!(a.header, b.header);
        assert_eq!(a.header.signing_message(), b.header.signing_message());
    }

    #[test]
    fn changing_one_value_changes_root() {
        let a = Batch::build(1, 0, 7200, [0; 32], sample()).unwrap();
        let mut tampered = sample();
        tampered[0].value_milli += 1;
        let b = Batch::build(1, 0, 7200, [0; 32], tampered).unwrap();
        assert_ne!(a.header.merkle_root, b.header.merkle_root);
    }

    #[test]
    fn every_reading_has_a_valid_proof() {
        let b = Batch::build(1, 0, 7200, [0; 32], sample()).unwrap();
        for (i, leaf) in b.leaves.iter().enumerate() {
            let p = b.proof(i);
            assert!(merkle::verify(leaf, i as u64, b.header.leaf_count as u64, &p, &b.header.merkle_root));
        }
    }

    #[test]
    fn rejects_out_of_window_and_conflicts() {
        assert!(matches!(
            Batch::build(1, 0, 3600, [0; 32], sample()),
            Err(BatchError::OutOfWindow { .. })
        ));
        let mut c = sample();
        c.push(reading("A801", 0, 9.9));
        assert!(matches!(Batch::build(1, 0, 7200, [0; 32], c), Err(BatchError::ConflictingValues { .. })));
    }

    #[test]
    fn exact_duplicates_are_merged() {
        let mut d = sample();
        d.push(sample()[0].clone());
        assert_eq!(Batch::build(1, 0, 7200, [0; 32], d).unwrap().header.leaf_count, 3);
    }

    #[test]
    fn signing_message_starts_with_domain() {
        let b = Batch::build(1, 0, 7200, [0; 32], sample()).unwrap();
        // domínio + 8 + 8 + 8 + 4 + 32 + 32 + 32 bytes
        let msg = b.header.signing_message();
        assert!(msg.starts_with(BATCH_DOMAIN));
        assert_eq!(msg.len(), BATCH_DOMAIN.len() + 124);
    }
}
