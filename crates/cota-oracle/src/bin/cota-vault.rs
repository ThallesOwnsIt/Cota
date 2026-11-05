//! Parametric vaults: locked SOL that the program releases on its own when Cota records a trigger.
//!
//! - `create`: creates a vault (any wallet can) and records it in `data/ledger/vaults/<name>.json`.
//! - `crank`: walks the accumulator's triggers (outlooks that fired, forecasts with an alert) and
//!   calls `trigger_payout` for each vault with the record's inclusion proof. Whoever cranks decides
//!   nothing: the program checks the proof and the conditions.

use std::path::{Path, PathBuf};

use anchor_lang::prelude::Pubkey;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use cota_oracle::{chain, chain::Record, keys, records, rpc::{explorer_tx, Rpc}};
use serde_json::{json, Value};
use solana_signer::Signer;

const SOL: f64 = 1e9;
const DAY: i64 = 86_400;

#[derive(Parser)]
#[command(about = "Parametric vaults triggered by Cota's triggers")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    #[arg(long, default_value = "devnet")]
    cluster: String,
    /// Vault creator (`create`) or whoever pays the crank's fees (`crank`).
    #[arg(long)]
    payer: PathBuf,
    #[arg(long, default_value = "data/ledger")]
    ledger: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    /// Rain outlook that fired the rule; subject = region.
    EarlyWarning,
    /// Flow forecast whose P90 crossed the station's rule; subject = station.
    FlowAlert,
}

#[derive(Subcommand)]
enum Cmd {
    Create {
        /// File name in the ledger.
        #[arg(long)]
        name: String,
        #[arg(long)]
        id: u64,
        #[arg(long, value_enum)]
        kind: Kind,
        #[arg(long)]
        subject: String,
        /// Public key or keypair path.
        #[arg(long)]
        beneficiary: String,
        #[arg(long)]
        payout_sol: f64,
        #[arg(long, default_value_t = 7)]
        cooldown_days: i64,
        /// Only triggers recorded on-chain from this moment: `now` or UTC seconds (0 = all).
        #[arg(long, default_value = "now")]
        accept_from: String,
        #[arg(long, default_value_t = 365)]
        expires_days: i64,
        #[arg(long)]
        deposit_sol: f64,
        #[arg(long, default_value = "")]
        description: String,
        /// The same description in English, for the dashboard.
        #[arg(long, default_value = "")]
        description_en: String,
    },
    Crank,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rpc = Rpc::new(&args.rpc)?;
    let payer = keys::load_keypair(&args.payer)?;
    let dir = args.ledger.join("vaults");
    match args.cmd {
        Cmd::Create { name, id, kind, subject, beneficiary, payout_sol, cooldown_days, accept_from, expires_days, deposit_sol, description, description_en } => {
            let beneficiary: Pubkey = match beneficiary.parse() {
                Ok(pk) => pk,
                Err(_) => keys::load_keypair(Path::new(&beneficiary))?.pubkey(),
            };
            let now = chrono::Utc::now().timestamp();
            let accept_from = if accept_from == "now" { now } else { accept_from.parse().context("--accept-from: `now` ou segundos")? };
            let vault_args = chain::VaultArgs {
                vault_id: id,
                beneficiary,
                kind: match kind {
                    Kind::EarlyWarning => chain::TRIGGER_EARLY_WARNING,
                    Kind::FlowAlert => chain::TRIGGER_FLOW_ALERT,
                },
                subject,
                payout_lamports: (payout_sol * SOL).round() as u64,
                cooldown_secs: cooldown_days * DAY,
                accept_from,
                expires_at: now + expires_days * DAY,
                deposit_lamports: (deposit_sol * SOL).round() as u64,
            };
            let vault = chain::vault_pda(&payer.pubkey(), id);
            let file = dir.join(format!("{name}.json"));
            if file.exists() {
                bail!("{} já existe", file.display());
            }
            let sig = rpc.send_and_confirm(&[chain::create_vault_instruction(payer.pubkey(), vault_args)], &payer)?;
            let v = chain::decode_vault(&rpc.account_data(&vault)?.context("cofre não apareceu on-chain")?)?;
            std::fs::create_dir_all(&dir)?;
            let entry = json!({
                "name": name,
                "description": description,
            "description_en": description_en,
                "vault": vault.to_string(),
                "created_tx": sig,
                "created_explorer": explorer_tx(&sig, &args.cluster),
                "cluster": args.cluster,
                "state": state_json(&v, rpc.balance(&vault)?),
                "payouts": [],
            });
            std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
            eprintln!("cofre {name} criado: {vault} ({deposit_sol} SOL, paga {payout_sol} SOL por evento)\n  tx {sig}");
        }
        Cmd::Crank => crank(&rpc, &payer, &args.ledger, &dir, &args.cluster)?,
    }
    Ok(())
}

