//! Administração do comitê: inicialização, registro de estações e status.

use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use cota_oracle::{chain, keys, outlook, rpc::{explorer_tx, Rpc}};
use serde::Deserialize;
use serde_json::json;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Administração do Cota on-chain")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    /// Carteira da autoridade do comitê.
    #[arg(long)]
    payer: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cria o comitê com as chaves públicas dos nós.
    Init {
        /// Keypairs dos nós (só a parte pública vai on-chain).
        #[arg(long, num_args = 1.., required = true)]
        members: Vec<PathBuf>,
        #[arg(long, default_value_t = 2)]
        threshold: u8,
    },
    /// Registra on-chain as estações do catálogo que ainda não existem.
    RegisterStations {
        #[arg(long, default_value = "config/stations.toml")]
        config: PathBuf,
    },
    /// Fixa on-chain o limiar de alerta definido no treino (`alert_rule` do manifesto).
    SetAlertRule {
        #[arg(long, default_value = "ml/models/manifest.json")]
        manifest: PathBuf,
    },
    /// Grava as previsões de um arquivo do `cota_ml.forecast` e emite os alertas cabíveis.
    CommitForecast {
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "devnet")]
        cluster: String,
        /// Onde registrar as contas e transações criadas.
        #[arg(long, default_value = "data/ledger")]
        ledger: PathBuf,
    },
    /// Passa a autoridade do comitê para outra chave (ex.: o operador do GitHub Actions).
    SetAuthority {
        /// Keypair ou chave pública (base58) da nova autoridade.
        #[arg(long)]
        new: String,
    },
    /// Mostra o estado do comitê e o saldo da carteira.
    /// Fixa (uma vez) a regra do alerta antecipado a partir de `ml/models/outlook-rule.json`.
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

    match args.cmd {
        Cmd::Init { members, threshold } => {
            let pubkeys = members
                .iter()
                .map(|p| keys::load_signing_key(p).map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())))
                .collect::<Result<Vec<_>>>()?;
            let sig = rpc.send_and_confirm(&[chain::init_committee_instruction(payer.pubkey(), pubkeys.clone(), threshold)], &payer)?;
            eprintln!("comitê {threshold}-de-{} criado em {}\n  tx {sig}", pubkeys.len(), chain::committee_pda());
        }
        Cmd::RegisterStations { config } => {
            for st in cota_ingest::config::load(&config)? {
                let pda = chain::station_pda(&st.id);
                if rpc.account_data(&pda)?.is_some() {
                    eprintln!("{:<16} já registrada", st.id);
                    continue;
                }
                rpc.send_and_confirm(&[chain::register_station_instruction(payer.pubkey(), &st)], &payer)
                    .with_context(|| format!("registrando {}", st.id))?;
                eprintln!("{:<16} registrada em {pda}", st.id);
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
        Cmd::CommitForecast { file, cluster, ledger } => {
            let f: ForecastFile = serde_json::from_str(&std::fs::read_to_string(&file)?)
                .with_context(|| format!("interpretando {}", file.display()))?;
            let committee = chain::decode_committee(&rpc.account_data(&chain::committee_pda())?.context("comitê não inicializado")?)?;
            if committee.next_batch_id - 1 != f.based_on_batch_id || hex::encode(committee.last_batch_root) != f.based_on_root {
                bail!("a previsão foi feita sobre o lote {}, mas o último ancorado é {}", f.based_on_batch_id, committee.next_batch_id - 1);
            }
            let rule = rpc.account_data(&chain::alert_rule_pda(&f.station_id))?.map(|d| chain::decode_alert_rule(&d)).transpose()?;

            let mut records = Vec::new();
            for h in &f.forecasts {
                let args = cota::ForecastArgs {
                    station_id: f.station_id.clone(),
                    target_ts: h.target_ts,
                    horizon_hours: h.horizon_hours,
                    p10: h.p10,
                    p50: h.p50,
                    p90: h.p90,
                    model_sha256: hex32(&f.model_sha256)?,
                    features_root: hex32(&f.features_root)?,
                };
                let pda = chain::forecast_pda(&f.station_id, f.based_on_batch_id, h.target_ts);
                let tx = rpc.send_and_confirm(&[chain::commit_forecast_instruction(payer.pubkey(), f.based_on_batch_id, args)], &payer)?;
                let mut alert = None;
                if let Some(r) = rule.as_ref().filter(|r| h.p90 >= r.threshold_milli) {
                    let atx = rpc.send_and_confirm(&[chain::issue_alert_instruction(payer.pubkey(), pda, &f.station_id)], &payer)?;
                    alert = Some(json!({
                        "account": chain::alert_pda(&pda).to_string(),
                        "threshold_milli": r.threshold_milli,
                        "level": r.level,
                        "tx_signature": atx,
                        "explorer": explorer_tx(&atx, &cluster),
                    }));
                }
                eprintln!(
                    "{} +{}h: P10 {:.0}  P50 {:.0}  P90 {:.0}{}",
                    f.issue_day,
                    h.horizon_hours,
                    h.p10 as f64 / 1e3,
                    h.p50 as f64 / 1e3,
                    h.p90 as f64 / 1e3,
                    if alert.is_some() { "  → ALERTA" } else { "" }
                );
                records.push(json!({
                    "horizon_hours": h.horizon_hours,
                    "target_ts": h.target_ts,
                    "p10": h.p10, "p50": h.p50, "p90": h.p90,
                    "account": pda.to_string(),
                    "tx_signature": tx,
                    "explorer": explorer_tx(&tx, &cluster),
                    "alert": alert,
                }));
            }
            let mut entry: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
            entry["cluster"] = json!(cluster);
            entry["onchain"] = json!(records);
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
            let args = cota::OutlookRuleArgs {
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
            match rpc.account_data(&chain::committee_pda())? {
                None => println!("comitê     não inicializado"),
                Some(data) => {
                    let c = chain::decode_committee(&data)?;
                    println!("comitê     {} ({}-de-{})", chain::committee_pda(), c.threshold, c.members.len());
                    println!("autoridade {}", c.authority);
                    for (i, m) in c.members.iter().enumerate() {
                        println!("  membro {i}  {m}");
                    }
                    println!("próximo    lote {}", c.next_batch_id);
                    println!("últ. raiz  {}", hex::encode(c.last_batch_root));
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
