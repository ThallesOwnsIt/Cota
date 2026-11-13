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
export function ledgerInput(mode, { entriesJsonl, checkpointFiles, v2Leaves }) {
  const entries = entriesJsonl
    .split("\n")
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l).entry);
  const checkpoints = checkpointFiles.map((c) => ({ checkpoint: c.checkpoint, signatures: c.signatures }));
  return { mode, v2_leaves: v2Leaves, entries, checkpoints };
}
