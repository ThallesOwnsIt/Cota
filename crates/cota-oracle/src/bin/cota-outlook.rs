//! Early warning from the forecast rain over the basin.
//!
//! - `observe` (each node, independently): downloads each model's 00Z run of day D,
//!   summarizes the rain for D+1..D+5 and signs it; writes to `data/nodes/node-N/outlook/`.
//! - `anchor` (aggregator, right after day D's batch): takes to the chain the outlook seen
//!   by ≥ k members; the program applies the rule and emits `EarlyWarning` if it fires.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Days, NaiveDate};
use clap::{Parser, Subcommand};
use cota_oracle::{attest, chain, chain::Record, keys, outlook, records, rpc::{explorer_tx, Rpc}};
use serde_json::json;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Forecast rain attested by Cota's committee (early warning)")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Observe {
        #[arg(long)]
        key: PathBuf,
        /// Day D (UTC) that has just closed; the window is D+1..D+5.
        #[arg(long)]
        day: NaiveDate,
        /// The basin-wide rule.
        #[arg(long, default_value = "ml/models/outlook-rule.json")]
        rule: PathBuf,
        /// Sub-basin rules (one outlook per region); a missing folder means the basin-wide rule only.
        #[arg(long, default_value = "ml/models/outlook-rules")]
        rules_dir: PathBuf,
        /// More rule folders (Etapa 5: the rest of RS, `ml/models/outlook-rules-rs`), for the v3 ledger.
        #[arg(long)]
        more_rules: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        /// Reads the daily totals (`outlook::Totals`) from a file instead of the API (tests; basin-wide rule only).
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
        Cmd::Observe { key, day, rule, rules_dir, more_rules, out, input } => observe(&key, day, &rule, &rules_dir, &more_rules, &out, input.as_deref()),
        Cmd::Anchor { rpc, cluster, payer, nodes_dir, ledger } => anchor(&rpc, &cluster, &payer, &nodes_dir, &ledger),
    }
}

fn observe(key: &Path, day: NaiveDate, rule: &Path, rules_dir: &Path, more_rules: &[PathBuf], out: &Path, input: Option<&Path>) -> Result<()> {
    let key = keys::load_signing_key(key)?;
    let mut rules = if input.is_some() { vec![outlook::load_rule(rule)?] } else { outlook::load_rules(rule, rules_dir)? };
    if input.is_none() {
        for dir in more_rules {
            rules.extend(outlook::load_dir(dir)?);
        }
    }
    let http = cota_ingest::http_client()?;
    let mut failed = 0;
    for rule in &rules {
        let region = &rule.spec.region;
        let file = out.join(outlook::file_name(region, day));
        if file.exists() {
            eprintln!("perspectiva {region} de {day} já assinada");
            continue;
        }
        let totals = match input {
            Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)?,
            None => outlook::fetch(&http, rule, day),
        };
        // One region without data does not stop the others.
        let o = match outlook::summarize(rule, day, &totals) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("perspectiva {region} de {day}: {e:#}");
                failed += 1;
                continue;
            }
        };
        std::fs::create_dir_all(out)?;
        std::fs::write(&file, serde_json::to_string_pretty(&outlook::sign(&key, &o, chrono::Utc::now().timestamp()))?)?;
        eprintln!(
            "[{}] {day} {region}: chuva prevista em {} dias {:.1} mm (modelos {}: {:.1}–{:.1} mm), limiar {} mm",
            &keys::pubkey_base58(&key)[..8],
            rule.spec.window_days,
            o.header.rain_mean_milli as f64 / 1000.0,
            o.header.models,
            o.header.rain_min_milli as f64 / 1000.0,
            o.header.rain_max_milli as f64 / 1000.0,
            rule.spec.threshold_mm,
        );
    }
    if failed == rules.len() {
        bail!("nenhuma perspectiva de chuva assinada para {day}");
    }
    Ok(())
}

fn day_of(issued_at: i64) -> NaiveDate {
    DateTime::from_timestamp(issued_at, 0).expect("timestamp válido").date_naive() - Days::new(1)
}

fn anchor(rpc_url: &str, cluster: &str, payer: &Path, nodes_dir: &Path, ledger: &Path) -> Result<()> {
    let rpc = Rpc::new(rpc_url)?;
    let payer = keys::load_keypair(payer)?;
    let committee = chain::decode_registry(&rpc.account_data(&chain::registry_pda())?.context("registry not initialized")?)?;
    records::sync(&rpc, ledger)?;
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
        let file = out_dir.join(outlook::file_name(&h.region, day));
        if file.exists() {
            continue;
        }
        let existing = records::load(ledger)?.into_iter().find(|s| {
            matches!(s.decode(), Ok(Record::Outlook(o)) if o.header.region == h.region && o.header.issued_at == h.issued_at)
        });
        let (index, tx) = match existing {
            Some(s) => (s.index, s.tx),
            None if h.issued_at != committee.last_t_end => {
                // The program only accepts the outlook for the day that has just been anchored.
                eprintln!("perspectiva {} de {day} fora de hora (último lote termina em {}); ignorada", h.region, committee.last_t_end);
                continue;
            }
            // A sub-basin rule goes on-chain through the multisig (24 h wait): until it is there, its outlooks stay
            // with the nodes instead of failing the round.
            None if rpc.account_data(&chain::outlook_rule_pda(&h.region))?.is_none() => {
                eprintln!("perspectiva {} de {day}: a regra ainda não está na cadeia; fica com os nós", h.region);
                continue;
            }
            None => {
                let ixs = chain::commit_outlook_instructions(payer.pubkey(), h, &a.signatures);
                let (tx, appended) = records::send(&rpc, ledger, &ixs, &payer, &[])?;
                (appended.first().context("commit_outlook emitted no record")?.index, tx)
            }
        };
        let Record::Outlook(rec) = records::load(ledger)?[index as usize].decode()? else {
            bail!("record {index} is not an outlook")
        };
        let entry = json!({
            "day": day.to_string(),
            "header": h,
            "content": a.observations[0].content,
            "record_index": index,
            "rule": chain::outlook_rule_pda(&h.region).to_string(),
            "based_on_batch_id": rec.based_on_batch_id,
            "threshold_milli": rec.threshold_milli,
            "triggered": rec.triggered,
            "slot": rec.slot,
            "committed_at": rec.committed_at,
            "signers": rec.signers,
            "explorer": explorer_tx(&tx, cluster),
            "tx_signature": tx,
            "cluster": cluster,
            "observations": a.observations.iter().map(|o| json!({"member": o.node, "seen_at": o.seen_at})).collect::<Vec<_>>(),
        });
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        eprintln!(
            "perspectiva {} de {day}: {:.1} mm em 5 dias (limiar {:.0}) → {} — slot {}, {} assinatura(s)",
            h.region,
            rec.header.rain_mean_milli as f64 / 1000.0,
            rec.threshold_milli as f64 / 1000.0,
            if rec.triggered { "ALERTA ANTECIPADO" } else { "sem alerta" },
            rec.slot,
            rec.signer_count,
        );
    }
    Ok(())
}
