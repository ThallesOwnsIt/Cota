//! Alerta antecipado pela chuva prevista na bacia.
//!
//! - `observe` (cada nó, de forma independente): baixa a rodada 00Z do dia D de cada modelo,
//!   resume a chuva de D+1..D+5 e assina; grava em `data/nodes/node-N/outlook/`.
//! - `anchor` (agregador, logo depois do lote do dia D): leva à cadeia a perspectiva vista
//!   por ≥ k membros; o programa aplica a regra e emite `EarlyWarning` se ela disparar.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Days, NaiveDate};
use clap::{Parser, Subcommand};
use cota_oracle::{attest, chain, keys, outlook, rpc::{explorer_tx, Rpc}};
use serde_json::json;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Chuva prevista atestada pelo comitê do Cota (alerta antecipado)")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Observe {
        #[arg(long)]
        key: PathBuf,
        /// Dia D (UTC) que acabou de fechar; a janela é D+1..D+5.
        #[arg(long)]
        day: NaiveDate,
        #[arg(long, default_value = "ml/models/outlook-rule.json")]
        rule: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Lê os totais diários (`outlook::Totals`) de um arquivo em vez da API (testes).
        #[arg(long)]
        input: Option<PathBuf>,
    },
    Anchor {
        #[arg(long, default_value = "http://127.0.0.1:8899")]
        rpc: String,
        #[arg(long, default_value = "devnet")]
        cluster: String,
        #[arg(long)]
        payer: PathBuf,
        #[arg(long, default_value = "data/nodes")]
        nodes_dir: PathBuf,
        #[arg(long, default_value = "data/ledger")]
        ledger: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().cmd {
        Cmd::Observe { key, day, rule, out, input } => observe(&key, day, &rule, &out, input.as_deref()),
        Cmd::Anchor { rpc, cluster, payer, nodes_dir, ledger } => anchor(&rpc, &cluster, &payer, &nodes_dir, &ledger),
    }
}

fn observe(key: &Path, day: NaiveDate, rule: &Path, out: &Path, input: Option<&Path>) -> Result<()> {
    let key = keys::load_signing_key(key)?;
    let rule = outlook::load_rule(rule)?;
    let file = out.join(format!("{day}.json"));
    if file.exists() {
        eprintln!("perspectiva de {day} já assinada");
        return Ok(());
    }
    let totals = match input {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)?,
        None => outlook::fetch(&cota_ingest::http_client()?, &rule, day),
    };
    let o = outlook::summarize(&rule, day, &totals)?;
    std::fs::create_dir_all(out)?;
    std::fs::write(&file, serde_json::to_string_pretty(&outlook::sign(&key, &o, chrono::Utc::now().timestamp()))?)?;
    eprintln!(
        "[{}] {day}: chuva prevista em {} dias {:.1} mm (modelos {}: {:.1}–{:.1} mm), limiar {} mm",
        &keys::pubkey_base58(&key)[..8],
        rule.spec.window_days,
        o.header.rain_mean_milli as f64 / 1000.0,
        o.header.models,
        o.header.rain_min_milli as f64 / 1000.0,
        o.header.rain_max_milli as f64 / 1000.0,
        rule.spec.threshold_mm,
    );
    Ok(())
}

fn day_of(issued_at: i64) -> NaiveDate {
    DateTime::from_timestamp(issued_at, 0).expect("timestamp válido").date_naive() - Days::new(1)
}

fn anchor(rpc_url: &str, cluster: &str, payer: &Path, nodes_dir: &Path, ledger: &Path) -> Result<()> {
    let rpc = Rpc::new(rpc_url)?;
    let payer = keys::load_keypair(payer)?;
    let committee = chain::decode_committee(&rpc.account_data(&chain::committee_pda())?.context("comitê não inicializado")?)?;
    let members: Vec<[u8; 32]> = committee.members.iter().map(|m| m.to_bytes()).collect();

    let mut subs = Vec::new();
    if nodes_dir.exists() {
        for node in std::fs::read_dir(nodes_dir)? {
            let dir = node?.path().join("outlook");
            if !dir.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&dir)? {
                let f = f?.path();
                let s: outlook::SignedOutlook =
                    serde_json::from_str(&std::fs::read_to_string(&f)?).with_context(|| format!("interpretando {}", f.display()))?;
                subs.push(s);
            }
        }
    }

    let out_dir = ledger.join("outlooks");
    std::fs::create_dir_all(&out_dir)?;
    for a in attest::select(&members, committee.threshold as usize, &subs)? {
        let h = &a.header;
        let day = day_of(h.issued_at);
        let file = out_dir.join(format!("{day}.json"));
        if file.exists() {
            continue;
        }
        let pda = chain::outlook_pda(&h.region, h.issued_at);
        let tx = if rpc.account_data(&pda)?.is_some() {
            None
        } else if h.issued_at != committee.last_t_end {
            // O programa só aceita a perspectiva do dia que acabou de ser ancorado.
            eprintln!("perspectiva de {day} fora de hora (último lote termina em {}); ignorada", committee.last_t_end);
            continue;
        } else {
            Some(rpc.send_and_confirm(&chain::commit_outlook_instructions(payer.pubkey(), h, &a.signatures), &payer)?)
        };
        let rec = chain::decode_outlook(&rpc.account_data(&pda)?.context("perspectiva não apareceu on-chain")?)?;
        let entry = json!({
            "day": day.to_string(),
            "header": h,
            "content": a.observations[0].content,
            "record": pda.to_string(),
            "rule": chain::outlook_rule_pda(&h.region).to_string(),
            "based_on_batch_id": rec.based_on_batch_id,
            "threshold_milli": rec.threshold_milli,
            "triggered": rec.triggered,
            "slot": rec.slot,
            "committed_at": rec.committed_at,
            "signers": rec.signers,
            "tx_signature": tx,
            "explorer": tx.as_ref().map(|t| explorer_tx(t, cluster)),
            "cluster": cluster,
            "observations": a.observations.iter().map(|o| json!({"member": o.node, "seen_at": o.seen_at})).collect::<Vec<_>>(),
        });
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        eprintln!(
            "perspectiva de {day}: {:.1} mm em 5 dias (limiar {:.0}) → {} — slot {}, {} assinatura(s)",
            rec.rain_mean_milli as f64 / 1000.0,
            rec.threshold_milli as f64 / 1000.0,
            if rec.triggered { "ALERTA ANTECIPADO" } else { "sem alerta" },
            rec.slot,
            rec.signer_count,
        );
    }
    Ok(())
}
