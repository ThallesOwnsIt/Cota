//! The v3 ledger on disk (`docs/ledger-v3.md`) and the two sides of a round.
//!
//! - **A node** replays the ledger with `cota_ledger` (so it signs only on top of a ledger it verified itself),
//!   builds the bodies of what it observed on its own (the day's batch, the rain outlooks, the third-party forecast,
//!   the flow forecast of its own model run, the official warnings, the catalog's stations, and the opening entries
//!   before Genesis), signs them, and signs the checkpoint of the ledger as it found it.
//! - **The aggregator** replays the ledger, publishes the checkpoint a majority signed, then turns every body a
//!   majority signed into an entry, applies it with the same rules and appends it.
//!
//! A checkpoint therefore covers the entries of the previous round: nodes sign the state they verified, and the
//! aggregator publishes it in the next round.
//!
//! Files of a node (`<node>/v3/`): `<kind>-<sha256 of the signing message, 16 hex>.json` and
//! `checkpoint-<size>.json`. Files of the ledger: `entries.jsonl`, `checkpoints/<size>.json`,
//! `batches/<stream>-<id>.json` (the readings).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{DateTime, NaiveDate};
use cota_ingest::{end_of_day, start_of_day};
use cota_ledger::{Applied, Body, Checkpoint, Entry, Hash, Key, Ledger, Quorum, Refusal, RuleSet, Signature, MAIN_COMMITTEE};
use ed25519_dalek::{Signature as DalekSignature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{attest::SignedAlert, extforecast::SignedForecast, node::SignedBatch, outlook, outlook::SignedOutlook};

// ---- configuration ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub catalog: PathBuf,
    pub streams: Vec<String>,
    pub main_stream: String,
    pub genesis: GenesisConfig,
    pub rules: RulesConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisConfig {
    pub v2_program: String,
    pub v2_registry: String,
    pub v1_archive: PathBuf,
    pub members: Vec<String>,
    pub threshold: u16,
    pub cutover: NaiveDate,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RulesConfig {
    pub flow_alert_manifest: PathBuf,
    pub outlook_main: PathBuf,
    pub outlook_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub id: String,
    pub stream: String,
    pub horizon_days: u16,
}

pub fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("interpretando {}", path.display()))
}

fn key_from_base58(s: &str) -> Result<Key> {
    let bytes = bs58::decode(s).into_vec().with_context(|| format!("chave base58 inválida: {s}"))?;
    bytes.try_into().map_err(|_| anyhow!("chave {s} não tem 32 bytes"))
}

fn base58(key: &Key) -> String {
    bs58::encode(key).into_string()
}

