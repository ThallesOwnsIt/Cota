// Cota's v3 ledger verifier: the same rules that build the ledger (crates/cota-ledger), compiled to WebAssembly
// (cota_verify.wasm, built by verifier/build-wasm.sh and checked by GitHub to rebuild to the same bytes).
//
// Browser:  const v = await createVerifier(await (await fetch("cota_verify.wasm")).arrayBuffer());
// Node:     const v = await createVerifier(readFileSync(new URL("cota_verify.wasm", import.meta.url)));
//           const summary = v.verify({ mode: "full", v2_leaves, entries, checkpoints });
//
// mode "light": every rule, the signatures of Genesis and committee changes, and the last checkpoint's.
// mode "full":  every rule and every signature of every entry and checkpoint.

export async function createVerifier(wasmBytes) {
  const { instance } = await WebAssembly.instantiate(wasmBytes, {});
  const { memory, alloc, verify, output_ptr } = instance.exports;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  return {
    verify(input) {
      const bytes = encoder.encode(JSON.stringify(input));
      const ptr = alloc(bytes.length);
      new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);
      const len = verify(ptr, bytes.length);
      return JSON.parse(decoder.decode(new Uint8Array(memory.buffer, output_ptr(), len)));
    },
  };
}

// The verifier's input from a ledger's published files: entries.jsonl lines, checkpoint files and v2's leaves.
// trustedMembers: the first committee from outside the ledger (v2's, read on-chain with registryMembers below);
// without it, a ledger made with any keys would check out. now: refuses batches dated in the future.
export function ledgerInput(mode, { entriesJsonl, checkpointFiles, v2Leaves, trustedMembers, now }) {
  const entries = entriesJsonl
    .split("\n")
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l).entry);
  const checkpoints = checkpointFiles.map((c) => ({ checkpoint: c.checkpoint, signatures: c.signatures }));
  const input = { mode, v2_leaves: v2Leaves, entries, checkpoints, now: now ?? Math.floor(Date.now() / 1000) };
  if (trustedMembers) input.trusted_members = trustedMembers;
  return input;
}

// v2's committee, in order, from the registry account on-chain (base58 keys): the trust anchor for Genesis.
// Registry layout: 8 discriminator ‖ 32 authority ‖ u32 n ‖ n × 32 members ‖ …
export async function registryMembers(rpc, registry = "6CNtD18LQ4bBZTGK1C2Z6RuJrSwyxCD3ToRpiWE3y9bR") {
  const res = await fetch(rpc, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getAccountInfo", params: [registry, { encoding: "base64" }] }),
  });
  const value = (await res.json()).result?.value;
  if (!value) throw new Error(`conta ${registry} não encontrada`);
  const data = Uint8Array.from(atob(value.data[0]), (c) => c.charCodeAt(0));
  const n = new DataView(data.buffer).getUint32(40, true);
  const members = [];
  for (let i = 0; i < n; i++) members.push(base58(data.subarray(44 + 32 * i, 76 + 32 * i)));
  return members;
}

const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function base58(bytes) {
  let n = 0n;
  for (const b of bytes) n = n * 256n + BigInt(b);
  let out = "";
  while (n > 0n) {
    out = ALPHABET[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = "1" + out;
  }
  return out;
}
