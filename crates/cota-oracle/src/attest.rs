//! Atestação de dados externos (avisos oficiais, chuva prevista): cada nó assina o que viu;
//! o agregador só leva à cadeia a versão que ≥ k membros viram com conteúdo idêntico.

use std::collections::BTreeMap;

use anyhow::Result;
use cota_core::{public_alert::content_hash, PublicAlertHeader};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::inmet_alerts::ObservedAlert;
use crate::keys::pubkey_base58;

/// O que um nó publica sobre uma versão de aviso.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAlert {
    pub node: String,
    pub header: PublicAlertHeader,
    #[serde(with = "crate::node::hex64")]
    pub signature: [u8; 64],
    /// Conteúdo canônico; `header.content_sha256` é o hash destes bytes.
    pub content: String,
    /// Quando o nó viu o aviso (relógio local, só informativo; o "quando" que vale é o slot).
    pub seen_at: i64,
    pub description: String,
    pub severity_label: String,
    pub states: String,
}

pub fn sign(key: &SigningKey, a: &ObservedAlert, seen_at: i64) -> SignedAlert {
    SignedAlert {
        node: pubkey_base58(key),
        signature: key.sign(&a.header.signing_message()).to_bytes(),
        header: a.header.clone(),
        content: a.content.clone(),
        seen_at,
        description: a.description.clone(),
        severity_label: a.severity_label.clone(),
        states: a.states.clone(),
    }
}

/// Algo que um nó assina e publica junto com o conteúdo de que o cabeçalho é o hash.
pub trait Attestation: Clone {
    type Header: Clone;
    fn node(&self) -> &str;
    fn signature(&self) -> &[u8; 64];
    fn header(&self) -> &Self::Header;
    fn message(&self) -> Vec<u8>;
    /// O conteúdo publicado é o que o hash do cabeçalho diz.
    fn content_matches(&self) -> bool;
}

impl Attestation for SignedAlert {
    type Header = PublicAlertHeader;
    fn node(&self) -> &str {
        &self.node
    }
    fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
    fn header(&self) -> &PublicAlertHeader {
        &self.header
    }
    fn message(&self) -> Vec<u8> {
        self.header.signing_message()
    }
    fn content_matches(&self) -> bool {
        content_hash(&self.content) == self.header.content_sha256
    }
}

/// Um cabeçalho pronto para a cadeia.
#[derive(Debug, Clone)]
pub struct Attested<S: Attestation> {
    pub header: S::Header,
    pub signatures: Vec<([u8; 32], [u8; 64])>,
    /// Submissões dos membros que assinaram (para o ledger).
    pub observations: Vec<S>,
}

/// Agrupa por mensagem assinada e devolve as que têm ≥ `threshold` membros distintos com
/// assinatura válida e conteúdo que bate com o hash.
pub fn select<S: Attestation>(members: &[[u8; 32]], threshold: usize, subs: &[S]) -> Result<Vec<Attested<S>>> {
    let mut groups: BTreeMap<Vec<u8>, Attested<S>> = BTreeMap::new();
    for s in subs {
        let pk: [u8; 32] = match bs58::decode(s.node()).into_vec().map(<[u8; 32]>::try_from) {
            Ok(Ok(pk)) => pk,
            _ => continue,
        };
        if !members.contains(&pk) || !s.content_matches() {
            continue;
        }
        let msg = s.message();
        let valid = VerifyingKey::from_bytes(&pk)
            .map(|vk| vk.verify_strict(&msg, &Signature::from_bytes(s.signature())).is_ok())
            .unwrap_or(false);
        if !valid {
            continue;
        }
        let g = groups.entry(msg).or_insert_with(|| Attested { header: s.header().clone(), signatures: vec![], observations: vec![] });
        if !g.signatures.iter().any(|(k, _)| *k == pk) {
            g.signatures.push((pk, *s.signature()));
            g.observations.push(s.clone());
        }
    }
    Ok(groups.into_values().filter(|g| g.signatures.len() >= threshold).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(fim_extra: i64) -> ObservedAlert {
        let content = format!("{{\"codigo\":\"x\",\"fim\":{fim_extra}}}");
        ObservedAlert {
            header: PublicAlertHeader {
                issuer: "inmet".into(),
                alert_id: "urn:x".into(),
                content_sha256: content_hash(&content),
                severity: 3,
                onset: 0,
                expires: 86_400 + fim_extra,
            },
            content,
            description: "Acumulado de Chuva".into(),
            severity_label: "Grande Perigo".into(),
            states: "Rio Grande do Sul".into(),
        }
    }

    fn keys() -> (Vec<SigningKey>, Vec<[u8; 32]>) {
        let k: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let m = k.iter().map(|k| k.verifying_key().to_bytes()).collect();
        (k, m)
    }

    #[test]
    fn needs_threshold_of_identical_versions() {
        let (k, m) = keys();
        let subs = vec![sign(&k[0], &observed(0), 1), sign(&k[1], &observed(0), 2), sign(&k[2], &observed(60), 3)];
        let out = select(&m, 2, &subs).unwrap();
        assert_eq!(out.len(), 1, "a versão vista só pelo nó 3 não tem quórum");
        assert_eq!(out[0].signatures.len(), 2);
        assert_eq!(out[0].header.expires, 86_400);
    }

    #[test]
    fn rejects_forged_content_outsiders_and_duplicates() {
        let (k, m) = keys();
        let mut forged = sign(&k[1], &observed(0), 2);
        forged.content.push(' '); // conteúdo não bate com o hash assinado
        let outsider = sign(&SigningKey::from_bytes(&[9; 32]), &observed(0), 2);
        let subs = vec![sign(&k[0], &observed(0), 1), sign(&k[0], &observed(0), 5), forged, outsider];
        assert!(select(&m, 2, &subs).unwrap().is_empty());
    }
}