/// SHA-256 of a folder's listing: one line `path\0sha256-hex\n` per file, sorted by path.
pub fn dir_digest(dir: &Path) -> Result<Hash> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<()> {
        for e in std::fs::read_dir(dir).with_context(|| format!("lendo {}", dir.display()))? {
            let path = e?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else {
                let rel = path.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
                out.push((rel, hex::encode(Sha256::digest(std::fs::read(&path)?))));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for (path, sha) in files {
        h.update(path.as_bytes());
        h.update([0]);
        h.update(sha.as_bytes());
        h.update(b"\n");
    }
    Ok(h.finalize().into())
}

// ---- the ledger on disk -----------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct EntryLine {
    index: u64,
    leaf: String,
    kind: String,
    entry: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct SigJson {
    member: u16,
    signature: String,
}

/// `checkpoints/<size>.json`: readable fields plus the signed bytes.
#[derive(Serialize, Deserialize)]
struct CheckpointFile {
    size: u64,
    root: String,
    prev: String,
    epoch: u32,
    signed_at: i64,
    /// Borsh of the checkpoint (what follows the domain in the signed message).
    checkpoint: String,
    signatures: Vec<SigJson>,
}

/// The v3 ledger, replayed and verified.
pub struct Book {
    pub dir: PathBuf,
    pub ledger: Ledger,
    pub entries: Vec<Entry>,
    pub checkpoints: Vec<(Checkpoint, Vec<Signature>)>,
    /// Latest registration of each station.
    pub stations: BTreeMap<String, cota_ledger::Station>,
    pub alert_versions: BTreeSet<Hash>,
    /// SHA-256 of the signing message of every entry: what a node does not need to sign again.
    pub digests: BTreeSet<Hash>,
    /// (stream, batch id) of every flow forecast's base.
    pub forecast_bases: BTreeSet<(String, u64)>,
    /// Every v2 record in the v2 ledger now (Genesis takes a prefix of them).
    v2_records: Vec<Vec<u8>>,
    /// The v2 ledger's folder (its content files fill what a v3 entry's content is missing).
    v2_dir: PathBuf,
}

fn decode_hex<const N: usize>(s: &str) -> Result<[u8; N]> {
    let mut out = [0u8; N];
    hex::decode_to_slice(s, &mut out).with_context(|| format!("hex inválido: {s}"))?;
    Ok(out)
}

impl Book {
    /// Reads `dir` and replays it on top of v2's records (`v2_ledger/records.jsonl`): every entry must pass the rules,
    /// every checkpoint must match the ledger at its size. Before Genesis the ledger holds all of v2's records.
    pub fn open(dir: &Path, v2_ledger: &Path) -> Result<Self> {
        let v2_records: Vec<Vec<u8>> =
            crate::records::load(v2_ledger)?.iter().map(|s| s.bytes()).collect::<Result<_>>().context("registros da v2")?;
        let entries_path = dir.join("entries.jsonl");
        let mut entries = Vec::new();
        if entries_path.exists() {
            for (i, line) in std::fs::read_to_string(&entries_path)?.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                let l: EntryLine = serde_json::from_str(line).with_context(|| format!("entries.jsonl, linha {}", i + 1))?;
                let bytes = hex::decode(&l.entry)?;
                let entry = Entry::from_bytes(&bytes).map_err(|r| anyhow!("entries.jsonl, linha {}: {r}", i + 1))?;
                ensure!(hex::encode(entry.leaf()) == l.leaf, "entries.jsonl, linha {}: folha não confere", i + 1);
                entries.push((l.index, entry));
            }
        }
        let mut checkpoints = Vec::new();
        let cp_dir = dir.join("checkpoints");
        if cp_dir.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&cp_dir)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<_>>()?;
            files.retain(|f| f.extension().is_some_and(|e| e == "json"));
            files.sort();
            for f in files {
                let file: CheckpointFile = serde_json::from_str(&std::fs::read_to_string(&f)?).with_context(|| format!("lendo {}", f.display()))?;
                let cp: Checkpoint = borsh::from_slice(&hex::decode(&file.checkpoint)?).with_context(|| format!("{}", f.display()))?;
                let sigs = file
                    .signatures
                    .iter()
                    .map(|s| Ok(Signature { member: s.member, signature: decode_hex(&s.signature)? }))
                    .collect::<Result<Vec<_>>>()?;
                checkpoints.push((cp, sigs));
            }
            checkpoints.sort_by_key(|(cp, _)| cp.size);
        }

        let n = match entries.first() {
            Some((_, Entry { body: Body::Genesis(g), .. })) => g.v2_leaf_count as usize,
            Some(_) => bail!("a primeira entrada do livro não é o registro de abertura"),
            None => v2_records.len(),
        };
        ensure!(n <= v2_records.len(), "o registro de abertura cita {n} registros da v2; o ledger da v2 tem {}", v2_records.len());
        let mut book = Book {
            dir: dir.to_path_buf(),
            ledger: Ledger::new(),
            entries: Vec::new(),
            checkpoints: Vec::new(),
            stations: BTreeMap::new(),
            alert_versions: BTreeSet::new(),
            digests: BTreeSet::new(),
            forecast_bases: BTreeSet::new(),
            v2_records,
            v2_dir: v2_ledger.to_path_buf(),
        };
        for r in &book.v2_records[..n] {
            book.ledger.push_v2_record(r).map_err(|e| anyhow!("{e}"))?;
        }
        let mut pending = checkpoints.into_iter().peekable();
        for (index, entry) in entries {
            ensure!(index == book.ledger.size(), "entries.jsonl: índice {index} fora de ordem");
            book.ledger.apply(&entry).map_err(|r| anyhow!("entrada {index} ({}): {r}", entry.body.kind()))?;
            book.track(&entry);
            book.entries.push(entry);
            while let Some((cp, sigs)) = pending.next_if(|(cp, _)| cp.size == book.ledger.size()) {
                book.ledger
                    .verify_checkpoint(&cp, &sigs, book.checkpoints.last().map(|(c, _)| c))
                    .map_err(|r| anyhow!("ponto de controle {}: {r}", cp.size))?;
                book.checkpoints.push((cp, sigs));
            }
        }
        if let Some((cp, _)) = pending.next() {
            bail!("ponto de controle {} não corresponde a nenhum tamanho do livro", cp.size);
        }
        Ok(book)
    }

    fn track(&mut self, entry: &Entry) {
        self.digests.insert(Sha256::digest(entry.body.signing_message()).into());
        match &entry.body {
            Body::Station(s) => {
                self.stations.insert(s.station_id.clone(), s.clone());
            }
            Body::PublicAlert(a) => {
                self.alert_versions.insert(a.content_sha256);
            }
            Body::Forecast(f) => {
                self.forecast_bases.insert((f.stream.clone(), f.based_on_batch_id));
            }
            _ => {}
        }
    }

    pub fn has_genesis(&self) -> bool {
        self.ledger.genesis_index().is_some()
    }

    /// The checkpoint the committee signs now: the current state, chained to the last published checkpoint, with
    /// `signed_at` = the ledger clock (deterministic, so every member signs the same bytes; real time comes from the
    /// anchors). `None` before Genesis, or when the last checkpoint already covers this size.
    pub fn next_checkpoint(&self) -> Option<Checkpoint> {
        let last = self.checkpoints.last().map(|(c, _)| c);
        if !self.has_genesis() || last.is_some_and(|c| c.size == self.ledger.size()) {
            return None;
        }
        self.ledger.checkpoint(last, self.ledger.clock()).ok()
    }

    fn append(&mut self, entry: Entry, applied: &Applied) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let line = EntryLine {
            index: applied.index,
            leaf: hex::encode(applied.leaf),
            kind: entry.body.kind().into(),
            entry: hex::encode(entry.to_bytes()),
        };
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("entries.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(&line)?)?;
        self.track(&entry);
        self.entries.push(entry);
        Ok(())
    }

    fn write_checkpoint(&mut self, cp: Checkpoint, sigs: Vec<Signature>) -> Result<()> {
        let dir = self.dir.join("checkpoints");
        std::fs::create_dir_all(&dir)?;
        let file = CheckpointFile {
            size: cp.size,
            root: hex::encode(cp.root),
            prev: hex::encode(cp.prev),
            epoch: cp.epoch,
            signed_at: cp.signed_at,
            checkpoint: hex::encode(borsh::to_vec(&cp)?),
            signatures: sigs.iter().map(|s| SigJson { member: s.member, signature: hex::encode(s.signature) }).collect(),
        };
        std::fs::write(dir.join(format!("{:06}.json", cp.size)), serde_json::to_string_pretty(&file)? + "\n")?;
        self.checkpoints.push((cp, sigs));
        Ok(())
    }
}

// ---- trust ------------------------------------------------------------------------------------------

/// The committee v2's registry holds on-chain (in order): the trust anchor for Genesis. `None` if the chain cannot be
/// read now.
pub fn chain_members(rpc_url: &str) -> Option<Vec<Key>> {
    let rpc = crate::rpc::Rpc::new(rpc_url).ok()?;
    let data = rpc.account_data(&crate::chain::registry_pda()).ok()??;
    let r = crate::chain::decode_registry(&data).ok()?;
    Some(r.members.iter().map(|m| m.to_bytes()).collect())
}

