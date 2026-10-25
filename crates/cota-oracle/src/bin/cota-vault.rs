//! Cofres paramétricos: SOL travado que o programa libera sozinho diante de um gatilho do Cota.
//!
//! - `create`: cria um cofre (qualquer carteira pode) e registra em `data/ledger/vaults/<nome>.json`.
//! - `crank`: percorre os gatilhos do ledger (perspectivas que dispararam, recibos de alerta) e
//!   aciona `trigger_payout` para cada cofre. Quem aciona não decide nada: o programa confere.

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
#[command(about = "Cofres paramétricos acionados pelos gatilhos do Cota")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    #[arg(long, default_value = "devnet")]
    cluster: String,
    /// Criador do cofre (`create`) ou quem paga as taxas do acionamento (`crank`).
    #[arg(long)]
    payer: PathBuf,
    #[arg(long, default_value = "data/ledger")]
    ledger: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    /// Perspectiva de chuva que disparou a regra (`EarlyWarning`); assunto = região.
    EarlyWarning,
    /// Recibo de alerta do modelo de vazão (`AlertReceipt`); assunto = estação.
    FlowAlert,
}

#[derive(Subcommand)]
enum Cmd {
    Create {
        /// Nome do arquivo no ledger.
        #[arg(long)]
        name: String,
        #[arg(long)]
        id: u64,
        #[arg(long, value_enum)]
        kind: Kind,
        #[arg(long)]
        subject: String,
        /// Chave pública ou caminho de keypair.
        #[arg(long)]
        beneficiary: String,
        #[arg(long)]
        payout_sol: f64,
        #[arg(long, default_value_t = 7)]
        cooldown_days: i64,
        /// Só gatilhos gravados on-chain a partir de quando: `now` ou segundos UTC (0 = todos).
        #[arg(long, default_value = "now")]
        accept_from: String,
        #[arg(long, default_value_t = 365)]
        expires_days: i64,
        #[arg(long)]
        deposit_sol: f64,
        #[arg(long, default_value = "")]
        description: String,
    },
    Crank,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rpc = Rpc::new(&args.rpc)?;
    let payer = keys::load_keypair(&args.payer)?;
    let dir = args.ledger.join("vaults");
    match args.cmd {
        Cmd::Create { name, id, kind, subject, beneficiary, payout_sol, cooldown_days, accept_from, expires_days, deposit_sol, description } => {
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

/// Gatilhos conhecidos pelo ledger: (conta, assunto, instante do evento).
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
                None // pago por outra execução; só falta o registro
            } else {
                // Pré-checagens locais para não gastar taxa com recusas certas; quem decide é o programa.
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
