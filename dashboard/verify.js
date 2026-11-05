// Verification in the browser, without trusting the dashboard's server:
// - Borsh of `Reading` and RFC 9162 Merkle (the same bytes as Rust and Python);
// - reading and decoding the program's accounts straight from devnet (JSON-RPC).

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

/** Same bytes as `Reading::canonical_bytes` (Borsh, in field order). */
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

/** RFC 9162 root from the leaf hashes. */
export async function rootFromLeaves(leaves) {
  if (leaves.length === 0) return sha256(new Uint8Array());
  if (leaves.length === 1) return leaves[0];
  const k = splitPoint(leaves.length);
  return hashNode(await rootFromLeaves(leaves.slice(0, k)), await rootFromLeaves(leaves.slice(k)));
}

/** RFC 9162 audit path PATH(m, D[n]): from the deepest sibling up to the root. */
export async function proof(leaves, m) {
  if (leaves.length <= 1) return [];
  const k = splitPoint(leaves.length);
  if (m < k) return [...(await proof(leaves.slice(0, k), m)), await rootFromLeaves(leaves.slice(k))];
  return [...(await proof(leaves.slice(k), m - k)), await rootFromLeaves(leaves.slice(0, k))];
}

/** RFC 9162 §2.1.3.2 verification. Returns the recomputed root and each step. */
export async function verifyInclusion(leaf, index, size, path) {
  let fn = index;
  let sn = size - 1;
  let r = leaf;
  const steps = [];
  for (const p of path) {
    if (sn === 0) return { ok: false, root: r, steps };
    if (fn % 2 === 1 || fn === sn) {
      r = await hashNode(p, r);
      steps.push({ side: "left", sibling: p, result: r });
      if (fn % 2 === 0) {
        while (fn % 2 === 0 && fn !== 0) {
          fn >>= 1;
          sn >>= 1;
        }
      }
    } else {
      r = await hashNode(r, p);
      steps.push({ side: "right", sibling: p, result: r });
    }
    fn >>= 1;
    sn >>= 1;
  }
  return { ok: sn === 0, root: r, steps };
}

export const featureLeaf = (f) => enc.encode(`${f.name}=${f.value_milli == null ? "null" : f.value_milli}`);

// ---- on-chain accounts ---------------------------------------------------

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
  return { owner: v.owner, data, lamports: v.lamports, slot: body.result.context.slot };
}