fn state_json(v: &chain::Vault, balance: u64) -> Value {
    json!({
        "creator": v.creator.to_string(),
        "vault_id": v.vault_id,
        "beneficiary": v.beneficiary.to_string(),
        "kind": v.kind,
        "subject": v.subject,
        "payout_lamports": v.payout_lamports,
        "cooldown_secs": v.cooldown_secs,
        "accept_from": v.accept_from,
        "expires_at": v.expires_at,
        "last_event_at": v.last_event_at,
        "payouts": v.payouts,
        "total_paid": v.total_paid,
        "created_at": v.created_at,
        "balance": balance,
    })
}

/// A trigger in the accumulator.
struct Trigger {
    index: u64,
    leaf: [u8; 32],
    subject: String,
    event_at: i64,
    recorded_at: i64,
}

/// Triggers of `kind` in the ledger's copy of the accumulator, oldest event first.
fn triggers(stored: &[records::Stored], kind: u8) -> Result<Vec<Trigger>> {
    let mut out = Vec::new();
    for st in stored {
        let t = match (kind, st.decode()?) {
            (chain::TRIGGER_EARLY_WARNING, Record::Outlook(o)) if o.triggered => (o.header.region, o.header.issued_at, o.committed_at),
            (chain::TRIGGER_FLOW_ALERT, Record::Forecast(f)) if f.alert.is_some() => (f.station_id, f.issued_at, f.committed_at),
            _ => continue,
        };
        let leaf = hex::decode(&st.leaf)?.try_into().map_err(|_| anyhow::anyhow!("leaf"))?;
        out.push(Trigger { index: st.index, leaf, subject: t.0, event_at: t.1, recorded_at: t.2 });
    }
    out.sort_by_key(|t| (t.event_at, t.index));
    Ok(out)
}

fn crank(rpc: &Rpc, payer: &solana_keypair::Keypair, ledger: &Path, dir: &Path, cluster: &str) -> Result<()> {
    if !dir.exists() {
        eprintln!("nenhum cofre no ledger");
        return Ok(());
    }
    records::sync(rpc, ledger)?;
    let now = chrono::Utc::now().timestamp();
    let mut files: Vec<_> = std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    files.sort();
    for file in files {
        let mut entry: Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
        let vault: Pubkey = entry["vault"].as_str().context("vault")?.parse()?;
        let Some(data) = rpc.account_data(&vault)? else {
            eprintln!("{}: cofre fechado", file.display());
            continue;
        };
        let mut v = chain::decode_vault(&data)?;
        let mut paid = 0;
        let stored = records::load(ledger)?;
        for t in triggers(&stored, v.kind)? {
            if t.subject != v.subject || now >= v.expires_at {
                continue;
            }
            let pda = chain::payout_pda(&vault, &t.leaf);
            let known = entry["payouts"].as_array().is_some_and(|ps| ps.iter().any(|p| p["trigger_index"] == json!(t.index)));
            let tx = if rpc.account_data(&pda)?.is_some() {
                if known {
                    continue;
                }
                None // paid by another run; only the record is missing
            } else {
                // Local pre-checks so as not to spend fees on certain rejections; the program is the one
                // that decides.
                if v.payouts > 0 && t.event_at < v.last_event_at + v.cooldown_secs || t.recorded_at < v.accept_from {
                    continue;
                }
                // The proof is against the current peaks: rebuild it right before sending.
                records::sync(rpc, ledger)?;
                let proof = records::proof(&records::load(ledger)?, t.index)?;
                match rpc.send_and_confirm(&[chain::trigger_payout_instruction(payer.pubkey(), vault, v.beneficiary, proof)], payer) {
                    Ok(sig) => Some(sig),
                    Err(e) => {
                        eprintln!("{}: pagamento pelo registro {} recusado: {e:#}", file.display(), t.index);
                        break;
                    }
                }
            };
            let p = chain::decode_payout(&rpc.account_data(&pda)?.context("pagamento não apareceu on-chain")?)?;
            entry["payouts"].as_array_mut().context("payouts")?.push(json!({
                "trigger_index": t.index,
                "trigger_leaf": hex::encode(t.leaf),
                "payout": pda.to_string(),
                "event_at": p.event_at,
                "amount": p.amount,
                "slot": p.slot,
                "paid_at": p.paid_at,
                "tx_signature": tx,
                "explorer": tx.as_ref().map(|t| explorer_tx(t, cluster)),
            }));
            v = chain::decode_vault(&rpc.account_data(&vault)?.context("cofre")?)?;
            paid += 1;
            eprintln!(
                "{}: pagou {:.3} SOL a {} pelo evento de {} (registro {})",
                entry["name"].as_str().unwrap_or("?"),
                p.amount as f64 / SOL,
                v.beneficiary,
                chrono::DateTime::from_timestamp(p.event_at, 0).map(|d| d.date_naive().to_string()).unwrap_or_default(),
                t.index,
            );
        }
        entry["state"] = state_json(&v, rpc.balance(&vault)?);
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        eprintln!("{}: {paid} pagamento(s) novo(s); saldo {:.3} SOL", entry["name"].as_str().unwrap_or("?"), entry["state"]["balance"].as_u64().unwrap_or(0) as f64 / SOL);
    }
    Ok(())
}
