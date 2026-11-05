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
    #[arg(long, default_value = "data/raw")]
    cache: PathBuf,
    /// This node's directory; writes `{batch_id:05}.json`.
    #[arg(long)]
    out: PathBuf,
    #[arg(long, value_enum, default_value = "none")]
    fault: FaultArg,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let key = keys::load_signing_key(&args.key)?;
    let stations = cota_ingest::config::load(&args.config)?;

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
    std::fs::create_dir_all(&args.out)?;
    let path = args.out.join(format!("{batch_id:05}.json"));

    match node::sign_day(&key, &stations, &args.cache, args.day, batch_id, prev_root, fault)? {
        Some(signed) => {
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
