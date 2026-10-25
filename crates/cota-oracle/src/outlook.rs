//! Chuva prevista na bacia (alerta antecipado): cada nó baixa a mesma rodada de cada
//! modelo (Open-Meteo Single Runs, rodada 00Z do dia D, imutável depois de publicada),
//! resume e assina. Nós honestos chegam aos mesmos bytes; o agregador exige k de n.
//!
//! A regra (pontos, modelos, janela, limiar) vem de `ml/models/outlook-rule.json`, cujo
//! SHA-256 está fixado on-chain na `OutlookRule`.

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

    /// Rodada usada para a emissão no fim do dia `day`.
    pub fn run(&self, day: NaiveDate) -> String {
        format!("{day}T{:02}:00", self.spec.run_hour_utc)
    }

    /// Dias da janela: D+1..=D+window_days.
    pub fn window(&self, day: NaiveDate) -> Vec<NaiveDate> {
        (1..=self.spec.window_days).map(|k| day + Days::new(k)).collect()
    }
}

/// Lê a regra e confere que o hash declarado é o do JSON canônico da especificação
/// (o mesmo que o Python grava e que vai on-chain).
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

/// Chuva diária em décimos de mm: {modelo: {ponto: [D+1, …]}}.
pub type Totals = BTreeMap<String, BTreeMap<String, Vec<i64>>>;

/// Soma as 24 horas de cada dia pedido; `None` se faltar alguma hora.
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

/// Baixa todos os modelos; um modelo com qualquer ponto faltando fica de fora.
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
    /// JSON canônico: os bytes cujo SHA-256 é `header.content_sha256`.
    pub content: String,
}

pub fn issued_at(day: NaiveDate) -> i64 {
    (day + Days::new(1)).and_time(NaiveTime::MIN).and_utc().timestamp()
}

/// Resume a chuva de cada modelo (média dos pontos, acumulada na janela) e monta o conteúdo.
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

/// O que um nó publica sobre a chuva prevista de um dia.
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
        signature: key.sign(&o.header.signing_message()).to_bytes(),
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
        self.header.signing_message()
    }
    fn content_matches(&self) -> bool {
        content_hash(&self.content) == self.header.content_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        t.insert("ecmwf_ifs025".into(), full(200)); // 20 mm/dia → 100 mm
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
