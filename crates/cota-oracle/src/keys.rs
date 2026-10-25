//! Chaves no formato de keypair da Solana (`solana-keygen new -o arquivo.json`):
//! JSON com 64 bytes = segredo (32) ‖ chave pública (32).

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

/// Chave de assinatura de um nó-oráculo. Confere que a metade pública corresponde ao segredo.
pub fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let bytes = read_bytes(path)?;
    let key = SigningKey::from_bytes(bytes[..32].try_into().expect("32 bytes"));
    if key.verifying_key().as_bytes() != &bytes[32..] {
        bail!("{}: chave pública não corresponde ao segredo", path.display());
    }
    Ok(key)
}

/// Keypair que paga taxas e assina transações (carteira do agregador/administrador).
pub fn load_keypair(path: &Path) -> Result<Keypair> {
    let bytes = read_bytes(path)?;
    Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn pubkey_base58(key: &SigningKey) -> String {
    bs58::encode(key.verifying_key().as_bytes()).into_string()
}
