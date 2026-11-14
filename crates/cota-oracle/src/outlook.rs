//! Forecast rain over the basin (early warning): each node downloads the same run of each
//! model (Open-Meteo Single Runs, the 00Z run of day D, immutable once published),
//! summarizes it and signs. Honest nodes reach the same bytes; the aggregator requires k of n.
//!
//! The rule (points, models, window, threshold) comes from `ml/models/outlook-rule.json`, whose
//! SHA-256 is fixed on-chain in the `OutlookRule`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use chrono::{Days, NaiveDate, NaiveTime};
use cota_core::{public_alert::content_hash, OutlookHeader};
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::attest::Attestation;
use crate::inmet_alerts::canonical_json;
use crate::keys::pubkey_base58;

pub const SINGLE_RUNS: &str = "https://single-runs-api.open-meteo.com/v1/forecast";
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Deserialize)]
pub struct Point {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Spec {
    pub region: String,
    pub points: Vec<Point>,
    pub models: Vec<String>,
    pub min_models: u8,
    pub run_hour_utc: u32,
    pub window_days: u64,
    pub threshold_mm: i64,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub sha256: [u8; 32],
    pub spec: Spec,
}

impl Rule {
    pub fn threshold_milli(&self) -> i64 {
        self.spec.threshold_mm * 1000
    }

    pub fn window_secs(&self) -> i64 {
        self.spec.window_days as i64 * DAY
    }

    /// Run used for the issue at the end of day `day`.
    pub fn run(&self, day: NaiveDate) -> String {
        format!("{day}T{:02}:00", self.spec.run_hour_utc)
    }

    /// Days in the window: D+1..=D+window_days.
    pub fn window(&self, day: NaiveDate) -> Vec<NaiveDate> {
        (1..=self.spec.window_days).map(|k| day + Days::new(k)).collect()
    }
}

/// The basin-wide rule's region: its outlooks keep their original file name (`<day>.json`).
pub const MAIN_REGION: &str = "guaiba";

/// File of one region's outlook for day `day`, at the nodes and in the ledger.
pub fn file_name(region: &str, day: NaiveDate) -> String {
    if region == MAIN_REGION {
        format!("{day}.json")
    } else {
        format!("{region}-{day}.json")
    }
}

/// The basin-wide rule plus every sub-basin rule in `dir` (sorted by file name), each checked by `load_rule`.
pub fn load_rules(main: &Path, dir: &Path) -> Result<Vec<Rule>> {
    let mut rules = vec![load_rule(main)?];
    if dir.is_dir() {
        let mut files: Vec<_> = std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<_>>()?;
        files.retain(|f| f.extension().is_some_and(|e| e == "json"));
        files.sort();
        for f in files {
            rules.push(load_rule(&f)?);
        }
    }
    Ok(rules)
}

/// Reads the rule and checks that the declared hash is that of the specification's canonical JSON
/// (the same one Python writes and that goes on-chain).
pub fn load_rule(path: &Path) -> Result<Rule> {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?)?;
    let spec = file.get("spec").context("regra sem `spec`")?;
    let mut canon = String::new();
    canonical_json(spec, &mut canon);
    let sha: [u8; 32] = Sha256::digest(canon.as_bytes()).into();
    let declared = file.get("rule_sha256").and_then(Value::as_str).unwrap_or_default();
    ensure!(hex::encode(sha) == declared, "{}: rule_sha256 não confere com a especificação", path.display());
    Ok(Rule { sha256: sha, spec: serde_json::from_value(spec.clone())? })
}

/// Daily rain in tenths of a mm: {model: {point: [D+1, …]}}.
pub type Totals = BTreeMap<String, BTreeMap<String, Vec<i64>>>;

/// Sums the 24 hours of each requested day; `None` if any hour is missing.
pub fn daily_tenths(times: &[String], values: &[Option<f64>], days: &[NaiveDate]) -> Option<Vec<i64>> {
    days.iter()
        .map(|d| {
            let prefix = d.to_string();
            let hours: Vec<f64> = times
                .iter()
                .zip(values)
                .filter(|(t, _)| t.starts_with(&prefix))
                .map(|(_, v)| *v)
                .collect::<Option<_>>()?;
            (hours.len() == 24).then(|| (hours.iter().sum::<f64>() * 10.0).round() as i64)
        })
        .collect()
}

