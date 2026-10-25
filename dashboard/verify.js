// Verificação no navegador, sem confiar no servidor do dashboard:
// - Borsh de `Reading` e Merkle RFC 9162 (os mesmos bytes do Rust e do Python);
// - leitura e decodificação das contas do programa direto da devnet (JSON-RPC).

const SOURCES = { inmet: 0, open_meteo_glofas: 1, ana: 2, cemaden: 3, public_record: 4 };
const VARIABLES = { precipitation: 0, river_stage: 1, river_discharge: 2 };
const enc = new TextEncoder();

export const toHex = (b) => [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
export const fromHex = (h) => Uint8Array.from(h.match(/../g).map((x) => parseInt(x, 16)));

class Writer {
  constructor() {
    this.parts = [];
  }
  u8(v) {
    this.parts.push(Uint8Array.of(v));
  }
  u32(v) {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, v, true);
    this.parts.push(b);
  }
  i64(v) {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigInt64(0, BigInt(v), true);
    this.parts.push(b);
  }
  bytes(b) {
    this.parts.push(b);
  }
  str(s) {
    const b = enc.encode(s);
    this.u32(b.length);
    this.bytes(b);
  }
  done() {
    const out = new Uint8Array(this.parts.reduce((n, p) => n + p.length, 0));
    let o = 0;
    for (const p of this.parts) {
      out.set(p, o);
      o += p.length;
    }
    return out;
  }
}

/** Mesmos bytes de `Reading::canonical_bytes` (Borsh, na ordem dos campos). */
export function readingBytes(r) {
  const w = new Writer();
  w.str(r.station_id);
  w.u8(VARIABLES[r.variable]);
  w.i64(r.ts_utc);
  w.u8(SOURCES[r.source]);
  w.i64(r.value_milli);
  if (r.datum_id == null) w.u8(0);
  else {
    w.u8(1);
    w.str(r.datum_id);
  }
  w.bytes(fromHex(r.raw_payload_sha256));
  return w.done();
}

async function sha256(...parts) {
  const len = parts.reduce((n, p) => n + p.length, 0);
  const buf = new Uint8Array(len);
  let o = 0;
  for (const p of parts) {
    buf.set(p, o);
    o += p.length;
  }
  return new Uint8Array(await crypto.subtle.digest("SHA-256", buf));
}

export const hashLeaf = (d) => sha256(Uint8Array.of(0), d);
export const hashNode = (l, r) => sha256(Uint8Array.of(1), l, r);

const splitPoint = (n) => 2 ** Math.floor(Math.log2(n - 1));

/** Raiz RFC 9162 a partir dos hashes de folha. */
export async function rootFromLeaves(leaves) {
  if (leaves.length === 0) return sha256(new Uint8Array());
  if (leaves.length === 1) return leaves[0];
  const k = splitPoint(leaves.length);
  return hashNode(await rootFromLeaves(leaves.slice(0, k)), await rootFromLeaves(leaves.slice(k)));
}

/** Caminho de auditoria PATH(m, D[n]) da RFC 9162: do irmão mais profundo à raiz. */
export async function proof(leaves, m) {
  if (leaves.length <= 1) return [];
  const k = splitPoint(leaves.length);
  if (m < k) return [...(await proof(leaves.slice(0, k), m)), await rootFromLeaves(leaves.slice(k))];
  return [...(await proof(leaves.slice(k), m - k)), await rootFromLeaves(leaves.slice(0, k))];
}

/** Verificação RFC 9162 §2.1.3.2. Devolve a raiz recalculada e cada passo. */
export async function verifyInclusion(leaf, index, size, path) {
  let fn = index;
  let sn = size - 1;
  let r = leaf;
  const steps = [];
  for (const p of path) {
    if (sn === 0) return { ok: false, root: r, steps };
    if (fn % 2 === 1 || fn === sn) {
      r = await hashNode(p, r);
      steps.push({ side: "esquerda", sibling: p, result: r });
      if (fn % 2 === 0) {
        while (fn % 2 === 0 && fn !== 0) {
          fn >>= 1;
          sn >>= 1;
        }
      }
    } else {
      r = await hashNode(r, p);
      steps.push({ side: "direita", sibling: p, result: r });
    }
    fn >>= 1;
    sn >>= 1;
  }
  return { ok: sn === 0, root: r, steps };
}

