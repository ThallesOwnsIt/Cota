//! Committee administration: initialization, station registration and status.

use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use cota_oracle::{chain, chain::Record, keys, outlook, records, rpc::{explorer_tx, Rpc}};
use serde::Deserialize;
use serde_json::json;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Cota on-chain administration")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    /// Wallet of the committee authority.
    #[arg(long)]
    payer: PathBuf,
    /// Ledger whose `records.jsonl` mirrors the accumulator.
    #[arg(long, global = true, default_value = "data/ledger")]
    ledger: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Creates the registry with the committee's public keys.
    Init {
        /// The nodes' keypairs (only the public half goes on-chain).
        #[arg(long, num_args = 1.., required = true)]
        members: Vec<PathBuf>,
        #[arg(long, default_value_t = 2)]
        threshold: u8,
    },
    /// Registers on-chain the catalog stations not registered yet.
    RegisterStations {
        #[arg(long, default_value = "config/stations.toml")]
        config: PathBuf,
    },
    /// Fixes on-chain the alert threshold set during training (the manifest's `alert_rule`).
    SetAlertRule {
        #[arg(long, default_value = "ml/models/manifest.json")]
        manifest: PathBuf,
    },
    /// Records the forecasts from a `cota_ml.forecast` file and issues the applicable alerts.
    CommitForecast {
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "devnet")]
        cluster: String,
    },
    /// Hands the committee authority to another key (e.g. the GitHub Actions operator).
    SetAuthority {
        /// Keypair or public key (base58) of the new authority.
        #[arg(long)]
        new: String,
    },
    /// Shows the committee state and the wallet balance.
    /// Fixes (once) the early-warning rule from `ml/models/outlook-rule.json`.
    SetOutlookRule {
        #[arg(long, default_value = "ml/models/outlook-rule.json")]
        rule: PathBuf,
    },
    Status,
}

#[derive(Deserialize)]
struct ForecastFile {
    station_id: String,
    issue_day: String,
    based_on_batch_id: u64,
    based_on_root: String,
    model_sha256: String,
    features_root: String,
    forecasts: Vec<Horizon>,
}

#[derive(Deserialize)]
struct Horizon {
    horizon_hours: u16,
    target_ts: i64,
    p10: i64,
    p50: i64,
    p90: i64,
}

