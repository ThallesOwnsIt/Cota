//! Keys in Solana keypair format (`solana-keygen new -o file.json`):
//! JSON with 64 bytes = secret (32) ‖ public key (32).

use std::path::Path;

use anyhow::{bail, Context, Result};
use ed25519_dalek::SigningKey;
use solana_keypair::Keypair;

fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lendo chave {}", path.display()))?;
    let bytes: Vec<u8> = serde_json::from_str(&text).with_context(|| format!("{} não é um keypair JSON", path.display()))?;
    if bytes.len() != 64 {
        bail!("{}: keypair deve ter 64 bytes, tem {}", path.display(), bytes.len());
    }
    Ok(bytes)
}

/// Signing key of an oracle node. Checks that the public half matches the secret.
pub fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let bytes = read_bytes(path)?;
    let key = SigningKey::from_bytes(bytes[..32].try_into().expect("32 bytes"));
    if key.verifying_key().as_bytes() != &bytes[32..] {
        bail!("{}: chave pública não corresponde ao segredo", path.display());
    }
    Ok(key)
}

/// Keypair that pays fees and signs transactions (the aggregator's/administrator's wallet).
pub fn load_keypair(path: &Path) -> Result<Keypair> {
    let bytes = read_bytes(path)?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn pubkey_base58(key: &SigningKey) -> String {
    bs58::encode(key.verifying_key().as_bytes()).into_string()
}
