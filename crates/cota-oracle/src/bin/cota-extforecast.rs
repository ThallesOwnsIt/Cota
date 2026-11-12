//! Third-party forecasts attested by Cota's committee (Etapa 3), kept in the ledger until program v3 records them.
//!
//! - `observe` (each node, independently): downloads the GloFAS discharge forecast for the 30 days after day D and
//!   signs its canonical content; writes `<out>/glofas-<D>.json`.
//! - `collect` (aggregator): keeps in `data/ledger/external_forecasts/glofas/<D>.json` the version the most
//!   committee members signed, with their signatures and whether that reached the threshold. A version that reached
//!   it is never replaced; one that did not is replaced if a later round reaches it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use cota_oracle::{attest, chain, extforecast, keys, rpc::Rpc};
use serde_json::json;

#[derive(Parser)]
#[command(about = "Third-party forecasts attested by Cota's committee (GloFAS discharge)")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Observe {
        #[arg(long)]
        key: PathBuf,
        /// Day D (UTC) that has just closed; the forecast covers D+1..D+30.
        #[arg(long)]
        day: NaiveDate,
        #[arg(long, default_value = "config/stations-live.toml")]
        config: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Collect {
        #[arg(long, default_value = "http://127.0.0.1:8899")]
        rpc: String,
        #[arg(long, default_value = "data/nodes")]
        nodes_dir: PathBuf,
        #[arg(long, default_value = "data/ledger")]
        ledger: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().cmd {
        Cmd::Observe { key, day, config, out } => observe(&key, day, &config, &out),
        Cmd::Collect { rpc, nodes_dir, ledger } => collect(&rpc, &nodes_dir, &ledger),
    }
}

fn observe(key: &Path, day: NaiveDate, config: &Path, out: &Path) -> Result<()> {
    let key = keys::load_signing_key(key)?;
    let file = out.join(format!("{}-{day}.json", extforecast::GLOFAS));
    if file.exists() {
        eprintln!("previsão GloFAS de {day} já assinada");
        return Ok(());
    }
    let catalog = cota_ingest::config::load(config)?;
    let content = extforecast::fetch_glofas(&cota_ingest::http_client()?, &catalog, day)?;
    let signed = extforecast::sign(&key, &content, chrono::Utc::now().timestamp())?;
    std::fs::create_dir_all(out)?;
    std::fs::write(&file, serde_json::to_string_pretty(&signed)?)?;
    // The headline: the highest median and the highest of the ensemble over the 30 days, at the basin's outlet.
    let outlet = content.points.iter().find(|p| p.id == "GLOFAS-GUAIBA").unwrap_or(&content.points[0]);
    let peak = |f: fn(&extforecast::DayForecast) -> Option<i64>| outlet.days.iter().filter_map(f).max().unwrap_or(0) as f64 / 1000.0;
    eprintln!(
        "[{}] {day}: previsão GloFAS de 30 dias em {} pontos; {}: mediana até {:.0} m³/s, cenário máximo {:.0} m³/s",
        &keys::pubkey_base58(&key)[..8],
        content.points.len(),
        outlet.id,
        peak(|d| d.median),
        peak(|d| d.max),
    );
    Ok(())
}

fn collect(rpc_url: &str, nodes_dir: &Path, ledger: &Path) -> Result<()> {
    let rpc = Rpc::new(rpc_url)?;
    let committee = chain::decode_registry(&rpc.account_data(&chain::registry_pda())?.context("registry not initialized")?)?;
    let members: Vec<[u8; 32]> = committee.members.iter().map(|m| m.to_bytes()).collect();
    let threshold = committee.threshold as usize;

    // Every node's submissions, grouped by (source, day).
    let mut by_day: BTreeMap<(String, NaiveDate), Vec<extforecast::SignedForecast>> = BTreeMap::new();
    if nodes_dir.exists() {
        for node in std::fs::read_dir(nodes_dir)? {
            let dir = node?.path().join("extforecast");
            if !dir.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&dir)? {
                let f = f?.path();
                let s: extforecast::SignedForecast =
                    serde_json::from_str(&std::fs::read_to_string(&f)?).with_context(|| format!("interpretando {}", f.display()))?;
                by_day.entry((s.header.source.clone(), s.header.day)).or_default().push(s);
            }
        }
    }

    for ((source, day), subs) in by_day {
        // Valid signatures of members, grouped by content; the largest group is the version kept.
        let Some(best) = attest::select(&members, 1, &subs)?.into_iter().max_by_key(|g| g.signatures.len()) else {
            continue;
        };
        let quorum = best.signatures.len() >= threshold;
        let dir = ledger.join("external_forecasts").join(&source);
        let file = dir.join(format!("{day}.json"));
        if let Ok(old) = std::fs::read_to_string(&file) {
            let had_quorum = serde_json::from_str::<serde_json::Value>(&old).ok().and_then(|v| v["quorum"].as_bool()).unwrap_or(false);
            if had_quorum || !quorum {
                continue;
            }
        }
        let entry = json!({
            "source": source,
            "day": day.to_string(),
            "header": best.header,
            "domain": String::from_utf8_lossy(extforecast::DOMAIN),
            "quorum": quorum,
            "threshold": threshold,
            "signatures": best.observations.iter().map(|o| json!({
                "member": o.node,
                "signature": hex::encode(o.signature),
                "seen_at": o.seen_at,
            })).collect::<Vec<_>>(),
            "content": best.observations[0].content,
        });
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        // A replaced version (without quorum before) loses the Bitcoin proof of the old bytes; scripts/timestamp.py
        // stamps the new ones.
        let _ = std::fs::remove_file(dir.join(format!("{day}.json.ots")));
        eprintln!(
            "previsão {source} de {day}: {} assinatura(s) de {}{}",
            best.signatures.len(),
            members.len(),
            if quorum { " (quórum)" } else { " (SEM quórum: guardada assim mesmo, com aviso)" }
        );
    }
    Ok(())
}
