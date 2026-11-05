//! The accumulator's records, as seen off-chain.
//!
//! The program emits every record it appends in a `RecordAppended` event. Whoever sends a transaction reads
//! the events back from its logs and appends them to `{ledger}/records.jsonl`, one line per record, in
//! accumulator order. From that file anyone can rebuild the MMR (and must reach the peaks stored on-chain)
//! and build the inclusion proofs the vaults need.
//!
//! Records appended by someone else (any account may submit committee-signed records) are picked up by
//! [`sync`], which reads the registry's transaction history.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anchor_lang::{AnchorDeserialize, Discriminator};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine as _;
use cota_core::{merkle::hash_leaf, mmr::Mmr};
use cota_v2::{events::RecordAppended, Record, TriggerProof};
use serde::{Deserialize, Serialize};

use crate::{chain, rpc::Rpc};

/// One line of `records.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stored {
    pub index: u64,
    /// `H(0x00 ‖ record)`, hex.
    pub leaf: String,
    /// Borsh of the `Record`, hex: exactly the bytes that were hashed.
    pub record: String,
    pub kind: String,
    pub tx: String,
}

impl Stored {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(hex::decode(&self.record)?)
    }

    pub fn decode(&self) -> Result<Record> {
        Record::try_from_slice(&self.bytes()?).with_context(|| format!("record {} does not decode", self.index))
    }
}

/// An appended record read from transaction logs.
#[derive(Debug, Clone)]
pub struct Appended {
    pub index: u64,
    pub leaf: [u8; 32],
    pub bytes: Vec<u8>,
}

impl Appended {
    pub fn record(&self) -> Result<Record> {
        Ok(Record::try_from_slice(&self.bytes)?)
    }
}

pub fn kind(r: &Record) -> &'static str {
    match r {
        Record::Station(_) => "station",
        Record::Batch(_) => "batch",
        Record::Degradation(_) => "degradation",
        Record::Forecast(_) => "forecast",
        Record::PublicAlert(_) => "public_alert",
        Record::Outlook(_) => "outlook",
    }
}

/// `RecordAppended` events in the logs of one transaction, in order.
pub fn parse_logs(logs: &[String]) -> Result<Vec<Appended>> {
    let mut out = Vec::new();
    for line in logs {
        let Some(b64) = line.strip_prefix("Program data: ") else { continue };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else { continue };
        if bytes.len() < 48 || bytes[..8] != *RecordAppended::DISCRIMINATOR {
            continue;
        }
        let index = u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
        let leaf: [u8; 32] = bytes[16..48].try_into().expect("32 bytes");
        let record = bytes[48..].to_vec();
        ensure!(hash_leaf(&record) == leaf, "event {index}: leaf does not match the record");
        out.push(Appended { index, leaf, bytes: record });
    }
    Ok(out)
}

fn path(ledger: &Path) -> PathBuf {
    ledger.join("records.jsonl")
}

pub fn load(ledger: &Path) -> Result<Vec<Stored>> {
    let p = path(ledger);
    if !p.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let mut out: Vec<Stored> = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let s: Stored = serde_json::from_str(line).with_context(|| format!("{}:{}", p.display(), n + 1))?;
        ensure!(s.index == out.len() as u64, "{}: record {} out of order", p.display(), s.index);
        out.push(s);
    }
    Ok(out)
}

/// Appends the records of transaction `tx` that the file does not have yet. They must continue the file
/// without a gap (a gap means someone else appended records first: run [`sync`]).
pub fn store(ledger: &Path, tx: &str, appended: &[Appended]) -> Result<()> {
    let have = load(ledger)?.len() as u64;
    std::fs::create_dir_all(ledger)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path(ledger))?;
    let mut next = have;
    for a in appended.iter().filter(|a| a.index >= have) {
        ensure!(a.index == next, "records.jsonl has {next} records, transaction {tx} brings #{}: run sync first", a.index);
        let s = Stored {
            index: a.index,
            leaf: hex::encode(a.leaf),
            record: hex::encode(&a.bytes),
            kind: kind(&a.record()?).to_string(),
            tx: tx.to_string(),
        };
        writeln!(f, "{}", serde_json::to_string(&s)?)?;
        next += 1;
    }
    Ok(())
}

pub fn mmr(stored: &[Stored]) -> Result<Mmr> {
    let mut m = Mmr::new();
    for s in stored {
        let leaf = hash_leaf(&s.bytes()?);
        ensure!(hex::encode(leaf) == s.leaf, "record {}: stored leaf does not match its bytes", s.index);
        m.push_leaf(leaf);
    }
    Ok(m)
}

/// Inclusion proof of record `index`, against the accumulator rebuilt from the ledger.
pub fn proof(stored: &[Stored], index: u64) -> Result<TriggerProof> {
    let m = mmr(stored)?;
    let p = m.proof(index).with_context(|| format!("record {index} is not in the ledger"))?;
    Ok(TriggerProof { record: stored[index as usize].bytes()?, leaf_index: index, siblings: p.siblings })
}

/// Sends the instructions, reads the appended records back from the transaction and stores them.
/// Returns the signature and the records.
pub fn send(
    rpc: &Rpc,
    ledger: &Path,
    instructions: &[anchor_lang::solana_program::instruction::Instruction],
    payer: &solana_keypair::Keypair,
    others: &[&solana_keypair::Keypair],
) -> Result<(String, Vec<Appended>)> {
    sync(rpc, ledger)?;
    let sig = rpc.send_and_confirm_with(instructions, payer, others)?;
    let appended = parse_logs(&rpc.transaction_logs(&sig)?)?;
    if store(ledger, &sig, &appended).is_err() {
        // Someone else appended records between our sync and our transaction: read the history.
        sync(rpc, ledger)?;
    }
    Ok((sig, appended))
}

/// Brings `records.jsonl` up to the registry's `leaf_count`, reading the events of the registry's
/// transactions that the file is missing. Then checks the peaks.
pub fn sync(rpc: &Rpc, ledger: &Path) -> Result<()> {
    let Some(data) = rpc.account_data(&chain::registry_pda())? else { return Ok(()) };
    let registry = chain::decode_registry(&data)?;
    let have = load(ledger)?.len() as u64;
    if have < registry.leaf_count {
        // Newest first: walk back until the missing range is covered, then store oldest first.
        let mut found: Vec<(String, Vec<Appended>)> = Vec::new();
        let mut before: Option<String> = None;
        'pages: loop {
            let page = rpc.signatures_for_address(&chain::registry_pda(), before.as_deref(), 100)?;
            if page.is_empty() {
                break;
            }
            for sig in &page {
                let appended = parse_logs(&rpc.transaction_logs(sig)?)?;
                let reaches_known = appended.iter().any(|a| a.index <= have);
                if !appended.is_empty() {
                    found.push((sig.clone(), appended));
                }
                if reaches_known {
                    break 'pages;
                }
            }
            before = page.last().cloned();
        }
        found.reverse();
        for (sig, appended) in &found {
            store(ledger, sig, appended)?;
        }
    }
    let stored = load(ledger)?;
    if stored.len() as u64 != registry.leaf_count {
        bail!("ledger has {} records, registry {}: history incomplete", stored.len(), registry.leaf_count);
    }
    let m = mmr(&stored)?;
    ensure!(m.peaks() == registry.peaks, "rebuilt accumulator does not match the on-chain peaks");
    Ok(())
}
