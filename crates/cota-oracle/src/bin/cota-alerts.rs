//! Avisos oficiais com prova on-chain.
//!
//! - `observe` (cada nó, de forma independente): consulta o INMET, assina cada versão de
//!   aviso dos estados configurados e grava em `data/nodes/node-N/alerts/`.
//! - `anchor` (agregador): leva à cadeia as versões vistas por ≥ k membros e registra o
//!   conteúdo em `data/ledger/public_alerts/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use cota_oracle::{attest, chain, inmet_alerts, keys, rpc::{explorer_tx, Rpc}};
use serde_json::json;
use solana_signer::Signer;

#[derive(Parser)]
#[command(about = "Avisos oficiais (INMET) atestados pelo comitê do Cota")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Observe {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Estados por extenso, como a API do INMET escreve.
        #[arg(long, default_value = "Rio Grande do Sul", value_delimiter = ',')]
        states: Vec<String>,
        /// Lê a resposta de um arquivo em vez da API (testes).
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
        Cmd::Observe { key, out, states, input } => observe(&key, &out, &states, input.as_deref()),
        Cmd::Anchor { rpc, cluster, payer, nodes_dir, ledger } => anchor(&rpc, &cluster, &payer, &nodes_dir, &ledger),
    }
}

fn observe(key: &Path, out: &Path, states: &[String], input: Option<&Path>) -> Result<()> {
    let key = keys::load_signing_key(key)?;
    let body = match input {
        Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)?,
        None => inmet_alerts::fetch(&cota_ingest::http_client()?)?,
    };
    let alerts = inmet_alerts::parse(&body, states)?;
    std::fs::create_dir_all(out)?;
    let now = chrono::Utc::now().timestamp();
    let mut new = 0;
    for a in &alerts {
        let file = out.join(format!("{}.json", hex::encode(a.header.content_sha256)));
        if file.exists() {
            continue; // já assinado: o primeiro "visto" do nó não muda
        }
        std::fs::write(&file, serde_json::to_string_pretty(&attest::sign(&key, a, now))?)?;
        new += 1;
    }
    eprintln!("[{}] {} aviso(s) vigente(s) em {:?}, {new} nova(s) versão(ões) assinada(s)", &keys::pubkey_base58(&key)[..8], alerts.len(), states);
    Ok(())
}

fn anchor(rpc_url: &str, cluster: &str, payer: &Path, nodes_dir: &Path, ledger: &Path) -> Result<()> {
    let rpc = Rpc::new(rpc_url)?;
    let payer = keys::load_keypair(payer)?;
    let committee = chain::decode_committee(&rpc.account_data(&chain::committee_pda())?.context("comitê não inicializado")?)?;
    let members: Vec<[u8; 32]> = committee.members.iter().map(|m| m.to_bytes()).collect();

    let mut subs = Vec::new();
    if nodes_dir.exists() {
        for node in std::fs::read_dir(nodes_dir)? {
            let dir = node?.path().join("alerts");
            if !dir.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&dir)? {
                let f = f?.path();
                let s: attest::SignedAlert =
                    serde_json::from_str(&std::fs::read_to_string(&f)?).with_context(|| format!("interpretando {}", f.display()))?;
                subs.push(s);
            }
        }
    }

    let out_dir = ledger.join("public_alerts");
    std::fs::create_dir_all(&out_dir)?;
    let mut anchored = 0;
    for a in attest::select(&members, committee.threshold as usize, &subs)? {
        let sha = hex::encode(a.header.content_sha256);
        let file = out_dir.join(format!("{sha}.json"));
        let pda = chain::public_alert_pda(&a.header.content_sha256);
        if file.exists() {
            continue;
        }
        let tx = if rpc.account_data(&pda)?.is_some() {
            None // já ancorado por outra execução; só falta o registro local
        } else {
            Some(rpc.send_and_confirm(&chain::record_public_alert_instructions(payer.pubkey(), &a.header, &a.signatures), &payer)?)
        };
        let rec = chain::decode_public_alert(&rpc.account_data(&pda)?.context("aviso não apareceu on-chain")?)?;
        let first = &a.observations[0];
        let entry = json!({
            "header": a.header,
            "content": first.content,
            "description": first.description,
            "severity_label": first.severity_label,
            "states": first.states,
            "record": pda.to_string(),
            "slot": rec.slot,
            "committed_at": rec.committed_at,
            "signers": rec.signers,
            "tx_signature": tx,
            "explorer": tx.as_ref().map(|t| explorer_tx(t, cluster)),
            "cluster": cluster,
            "observations": a.observations.iter().map(|o| json!({"member": o.node, "seen_at": o.seen_at})).collect::<Vec<_>>(),
        });
        std::fs::write(&file, serde_json::to_string_pretty(&entry)?)?;
        anchored += 1;
        eprintln!(
            "aviso {} ({}, {}) ancorado no slot {} com {} assinatura(s)",
            a.header.alert_id, first.description, first.severity_label, rec.slot, rec.signer_count
        );
    }
    eprintln!("{anchored} aviso(s) novo(s) registrado(s)");
    Ok(())
}