export const featureLeaf = (f) => enc.encode(`${f.name}=${f.value_milli == null ? "null" : f.value_milli}`);

// ---- contas on-chain ------------------------------------------------------

export async function fetchAccount(rpc, address) {
  const res = await fetch(rpc, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "getAccountInfo",
      params: [address, { encoding: "base64", commitment: "confirmed" }],
    }),
  });
  if (!res.ok) throw new Error(`RPC respondeu ${res.status}`);
  const body = await res.json();
  if (body.error) throw new Error(body.error.message);
  const v = body.result.value;
  if (!v) throw new Error("conta não encontrada na rede");
  const data = Uint8Array.from(atob(v.data[0]), (c) => c.charCodeAt(0));
  return { owner: v.owner, data, slot: body.result.context.slot };
}

class Reader {
  constructor(data) {
    this.d = data;
    this.v = new DataView(data.buffer, data.byteOffset, data.byteLength);
    this.o = 8; // discriminador do Anchor
  }
  u8() {
    return this.d[this.o++];
  }
  u16() {
    const x = this.v.getUint16(this.o, true);
    this.o += 2;
    return x;
  }
  u32() {
    const x = this.v.getUint32(this.o, true);
    this.o += 4;
    return x;
  }
  i64() {
    const x = this.v.getBigInt64(this.o, true);
    this.o += 8;
    return Number(x);
  }
  u64() {
    const x = this.v.getBigUint64(this.o, true);
    this.o += 8;
    return Number(x);
  }
  bytes(n) {
    const x = this.d.slice(this.o, this.o + n);
    this.o += n;
    return x;
  }
  str() {
    return new TextDecoder().decode(this.bytes(this.u32()));
  }
}

/** Layout de `BatchCommitment` (programs/cota/src/state.rs). */
export function decodeBatch(data) {
  const r = new Reader(data);
  return {
    batch_id: r.u64(),
    t_start: r.i64(),
    t_end: r.i64(),
    leaf_count: r.u32(),
    merkle_root: toHex(r.bytes(32)),
    station_set_hash: toHex(r.bytes(32)),
    prev_batch_root: toHex(r.bytes(32)),
    signers: r.u8(),
    signer_count: r.u8(),
    slot: r.u64(),
    committed_at: r.i64(),
  };
}

/** Layout de `ForecastCommitment`. */
export function decodeForecast(data) {
  const r = new Reader(data);
  return {
    station_id: r.str(),
    variable: r.u8(),
    based_on_batch_id: r.u64(),
    based_on_root: toHex(r.bytes(32)),
    issued_at: r.i64(),
    target_ts: r.i64(),
    horizon_hours: r.u16(),
    p10: r.i64(),
    p50: r.i64(),
    p90: r.i64(),
    model_sha256: toHex(r.bytes(32)),
    features_root: toHex(r.bytes(32)),
    slot: r.u64(),
    committed_at: r.i64(),
  };
}

export async function sha256Hex(text) {
  return toHex(await sha256(enc.encode(text)));
}

/** Layout de `PublicAlertRecord`. */
export function decodePublicAlert(data) {
  const r = new Reader(data);
  return {
    issuer: r.str(),
    alert_id: r.str(),
    content_sha256: toHex(r.bytes(32)),
    severity: r.u8(),
    onset: r.i64(),
    expires: r.i64(),
    signers: r.u8(),
    signer_count: r.u8(),
    slot: r.u64(),
    committed_at: r.i64(),
  };
}
