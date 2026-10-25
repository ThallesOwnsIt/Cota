//! Agregação k-de-n: escolhe o cabeçalho que tem assinaturas válidas suficientes e
//! classifica cada membro do comitê (assinou, discordou, ausente, assinatura inválida).

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use cota_core::BatchHeader;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Serialize;

use crate::node::SignedBatch;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NodeStatus {
    Signed,
    /// Assinou outro cabeçalho (outra raiz Merkle); a assinatura não entra no lote.
    Disagreed { merkle_root: String },
    Absent,
    InvalidSignature,
    /// Publicou com uma chave que não pertence ao comitê.
    NotMember,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberReport {
    pub member: String,
    #[serde(flatten)]
    pub status: NodeStatus,
}

#[derive(Debug, Clone)]
pub struct Aggregate {
    pub header: BatchHeader,
    /// (chave pública, assinatura) dos membros que assinaram o cabeçalho escolhido.
    pub signatures: Vec<([u8; 32], [u8; 64])>,
    /// Lote de um dos nós que assinou o cabeçalho escolhido (leituras canônicas).
    pub batch: SignedBatch,
    pub members: Vec<MemberReport>,
    /// Lotes com assinatura válida de membros (assinantes e discordantes), na ordem do
    /// comitê: a base da comparação por MAD entre nós.
    pub valid: Vec<SignedBatch>,
}

/// `members` na ordem do comitê on-chain (a mesma ordem do bitmap de assinantes).
pub fn aggregate(members: &[[u8; 32]], threshold: usize, submissions: &[SignedBatch]) -> Result<Aggregate> {
    // Só entram assinaturas válidas de membros; agrupadas pela mensagem assinada.
    let mut groups: BTreeMap<Vec<u8>, Vec<&SignedBatch>> = BTreeMap::new();
    let mut status: BTreeMap<[u8; 32], NodeStatus> = members.iter().map(|m| (*m, NodeStatus::Absent)).collect();

    for sub in submissions {
        let pk = decode_pubkey(&sub.node)?;
        if !members.contains(&pk) {
            continue;
        }
        let msg = sub.header.signing_message();
        let valid = VerifyingKey::from_bytes(&pk)
            .map(|vk| vk.verify_strict(&msg, &Signature::from_bytes(&sub.signature)).is_ok())
            .unwrap_or(false);
        if !valid {
            status.insert(pk, NodeStatus::InvalidSignature);
            continue;
        }
        groups.entry(msg).or_default().push(sub);
    }

    // Grupo vencedor: o maior; empate desfeito pela ordem dos bytes (determinístico).
    let Some((_, winners)) = groups.iter().max_by_key(|(_, subs)| subs.len()) else {
        bail!("nenhuma assinatura válida de membro do comitê");
    };
    if winners.len() < threshold {
        bail!("apenas {} assinatura(s) concordante(s); limiar é {threshold}", winners.len());
    }
    let header = winners[0].header.clone();

    let mut signatures = Vec::new();
    for sub in submissions {
        let pk = decode_pubkey(&sub.node)?;
        if !members.contains(&pk) || status.get(&pk) == Some(&NodeStatus::InvalidSignature) {
            continue;
        }
        if sub.header == header {
            if !signatures.iter().any(|(k, _)| *k == pk) {
                signatures.push((pk, sub.signature));
            }
            status.insert(pk, NodeStatus::Signed);
        } else {
            status.insert(pk, NodeStatus::Disagreed { merkle_root: hex::encode(sub.header.merkle_root) });
        }
    }

    let mut reports: Vec<MemberReport> = members
        .iter()
        .map(|m| MemberReport { member: bs58::encode(m).into_string(), status: status[m].clone() })
        .collect();
    for sub in submissions {
        if !members.contains(&decode_pubkey(&sub.node)?) {
            reports.push(MemberReport { member: sub.node.clone(), status: NodeStatus::NotMember });
        }
    }

    let mut valid = Vec::new();
    for m in members {
        if matches!(status[m], NodeStatus::Signed | NodeStatus::Disagreed { .. }) {
            let b58 = bs58::encode(m).into_string();
            if let Some(sub) = submissions.iter().find(|s| s.node == b58) {
                valid.push(sub.clone());
            }
        }
    }

    Ok(Aggregate { header, signatures, batch: (*winners[0]).clone(), members: reports, valid })
}

fn decode_pubkey(b58: &str) -> Result<[u8; 32]> {
    let bytes = bs58::decode(b58).into_vec()?;
    bytes.try_into().map_err(|_| anyhow::anyhow!("chave pública inválida: {b58}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use cota_core::{Batch, Reading, Source, Variable};
    use ed25519_dalek::{Signer, SigningKey};

    fn readings(extra_mm: i64) -> Vec<Reading> {
        vec![Reading {
            station_id: "A801".into(),
            variable: Variable::Precipitation,
            ts_utc: 3600,
            source: Source::Inmet,
            value_milli: 17_200 + extra_mm * 1000,
            datum_id: None,
            raw_payload_sha256: [1; 32],
        }]
    }

    fn signed(key: &SigningKey, extra_mm: i64) -> SignedBatch {
        let b = Batch::build(1, 0, 86_400, [0; 32], readings(extra_mm)).unwrap();
        SignedBatch {
            node: crate::keys::pubkey_base58(key),
            day: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
            signature: key.sign(&b.header.signing_message()).to_bytes(),
            header: b.header,
            readings: b.readings,
            ingest: vec![],
        }
    }

    fn keys() -> (Vec<SigningKey>, Vec<[u8; 32]>) {
        let k: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let m = k.iter().map(|k| k.verifying_key().to_bytes()).collect();
        (k, m)
    }

    #[test]
    fn honest_majority_wins_and_liar_is_flagged() {
        let (k, m) = keys();
        let agg = aggregate(&m, 2, &[signed(&k[0], 0), signed(&k[1], 10), signed(&k[2], 0)]).unwrap();
        assert_eq!(agg.header.merkle_root, signed(&k[0], 0).header.merkle_root);
        assert_eq!(agg.signatures.len(), 2);
        assert_eq!(agg.members[0].status, NodeStatus::Signed);
        assert!(matches!(agg.members[1].status, NodeStatus::Disagreed { .. }));
        assert_eq!(agg.members[2].status, NodeStatus::Signed);
    }

    #[test]
    fn absent_node_is_reported_and_two_suffice() {
        let (k, m) = keys();
        let agg = aggregate(&m, 2, &[signed(&k[0], 0), signed(&k[1], 0)]).unwrap();
        assert_eq!(agg.signatures.len(), 2);
        assert_eq!(agg.members[2].status, NodeStatus::Absent);
    }

    #[test]
    fn bad_signature_is_excluded() {
        let (k, m) = keys();
        let mut forged = signed(&k[2], 0);
        forged.signature[0] ^= 1;
        let agg = aggregate(&m, 2, &[signed(&k[0], 0), signed(&k[1], 0), forged]).unwrap();
        assert_eq!(agg.signatures.len(), 2);
        assert_eq!(agg.members[2].status, NodeStatus::InvalidSignature);
    }

    #[test]
    fn no_quorum_fails() {
        let (k, m) = keys();
        assert!(aggregate(&m, 2, &[signed(&k[0], 0), signed(&k[1], 10)]).is_err());
        assert!(aggregate(&m, 2, &[signed(&k[0], 0)]).is_err());
    }

    #[test]
    fn outsider_is_ignored() {
        let (k, m) = keys();
        let outsider = SigningKey::from_bytes(&[77; 32]);
        let agg = aggregate(&m, 2, &[signed(&k[0], 0), signed(&outsider, 0), signed(&k[1], 0)]).unwrap();
        assert_eq!(agg.signatures.len(), 2);
        assert_eq!(agg.members.last().unwrap().status, NodeStatus::NotMember);
    }
}
