//! Minimal Solana JSON-RPC client: only what Cota uses.

use std::str::FromStr;
use std::time::{Duration, Instant};

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction};
use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;

const CONFIRM_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BACKOFF: Duration = Duration::from_secs(16);

pub struct Rpc {
    url: String,
    http: reqwest::blocking::Client,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self { url: url.to_string(), http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build()? })
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        // Public RPCs (devnet) answer 429 when we go over the limit: wait and try again.
        let mut wait = Duration::from_secs(1);
        let resp = loop {
            let r = self.http.post(&self.url).json(&body).send().with_context(|| format!("RPC {method}"))?;
            if r.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || wait > MAX_BACKOFF {
                break r;
            }
            std::thread::sleep(wait);
            wait *= 2;
        };
        let resp: Value = resp.error_for_status().with_context(|| format!("RPC {method}"))?.json()?;
        if let Some(err) = resp.get("error") {
            let logs = err.pointer("/data/logs").and_then(Value::as_array).map(|l| {
                l.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n    ")
            });
            bail!("RPC {method}: {}{}", err["message"], logs.map(|l| format!("\n  logs:\n    {l}")).unwrap_or_default());
        }
        Ok(resp["result"].clone())
    }

    pub fn latest_blockhash(&self) -> Result<Hash> {
        let r = self.call("getLatestBlockhash", json!([{ "commitment": "confirmed" }]))?;
        let text = r.pointer("/value/blockhash").and_then(Value::as_str).context("resposta sem blockhash")?;
        Hash::from_str(text).map_err(|e| anyhow::anyhow!("blockhash inválido: {e:?}"))
    }

    pub fn balance(&self, pk: &Pubkey) -> Result<u64> {
        let r = self.call("getBalance", json!([pk.to_string(), { "commitment": "confirmed" }]))?;
        r["value"].as_u64().context("saldo ausente")
    }

    /// Account data, or `None` if it does not exist.
    pub fn account_data(&self, pk: &Pubkey) -> Result<Option<Vec<u8>>> {
        let r = self.call("getAccountInfo", json!([pk.to_string(), { "encoding": "base64", "commitment": "confirmed" }]))?;
        if r["value"].is_null() {
            return Ok(None);
        }
        let b64 = r.pointer("/value/data/0").and_then(Value::as_str).context("conta sem dados")?;
        Ok(Some(base64::engine::general_purpose::STANDARD.decode(b64)?))
    }

    /// Log messages of a confirmed transaction (where the program's events are). Retries briefly: right
    /// after confirmation some RPC nodes do not serve the transaction yet.
    pub fn transaction_logs(&self, sig: &str) -> Result<Vec<String>> {
        for attempt in 0..10 {
            let r = self.call(
                "getTransaction",
                json!([sig, { "encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0 }]),
            )?;
            if let Some(logs) = r.pointer("/meta/logMessages").and_then(Value::as_array) {
                return Ok(logs.iter().filter_map(Value::as_str).map(str::to_string).collect());
            }
            std::thread::sleep(Duration::from_millis(500 * (attempt + 1)));
        }
        bail!("transaction {sig} not available from the RPC")
    }

    /// Signatures of successful transactions that touched `address`, newest first.
    pub fn signatures_for_address(&self, address: &Pubkey, before: Option<&str>, limit: usize) -> Result<Vec<String>> {
        let mut opts = json!({ "limit": limit, "commitment": "confirmed" });
        if let Some(b) = before {
            opts["before"] = json!(b);
        }
        let r = self.call("getSignaturesForAddress", json!([address.to_string(), opts]))?;
        Ok(r.as_array()
            .context("getSignaturesForAddress without a list")?
            .iter()
            .filter(|s| s["err"].is_null())
            .filter_map(|s| s["signature"].as_str().map(str::to_string))
            .collect())
    }

    /// Signs with `payer`, sends (after simulating) and waits for confirmation. Returns the
    /// signature.
    pub fn send_and_confirm(&self, instructions: &[Instruction], payer: &Keypair) -> Result<String> {
        self.send_and_confirm_with(instructions, payer, &[])
    }

    /// Like [`Rpc::send_and_confirm`], with signers besides the payer.
    pub fn send_and_confirm_with(&self, instructions: &[Instruction], payer: &Keypair, others: &[&Keypair]) -> Result<String> {
        let blockhash = self.latest_blockhash()?;
        let mut signers = vec![payer];
        signers.extend(others.iter().copied().filter(|k| k.pubkey() != payer.pubkey()));
        let tx = Transaction::new_signed_with_payer(instructions, Some(&payer.pubkey()), &signers, blockhash);
        let wire = base64::engine::general_purpose::STANDARD.encode(bincode::serialize(&tx)?);
        let sig = self.call(
            "sendTransaction",
            json!([wire, { "encoding": "base64", "preflightCommitment": "confirmed" }]),
        )?;
        let sig = sig.as_str().context("sendTransaction sem assinatura")?.to_string();

        let start = Instant::now();
        while start.elapsed() < CONFIRM_TIMEOUT {
            let st = self.call("getSignatureStatuses", json!([[sig]]))?;
            let status = &st["value"][0];
            if !status.is_null() {
                if !status["err"].is_null() {
                    bail!("transação {sig} falhou: {}", status["err"]);
                }
                if matches!(status["confirmationStatus"].as_str(), Some("confirmed" | "finalized")) {
                    return Ok(sig);
                }
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
        bail!("transação {sig} não confirmou em {}s", CONFIRM_TIMEOUT.as_secs())
    }
}

pub fn explorer_tx(sig: &str, cluster: &str) -> String {
    format!("https://explorer.solana.com/tx/{sig}?cluster={cluster}")
}

pub fn explorer_account(pk: &Pubkey, cluster: &str) -> String {
    format!("https://explorer.solana.com/address/{pk}?cluster={cluster}")
}
