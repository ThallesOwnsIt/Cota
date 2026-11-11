// Checks Cota's public ledger against the chain, with no website in between: the same verify.js that runs in the
// site's footer link, run by Node.
//
//   node verifier/check.mjs [--rpc URL] [--ledger data/ledger]
//
// 1. Every record in records.jsonl: its leaf hash is recomputed from the record's bytes (RFC 9162 leaf, 0x00 ‖ data)
//    and the record must decode completely.
// 2. The accumulator's peaks are rebuilt from those leaves and compared with the peaks the program keeps on-chain
//    (Registry account), for the same number of records.
// 3. An inclusion proof is built and checked for every record, up to its on-chain peak.
//
// Exit code 0 when everything matches, 1 otherwise. Needs Node 20+ (fetch and WebCrypto built in).

import { readFileSync } from "node:fs";
import { decodeRecord, decodeRegistry, fetchAccount, fromHex, hashLeaf, mmrPeaks, mmrProof, mmrVerify, toHex } from "./verify.js";

const REGISTRY = "6CNtD18LQ4bBZTGK1C2Z6RuJrSwyxCD3ToRpiWE3y9bR"; // PDA ["registry"] of program v2
const arg = (name, fallback) => {
  const i = process.argv.indexOf(name);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const rpc = arg("--rpc", process.env.RPC ?? "https://api.devnet.solana.com");
const ledger = arg("--ledger", new URL("../data/ledger", import.meta.url).pathname);

const lines = readFileSync(`${ledger}/records.jsonl`, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const problems = [];

const leaves = [];
for (const [n, r] of lines.entries()) {
  if (r.index !== n) problems.push(`linha ${n}: índice ${r.index} fora de ordem`);
  const bytes = fromHex(r.record);
  const leaf = await hashLeaf(bytes);
  if (toHex(leaf) !== r.leaf) problems.push(`registro ${n}: a folha não corresponde aos bytes do registro`);
  try {
    decodeRecord(bytes);
  } catch (e) {
    problems.push(`registro ${n}: não decodifica (${e.message})`);
  }
  leaves.push(leaf);
}

const account = await fetchAccount(rpc, REGISTRY);
if (!account) throw new Error(`conta do registro ${REGISTRY} não encontrada em ${rpc}`);
const reg = decodeRegistry(account.data);
const count = Number(reg.leaf_count);
if (leaves.length < count) problems.push(`o ledger tem ${leaves.length} registros e a cadeia ${count}: faltam registros (git pull?)`);
const ours = leaves.slice(0, count);

const peaks = await mmrPeaks(ours);
const differ = peaks.map((p, h) => [h, p]).filter(([h, p]) => p !== reg.peaks[h]);
for (const [h] of differ) problems.push(`pico de altura ${h} diferente da cadeia`);

let proven = 0;
if (differ.length === 0) {
  for (let i = 0; i < ours.length; i++) {
    const { siblings } = await mmrProof(ours, i);
    const v = await mmrVerify(reg.peaks, count, ours[i], i, siblings);
    if (v.ok) proven++;
    else problems.push(`registro ${i}: a prova de inclusão não chega ao pico da cadeia`);
  }
}

const kinds = {};
for (const r of lines.slice(0, count)) kinds[r.kind] = (kinds[r.kind] ?? 0) + 1;
console.log(`cadeia: ${count} registros (${rpc})`);
console.log(`ledger: ${lines.length} registros em ${ledger}/records.jsonl`);
console.log(`tipos: ${Object.entries(kinds).map(([k, v]) => `${k} ${v}`).join(", ")}`);
console.log(`picos da árvore: ${differ.length === 0 ? "iguais aos da cadeia" : `${differ.length} diferente(s)`}`);
console.log(`provas de inclusão conferidas: ${proven}/${count}`);
if (problems.length) {
  console.log(`\n${problems.length} problema(s):\n- ${problems.slice(0, 20).join("\n- ")}`);
  process.exit(1);
}
console.log("\ntudo confere: cada registro do ledger público está na árvore guardada na cadeia");