class Reader {
  /** `offset` 8 skips the Anchor discriminator of an account; records (v2) have none. */
  constructor(data, offset = 8) {
    this.d = data;
    this.v = new DataView(data.buffer, data.byteOffset, data.byteLength);
    this.o = offset;
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
  i32() {
    const x = this.v.getInt32(this.o, true);
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

/** Layout of `BatchCommitment` (programs/cota/src/state.rs). */
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

/** Layout of `ForecastCommitment`. */
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

/** Layout of `PublicAlertRecord`. */
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

/** Layout of `OutlookRecord`. */
export function decodeOutlook(data) {
  const r = new Reader(data);
  return {
    region: r.str(),
    based_on_batch_id: r.u64(),
    issued_at: r.i64(),
    window_end: r.i64(),
    models: r.u8(),
    rain_mean_milli: r.i64(),
    rain_min_milli: r.i64(),
    rain_max_milli: r.i64(),
    content_sha256: toHex(r.bytes(32)),
    threshold_milli: r.i64(),
    triggered: r.u8() === 1,
    signers: r.u8(),
    signer_count: r.u8(),
    slot: r.u64(),
    committed_at: r.i64(),
  };
}

const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/** Base58 (Bitcoin/Solana alphabet), to compare keys read from the accounts. */
export function base58(bytes) {
  let n = BigInt("0x" + (toHex(bytes) || "0"));
  let out = "";
  while (n > 0n) {
    out = B58[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = "1" + out;
  }
  return out;
}

/** Layout of `Vault`. */
export function decodeVault(data) {
  const r = new Reader(data);
  return {
    creator: base58(r.bytes(32)),
    vault_id: r.u64(),
    beneficiary: base58(r.bytes(32)),
    kind: r.u8(),
    subject: r.str(),
    payout_lamports: r.u64(),
    cooldown_secs: r.i64(),
    accept_from: r.i64(),
    expires_at: r.i64(),
    last_event_at: r.i64(),
    payouts: r.u32(),
    total_paid: r.u64(),
    created_at: r.i64(),
  };
}

/** Layout of `Payout`. */
export function decodePayout(data) {
  const r = new Reader(data);
  return {
    vault: base58(r.bytes(32)),
    trigger: base58(r.bytes(32)),
    event_at: r.i64(),
    amount: r.u64(),
    beneficiary: base58(r.bytes(32)),
    slot: r.u64(),
    paid_at: r.i64(),
  };
}

/** Layout of `AlertReceipt`. */
export function decodeAlertReceipt(data) {
  const r = new Reader(data);
  return {
    forecast: base58(r.bytes(32)),
    station_id: r.str(),
    issued_at: r.i64(),
    target_ts: r.i64(),
    p90: r.i64(),
    threshold_milli: r.i64(),
    level: r.u8(),
    slot: r.u64(),
    committed_at: r.i64(),
  };
}

// ---- program v2: one accumulator (Merkle Mountain Range) for every record ----------------------
// Same algorithms as `cota_core::mmr` and the program: leaf H(0x00 ‖ record), node H(0x01 ‖ left ‖ right),
// peaks indexed by height (peak h exists when bit h of the record count is set).

export const MAX_PEAKS = 32;

/** Layout of `Registry` (programs/cota-v2/src/state.rs). */
export function decodeRegistry(data) {
  const r = new Reader(data);
  const authority = base58(r.bytes(32));
  const members = Array.from({ length: r.u32() }, () => base58(r.bytes(32)));
  return {
    authority,
    members,
    threshold: r.u8(),
    next_batch_id: r.u64(),
    last_batch_root: toHex(r.bytes(32)),
    last_t_end: r.i64(),
    leaf_count: r.u64(),
    peaks: Array.from({ length: MAX_PEAKS }, () => toHex(r.bytes(32))),
  };
}

/** Peaks of the accumulator over these leaf hashes (binary-counter append, like the program). */
export async function mmrPeaks(leaves) {
  const peaks = Array(MAX_PEAKS).fill(null);
  for (let n = 0; n < leaves.length; n++) {
    let carry = leaves[n];
    let h = 0;
    while (Math.floor(n / 2 ** h) % 2 === 1) {
      carry = await hashNode(peaks[h], carry);
      peaks[h] = null;
      h++;
    }
    peaks[h] = carry;
  }
  return peaks.map((p) => (p ? toHex(p) : "0".repeat(64)));
}

/** The perfect trees of an accumulator with `n` leaves, oldest first: [height, first index]. */
function ranges(n) {
  const out = [];
  let start = 0;
  for (let h = MAX_PEAKS - 1; h >= 0; h--) {
    if (Math.floor(n / 2 ** h) % 2 === 1) {
      out.push([h, start]);
      start += 2 ** h;
    }
  }
  return out;
}

/** Inclusion proof of leaf `index`: its peak's height and the siblings up to it. */
export async function mmrProof(leaves, index) {
  const [height, start] = ranges(leaves.length).find(([h, s]) => index >= s && index < s + 2 ** h);
  let level = leaves.slice(start, start + 2 ** height);
  let i = index - start;
  const siblings = [];
  while (level.length > 1) {
    siblings.push(level[i ^ 1]);
    const next = [];
    for (let k = 0; k < level.length; k += 2) next.push(await hashNode(level[k], level[k + 1]));
    level = next;
    i = Math.floor(i / 2);
  }
  return { height, siblings };
}

/** Walks `leaf` up to its peak and compares with the stored one. Returns the steps for display. */
export async function mmrVerify(peaks, count, leaf, index, siblings) {
  const range = ranges(count).find(([h, s]) => index >= s && index < s + 2 ** h);
  if (!range || siblings.length !== range[0]) return { ok: false, steps: [], peak: null, height: null };
  const local = index - range[1];
  let node = leaf;
  const steps = [];
  for (let k = 0; k < siblings.length; k++) {
    const left = Math.floor(local / 2 ** k) % 2 === 0;
    node = left ? await hashNode(node, siblings[k]) : await hashNode(siblings[k], node);
    steps.push({ side: left ? "right" : "left", sibling: siblings[k], result: node });
  }
  return { ok: toHex(node) === peaks[range[0]], steps, peak: toHex(node), height: range[0] };
}

const RECORD_KINDS = ["station", "batch", "degradation", "forecast", "public_alert", "outlook"];

/** Borsh of a v2 `Record`. Field names follow the v1 account decoders above. */
export function decodeRecord(bytes) {
  const r = new Reader(bytes, 0);
  const kind = RECORD_KINDS[r.u8()];
  switch (kind) {
    case "station": {
      const station_id = r.str(), name = r.str(), source = r.u8(), variable = r.u8(), lat_e6 = r.i32(), lon_e6 = r.i32();
      const datum_id = r.u8() ? r.str() : null;
      return { kind, station_id, name, source, variable, lat_e6, lon_e6, datum_id, slot: r.u64(), registered_at: r.i64() };
    }
    case "batch":
      return {
        kind,
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
    case "degradation":
      return { kind, batch_id: r.u64(), station_id: r.str(), score_milli: r.u16(), reason: r.u8(), slot: r.u64() };
    case "forecast": {
      const f = {
        kind,
        station_id: r.str(),
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
      };
      f.alert = r.u8() ? { threshold_milli: r.i64(), level: r.u8() } : null;
      f.slot = r.u64();
      f.committed_at = r.i64();
      return f;
    }
    case "public_alert":
      return {
        kind,
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
    case "outlook": {
      const o = {
        kind,
        region: r.str(),
        issued_at: r.i64(),
        window_end: r.i64(),
        models: r.u8(),
        rain_mean_milli: r.i64(),
        rain_min_milli: r.i64(),
        rain_max_milli: r.i64(),
        content_sha256: toHex(r.bytes(32)),
      };
      o.based_on_batch_id = r.u64();
      o.threshold_milli = r.i64();
      o.triggered = r.u8() === 1;
      o.signers = r.u8();
      o.signer_count = r.u8();
      o.slot = r.u64();
      o.committed_at = r.i64();
      return o;
    }
    default:
      throw new Error(`unknown record kind ${bytes[0]}`);
  }
}

/** Layout of the v2 `Payout`. */
export function decodePayoutV2(data) {
  const r = new Reader(data);
  return {
    vault: base58(r.bytes(32)),
    trigger_leaf: toHex(r.bytes(32)),
    trigger_index: r.u64(),
    event_at: r.i64(),
    amount: r.u64(),
    beneficiary: base58(r.bytes(32)),
    slot: r.u64(),
    paid_at: r.i64(),
  };
}