/// The committee this node will sign for: Genesis's (or, before it, the config's), checked against v2's committee on
/// the chain, or against the config when the chain cannot be read. A node never signs on top of a ledger opened by
/// other keys, nor an opening that names them.
pub fn check_trust(cfg: &Config, book: &Book, rpc_url: Option<&str>) -> Result<()> {
    let configured = cfg.genesis.members.iter().map(|m| key_from_base58(m)).collect::<Result<Vec<_>>>()?;
    let opened = match book.entries.first().map(|e| &e.body) {
        Some(Body::Genesis(g)) => g.members.clone(),
        _ => configured.clone(),
    };
    let (trusted, source) = match rpc_url.and_then(chain_members) {
        Some(m) => (m, "o comitê da v2 na cadeia"),
        None => {
            eprintln!("aviso: não consegui ler o comitê da v2 na cadeia; confiro com config/ledger-v3.toml");
            (configured, "config/ledger-v3.toml")
        }
    };
    ensure!(opened == trusted, "o comitê da abertura do livro v3 difere de {source}: não assino");
    Ok(())
}

// ---- what a node signs ------------------------------------------------------------------------------

/// A node's signature over one body.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedBody {
    pub member: String,
    pub kind: String,
    /// Borsh of the `Body`.
    pub body: String,
    pub signature: String,
}

/// A node's signature over a checkpoint.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedCheckpoint {
    pub member: String,
    /// Borsh of the `Checkpoint`.
    pub checkpoint: String,
    pub signature: String,
}

/// The opening entries: Genesis over v2's current records, the streams, the rules and the sources.
pub fn opening_bodies(cfg: &Config, book: &Book) -> Result<Vec<Body>> {
    let g = &cfg.genesis;
    let cutover_at = start_of_day(g.cutover);
    let members = g.members.iter().map(|m| key_from_base58(m)).collect::<Result<Vec<_>>>()?;
    let mut bodies = vec![Body::Genesis(cota_ledger::Genesis {
        v2_program: key_from_base58(&g.v2_program)?,
        v2_registry: key_from_base58(&g.v2_registry)?,
        v2_leaf_count: book.ledger.size(),
        v2_root: book.ledger.root().unwrap_or_default(),
        v1_archive_sha256: dir_digest(&g.v1_archive)?,
        members,
        threshold: g.threshold,
        cutover_at,
    })];
    for s in &cfg.streams {
        bodies.push(Body::StreamCreate(cota_ledger::StreamCreate { id: s.clone(), starts_at: cutover_at }));
    }
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(&cfg.rules.flow_alert_manifest)?)?;
    let rule = &manifest["alert_rule"];
    bodies.push(Body::RuleSet(RuleSet::FlowAlert {
        station_id: rule["station_id"].as_str().context("alert_rule.station_id")?.into(),
        stream: cfg.main_stream.clone(),
        threshold_milli: rule["threshold_milli"].as_i64().context("alert_rule.threshold_milli")?,
        level: rule["level"].as_u64().context("alert_rule.level")? as u8,
    }));
    for r in outlook::load_rules(&cfg.rules.outlook_main, &cfg.rules.outlook_dir)? {
        bodies.push(Body::RuleSet(RuleSet::Outlook {
            region: r.spec.region.clone(),
            stream: cfg.main_stream.clone(),
            threshold_milli: r.threshold_milli(),
            window_secs: r.window_secs(),
            min_models: r.spec.min_models,
            spec_sha256: r.sha256,
        }));
    }
    for s in &cfg.sources {
        bodies.push(Body::RuleSet(RuleSet::ExternalSource { source: s.id.clone(), stream: s.stream.clone() }));
    }
    Ok(bodies)
}

/// The catalog's stations that the ledger does not have yet, or has with other data.
pub fn station_bodies(cfg: &Config, book: &Book) -> Result<Vec<Body>> {
    let mut out = Vec::new();
    for st in cota_ingest::config::load(&cfg.catalog)? {
        let s = cota_ledger::Station {
            station_id: st.id.clone(),
            name: st.name.clone(),
            source: st.source as u8,
            variable: st.variable as u8,
            lat_e6: (st.lat * 1e6).round() as i32,
            lon_e6: (st.lon * 1e6).round() as i32,
            datum_id: st.datum_id.clone(),
            stream: cfg.main_stream.clone(),
        };
        if book.stations.get(&st.id) != Some(&s) {
            out.push(Body::Station(s));
        }
    }
    Ok(out)
}

fn read_json_files<T: serde::de::DeserializeOwned>(dir: &Path) -> Vec<T> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files.into_iter().filter_map(|f| serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()).collect()
}

/// The node's v2 batch files (`<node>/<id>.json`), by day.
fn node_batches(node_dir: &Path) -> BTreeMap<NaiveDate, SignedBatch> {
    read_json_files::<SignedBatch>(node_dir).into_iter().map(|b| (b.day, b)).collect()
}

