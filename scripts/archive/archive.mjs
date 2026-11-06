// Permanent copy of Cota's ledger on Arweave, through ArDrive Turbo.
//
//   node archive.mjs [--dry-run] [--limit N]
//
// GitHub is a single point of failure for the data the chain only holds hashes of. Every file of
// data/ledger and data/ledger-v1 (plus the rule and model manifests) is gzipped and uploaded once; changed files
// are uploaded again. records.jsonl, which only grows, goes in append-only chunks of the new lines. Each upload
// carries tags (App-Name=Cota, Cota-Path, Cota-SHA256 of the uncompressed bytes), so anyone can list them by owner
// on any Arweave gateway (GraphQL) without this repository, and check every byte against the hashes on-chain.
//
// Turbo uploads items up to 105 KiB for free (10 MiB per signing key, for life): a few years of Cota.
// Signer: keys/archive.json (a Solana keypair holding nothing), or $ARCHIVE_KEY with its JSON in CI.
// The map path -> Arweave id is data/ledger/archive.json.

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import crypto from "node:crypto";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import bs58 from "bs58";
import { TurboFactory } from "@ardrive/turbo-sdk";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const MANIFEST = path.join(ROOT, "data/ledger/archive.json");
const ROOTS = ["data/ledger", "data/ledger-v1"];
const EXTRA = ["ml/models/manifest.json", "ml/models/outlook-rule.json", "config/stations.toml", "config/stations-live.toml", "config/upgrade-multisig.json"];
// Files that are only state of the tools, or this manifest itself.
const SKIP = new Set(["data/ledger/archive.json", "data/ledger/quality-state.json", "data/ledger-v1/quality-state.json"]);
const RECORDS = "data/ledger/records.jsonl";
const FREE_ITEM = 107_520;
const dry = process.argv.includes("--dry-run");

const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");

function walk(dir) {
  const out = [];
  for (const e of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
    const rel = `${dir}/${e.name}`;
    if (e.isDirectory()) out.push(...walk(rel));
    else if (!SKIP.has(rel) && rel !== RECORDS) out.push(rel);
  }
  return out;
}

function secretKey() {
  const json = process.env.ARCHIVE_KEY ?? fs.readFileSync(path.join(ROOT, "keys/archive.json"), "utf8");
  return Uint8Array.from(JSON.parse(json));
}

async function main() {
  const manifest = fs.existsSync(MANIFEST)
    ? JSON.parse(fs.readFileSync(MANIFEST, "utf8"))
    : { gateway: "https://arweave.net", note: "path -> Arweave id of its latest gzipped copy; records.jsonl in chunks", files: {}, records: [], bytes: 0 };
  const key = secretKey();
  manifest.signer = bs58.encode(key.subarray(32));
  const turbo = dry ? null : TurboFactory.authenticated({ privateKey: bs58.encode(key), token: "solana" });

  // Work to do: new or changed files, and the records.jsonl lines not archived yet.
  const jobs = [];
  for (const rel of [...ROOTS.flatMap((r) => (fs.existsSync(path.join(ROOT, r)) ? walk(r) : [])), ...EXTRA]) {
    const raw = fs.readFileSync(path.join(ROOT, rel));
    const digest = sha256(raw);
    if (manifest.files[rel]?.sha256 !== digest) jobs.push({ rel, raw, digest });
  }
  const lines = fs.readFileSync(path.join(ROOT, RECORDS), "utf8").split("\n").filter(Boolean);
  const archived = manifest.records.reduce((n, c) => Math.max(n, c.to + 1), 0);
  for (let from = archived; from < lines.length; ) {
    // As many new lines as fit in one free item once compressed.
    let to = from;
    while (to + 1 < lines.length && zlib.gzipSync(lines.slice(from, to + 2).join("\n") + "\n").length < FREE_ITEM * 0.9) to++;
    const raw = Buffer.from(lines.slice(from, to + 1).join("\n") + "\n");
    jobs.push({ rel: `${RECORDS}#${from}-${to}`, raw, digest: sha256(raw), chunk: { from, to } });
    from = to + 1;
  }

  let sent = 0;
  const limit = Number(process.argv[process.argv.indexOf("--limit") + 1] ?? Infinity);
  for (const job of process.argv.includes("--limit") ? jobs.slice(0, limit) : jobs) {
    const gz = zlib.gzipSync(job.raw, { level: 9 });
    if (gz.length > FREE_ITEM) {
      console.error(`${job.rel}: ${gz.length} bytes comprimido, acima do item gratuito; ficou de fora`);
      continue;
    }
    const tags = [
      { name: "App-Name", value: "Cota" },
      { name: "Content-Type", value: job.rel.endsWith(".json") ? "application/json" : "text/plain" },
      { name: "Content-Encoding", value: "gzip" },
      { name: "Cota-Path", value: job.rel },
      { name: "Cota-SHA256", value: job.digest },
    ];
    if (dry) {
      console.log(`[simulação] ${job.rel}: ${job.raw.length} → ${gz.length} bytes`);
      sent += gz.length;
      continue;
    }
    const res = await turbo.uploadFile({ fileStreamFactory: () => Readable.from(gz), fileSizeFactory: () => gz.length, dataItemOpts: { tags } });
    const entry = { sha256: job.digest, id: res.id, bytes: gz.length };
    if (job.chunk) manifest.records.push({ ...job.chunk, ...entry });
    else manifest.files[job.rel] = entry;
    manifest.bytes += gz.length;
    sent += gz.length;
    // Saved after every upload, so an interruption never loses the ids already paid for.
    fs.writeFileSync(MANIFEST, JSON.stringify(manifest, null, 1) + "\n");
    console.log(`${job.rel} → ${res.id} (${gz.length} bytes)`);
  }
  console.log(`${jobs.length} item(ns), ${(sent / 1024).toFixed(0)} KiB enviados${dry ? " (simulação)" : ""}; total arquivado ${(manifest.bytes / 1024).toFixed(0)} KiB de 10240 KiB gratuitos`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