pub fn fetch_point(http: &reqwest::blocking::Client, rule: &Rule, model: &str, p: &Point, day: NaiveDate) -> Result<Vec<i64>> {
    let url = format!(
        "{SINGLE_RUNS}?latitude={}&longitude={}&hourly=precipitation&models={model}&run={}&forecast_days={}&timezone=UTC",
        p.lat,
        p.lon,
        rule.run(day),
        rule.spec.window_days + 2,
    );
    let body: Value = http.get(&url).send().and_then(|r| r.error_for_status()).with_context(|| format!("{model} em {}", p.id))?.json()?;
    let times: Vec<String> = serde_json::from_value(body["hourly"]["time"].clone()).context("resposta sem hourly.time")?;
    let values: Vec<Option<f64>> = serde_json::from_value(body["hourly"]["precipitation"].clone()).context("resposta sem precipitação")?;
    daily_tenths(&times, &values, &rule.window(day)).with_context(|| format!("{model} em {}: horas faltando na rodada {}", p.id, rule.run(day)))
}

/// Downloads every model; a model with any point missing is left out.
pub fn fetch(http: &reqwest::blocking::Client, rule: &Rule, day: NaiveDate) -> Totals {
    let mut out = Totals::new();
    'model: for m in &rule.spec.models {
        let mut points = BTreeMap::new();
        for p in &rule.spec.points {
            match fetch_point(http, rule, m, p, day) {
                Ok(v) => {
                    points.insert(p.id.clone(), v);
                }
                Err(e) => {
                    eprintln!("aviso: {e:#}; {m} fica de fora");
                    continue 'model;
                }
            }
        }
        out.insert(m.clone(), points);
    }
    out
}

#[derive(Debug, Clone)]
pub struct Observed {
    pub header: OutlookHeader,
    /// Canonical JSON: the bytes whose SHA-256 is `header.content_sha256`.
    pub content: String,
}

pub fn issued_at(day: NaiveDate) -> i64 {
    (day + Days::new(1)).and_time(NaiveTime::MIN).and_utc().timestamp()
}

/// Summarizes each model's rain (mean of the points, accumulated over the window) and builds the
/// content.
pub fn summarize(rule: &Rule, day: NaiveDate, totals: &Totals) -> Result<Observed> {
    let n_points = rule.spec.points.len() as i64;
    let mut kept = Totals::new();
    let mut per_model = Vec::new();
    for m in &rule.spec.models {
        let Some(points) = totals.get(m) else { continue };
        let complete = rule.spec.points.iter().all(|p| points.get(&p.id).is_some_and(|v| v.len() == rule.spec.window_days as usize));
        if !complete {
            continue;
        }
        let tenths: i64 = points.values().flatten().sum();
        per_model.push(tenths * 100 / n_points);
        kept.insert(m.clone(), points.clone());
    }
    if per_model.len() < rule.spec.min_models as usize {
        bail!("só {} modelo(s) completo(s); a regra exige {}", per_model.len(), rule.spec.min_models);
    }
    let issued = issued_at(day);
    let window = rule.window(day);
    let content_value = json!({
        "issued_at": issued,
        "models": kept,
        "region": rule.spec.region,
        "rule_sha256": hex::encode(rule.sha256),
        "run": format!("{}Z", rule.run(day)),
        "unit": "0.1 mm/dia",
        "window": [window[0].to_string(), window[window.len() - 1].to_string()],
    });
    let mut content = String::new();
    canonical_json(&content_value, &mut content);
    let header = OutlookHeader {
        region: rule.spec.region.clone(),
        issued_at: issued,
        window_end: issued + rule.window_secs(),
        models: per_model.len() as u8,
        rain_mean_milli: per_model.iter().sum::<i64>() / per_model.len() as i64,
        rain_min_milli: *per_model.iter().min().unwrap(),
        rain_max_milli: *per_model.iter().max().unwrap(),
        content_sha256: content_hash(&content),
    };
    Ok(Observed { header, content })
}

/// What a node publishes about a day's forecast rain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedOutlook {
    pub node: String,
    pub header: OutlookHeader,
    #[serde(with = "crate::node::hex64")]
    pub signature: [u8; 64],
    pub content: String,
    pub seen_at: i64,
}

pub fn sign(key: &SigningKey, o: &Observed, seen_at: i64) -> SignedOutlook {
    SignedOutlook {
        node: pubkey_base58(key),
        signature: key.sign(&o.header.signing_message_for(crate::PROTOCOL)).to_bytes(),
        header: o.header.clone(),
        content: o.content.clone(),
        seen_at,
    }
}