/// What this node observed for the main stream: the batch of the next day it has (gaps allowed), and the outlooks
/// and third-party forecasts of that day and of the day the ledger last closed.
pub fn day_bodies(cfg: &Config, book: &Book, node_dir: &Path) -> Result<Vec<Body>> {
    let Some(s) = book.ledger.stream(&cfg.main_stream) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    let mut days: Vec<i64> = vec![];
    if s.next_batch_id > 1 {
        days.push(s.last_t_end);
    }
    if let Some((day, b)) = node_batches(node_dir).into_iter().find(|(d, _)| start_of_day(*d) >= s.last_t_end) {
        out.push(Body::Batch(cota_ledger::Batch {
            stream: cfg.main_stream.clone(),
            batch_id: s.next_batch_id,
            t_start: b.header.t_start,
            t_end: b.header.t_end,
            leaf_count: b.header.leaf_count,
            merkle_root: b.header.merkle_root,
            station_set_hash: b.header.station_set_hash,
            prev_batch_root: s.last_batch_root,
        }));
        days.push(end_of_day(day));
    }
    // Outlooks issued at the end of those days (issued_at = 00:00 of the next day).
    for o in read_json_files::<SignedOutlook>(&node_dir.join("outlook")) {
        if days.contains(&o.header.issued_at) {
            let h = o.header;
            out.push(Body::Outlook(cota_ledger::Outlook {
                region: h.region,
                issued_at: h.issued_at,
                window_end: h.window_end,
                models: h.models,
                rain_mean_milli: h.rain_mean_milli,
                rain_min_milli: h.rain_min_milli,
                rain_max_milli: h.rain_max_milli,
                content_sha256: h.content_sha256,
            }));
        }
    }
    for f in read_json_files::<SignedForecast>(&node_dir.join("extforecast")) {
        let issued_at = end_of_day(f.header.day);
        let Some(src) = cfg.sources.iter().find(|s| s.id == f.header.source) else { continue };
        if days.contains(&issued_at) {
            out.push(Body::ExternalForecast(cota_ledger::ExternalForecast {
                source: src.id.clone(),
                stream: src.stream.clone(),
                issued_at,
                horizon_days: src.horizon_days,
                content_sha256: decode_hex(&f.header.content_sha256)?,
            }));
        }
    }
    Ok(out)
}

/// The day whose flow forecast the nodes should make now: the day of the main stream's latest batch, if no forecast
/// is based on it yet. The model reads that batch from the v2 ledger, where it lands at the end of the round that
/// anchors it, so the nodes forecast one round after the batch.
pub fn forecast_day(cfg: &Config, book: &Book) -> Option<NaiveDate> {
    let s = book.ledger.stream(&cfg.main_stream)?;
    if s.next_batch_id == 1 || book.forecast_bases.contains(&(cfg.main_stream.clone(), s.next_batch_id - 1)) {
        return None;
    }
    DateTime::from_timestamp(s.last_t_end - 1, 0).map(|d| d.date_naive())
}

/// The next day the main stream waits for, once it has closed and `settle_secs` have passed (the sources publish a
/// day's last hours late): the day, the batch number it will get and the root it chains to. `None` before Genesis or
/// when that day is still open.
pub fn next_day(cfg: &Config, book: &Book, now: i64, settle_secs: i64) -> Option<(NaiveDate, u64, Hash)> {
    let s = book.ledger.stream(&cfg.main_stream)?;
    let day = DateTime::from_timestamp(s.last_t_end, 0)?.date_naive();
    (end_of_day(day) + settle_secs <= now).then_some((day, s.next_batch_id, s.last_batch_root))
}

/// Whole m³/s (×1000): machines that differ in the last bits of a float reach the same bytes.
fn round_milli(v: i64) -> i64 {
    (v as f64 / 1000.0).round() as i64 * 1000
}

