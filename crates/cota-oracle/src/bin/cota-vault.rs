//! Parametric vaults: locked SOL that the program releases on its own when Cota records a trigger.
//!
//! - `create`: creates a vault (any wallet can) and records it in `data/ledger/vaults/<name>.json`.
//! - `crank`: walks the ledger's triggers (outlooks that fired, alert receipts) and
//!   calls `trigger_payout` for each vault. Whoever cranks decides nothing: the program checks.

use std::path::{Path, PathBuf};

use anchor_lang::prelude::Pubkey;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use cota_oracle::{chain, keys, rpc::{explorer_tx, Rpc}};
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
    /// Rain outlook that fired the rule (`EarlyWarning`); subject = region.
    EarlyWarning,
    /// Alert receipt from the flow model (`AlertReceipt`); subject = station.
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
            let vault_args = cota::VaultArgs {
                vault_id: id,
                beneficiary,
                kind: match kind {
                    Kind::EarlyWarning => cota::TRIGGER_EARLY_WARNING,
                    Kind::FlowAlert => cota::TRIGGER_FLOW_ALERT,
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

fn state_json(v: &cota::state::Vault, balance: u64) -> Value {
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

/// Triggers known to the ledger: (account, subject, event instant).
fn triggers(ledger: &Path, kind: u8) -> Result<Vec<(Pubkey, String, i64)>> {
    let mut out = Vec::new();
    let read = |dir: PathBuf| -> Result<Vec<Value>> {
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<_> = std::fs::read_dir(&dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
        files.sort();
        files.iter().map(|f| Ok(serde_json::from_str(&std::fs::read_to_string(f)?)?)).collect()
    };
    if kind == cota::TRIGGER_EARLY_WARNING {
        for e in read(ledger.join("outlooks"))? {
            if e["triggered"].as_bool() == Some(true) {
                let h = &e["header"];
                out.push((e["record"].as_str().context("record")?.parse()?, h["region"].as_str().unwrap_or_default().into(), h["issued_at"].as_i64().unwrap_or_default()));
            }
        }
    } else {
        for e in read(ledger.join("forecasts"))? {
            for o in e["onchain"].as_array().into_iter().flatten() {
                if let Some(a) = o["alert"]["account"].as_str() {
                    out.push((a.parse()?, e["station_id"].as_str().unwrap_or_default().into(), e["issued_at"].as_i64().unwrap_or_default()));
                }
            }
        }
    }
    out.sort_by_key(|t| t.2);
    Ok(out)
}

fn crank(rpc: &Rpc, payer: &solana_keypair::Keypair, ledger: &Path, dir: &Path, cluster: &str) -> Result<()> {
    if !dir.exists() {
        eprintln!("nenhum cofre no ledger");
        return Ok(());
    }
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
        for (trigger, subject, event_at) in triggers(ledger, v.kind)? {
            if subject != v.subject || now >= v.expires_at {
                continue;
            }
            let pda = chain::payout_pda(&vault, &trigger);
            let known = entry["payouts"].as_array().is_some_and(|ps| ps.iter().any(|p| p["trigger"] == trigger.to_string()));
            let tx = if rpc.account_data(&pda)?.is_some() {
                if known {
                    continue;
                }
                None // paid by another run; only the record is missing
            } else {
                // Local pre-checks so as not to spend fees on certain rejections; the program is the one
                // that decides.
                if v.payouts > 0 && event_at < v.last_event_at + v.cooldown_secs {
                    continue;
                }
                let recorded_at = match v.kind {
                    cota::TRIGGER_EARLY_WARNING => chain::decode_outlook(&rpc.account_data(&trigger)?.context("gatilho")?)?.committed_at,
                    _ => chain::decode_alert(&rpc.account_data(&trigger)?.context("gatilho")?)?.committed_at,
                };
                if recorded_at < v.accept_from {
                    continue;
                }
                match rpc.send_and_confirm(&[chain::trigger_payout_instruction(payer.pubkey(), vault, trigger, v.beneficiary)], payer) {
                    Ok(sig) => Some(sig),
                    Err(e) => {
                        eprintln!("{}: pagamento por {trigger} recusado: {e:#}", file.display());
                        break;
                    }
                }
            };
            let p = chain::decode_payout(&rpc.account_data(&pda)?.context("pagamento não apareceu on-chain")?)?;
            entry["payouts"].as_array_mut().context("payouts")?.push(json!({
                "trigger": trigger.to_string(),
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
                "{}: pagou {:.3} SOL a {} pelo evento de {} (gatilho {trigger})",
                entry["name"].as_str().unwrap_or("?"),
                p.amount as f64 / SOL,
                v.beneficiary,
                chrono::DateTime::from_timestamp(p.event_at, 0).map(|d| d.date_naive().to_string()).unwrap_or_default(),
            );
        }
        entry["state"] = state_json(&v, rpc.balance(&vault)?);
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        eprintln!("{}: {paid} pagamento(s) novo(s); saldo {:.3} SOL", entry["name"].as_str().unwrap_or("?"), entry["state"]["balance"].as_u64().unwrap_or(0) as f64 / SOL);
    }
    Ok(())
}