impl Attestation for SignedOutlook {
    type Header = OutlookHeader;
    fn node(&self) -> &str {
        &self.node
    }
    fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
    fn header(&self) -> &OutlookHeader {
        &self.header
    }
    fn message(&self) -> Vec<u8> {
        self.header.signing_message_for(crate::PROTOCOL)
    }
    fn content_matches(&self) -> bool {
        content_hash(&self.content) == self.header.content_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sub_basin_rule_has_its_hash_and_fits_the_program() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let rules = load_rules(&root.join("ml/models/outlook-rule.json"), &root.join("ml/models/outlook-rules")).unwrap();
        let mut regions: Vec<&str> = rules.iter().map(|r| r.spec.region.as_str()).collect();
        assert!(rules.len() >= 6, "{regions:?}");
        assert!(regions.iter().all(|r| !r.is_empty() && r.len() <= 16), "{regions:?}");
        regions.sort();
        regions.dedup();
        assert_eq!(regions.len(), rules.len(), "regiões repetidas");
        assert_eq!(file_name("guaiba", NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()), "2026-11-12.json");
        assert_eq!(file_name("cai", NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()), "cai-2026-11-12.json");
    }

    /// The rules for the rest of RS (Etapa 5.3) wait in their own folder, which the round does not read, until 5.4:
    /// they must already load, with their hash, and not repeat a region of the rules in use.
    #[test]
    fn rules_for_the_rest_of_rs_load_and_do_not_clash() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let live = load_rules(&root.join("ml/models/outlook-rule.json"), &root.join("ml/models/outlook-rules")).unwrap();
        let dir = root.join("ml/models/outlook-rules-rs");
        let mut regions: Vec<String> = live.iter().map(|r| r.spec.region.clone()).collect();
        let mut count = 0;
        for f in std::fs::read_dir(&dir).unwrap() {
            let r = load_rule(&f.unwrap().path()).unwrap();
            assert!(!r.spec.region.is_empty() && r.spec.region.len() <= 16, "{}", r.spec.region);
            assert!(r.spec.points.len() >= 2 && r.threshold_milli() > 0);
            regions.push(r.spec.region.clone());
            count += 1;
        }
        assert!(count >= 1);
        let n = regions.len();
        regions.sort();
        regions.dedup();
        assert_eq!(regions.len(), n, "regiões repetidas");
    }

    fn rule() -> Rule {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ml/models/outlook-rule.json");
        load_rule(&root).unwrap()
    }

    #[test]
    fn rule_hash_matches_python_canonical_json() {
        let r = rule();
        assert_eq!(r.spec.region, "guaiba");
        assert_eq!(r.spec.points.len(), 6);
        assert_eq!(r.threshold_milli(), 90_000);
    }

    #[test]
    fn daily_tenths_needs_every_hour() {
        let days = [NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()];
        let times: Vec<String> = (0..24).map(|h| format!("2026-10-25T{h:02}:00")).chain(["2026-10-26T00:00".into()]).collect();
        let mut values: Vec<Option<f64>> = vec![Some(0.25); 25];
        assert_eq!(daily_tenths(&times, &values, &days), Some(vec![60]));
        values[3] = None;
        assert_eq!(daily_tenths(&times, &values, &days), None);
        assert_eq!(daily_tenths(&times[..23], &values[..23], &days), None);
    }

    #[test]
    fn summary_is_deterministic_and_drops_incomplete_models() {
        let r = rule();
        let day = NaiveDate::from_ymd_opt(2024, 4, 26).unwrap();
        let full = |mm: i64| r.spec.points.iter().map(|p| (p.id.clone(), vec![mm; 5])).collect::<BTreeMap<_, _>>();
        let mut t = Totals::new();
        t.insert("ecmwf_ifs025".into(), full(200)); // 20 mm/day → 100 mm
        t.insert("gfs_global".into(), full(100)); // 50 mm
        let mut partial = full(999);
        partial.pop_first();
        t.insert("icon_global".into(), partial);

        let a = summarize(&r, day, &t).unwrap();
        assert_eq!(a.header.models, 2);
        assert_eq!((a.header.rain_mean_milli, a.header.rain_min_milli, a.header.rain_max_milli), (75_000, 50_000, 100_000));
        assert_eq!(a.header.issued_at, issued_at(day));
        assert_eq!(a.header.window_end - a.header.issued_at, 5 * DAY);
        assert!(!a.content.contains("icon_global"));
        assert_eq!(summarize(&r, day, &t.clone()).unwrap().content, a.content);

        t.remove("gfs_global");
        assert!(summarize(&r, day, &t).is_err(), "um modelo só não basta");
    }
}