/// The flow forecast this node computed with the public model (`<node>/forecast/<day>.json`, written by
/// `cota_ml.forecast --out`), as bodies based on the main stream's latest batch.
pub fn forecast_bodies(cfg: &Config, book: &Book, node_dir: &Path) -> Result<Vec<Body>> {
    let Some(day) = forecast_day(cfg, book) else { return Ok(Vec::new()) };
    let path = node_dir.join("forecast").join(format!("{day}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(Vec::new()) };
    let f: Value = serde_json::from_str(&text).with_context(|| format!("interpretando {}", path.display()))?;
    let s = book.ledger.stream(&cfg.main_stream).expect("forecast_day checked it");
    // The model read the same readings as the batch it is based on.
    if f["based_on_root"].as_str() != Some(hex::encode(s.last_batch_root).as_str()) {
        eprintln!("aviso: {} parte de outro lote; previsão não assinada", path.display());
        return Ok(Vec::new());
    }
    let station = f["station_id"].as_str().context("station_id")?.to_string();
    let model_sha256 = decode_hex(f["model_sha256"].as_str().context("model_sha256")?)?;
    let features_root = decode_hex(f["features_root"].as_str().context("features_root")?)?;
    let mut out = Vec::new();
    for h in f["forecasts"].as_array().context("forecasts")? {
        let q = |k: &str| h[k].as_i64().with_context(|| format!("{k} em {}", path.display()));
        out.push(Body::Forecast(cota_ledger::Forecast {
            stream: cfg.main_stream.clone(),
            station_id: station.clone(),
            based_on_batch_id: s.next_batch_id - 1,
            based_on_root: s.last_batch_root,
            target_ts: q("target_ts")?,
            horizon_hours: q("horizon_hours")? as u16,
            p10: round_milli(q("p10")?),
            p50: round_milli(q("p50")?),
            p90: round_milli(q("p90")?),
            model_sha256,
            features_root,
        }));
    }
    Ok(out)
}

/// The official warnings this node saw that the ledger does not have yet.
pub fn alert_bodies(book: &Book, node_dir: &Path) -> Vec<Body> {
    let mut seen = BTreeSet::new();
    read_json_files::<SignedAlert>(&node_dir.join("alerts"))
        .into_iter()
        .filter(|a| !book.alert_versions.contains(&a.header.content_sha256) && seen.insert(a.header.content_sha256))
        .map(|a| {
            let h = a.header;
            Body::PublicAlert(cota_ledger::PublicAlert {
                issuer: h.issuer,
                alert_id: h.alert_id,
                content_sha256: h.content_sha256,
                severity: h.severity,
                onset: h.onset,
                expires: h.expires,
            })
        })
        .collect()
}

fn short_digest(message: &[u8]) -> String {
    hex::encode(&Sha256::digest(message)[..8])
}

/// A node's round: signs the checkpoint of the ledger as it found it, and every body it observed. Returns how many
/// bodies it signed.
pub fn sign_round(key: &SigningKey, cfg: &Config, book: &Book, node_dir: &Path) -> Result<usize> {
    let out = node_dir.join("v3");
    // The folder holds exactly what this node attests now: what it signed before and the ledger already has, or no
    // longer applies, goes away.
    if out.is_dir() {
        for f in std::fs::read_dir(&out)? {
            let f = f?.path();
            if f.extension().is_some_and(|e| e == "json") {
                std::fs::remove_file(f)?;
            }
        }
    }
    std::fs::create_dir_all(&out)?;
    let member = bs58::encode(key.verifying_key().as_bytes()).into_string();
    if let Some(cp) = book.next_checkpoint() {
        let signed = SignedCheckpoint {
            member: member.clone(),
            checkpoint: hex::encode(borsh::to_vec(&cp)?),
            signature: hex::encode(key.sign(&cp.signing_message()).to_bytes()),
        };
        std::fs::write(out.join(format!("checkpoint-{:06}.json", cp.size)), serde_json::to_string_pretty(&signed)?)?;
    }
    let mut bodies = Vec::new();
    if !book.has_genesis() {
        bodies.extend(opening_bodies(cfg, book)?);
        // The stations can go in the same round: their stream is created before them.
        let empty = Book { stations: BTreeMap::new(), ..book.shallow() };
        bodies.extend(station_bodies(cfg, &empty)?);
    } else {
        bodies.extend(station_bodies(cfg, book)?);
        bodies.extend(day_bodies(cfg, book, node_dir)?);
        bodies.extend(forecast_bodies(cfg, book, node_dir)?);
        bodies.extend(alert_bodies(book, node_dir));
    }
    bodies.retain(|b| !book.digests.contains(&<[u8; 32]>::from(Sha256::digest(b.signing_message()))));
    for body in &bodies {
        let message = body.signing_message();
        let signed = SignedBody {
            member: member.clone(),
            kind: body.kind().into(),
            body: hex::encode(borsh::to_vec(body)?),
            signature: hex::encode(key.sign(&message).to_bytes()),
        };
        std::fs::write(out.join(format!("{}-{}.json", body.kind(), short_digest(&message))), serde_json::to_string_pretty(&signed)?)?;
    }
    Ok(bodies.len())
}

impl Book {
    /// A copy of the bookkeeping without the ledger (enough for `station_bodies`).
    fn shallow(&self) -> Book {
        Book {
            dir: self.dir.clone(),
            ledger: Ledger::new(),
            entries: Vec::new(),
            checkpoints: Vec::new(),
            stations: self.stations.clone(),
            alert_versions: self.alert_versions.clone(),
            digests: self.digests.clone(),
            forecast_bases: self.forecast_bases.clone(),
            v2_records: Vec::new(),
            v2_dir: self.v2_dir.clone(),
        }
    }
}

// ---- what the aggregator collects -------------------------------------------------------------------

/// What happened to one body in a collection.
#[derive(Debug, Clone)]
pub enum Collected {
    Appended { index: u64, kind: String, signers: usize },
    /// Fewer valid member signatures than the threshold.
    NoQuorum { kind: String, signers: usize, need: u16 },
    Refused { kind: String, reason: Refusal },
}

#[derive(Debug, Default)]
pub struct Report {
    pub checkpoint: Option<(u64, usize)>,
    pub results: Vec<Collected>,
    /// Contents filled for entries already in.
    pub backfilled: usize,
}

fn read_node_files(nodes_dir: &Path) -> (Vec<SignedBody>, Vec<SignedCheckpoint>) {
    let mut bodies = Vec::new();
    let mut checkpoints = Vec::new();
    let Ok(rd) = std::fs::read_dir(nodes_dir) else { return (bodies, checkpoints) };
    let mut nodes: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path().join("v3"))).filter(|p| p.is_dir()).collect();
    nodes.sort();
    for dir in nodes {
        for f in read_json_files::<Value>(&dir) {
            if f.get("checkpoint").is_some() {
                if let Ok(c) = serde_json::from_value(f) {
                    checkpoints.push(c);
                }
            } else if let Ok(b) = serde_json::from_value(f) {
                bodies.push(b);
            }
        }
    }
    (bodies, checkpoints)
}

/// The valid signatures of members of `committee` over `message`, one per member, sorted by position.
fn member_signatures<'a>(
    committee: &cota_ledger::Committee,
    message: &[u8],
    signers: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<Signature> {
    let mut by_position = BTreeMap::new();
    for (member, sig) in signers {
        let Ok(key) = key_from_base58(member) else { continue };
        let Some(position) = committee.members.iter().position(|m| *m == key) else { continue };
        let Ok(sig) = decode_hex::<64>(sig) else { continue };
        let Ok(vk) = VerifyingKey::from_bytes(&key) else { continue };
        if vk.verify_strict(message, &DalekSignature::from_bytes(&sig)).is_ok() {
            by_position.insert(position as u16, Signature { member: position as u16, signature: sig });
        }
    }
    by_position.into_values().collect()
}

/// Application order within a round: the opening first, then stations, batches (by stream and number), and what
/// depends on a closed day.
fn order(body: &Body) -> (u8, String, u64) {
    match body {
        Body::Genesis(_) => (0, String::new(), 0),
        Body::StreamCreate(s) => (1, s.id.clone(), 0),
        Body::RuleSet(_) => (2, String::new(), 0),
        Body::Station(s) => (3, s.station_id.clone(), 0),
        Body::Batch(b) => (4, b.stream.clone(), b.batch_id),
        Body::Degradation(_) => (5, String::new(), 0),
        Body::ExternalForecast(_) => (6, String::new(), 0),
        Body::Outlook(_) => (7, String::new(), 0),
        Body::PublicAlert(_) => (8, String::new(), 0),
        Body::Forecast(_) => (9, String::new(), 0),
        Body::Correction(_) => (10, String::new(), 0),
        Body::CommitteeChange(_) => (11, String::new(), 0),
    }
}

