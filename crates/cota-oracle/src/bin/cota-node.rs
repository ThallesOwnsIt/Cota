//! Uma rodada de um nó-oráculo: ingere um dia, monta o lote e assina o cabeçalho.
//!
//! O número do lote e a raiz anterior vêm da cadeia (`--rpc`) ou são passados à mão
//! (`--batch-id` + `--prev-root`), para rodar sem rede.

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
#[command(about = "Nó-oráculo do Cota")]
struct Args {
    /// Keypair do nó (formato solana-keygen).
    #[arg(long)]
    key: PathBuf,
    /// Dia UTC a ancorar.
    #[arg(long)]
    day: NaiveDate,
    /// RPC para ler o estado do comitê (next_batch_id, last_batch_root).
    #[arg(long, conflicts_with_all = ["batch_id", "prev_root"])]
    rpc: Option<String>,
    #[arg(long, requires = "prev_root")]
    batch_id: Option<u64>,
    /// Raiz do lote anterior em hex (64 zeros para o primeiro).
    #[arg(long)]
    prev_root: Option<String>,
    #[arg(long, default_value = "config/stations.toml")]
    config: PathBuf,
    #[arg(long, default_value = "data/raw")]
    cache: PathBuf,
    /// Diretório deste nó; grava `{batch_id:05}.json`.
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
            let data = Rpc::new(url)?.account_data(&chain::committee_pda())?.context("comitê não inicializado on-chain")?;
            let c = chain::decode_committee(&data)?;
            (c.next_batch_id, c.last_batch_root)
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
            // Um arquivo antigo para o mesmo lote não pode contar como presença.
            let _ = std::fs::remove_file(&path);
            eprintln!("[{}] lote {batch_id} ({}): nó mudo", &keys::pubkey_base58(&key)[..8], args.day);
        }
    }
    Ok(())
}
