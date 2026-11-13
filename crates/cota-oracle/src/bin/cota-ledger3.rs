//! Cota's own ledger (v3, `docs/ledger-v3.md`): the round's two sides and a verifier.
//!
//! - `sign` (each node): replays the ledger, signs the checkpoint of the state it verified and what it observed.
//! - `collect` (aggregator): publishes the checkpoint a majority signed and appends every body a majority signed.
//! - `verify` (anyone): replays the ledger and checks every entry and checkpoint.
//!
//! During the rehearsal the ledger is `data/ledger-v3-ensaio`, written next to v2 without touching it.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use cota_oracle::{keys, ledger3};

#[derive(Parser)]
#[command(about = "Cota's own ledger (v3): sign, collect, verify")]
struct Args {
    /// The ledger's folder.
    #[arg(long, global = true, default_value = "data/ledger-v3-ensaio")]
    ledger: PathBuf,
    /// The v2 ledger (its records.jsonl precedes Genesis in the accumulator).
    #[arg(long, global = true, default_value = "data/ledger")]
    v2_ledger: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Sign {
        #[arg(long)]
        key: PathBuf,
        /// This node's folder (its v2 observations; writes `v3/`).
        #[arg(long)]
        node_dir: PathBuf,
        #[arg(long, default_value = "config/ledger-v3.toml")]
        config: PathBuf,
        /// RPC to read v2's committee (the trust anchor for Genesis); without it, the config's list is used.
        #[arg(long)]
        rpc: Option<String>,
    },
    Collect {
        #[arg(long, default_value = "data/nodes")]
        nodes_dir: PathBuf,
    },
    Verify {
        /// RPC to compare Genesis with v2's committee on the chain.
        #[arg(long)]
        rpc: Option<String>,
    },
    /// Prints the day whose flow forecast the nodes should make now (nothing if none).
    ForecastDay {
        #[arg(long, default_value = "config/ledger-v3.toml")]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Sign { key, node_dir, config, rpc } => {
            let key = keys::load_signing_key(&key)?;
            let cfg = ledger3::load_config(&config)?;
            let book = ledger3::Book::open(&args.ledger, &args.v2_ledger)?;
            ledger3::check_trust(&cfg, &book, rpc.as_deref())?;
            let n = ledger3::sign_round(&key, &cfg, &book, &node_dir)?;
            let cp = book.next_checkpoint().map(|c| format!(", ponto de controle {}", c.size)).unwrap_or_default();
            eprintln!("[{}] livro v3: {n} entrada(s) assinada(s){cp}", &keys::pubkey_base58(&key)[..8]);
        }
        Cmd::Collect { nodes_dir } => {
            let mut book = ledger3::Book::open(&args.ledger, &args.v2_ledger)?;
            let report = ledger3::collect(&mut book, &nodes_dir, chrono::Utc::now().timestamp())?;
            if let Some((size, signers)) = report.checkpoint {
                eprintln!("livro v3: ponto de controle {size} publicado ({signers} assinaturas)");
            }
            let mut appended = std::collections::BTreeMap::<String, usize>::new();
            for r in &report.results {
                match r {
                    ledger3::Collected::Appended { kind, .. } => *appended.entry(kind.clone()).or_default() += 1,
                    ledger3::Collected::NoQuorum { kind, signers, need } => {
                        eprintln!("livro v3: {kind} sem quórum ({signers} de {need} assinaturas); fica para a próxima rodada")
                    }
                    ledger3::Collected::Refused { kind, reason } => eprintln!("livro v3: {kind} recusada: {reason}"),
                }
            }
            let summary: Vec<String> = appended.iter().map(|(k, n)| format!("{n} {k}")).collect();
            eprintln!(
                "livro v3: {} entrada(s) nova(s){}; {} folhas, relógio {}",
                appended.values().sum::<usize>(),
                if summary.is_empty() { String::new() } else { format!(" ({})", summary.join(", ")) },
                book.ledger.size(),
                ledger3::iso(book.ledger.clock()),
            );
        }
        Cmd::ForecastDay { config } => {
            let cfg = ledger3::load_config(&config)?;
            let book = ledger3::Book::open(&args.ledger, &args.v2_ledger)?;
            if let Some(day) = ledger3::forecast_day(&cfg, &book) {
                println!("{day}");
            }
        }
        Cmd::Verify { rpc } => {
            let book = ledger3::Book::open(&args.ledger, &args.v2_ledger)?;
            let (readings_ok, problems) = ledger3::check_readings(&book);
            match book.ledger.genesis_index() {
                None => println!("livro v3 ainda sem registro de abertura ({} registros da v2)", book.ledger.size()),
                Some(g) => {
                    let committee = book.ledger.committee().expect("after Genesis");
                    println!("abertura   registro {g} (depois de {g} registros da v2)");
                    println!("entradas   {} da v3, {} folhas no total", book.entries.len(), book.ledger.size());
                    println!(
                        "comitê     época {}: {} de {} ({})",
                        book.ledger.epoch().unwrap_or(0),
                        committee.threshold,
                        committee.members.len(),
                        committee.members.iter().map(ledger3::member_name).collect::<Vec<_>>().join(", ")
                    );
                    if let Some(p) = book.ledger.pending() {
                        println!("pendente   comitê da época {} vale a partir de {}", p.epoch, ledger3::iso(p.effective_at));
                    }
                    println!("relógio    {}", ledger3::iso(book.ledger.clock()));
                    println!("raiz       {}", hex::encode(book.ledger.root().unwrap_or_default()));
                    println!("pontos     {} ponto(s) de controle conferido(s)", book.checkpoints.len());
                    println!("leituras   {readings_ok} lote(s) refeitos a partir das leituras publicadas, com a raiz assinada");
                    let opened = match book.entries.first().map(|e| &e.body) {
                        Some(cota_ledger::Body::Genesis(g)) => g.members.clone(),
                        _ => Vec::new(),
                    };
                    let mut bad = problems;
                    match rpc.as_deref().map(ledger3::chain_members) {
                        Some(Some(m)) if m == opened => println!("confiança  o comitê da abertura é o comitê da v2 na cadeia"),
                        Some(Some(_)) => bad.push("o comitê da abertura difere do comitê da v2 na cadeia".into()),
                        Some(None) => bad.push("não consegui ler o comitê da v2 na cadeia".into()),
                        None => println!("confiança  (sem --rpc: o comitê da abertura não foi comparado com a cadeia)"),
                    }
                    if !bad.is_empty() {
                        anyhow::bail!("{} problema(s):\n- {}", bad.len(), bad.join("\n- "));
                    }
                    println!("tudo confere: cada entrada passou pelas regras e cada ponto de controle bate com o livro");
                }
            }
        }
    }
    Ok(())
}