/// The aggregator's round: publishes the checkpoint a majority signed for the current state, then appends every body
/// a majority signed, in `order`, through the rules.
pub fn collect(book: &mut Book, nodes_dir: &Path, now: i64, quality: Option<&QualityInput>) -> Result<Report> {
    let mut report = Report::default();
    let (bodies, checkpoints) = read_node_files(nodes_dir);

    // 0. Contents missing for entries already in (written before contents were published): filled from the nodes or
    //    the v2 ledger, each checked against its hash.
    let (_, missing) = check_contents(book);
    if !missing.is_empty() {
        let base = book.ledger.genesis_index().unwrap_or(0);
        for (i, e) in book.entries.iter().enumerate() {
            write_content(book, nodes_dir, e, base + i as u64)?;
        }
        let (_, still) = check_contents(book);
        report.backfilled = missing.len() - still.len();
    }

    // 1. The checkpoint of the state the nodes verified.
    if let (Some(cp), Some(committee)) = (book.next_checkpoint(), book.ledger.committee().cloned()) {
        let message = cp.signing_message();
        let wanted = hex::encode(borsh::to_vec(&cp)?);
        let sigs = member_signatures(
            &committee,
            &message,
            checkpoints.iter().filter(|c| c.checkpoint == wanted).map(|c| (c.member.as_str(), c.signature.as_str())),
        );
        if sigs.len() >= committee.threshold as usize {
            book.ledger.verify_checkpoint(&cp, &sigs, book.checkpoints.last().map(|(c, _)| c)).map_err(|r| anyhow!("{r}"))?;
            report.checkpoint = Some((cp.size, sigs.len()));
            book.write_checkpoint(cp, sigs)?;
        }
    }

    // 2. The bodies, grouped by their exact bytes.
    let mut groups: BTreeMap<String, Vec<&SignedBody>> = BTreeMap::new();
    for b in &bodies {
        groups.entry(b.body.clone()).or_default().push(b);
    }
    let mut decoded: Vec<(Body, Vec<&SignedBody>)> = groups
        .into_iter()
        .filter_map(|(hex_body, signers)| Some((borsh::from_slice::<Body>(&hex::decode(hex_body).ok()?).ok()?, signers)))
        .collect();
    decoded.sort_by_key(|(b, _)| order(b));

    for (body, signers) in decoded {
        let kind = body.kind().to_string();
        let message = body.signing_message();
        // Before Genesis, the committee is the one Genesis names; the opening entries need it applied first.
        let (committee, epoch) = match (&body, book.ledger.committee()) {
            (Body::Genesis(g), None) => {
                let Ok(c) = cota_ledger::Committee::new(g.members.clone(), g.threshold) else {
                    report.results.push(Collected::Refused { kind, reason: Refusal::InvalidCommittee });
                    continue;
                };
                // Genesis sits right after the v2 records it cites, not after those v2 appended since.
                let n = g.v2_leaf_count as usize;
                if n > book.v2_records.len() {
                    report.results.push(Collected::Refused { kind, reason: Refusal::GenesisMismatch });
                    continue;
                }
                book.ledger = Ledger::new();
                for r in &book.v2_records[..n] {
                    book.ledger.push_v2_record(r).map_err(|e| anyhow!("{e}"))?;
                }
                (c, 0)
            }
            (_, Some(c)) => (c.clone(), book.ledger.epoch().unwrap_or(0)),
            (_, None) => {
                report.results.push(Collected::Refused { kind, reason: Refusal::NoGenesis });
                continue;
            }
        };
        let sigs = member_signatures(&committee, &message, signers.iter().map(|s| (s.member.as_str(), s.signature.as_str())));
        if sigs.len() < committee.threshold as usize {
            report.results.push(Collected::NoQuorum { kind, signers: sigs.len(), need: committee.threshold });
            continue;
        }
        let signers_count = sigs.len();
        // A batch window that has not ended yet: never published, even with a majority (the ledger clock must not move
        // ahead of real time, see docs/security-review-v3.md).
        if let Body::Batch(b) = &body {
            if b.t_end > now + 3600 {
                report.results.push(Collected::Refused { kind, reason: Refusal::FutureBatch });
                continue;
            }
        }
        let entry = Entry { body, quorum: Quorum { committee: MAIN_COMMITTEE, epoch, signatures: sigs } };
        match book.ledger.apply(&entry) {
            Ok(applied) => {
                if let Body::Batch(b) = &entry.body {
                    write_batch_readings(book, nodes_dir, b, quality)?;
                }
                write_content(book, nodes_dir, &entry, applied.index)?;
                book.append(entry, &applied)?;
                report.results.push(Collected::Appended { index: applied.index, kind, signers: signers_count });
            }
            // Already in (a node that has not seen the latest entries yet signs them again).
            Err(Refusal::Duplicate | Refusal::GenesisAgain) => {}
            Err(reason) => report.results.push(Collected::Refused { kind, reason }),
        }
    }
    Ok(report)
}

/// The Merkle root (RFC 9162) of a forecast's features, as `cota_ml.canonical` builds it: leaf data `name=value`, or
/// `name=null`.
pub fn features_root(features: &Value) -> Option<Hash> {
    let leaves: Vec<Hash> = features
        .as_array()?
        .iter()
        .map(|f| {
            let value = f["value_milli"].as_i64().map(|v| v.to_string()).unwrap_or_else(|| "null".into());
            cota_core::merkle::hash_leaf(format!("{}={value}", f["name"].as_str().unwrap_or_default()).as_bytes())
        })
        .collect();
    Some(cota_core::merkle::root_from_leaves(&leaves))
}

