//! Cota's v3 ledger verifier, for the browser and for `verifier/check.mjs`: the `cota-ledger` rules compiled to
//! WebAssembly. The interface is plain memory, so no generated JavaScript is needed (`verifier/ledger.mjs` is the
//! whole glue):
//!
//! 1. `alloc(len)` → a pointer; the caller writes the input JSON there.
//! 2. `verify(ptr, len)` → the length of the output JSON, at `output_ptr()`.
//!
//! Input: `{"mode": "light" | "full", "v2_leaves": [hex…], "entries": [hex…],
//!          "checkpoints": [{"checkpoint": hex, "signatures": [{"member": n, "signature": hex}]}],
//!          "trusted_members": [base58…], "now": seconds}`
//! `trusted_members` is the first committee from a source the ledger does not control (v2's, read on-chain): without
//! it a ledger made with any keys checks out, and the output says `"anchored": false`. `now` refuses batches dated in
//! the future.
//! Output: a summary of the ledger, or `{"ok": false, "error": …}` naming the first entry or checkpoint that fails.

use cota_ledger::{replay, Anchor, Check, Checkpoint, Hash, Key, ReplayError, Signature};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Input {
    mode: String,
    #[serde(default)]
    v2_leaves: Vec<String>,
    #[serde(default)]
    entries: Vec<String>,
    #[serde(default)]
    checkpoints: Vec<CheckpointIn>,
    #[serde(default)]
    trusted_members: Option<Vec<String>>,
    #[serde(default)]
    now: Option<i64>,
}

#[derive(Deserialize)]
struct CheckpointIn {
    checkpoint: String,
    signatures: Vec<SigIn>,
}

#[derive(Deserialize)]
struct SigIn {
    member: u16,
    signature: String,
}

fn hex_array<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).map_err(|_| format!("hex inválido: {s}"))?;
    Ok(out)
}

/// The whole check, from input JSON to output JSON (also used by the native tests).
pub fn run(input: &[u8]) -> Value {
    match check(input) {
        Ok(v) => v,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

fn check(input: &[u8]) -> Result<Value, String> {
    let input: Input = serde_json::from_slice(input).map_err(|e| format!("entrada inválida: {e}"))?;
    let mode = match input.mode.as_str() {
        "full" => Check::Full,
        "light" => Check::Light,
        other => return Err(format!("modo desconhecido: {other}")),
    };
    let v2: Vec<Hash> = input.v2_leaves.iter().map(|l| hex_array(l)).collect::<Result<_, _>>()?;
    let entries: Vec<Vec<u8>> = input.entries.iter().map(|e| hex::decode(e).map_err(|_| "entrada com hex inválido".to_string())).collect::<Result<_, _>>()?;
    let mut checkpoints = Vec::new();
    for c in &input.checkpoints {
        let cp: Checkpoint =
            borsh::from_slice(&hex::decode(&c.checkpoint).map_err(|_| "ponto de controle com hex inválido")?).map_err(|_| "ponto de controle malformado")?;
        let sigs = c.signatures.iter().map(|s| Ok(Signature { member: s.member, signature: hex_array(&s.signature)? })).collect::<Result<Vec<_>, String>>()?;
        checkpoints.push((cp, sigs));
    }
    checkpoints.sort_by_key(|(cp, _)| cp.size);
    let genesis_members = match &input.trusted_members {
        None => None,
        Some(list) => Some(
            list.iter()
                .map(|m| bs58::decode(m).into_vec().ok().and_then(|b| <Key>::try_from(b).ok()).ok_or(format!("chave inválida: {m}")))
                .collect::<Result<Vec<Key>, String>>()?,
        ),
    };
    let anchored = genesis_members.is_some();
    let anchor = Anchor { genesis_members, now: input.now };

    let r = match replay(&v2, &entries, &checkpoints, mode, &anchor) {
        Ok(r) => r,
        Err(ReplayError::Entry { index, kind, refusal }) => {
            return Ok(json!({ "ok": false, "error": format!("entrada {index} ({kind}): {refusal}"), "index": index, "kind": kind }))
        }
        Err(ReplayError::Checkpoint { size, refusal }) => {
            return Ok(json!({ "ok": false, "error": format!("ponto de controle {size}: {refusal}"), "checkpoint": size }))
        }
        Err(ReplayError::OrphanCheckpoint { size }) => {
            return Ok(json!({ "ok": false, "error": format!("ponto de controle {size} não corresponde a nenhum tamanho do livro"), "checkpoint": size }))
        }
    };
    let l = &r.ledger;
    let committee = l.committee().map(|c| {
        json!({ "members": c.members.iter().map(|m| bs58::encode(m).into_string()).collect::<Vec<_>>(), "threshold": c.threshold })
    });
    let last = r.last_checkpoint.as_ref().map(|(cp, sigs)| {
        json!({
            "size": cp.size,
            "root": hex::encode(cp.root),
            "epoch": cp.epoch,
            "signed_at": cp.signed_at,
            "signers": sigs.iter().map(|s| s.member).collect::<Vec<_>>(),
            "covers_all": cp.size == l.size(),
        })
    });
    Ok(json!({
        "ok": true,
        "mode": input.mode,
        "anchored": anchored,
        "genesis_index": l.genesis_index(),
        "entries": entries.len(),
        "size": l.size(),
        "root": l.root().map(hex::encode),
        "clock": l.clock(),
        "epoch": l.epoch(),
        "committee": committee,
        "pending": l.pending().map(|p| json!({ "epoch": p.epoch, "effective_at": p.effective_at })),
        "kinds": r.kinds,
        "alerts_fired": r.alerts_fired,
        "outlooks_triggered": r.outlooks_triggered,
        "checkpoints": r.checkpoints,
        "checkpoints_signed": r.checkpoints_signed,
        "last_checkpoint": last,
    }))
}

// ---- the WebAssembly interface ----------------------------------------------------------------------

static mut OUTPUT: Vec<u8> = Vec::new();

/// Memory for the caller to write `len` bytes into; freed by `verify`.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Runs the check on the `len` bytes at `ptr` (from `alloc`) and returns the output's length; the output stays at
/// `output_ptr()` until the next call.
///
/// # Safety
/// `ptr` must come from `alloc(len)`, with `len` bytes written; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify(ptr: *mut u8, len: usize) -> usize {
    // SAFETY: the caller passes the buffer `alloc(len)` returned, filled with `len` bytes.
    let input = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let out = serde_json::to_vec(&run(&input)).expect("JSON into a Vec does not fail");
    // SAFETY: WebAssembly runs single-threaded; OUTPUT is only touched here and in output_ptr.
    unsafe {
        let output = &mut *std::ptr::addr_of_mut!(OUTPUT);
        *output = out;
        output.len()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn output_ptr() -> *const u8 {
    // SAFETY: see `verify`.
    unsafe { (*std::ptr::addr_of!(OUTPUT)).as_ptr() }
}