fn hex32(s: &str) -> Result<[u8; 32]> {
    hex::decode(s)?.try_into().map_err(|_| anyhow::anyhow!("hash de 32 bytes esperado"))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rpc = Rpc::new(&args.rpc)?;
    let payer = keys::load_keypair(&args.payer)?;
    let ledger = &args.ledger;

    match args.cmd {
        Cmd::Init { members, threshold } => {
            let pubkeys = members
                .iter()
                .map(|p| keys::load_signing_key(p).map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())))
                .collect::<Result<Vec<_>>>()?;
            let sig = rpc.send_and_confirm(&[chain::init_registry_instruction(payer.pubkey(), pubkeys.clone(), threshold)], &payer)?;
            eprintln!("registry {threshold}-de-{} criado em {}\n  tx {sig}", pubkeys.len(), chain::registry_pda());
        }
        Cmd::RegisterStations { config } => {
            records::sync(&rpc, ledger)?;
            let registered: Vec<String> = records::load(ledger)?
                .iter()
                .filter_map(|s| match s.decode() {
                    Ok(Record::Station(st)) => Some(st.station.station_id),
                    _ => None,
                })
                .collect();
            for st in cota_ingest::config::load(&config)? {
                if registered.contains(&st.id) {
                    eprintln!("{:<16} já registrada", st.id);
                    continue;
                }
                let (_, appended) = records::send(&rpc, ledger, &[chain::register_station_instruction(payer.pubkey(), &st)], &payer, &[])
                    .with_context(|| format!("registrando {}", st.id))?;
                eprintln!("{:<16} registrada (registro {})", st.id, appended[0].index);
            }
        }
        Cmd::SetAlertRule { manifest } => {
            let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest)?)?;
            let rule = &m["alert_rule"];
            let station = rule["station_id"].as_str().context("alert_rule.station_id")?;
            let threshold = rule["threshold_milli"].as_i64().context("alert_rule.threshold_milli")?;
            let level = rule["level"].as_u64().context("alert_rule.level")? as u8;
            let sig = rpc.send_and_confirm(&[chain::set_alert_rule_instruction(payer.pubkey(), station, threshold, level)], &payer)?;
            eprintln!("regra de alerta {station}: P90 ≥ {:.0} (nível {level}) em {}\n  tx {sig}", threshold as f64 / 1000.0, chain::alert_rule_pda(station));
        }
        Cmd::CommitForecast { file, cluster } => {
            let f: ForecastFile = serde_json::from_str(&std::fs::read_to_string(&file)?)
                .with_context(|| format!("interpretando {}", file.display()))?;
            let registry = chain::decode_registry(&rpc.account_data(&chain::registry_pda())?.context("registry not initialized")?)?;
            if registry.next_batch_id - 1 != f.based_on_batch_id || hex::encode(registry.last_batch_root) != f.based_on_root {
                bail!("a previsão foi feita sobre o lote {}, mas o último ancorado é {}", f.based_on_batch_id, registry.next_batch_id - 1);
            }

            let mut onchain = Vec::new();
            for h in &f.forecasts {
                let args = chain::ForecastArgs {
                    station_id: f.station_id.clone(),
                    target_ts: h.target_ts,
                    horizon_hours: h.horizon_hours,
                    p10: h.p10,
                    p50: h.p50,
                    p90: h.p90,
                    model_sha256: hex32(&f.model_sha256)?,
                    features_root: hex32(&f.features_root)?,
                };
                let (tx, appended) =
                    records::send(&rpc, ledger, &[chain::commit_forecast_instruction(payer.pubkey(), args)], &payer, &[])?;
                let a = appended.first().context("commit_forecast emitted no record")?;
                let Record::Forecast(rec) = a.record()? else { bail!("record {} is not a forecast", a.index) };
                // The alert, if any, was decided by the program in the same instruction.
                let alert = rec.alert.as_ref().map(|al| json!({ "threshold_milli": al.threshold_milli, "level": al.level }));
                eprintln!(
                    "{} +{}h: P10 {:.0}  P50 {:.0}  P90 {:.0}{}",
                    f.issue_day,
                    h.horizon_hours,
                    h.p10 as f64 / 1e3,
                    h.p50 as f64 / 1e3,
                    h.p90 as f64 / 1e3,
                    if alert.is_some() { "  → ALERTA" } else { "" }
                );
                onchain.push(json!({
                    "horizon_hours": h.horizon_hours,
                    "target_ts": h.target_ts,
                    "p10": h.p10, "p50": h.p50, "p90": h.p90,
                    "record_index": a.index,
                    "slot": rec.slot,
                    "committed_at": rec.committed_at,
                    "tx_signature": tx,
                    "explorer": explorer_tx(&tx, &cluster),
                    "alert": alert,
                }));
            }
            let mut entry: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
            entry["cluster"] = json!(cluster);
            entry["onchain"] = json!(onchain);
            let dir = ledger.join("forecasts");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(format!("{}.json", f.issue_day)), serde_json::to_string_pretty(&entry)?)?;
        }
        Cmd::SetAuthority { new } => {
            let new_pk: Pubkey = match new.parse() {
                Ok(pk) => pk,
                Err(_) => keys::load_keypair(std::path::Path::new(&new))?.pubkey(),
            };
            let sig = rpc.send_and_confirm(&[chain::set_authority_instruction(payer.pubkey(), new_pk)], &payer)?;
            eprintln!("autoridade do comitê: {} → {new_pk}\n  tx {sig}", payer.pubkey());
        }
        Cmd::SetOutlookRule { rule } => {
            let r = outlook::load_rule(&rule)?;
            let args = chain::OutlookRuleArgs {
                region: r.spec.region.clone(),
                threshold_milli: r.threshold_milli(),
                window_secs: r.window_secs(),
                min_models: r.spec.min_models,
                rule_sha256: r.sha256,
            };
            let sig = rpc.send_and_confirm(&[chain::set_outlook_rule_instruction(payer.pubkey(), args)], &payer)?;
            eprintln!(
                "regra de alerta antecipado {}: chuva ≥ {} mm em {} dias (≥ {} modelos), rule_sha256 {}\n  conta {}\n  tx {sig}",
                r.spec.region,
                r.spec.threshold_mm,
                r.spec.window_days,
                r.spec.min_models,
                hex::encode(r.sha256),
                chain::outlook_rule_pda(&r.spec.region),
            );
        }
        Cmd::Status => {
            println!("programa   {}", chain::PROGRAM_ID);
            println!("carteira   {} ({:.4} SOL)", payer.pubkey(), rpc.balance(&payer.pubkey())? as f64 / 1e9);
            match rpc.account_data(&chain::registry_pda())? {
                None => println!("registry   não inicializado"),
                Some(data) => {
                    let r = chain::decode_registry(&data)?;
                    println!("registry   {} ({}-de-{})", chain::registry_pda(), r.threshold, r.members.len());
                    println!("autoridade {}", r.authority);
                    for (i, m) in r.members.iter().enumerate() {
                        println!("  membro {i}  {m}");
                    }
                    println!("próximo    lote {}", r.next_batch_id);
                    println!("últ. raiz  {}", hex::encode(r.last_batch_root));
                    println!("registros  {} na árvore", r.leaf_count);
                    let local = records::load(ledger)?;
                    let ok = records::mmr(&local).map(|m| m.peaks() == r.peaks && m.len() == r.leaf_count).unwrap_or(false);
                    println!("ledger     {} registros, {}", local.len(), if ok { "picos conferem" } else { "DIVERGE da cadeia (rode sync)" });
                    if let Some(rule) = rpc.account_data(&chain::alert_rule_pda("GLOFAS-GUAIBA"))? {
                        let r = chain::decode_alert_rule(&rule)?;
                        println!("alerta     {} P90 ≥ {:.0}", r.station_id, r.threshold_milli as f64 / 1000.0);
                    }
                    if let Some(rule) = rpc.account_data(&chain::outlook_rule_pda("guaiba"))? {
                        let r = chain::decode_outlook_rule(&rule)?;
                        println!("antecipado {} chuva ≥ {:.0} mm em {} d", r.region, r.threshold_milli as f64 / 1000.0, r.window_secs / 86_400);
                    }
                }
            }
        }
    }
    Ok(())
}
