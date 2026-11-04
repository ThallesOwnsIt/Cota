//! Attestation of external data (official warnings, forecast rain): each node signs what it saw;
//! the aggregator only takes to the chain the version that ≥ k members saw with identical content.

use std::collections::BTreeMap;

use anyhow::Result;
use cota_core::{public_alert::content_hash, PublicAlertHeader};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::inmet_alerts::ObservedAlert;
use crate::keys::pubkey_base58;

/// What a node publishes about one version of a warning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAlert {
    pub node: String,
    pub header: PublicAlertHeader,
    #[serde(with = "crate::node::hex64")]
    pub signature: [u8; 64],
    /// Canonical content; `header.content_sha256` is the hash of these bytes.
    pub content: String,
    /// When the node saw the warning (local clock, informational only; the "when" that counts is
    /// the slot).
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

/// Something a node signs and publishes together with the content the header hashes.
pub trait Attestation: Clone {
    type Header: Clone;
    fn node(&self) -> &str;
    fn signature(&self) -> &[u8; 64];
    fn header(&self) -> &Self::Header;
    fn message(&self) -> Vec<u8>;
    /// The published content is what the header's hash says.
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

/// A header ready for the chain.
#[derive(Debug, Clone)]
pub struct Attested<S: Attestation> {
    pub header: S::Header,
    pub signatures: Vec<([u8; 32], [u8; 64])>,
    /// Submissions from the members who signed (for the ledger).
    pub observations: Vec<S>,
}

/// Groups by signed message and returns those with ≥ `threshold` distinct members with a
/// valid signature and content matching the hash.
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
        forged.content.push(' '); // content does not match the signed hash
        let outsider = sign(&SigningKey::from_bytes(&[9; 32]), &observed(0), 2);
        let subs = vec![sign(&k[0], &observed(0), 1), sign(&k[0], &observed(0), 5), forged, outsider];
        assert!(select(&m, 2, &subs).unwrap().is_empty());
    }
}
