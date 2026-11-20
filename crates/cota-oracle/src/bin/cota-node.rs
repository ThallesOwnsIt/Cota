//! One round of an oracle node: ingests a day, builds the batch and signs the header.
//!
//! The batch number and the previous root come from the chain (`--rpc`) or are passed by hand
//! (`--batch-id` + `--prev-root`), to run without a network.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use clap::{Parser, ValueEnum};
use cota_oracle::{chain, keys, node, rpc::Rpc};

#[derive(Clone, Copy, ValueEnum)]
enum FaultArg {
    None,
    Silent,
    Tamper,
}

#[derive(Parser)]
#[command(about = "Cota oracle node")]
struct Args {
    /// The node's keypair (solana-keygen format).
    #[arg(long)]
    key: PathBuf,
    /// UTC day to anchor.
    #[arg(long)]
    day: NaiveDate,
    /// RPC used to read the registry state (next_batch_id, last_batch_root).
    #[arg(long, conflicts_with_all = ["batch_id", "prev_root"])]
    rpc: Option<String>,
    #[arg(long, requires = "prev_root")]
    batch_id: Option<u64>,
    /// Previous batch's root in hex (64 zeros for the first).
    #[arg(long)]
    prev_root: Option<String>,
    #[arg(long, default_value = "config/stations.toml")]
    config: PathBuf,
    /// v3: the ledger's config; the stations are those of `--stream` in its catalogs (instead of `--config`), and the
    /// batch file goes to that stream's folder.
    #[arg(long, requires = "stream")]
    ledger_config: Option<PathBuf>,
    /// v3: the stream (region) whose batch this is.
    #[arg(long, requires = "ledger_config")]
    stream: Option<String>,
    #[arg(long, default_value = "data/raw")]
    cache: PathBuf,
    /// This node's directory; writes `{batch_id:05}.json`.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, value_enum, default_value = "none")]
    fault: FaultArg,
    /// Hours after the day closes during which the node waits (signs nothing, tries again next round) when a whole
    /// source did not answer, instead of signing a day without it.
    #[arg(long, default_value_t = node::COMPLETE_BY_HOURS)]
    complete_by_hours: i64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let key = keys::load_signing_key(&args.key)?;
    let (stations, out) = match (&args.ledger_config, &args.stream) {
        (Some(path), Some(stream)) => {
            let cfg = cota_oracle::ledger3::load_config(path)?;
            (cfg.stream_stations(stream)?, cfg.stream_dir(&args.out, stream))
        }
        _ => (cota_ingest::config::load(&args.config)?, args.out.clone()),
    };

    let (batch_id, prev_root) = match (&args.rpc, args.batch_id, &args.prev_root) {
        (Some(url), _, _) => {
            let data = Rpc::new(url)?.account_data(&chain::registry_pda())?.context("registry not initialized on-chain")?;
            let r = chain::decode_registry(&data)?;
            (r.next_batch_id, r.last_batch_root)
        }
        (None, Some(id), Some(root)) => {
            let mut r = [0u8; 32];
            hex::decode_to_slice(root, &mut r).context("--prev-root deve ter 64 dígitos hex")?;
            (id, r)
        }
        _ => bail!("informe --rpc ou --batch-id e --prev-root"),
    };

    let fault = match args.fault {
        FaultArg::None => node::Fault::None,
        FaultArg::Silent => node::Fault::Silent,
        FaultArg::Tamper => node::Fault::Tamper,
    };
    std::fs::create_dir_all(&out)?;
    let path = out.join(format!("{batch_id:05}.json"));

    match node::sign_day(&key, &stations, &args.cache, args.day, batch_id, prev_root, fault)? {
        Some(signed) => {
            let now = chrono::Utc::now().timestamp();
            if let Some(reason) = node::wait_reason(&signed.ingest, args.day, now, args.complete_by_hours) {
                // Not a signature on a day a source missed: the day stays pending and the next round tries again.
                let _ = std::fs::remove_file(&path);
                let deadline = cota_ingest::end_of_day(args.day) + args.complete_by_hours * 3600;
                eprintln!(
                    "[{}] lote {batch_id} ({}): {reason}. Não assino ainda; tento de novo na próxima rodada (até {} UTC, depois assino o que houver)",
                    &signed.node[..8],
                    args.day,
                    chrono::DateTime::from_timestamp(deadline, 0).map(|d| d.format("%d/%m %H:%M").to_string()).unwrap_or_default(),
                );
                return Ok(());
            }
            if !node::sources_down(&signed.ingest).is_empty() {
                eprintln!("[{}] aviso: o prazo para as fontes responderem passou; assino {} sem elas", &signed.node[..8], args.day);
            }
            std::fs::write(&path, serde_json::to_string_pretty(&signed)?)?;
            eprintln!(
                "[{}] lote {batch_id} ({}) raiz {} com {} leituras{}",
                &signed.node[..8],
                args.day,
                &hex::encode(signed.header.merkle_root)[..16],
                signed.header.leaf_count,
                if matches!(fault, node::Fault::Tamper) { "  ⚠ ADULTERADO" } else { "" }
            );
        }
        None => {
            // An old file for the same batch must not count as presence.
            let _ = std::fs::remove_file(&path);
            eprintln!("[{}] lote {batch_id} ({}): nó mudo", &keys::pubkey_base58(&key)[..8], args.day);
        }
    }
    Ok(())
}
