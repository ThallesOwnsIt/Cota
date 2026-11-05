//! Aggregator: reads the nodes' signatures for the next batch, picks the header with
//! a quorum and sends `[Ed25519, commit_batch]` to the network. It then assesses the
//! batch's quality and emits `SourceDegraded` for each degraded station.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use cota_oracle::{aggregate, chain, keys, ledger, node::SignedBatch, quality, records, rpc::Rpc};
use cota_quality::Limits;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Cota's k-of-n aggregator")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    /// Cluster name for the explorer links (devnet, localnet...).
    #[arg(long, default_value = "devnet")]
    cluster: String,
    /// Wallet that pays the fees (any account; authorization comes from the nodes' signatures).
    #[arg(long)]
    payer: PathBuf,
    /// Directory with one subfolder per node (`node-1/`, `node-2/`...).
    #[arg(long, default_value = "data/nodes")]
    nodes_dir: PathBuf,
    #[arg(long, default_value = "data/ledger")]
    ledger: PathBuf,
    /// Committee authority, which signs `report_degradation` (default: the same as `--payer`).
    #[arg(long)]
    authority: Option<PathBuf>,
    #[arg(long, default_value = "config/stations.toml")]
    config: PathBuf,
    #[arg(long, default_value = "config/quality-limits.toml")]
    limits: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rpc = Rpc::new(&args.rpc)?;
    let payer = keys::load_keypair(&args.payer)?;

    let committee = chain::decode_registry(
        &rpc.account_data(&chain::registry_pda())?.context("registry not initialized on-chain")?,
    )?;
    let batch_id = committee.next_batch_id;
    let members: Vec<[u8; 32]> = committee.members.iter().map(|m| m.to_bytes()).collect();

    let mut submissions = Vec::new();
    for entry in std::fs::read_dir(&args.nodes_dir).with_context(|| format!("lendo {}", args.nodes_dir.display()))? {
        let file = entry?.path().join(format!("{batch_id:05}.json"));
        if file.exists() {
            let sub: SignedBatch = serde_json::from_str(&std::fs::read_to_string(&file)?)
                .with_context(|| format!("interpretando {}", file.display()))?;
            submissions.push(sub);
        }
    }

    let agg = aggregate::aggregate(&members, committee.threshold as usize, &submissions)
        .with_context(|| format!("lote {batch_id}"))?;
    if agg.header.batch_id != batch_id || agg.header.prev_batch_root != committee.last_batch_root {
        bail!("lote {batch_id}: cabeçalho escolhido não encadeia no estado on-chain");
    }

    let ixs = chain::commit_batch_instructions(payer.pubkey(), &agg.header, &agg.signatures);
    let (sig, appended) = records::send(&rpc, &args.ledger, &ixs, &payer, &[])?;
    let record_index = appended.first().context("commit_batch emitted no record")?.index;

    // Quality of the batch just anchored: the report goes on-chain "on the spot".
    let catalog: Vec<_> = cota_ingest::config::load(&args.config)?.into_iter().map(|s| (s.id, s.source)).collect();
    let limits = Limits::load(&args.limits)?;
    let mut outcome = quality::evaluate(&args.ledger.join("quality-state.json"), &limits, &catalog, &agg)?;
    // The batch is already on-chain: record it before any other transaction can fail.
    ledger::write_entry(&args.ledger, &agg, record_index, sig.clone(), &args.cluster, &outcome)?;
    let authority = match &args.authority {
        Some(p) => keys::load_keypair(p)?,
        None => payer.insecure_clone(),
    };
    for q in &outcome.stations {
        let Some(reason) = q.degraded else { continue };
        let score_milli = (q.score * 1000.0).round().clamp(0.0, 1000.0) as u16;
        let ix = chain::report_degradation_instruction(authority.pubkey(), batch_id, &q.station_id, score_milli, reason as u8);
        let (tx, _) = records::send(&rpc, &args.ledger, &[ix], &payer, &[&authority])?;
        eprintln!("  SourceDegraded {} ({reason:?}, C={:.3})", q.station_id, q.score);
        outcome.degradations.push(quality::DegradationReport {
            station_id: q.station_id.clone(),
            reason,
            score_milli,
            explorer: cota_oracle::rpc::explorer_tx(&tx, &args.cluster),
            tx_signature: tx,
        });
    }
    ledger::write_entry(&args.ledger, &agg, record_index, sig.clone(), &args.cluster, &outcome)?;

    let statuses: Vec<String> = agg
        .members
        .iter()
        .map(|m| format!("{}={}", &m.member[..8], serde_json::to_value(&m.status).unwrap()["status"].as_str().unwrap_or("?")))
        .collect();
    eprintln!(
        "lote {batch_id} ({}) ancorado com {}/{} assinaturas [{}]\n  tx {}",
        agg.batch.day,
        agg.signatures.len(),
        members.len(),
        statuses.join(", "),
        cota_oracle::rpc::explorer_tx(&sig, &args.cluster)
    );
    Ok(())
}