fn sha256_hex(text: &str) -> Hash {
    Sha256::digest(text.as_bytes()).into()
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

/// The content an entry commits to by hash, from a node that has it (its hash checked here), next to the entry:
/// `outlooks/<region>-<day>.json`, `external_forecasts/<source>/<day>.json`, `public_alerts/<sha256>.json`,
/// `forecasts/<day>.json` (the model run: features, quantiles). Without it the entry would point to nothing.
fn day_of(issued_at: i64) -> String {
    DateTime::from_timestamp(issued_at - 1, 0).map(|d| d.date_naive().to_string()).unwrap_or_default()
}

/// A v2 content file's `content`, if it gives `hash` (the rehearsal's entries before contents were published).
fn v2_content(path: PathBuf, hash: &Hash) -> Option<(String, Value)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let c = v["content"].as_str()?.to_string();
    (sha256_hex(&c) == *hash).then_some((c, v))
}

fn write_content(book: &Book, nodes_dir: &Path, entry: &Entry, index: u64) -> Result<()> {
    let nodes: Vec<PathBuf> = std::fs::read_dir(nodes_dir).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    match &entry.body {
        Body::Outlook(o) => {
            let found = nodes
                .iter()
                .flat_map(|n| read_json_files::<SignedOutlook>(&n.join("outlook")))
                .find(|s| s.header.content_sha256 == o.content_sha256 && sha256_hex(&s.content) == o.content_sha256)
                .map(|s| s.content)
                .or_else(|| {
                    let day = NaiveDate::parse_from_str(&day_of(o.issued_at), "%Y-%m-%d").ok()?;
                    v2_content(book.v2_dir.join("outlooks").join(outlook::file_name(&o.region, day)), &o.content_sha256).map(|(c, _)| c)
                });
            if let Some(content) = found {
                let path = book.dir.join("outlooks").join(format!("{}-{}.json", o.region, day_of(o.issued_at)));
                write_json(&path, &serde_json::json!({ "index": index, "region": o.region, "issued_at": o.issued_at, "content_sha256": hex::encode(o.content_sha256), "content": content }))?;
            }
        }
        Body::ExternalForecast(e) => {
            let wanted = hex::encode(e.content_sha256);
            let found = nodes
                .iter()
                .flat_map(|n| read_json_files::<SignedForecast>(&n.join("extforecast")))
                .find(|s| s.header.content_sha256 == wanted && sha256_hex(&s.content) == e.content_sha256)
                .map(|s| s.content)
                .or_else(|| v2_content(book.v2_dir.join("external_forecasts").join(&e.source).join(format!("{}.json", day_of(e.issued_at))), &e.content_sha256).map(|(c, _)| c));
            if let Some(content) = found {
                let path = book.dir.join("external_forecasts").join(&e.source).join(format!("{}.json", day_of(e.issued_at)));
                write_json(&path, &serde_json::json!({ "index": index, "source": e.source, "issued_at": e.issued_at, "content_sha256": wanted, "content": content }))?;
            }
        }
        Body::PublicAlert(a) => {
            let found = nodes
                .iter()
                .flat_map(|n| read_json_files::<SignedAlert>(&n.join("alerts")))
                .find(|s| s.header.content_sha256 == a.content_sha256 && sha256_hex(&s.content) == a.content_sha256)
                .map(|s| (s.content, s.description, s.severity_label, s.states))
                .or_else(|| {
                    let (c, v) = v2_content(book.v2_dir.join("public_alerts").join(format!("{}.json", hex::encode(a.content_sha256))), &a.content_sha256)?;
                    let field = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                    Some((c, field("description"), field("severity_label"), field("states")))
                });
            if let Some((content, description, severity_label, states)) = found {
                let path = book.dir.join("public_alerts").join(format!("{}.json", hex::encode(a.content_sha256)));
                write_json(
                    &path,
                    &serde_json::json!({
                        "index": index, "issuer": a.issuer, "alert_id": a.alert_id, "content_sha256": hex::encode(a.content_sha256),
                        "description": description, "severity_label": severity_label, "states": states, "content": content,
                    }),
                )?;
            }
        }
        Body::Forecast(f) => {
            // The run of any node with the same base and features (all of them, when they agree).
            for n in &nodes {
                for run in read_json_files::<Value>(&n.join("forecast")) {
                    if run["based_on_root"].as_str() == Some(hex::encode(f.based_on_root).as_str()) && features_root(&run["features"]) == Some(f.features_root) {
                        let day = run["issue_day"].as_str().unwrap_or_default().to_string();
                        let path = book.dir.join("forecasts").join(format!("{day}.json"));
                        let mut out = run.clone();
                        let indexes = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["indexes"].as_array().cloned()).unwrap_or_default();
                        let mut indexes: Vec<Value> = indexes.into_iter().filter(|i| i.as_u64() != Some(index)).collect();
                        indexes.push(index.into());
                        out["indexes"] = Value::Array(indexes);
                        return write_json(&path, &out);
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// What the station quality check needs: its limits and the catalog (id, source).
pub struct QualityInput {
    pub limits: cota_quality::Limits,
    pub catalog: Vec<(String, cota_core::Source)>,
}

impl QualityInput {
    pub fn load(cfg: &Config, limits: &Path) -> Result<Self> {
        Ok(Self {
            limits: cota_quality::Limits::load(limits)?,
            catalog: cota_ingest::config::load(&cfg.catalog)?.into_iter().map(|s| (s.id, s.source)).collect(),
        })
    }
}

/// `batches/<stream>-<id>.json`: the readings of a batch, from a node whose batch has the same Merkle root, and the
/// quality of each station (`cota-quality`, as v2's aggregator does): stuck or jumping readings, gaps, and the nodes
/// that saw different readings. Quality is derived data, not signed; its state carries over in `quality-state.json`.
fn write_batch_readings(book: &Book, nodes_dir: &Path, b: &cota_ledger::Batch, quality: Option<&QualityInput>) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(nodes_dir) else { return Ok(()) };
    let mut nodes: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    nodes.sort();
    let day_batches: Vec<SignedBatch> = nodes.iter().filter_map(|n| node_batches(n).into_values().find(|s| s.header.t_start == b.t_start)).collect();
    let Some(found) = day_batches.iter().find(|s| s.header.merkle_root == b.merkle_root) else { return Ok(()) };
    let quality_json = match quality {
        None => Value::Null,
        Some(q) => {
            let members: Vec<String> = book.ledger.committee().map(|c| c.members.iter().map(base58).collect()).unwrap_or_default();
            let valid: Vec<&SignedBatch> = day_batches.iter().filter(|s| members.contains(&s.node)).collect();
            let readings: Vec<&[cota_core::Reading]> = valid.iter().map(|s| s.readings.as_slice()).collect();
            let agreement = cota_quality::node_agreement(&readings);
            let state_path = book.dir.join("quality-state.json");
            let mut state: cota_quality::State =
                std::fs::read_to_string(&state_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let stations = cota_quality::assess(&mut state, &q.limits, &q.catalog, b.t_start, b.t_end, &found.readings, &agreement);
            std::fs::write(&state_path, serde_json::to_string_pretty(&state)? + "\n")?;
            let dissents: Vec<Value> = agreement
                .dissents
                .iter()
                .map(|d| {
                    let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
                    v["member"] = Value::String(valid[d.node].node.clone());
                    v
                })
                .collect();
            serde_json::json!({ "stations": stations, "dissents": dissents })
        }
    };
    let file = serde_json::json!({
        "stream": b.stream,
        "batch_id": b.batch_id,
        "day": found.day.to_string(),
        "merkle_root": hex::encode(b.merkle_root),
        "leaf_count": b.leaf_count,
        "readings": found.readings,
        "quality": quality_json,
    });
    write_json(&book.dir.join("batches").join(format!("{}-{:05}.json", b.stream, b.batch_id)), &file)
}

/// Every batch's published readings (`batches/<stream>-<id>.json`) rebuild the Merkle root, count and station set the
/// committee signed. Returns how many batches were checked and the problems found (a missing file is one).
pub fn check_readings(book: &Book) -> (usize, Vec<String>) {
    let mut checked = 0;
    let mut problems = Vec::new();
    for e in &book.entries {
        let Body::Batch(b) = &e.body else { continue };
        let path = book.dir.join("batches").join(format!("{}-{:05}.json", b.stream, b.batch_id));
        let readings = std::fs::read_to_string(&path)
            .map_err(|_| "arquivo ausente".to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .and_then(|v| serde_json::from_value::<Vec<cota_core::Reading>>(v["readings"].clone()).map_err(|e| e.to_string()));
        match readings.map(|r| cota_core::Batch::build(b.batch_id, b.t_start, b.t_end, b.prev_batch_root, r)) {
            Ok(Ok(built)) if built.header.merkle_root == b.merkle_root && built.header.leaf_count == b.leaf_count && built.header.station_set_hash == b.station_set_hash => checked += 1,
            Ok(Ok(_)) => problems.push(format!("lote {} de {}: as leituras publicadas não dão a raiz assinada", b.batch_id, b.stream)),
            Ok(Err(e)) => problems.push(format!("lote {} de {}: leituras inválidas ({e})", b.batch_id, b.stream)),
            Err(e) => problems.push(format!("lote {} de {}: {e} ({})", b.batch_id, b.stream, path.display())),
        }
    }
    (checked, problems)
}

/// Every published content (outlooks, third-party forecasts, warnings, flow forecast runs) gives the hash its entry
/// committed to. Returns how many were checked and the problems found (a missing file is one).
pub fn check_contents(book: &Book) -> (usize, Vec<String>) {
    let mut checked = 0;
    let mut problems = Vec::new();
    let read = |p: PathBuf| std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let base = book.ledger.genesis_index().unwrap_or(0);
    for (i, e) in book.entries.iter().enumerate() {
        let index = base + i as u64;
        let (path, ok) = match &e.body {
            Body::Outlook(o) => {
                let p = book.dir.join("outlooks").join(format!("{}-{}.json", o.region, day_of(o.issued_at)));
                let ok = read(p.clone()).is_some_and(|v| v["content"].as_str().is_some_and(|c| sha256_hex(c) == o.content_sha256));
                (p, ok)
            }
            Body::ExternalForecast(x) => {
                let p = book.dir.join("external_forecasts").join(&x.source).join(format!("{}.json", day_of(x.issued_at)));
                let ok = read(p.clone()).is_some_and(|v| v["content"].as_str().is_some_and(|c| sha256_hex(c) == x.content_sha256));
                (p, ok)
            }
            Body::PublicAlert(a) => {
                let p = book.dir.join("public_alerts").join(format!("{}.json", hex::encode(a.content_sha256)));
                let ok = read(p.clone()).is_some_and(|v| v["content"].as_str().is_some_and(|c| sha256_hex(c) == a.content_sha256));
                (p, ok)
            }
            Body::Forecast(f) => {
                // Issue day: the target minus the horizon (a +24 h forecast of D+1 was issued on D).
                let day = DateTime::from_timestamp(f.target_ts - i64::from(f.horizon_hours) * 3600, 0).map(|d| d.date_naive().to_string()).unwrap_or_default();
                let p = book.dir.join("forecasts").join(format!("{day}.json"));
                let ok = read(p.clone()).is_some_and(|v| features_root(&v["features"]) == Some(f.features_root));
                (p, ok)
            }
            _ => continue,
        };
        if ok {
            checked += 1;
        } else {
            problems.push(format!("entrada {index} ({}): conteúdo ausente ou que não dá o hash assinado ({})", e.body.kind(), path.display()));
        }
    }
    (checked, problems)
}

/// For messages: a timestamp as an ISO date-time.
pub fn iso(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d %H:%MZ").to_string()).unwrap_or_default()
}

/// For messages: a committee member's key.
pub fn member_name(key: &Key) -> String {
    base58(key)[..8].to_string()
}
