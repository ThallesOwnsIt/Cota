//! Independent audit: checks the local ledger against the chain.
//!
//! First the accumulator: `records.jsonl` is rebuilt into an MMR and its peaks must equal the ones in the
//! on-chain `Registry`. One account read proves that every record in the file is exactly what the program
//! accepted, in that order. Every other check runs on those proven records:
//!
//! For each batch: the Merkle root recomputed from the readings, the chaining and the signers.
//! For each forecast: quantiles, `model_sha256`, recomputed `features_root`, the batch it was based on
//! and, by slot, that it was recorded before the batch containing the forecast observation. If it carries
//! an alert: P90 ≥ the station's rule.
//! For each official warning: the SHA-256 of the stored content matches the record.
//! For each rain outlook: the stored content reproduces the header under the published rule, the hash and
//! the rule's result match the record, and it was recorded before the next batch.
//! For each vault: every payout exists on-chain, points to a valid trigger record of the vault's type and
//! subject, and the vault's totals match the sum of the payouts.
//! Completeness: every batch, forecast, warning and outlook record has its ledger file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anyhow::{Context, Result};
use clap::Parser;
use cota_core::{merkle, mmr, Reading};
use cota_oracle::{
    chain::{self, Record},
    outlook, records,
    rpc::Rpc,
};
use cota_v2::{BatchRecord, ForecastRecord, OutlookRecord, PublicAlertRecord};
use serde::Serialize;
use serde_json::Value;

#[derive(Parser)]
#[command(about = "Checks Cota's ledger against the chain")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    #[arg(long, default_value = "data/ledger")]
    ledger: PathBuf,
    #[arg(long, default_value = "ml/models/outlook-rule.json")]
    outlook_rule: PathBuf,
}

#[derive(Serialize)]
struct AccumulatorAudit {
    records: u64,
    on_chain: u64,
    peaks_match: bool,
    /// The peaks folded into one hash (`cota_core::mmr::bag`): what gets timestamped off-chain.
    root: Option<String>,
    kinds: BTreeMap<&'static str, usize>,
}

#[derive(Serialize)]
struct BatchAudit {
    batch_id: u64,
    record_index: u64,
    day: String,
    t_start: i64,
    t_end: i64,
    slot: u64,
    committed_at: i64,
    signers: u8,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct ForecastAudit {
    issue_day: String,
    horizon_hours: u16,
    target_ts: i64,
    record_index: u64,
    slot: u64,
    p10: i64,
    p50: i64,
    p90: i64,
    /// Slot of the batch containing the forecast reading (absent if not anchored yet).
    observation_slot: Option<u64>,
    observed_milli: Option<i64>,
    recorded_before_observation: Option<bool>,
    inside_p10_p90: Option<bool>,
    alert: Option<AlertAudit>,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct AlertAudit {
    threshold_milli: i64,
    level: u8,
    /// The alert is decided in the forecast's own instruction, so it has the forecast's slot.
    slot: u64,
    recorded_before_observation: Option<bool>,
}

#[derive(Serialize)]
struct PublicAlertAudit {
    record_index: u64,
    alert_id: String,
    slot: u64,
    committed_at: i64,
    signers: u8,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct OutlookAudit {
    day: String,
    record_index: u64,
    rain_mean_milli: i64,
    triggered: bool,
    slot: u64,
    /// Slot of the batch that anchored the first day of the window, if it exists already.
    next_batch_slot: Option<u64>,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct VaultAudit {
    name: String,
    vault: String,
    payouts: usize,
    total_paid: u64,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    program: String,
    /// The `Registry` account (committee and peaks) the dashboard reads to verify records.
    registry: String,
    accumulator: AccumulatorAudit,
    batches: Vec<BatchAudit>,
    forecasts: Vec<ForecastAudit>,
    public_alerts: Vec<PublicAlertAudit>,
    outlooks: Vec<OutlookAudit>,
    vaults: Vec<VaultAudit>,
    /// Records with no ledger file (or ledger files pointing at the wrong record).
    unmatched: Vec<String>,
    summary: BTreeMap<&'static str, usize>,
}

fn read_dir_sorted(dir: &std::path::Path) -> Result<Vec<Value>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("lendo {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    files.sort();
    files
        .iter()
        .map(|f| serde_json::from_str(&std::fs::read_to_string(f)?).with_context(|| format!("interpretando {}", f.display())))
        .collect()
}

fn hex32(v: &Value) -> Result<[u8; 32]> {
    hex::decode(v.as_str().context("hash ausente")?)?.try_into().map_err(|_| anyhow::anyhow!("hash de 32 bytes esperado"))
}

fn check(problems: &mut Vec<String>, ok: bool, what: &str) {
    if !ok {
        problems.push(what.to_string());
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let rpc = Rpc::new(&args.rpc)?;
    let registry = chain::decode_registry(&rpc.account_data(&chain::registry_pda())?.context("registry não encontrado")?)?;

    // 1. The accumulator: the file must rebuild exactly the on-chain peaks.
    let stored = records::load(&args.ledger)?;
    let tree = records::mmr(&stored)?;
    let peaks_match = tree.len() == registry.leaf_count && tree.peaks() == registry.peaks;
    let decoded: Vec<Record> = stored.iter().map(|s| s.decode()).collect::<Result<_>>()?;
    let mut kinds = BTreeMap::new();
    for r in &decoded {
        *kinds.entry(records::kind(r)).or_insert(0) += 1;
    }
    let accumulator = AccumulatorAudit {
        records: tree.len(),
        on_chain: registry.leaf_count,
        peaks_match,
        root: mmr::bag(&registry.peaks, registry.leaf_count).map(hex::encode),
        kinds,
    };
    anyhow::ensure!(
        peaks_match,
        "records.jsonl ({} registros) não reconstrói os picos on-chain ({} registros): rode `records sync` ou investigue",
        tree.len(),
        registry.leaf_count
    );

    let batch_records: BTreeMap<u64, (u64, &BatchRecord)> = decoded
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Record::Batch(b) => Some((b.header.batch_id, (i as u64, b))),
            _ => None,
        })
        .collect();
    let mut unmatched = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let get = |index: u64| -> Option<&Record> { decoded.get(index as usize) };

    // 2. Batches.
    let mut batches = Vec::new();
    let mut by_id: BTreeMap<u64, (u64, Vec<Reading>)> = BTreeMap::new();
    let mut prev = [0u8; 32];
    for e in read_dir_sorted(&args.ledger.join("batches"))? {
        let id = e["batch_id"].as_u64().context("batch_id")?;
        let readings: Vec<Reading> = serde_json::from_value(e["readings"].clone())?;
        let root = merkle::root_from_leaves(&readings.iter().map(|r| merkle::hash_leaf(&r.canonical_bytes())).collect::<Vec<_>>());
        let Some(&(index, rec)) = batch_records.get(&id) else {
            unmatched.push(format!("lote {id} do ledger não está no acumulador"));
            continue;
        };
        seen.insert(index);
        let h = &rec.header;
        let mut problems = Vec::new();
        check(&mut problems, e["record_index"].as_u64() == Some(index), "record_index do ledger difere");
        check(&mut problems, root == h.merkle_root, "raiz recalculada difere da registrada");
        check(&mut problems, hex32(&e["header"]["merkle_root"])? == h.merkle_root, "raiz do ledger difere da registrada");
        check(&mut problems, h.prev_batch_root == prev, "encadeamento quebrado");
        check(&mut problems, h.leaf_count as usize == readings.len(), "leaf_count difere");
        let mut bitmap = 0u8;
        for s in e["signatures"].as_array().context("signatures")? {
            let pk: Pubkey = s["member"].as_str().context("member")?.parse().map_err(|_| anyhow::anyhow!("chave inválida"))?;
            if let Some(i) = registry.members.iter().position(|m| *m == pk) {
                bitmap |= 1 << i;
            }
        }
        check(&mut problems, bitmap == rec.signers, "assinantes do ledger diferem do bitmap registrado");
        check(&mut problems, rec.signer_count >= registry.threshold, "assinaturas abaixo do limiar");
        prev = h.merkle_root;
        batches.push(BatchAudit {
            batch_id: id,
            record_index: index,
            day: e["day"].as_str().unwrap_or_default().to_string(),
            t_start: h.t_start,
            t_end: h.t_end,
            slot: rec.slot,
            committed_at: rec.committed_at,
            signers: rec.signers,
            problems,
        });
        by_id.insert(id, (index, readings));
    }
    let batch_of = |ts: i64| batches.iter().find(|b| b.t_start <= ts && ts < b.t_end);

    // 3. Forecasts and their alerts.
    let rule_accounts: BTreeMap<String, Option<cota_v2::AlertRule>> = BTreeMap::new();
    let mut rule_accounts = rule_accounts;
    let mut forecasts = Vec::new();
    for f in read_dir_sorted(&args.ledger.join("forecasts"))? {
        let station = f["station_id"].as_str().context("station_id")?;
        let based_on = f["based_on_batch_id"].as_u64().context("based_on_batch_id")?;
        let leaves: Vec<_> = f["features"]
            .as_array()
            .context("features")?
            .iter()
            .map(|x| {
                let v = x["value_milli"].as_i64().map_or("null".to_string(), |v| v.to_string());
                merkle::hash_leaf(format!("{}={v}", x["name"].as_str().unwrap_or_default()).as_bytes())
            })
            .collect();
        let features_root = merkle::root_from_leaves(&leaves);
        if !rule_accounts.contains_key(station) {
            let rule = rpc.account_data(&chain::alert_rule_pda(station))?.map(|d| chain::decode_alert_rule(&d)).transpose()?;
            rule_accounts.insert(station.to_string(), rule);
        }
        let rule = rule_accounts[station].as_ref();

        for rec in f["onchain"].as_array().context("onchain")? {
            let index = rec["record_index"].as_u64().context("record_index")?;
            let Some(Record::Forecast(fr)) = get(index) else {
                unmatched.push(format!("previsão {} aponta para o registro {index}, que não é uma previsão", f["issue_day"]));
                continue;
            };
            let fr: &ForecastRecord = fr;
            seen.insert(index);
            let mut problems = Vec::new();
            check(&mut problems, fr.station_id == station, "estação difere");
            check(&mut problems, fr.based_on_batch_id == based_on, "lote-base difere");
            check(
                &mut problems,
                (fr.p10, fr.p50, fr.p90) == (rec["p10"].as_i64().unwrap_or(-1), rec["p50"].as_i64().unwrap_or(-1), rec["p90"].as_i64().unwrap_or(-1)),
                "quantis diferem",
            );
            check(&mut problems, fr.model_sha256 == hex32(&f["model_sha256"])?, "model_sha256 difere");
            check(&mut problems, fr.features_root == features_root, "features_root recalculada difere");
            match batch_records.get(&based_on) {
                Some((_, b)) => {
                    check(&mut problems, fr.based_on_root == b.header.merkle_root, "based_on_root não é a raiz do lote-base");
                    check(&mut problems, fr.issued_at == b.header.t_end, "issued_at não é o fim da janela do lote-base");
                }
                None => problems.push("lote-base ausente do acumulador".into()),
            }
            let observation = batch_of(fr.target_ts);
            let observation_slot = observation.map(|b| b.slot);
            let observed = observation
                .and_then(|b| by_id.get(&b.batch_id))
                .and_then(|(_, rs)| rs.iter().find(|r| r.station_id == station && r.ts_utc == fr.target_ts))
                .map(|r| r.value_milli);
            let alert = fr.alert.as_ref().map(|a| {
                check(&mut problems, fr.p90 >= a.threshold_milli, "alerta com P90 abaixo do limiar");
                check(&mut problems, rule.is_some_and(|r| r.threshold_milli == a.threshold_milli), "alerta com limiar diferente da regra on-chain");
                AlertAudit {
                    threshold_milli: a.threshold_milli,
                    level: a.level,
                    slot: fr.slot,
                    recorded_before_observation: observation_slot.map(|s| fr.slot < s),
                }
            });
            if fr.alert.is_none() {
                check(&mut problems, rule.is_none_or(|r| fr.p90 < r.threshold_milli), "P90 cruzou a regra e o alerta não saiu");
            }
            forecasts.push(ForecastAudit {
                issue_day: f["issue_day"].as_str().unwrap_or_default().to_string(),
                horizon_hours: fr.horizon_hours,
                target_ts: fr.target_ts,
                record_index: index,
                slot: fr.slot,
                p10: fr.p10,
                p50: fr.p50,
                p90: fr.p90,
                observation_slot,
                observed_milli: observed,
                recorded_before_observation: observation_slot.map(|s| fr.slot < s),
                inside_p10_p90: observed.map(|o| fr.p10 <= o && o <= fr.p90),
                alert,
                problems,
            });
        }
    }

    // 4. Official warnings.
    let mut public_alerts = Vec::new();
    for e in read_dir_sorted(&args.ledger.join("public_alerts"))? {
        let header: cota_core::PublicAlertHeader = serde_json::from_value(e["header"].clone())?;
        let index = e["record_index"].as_u64().context("record_index")?;
        let Some(Record::PublicAlert(pr)) = get(index) else {
            unmatched.push(format!("aviso {} aponta para o registro {index}, que não é um aviso", header.alert_id));
            continue;
        };
        let pr: &PublicAlertRecord = pr;
        seen.insert(index);
        let mut problems = Vec::new();
        let content = e["content"].as_str().context("content")?;
        check(&mut problems, cota_core::public_alert::content_hash(content) == pr.header.content_sha256, "conteúdo guardado não bate com o hash registrado");
        check(&mut problems, chain::to_program_alert_header(&header) == pr.header, "cabeçalho do ledger difere do registrado");
        check(&mut problems, pr.signer_count >= registry.threshold, "assinaturas abaixo do limiar");
        public_alerts.push(PublicAlertAudit {
            record_index: index,
            alert_id: header.alert_id,
            slot: pr.slot,
            committed_at: pr.committed_at,
            signers: pr.signers,
            problems,
        });
    }

    // 5. Rain outlooks (early warning).
    let mut outlooks = Vec::new();
    let entries = read_dir_sorted(&args.ledger.join("outlooks"))?;
    let rule = if entries.is_empty() { None } else { Some(outlook::load_rule(&args.outlook_rule)?) };
    for e in entries {
        let rule = rule.as_ref().expect("regra carregada quando há perspectivas");
        let header: cota_core::OutlookHeader = serde_json::from_value(e["header"].clone())?;
        let index = e["record_index"].as_u64().context("record_index")?;
        let Some(Record::Outlook(or)) = get(index) else {
            unmatched.push(format!("perspectiva {} aponta para o registro {index}, que não é uma perspectiva", e["day"]));
            continue;
        };
        let or: &OutlookRecord = or;
        seen.insert(index);
        let on_rule =
            chain::decode_outlook_rule(&rpc.account_data(&chain::outlook_rule_pda(&header.region))?.context("regra de perspectiva ausente")?)?;
        let day: chrono::NaiveDate = e["day"].as_str().context("day")?.parse()?;
        let content = e["content"].as_str().context("content")?;
        let mut problems = Vec::new();
        check(&mut problems, cota_core::public_alert::content_hash(content) == or.header.content_sha256, "conteúdo guardado não bate com o hash registrado");
        check(&mut problems, on_rule.rule_sha256 == rule.sha256, "regra local difere da fixada on-chain");
        let totals: outlook::Totals = serde_json::from_value(serde_json::from_str::<Value>(content)?["models"].clone())?;
        match outlook::summarize(rule, day, &totals) {
            Ok(re) => {
                check(&mut problems, re.content == content, "conteúdo não é reproduzível pela regra");
                check(&mut problems, re.header == header, "cabeçalho não é reproduzível a partir do conteúdo");
            }
            Err(err) => problems.push(format!("conteúdo não reproduz: {err:#}")),
        }
        check(&mut problems, chain::to_program_outlook_header(&header) == or.header, "valores registrados diferem do cabeçalho");
        check(&mut problems, or.threshold_milli == on_rule.threshold_milli, "limiar registrado difere da regra");
        check(&mut problems, or.triggered == (or.header.rain_mean_milli >= or.threshold_milli), "resultado da regra incoerente");
        check(&mut problems, or.signer_count >= registry.threshold, "assinaturas abaixo do limiar");
        match batch_records.get(&or.based_on_batch_id) {
            Some((_, b)) => check(&mut problems, b.header.t_end == or.header.issued_at, "emissão não é o fim do lote de base"),
            None => problems.push("lote de base ausente do acumulador".into()),
        }
        let next_batch_slot = batch_records.get(&(or.based_on_batch_id + 1)).map(|(_, b)| b.slot);
        check(&mut problems, next_batch_slot.is_none_or(|s| or.slot < s), "gravada depois do lote seguinte");
        outlooks.push(OutlookAudit {
            day: day.to_string(),
            record_index: index,
            rain_mean_milli: or.header.rain_mean_milli,
            triggered: or.triggered,
            slot: or.slot,
            next_batch_slot,
            problems,
        });
    }

    // 6. Parametric vaults.
    let mut vaults = Vec::new();
    for e in read_dir_sorted(&args.ledger.join("vaults"))? {
        let name = e["name"].as_str().unwrap_or_default().to_string();
        let addr: Pubkey = e["vault"].as_str().context("vault")?.parse()?;
        let mut problems = Vec::new();
        let Some(data) = rpc.account_data(&addr)? else {
            problems.push("cofre não está on-chain".into());
            vaults.push(VaultAudit { name, vault: addr.to_string(), payouts: 0, total_paid: 0, problems });
            continue;
        };
        let v = chain::decode_vault(&data)?;
        let mut sum = 0;
        let payouts = e["payouts"].as_array().cloned().unwrap_or_default();
        for p in &payouts {
            let index = p["trigger_index"].as_u64().context("trigger_index")?;
            let leaf = hex::decode(&stored.get(index as usize).context("gatilho fora do acumulador")?.leaf)?;
            let leaf: [u8; 32] = leaf.try_into().map_err(|_| anyhow::anyhow!("leaf"))?;
            let Some(pd) = rpc.account_data(&chain::payout_pda(&addr, &leaf))? else {
                problems.push(format!("pagamento pelo registro {index} não está on-chain"));
                continue;
            };
            let rec = chain::decode_payout(&pd)?;
            sum += rec.amount;
            check(
                &mut problems,
                rec.amount == p["amount"].as_u64().unwrap_or_default() && rec.beneficiary == v.beneficiary && rec.trigger_index == index,
                "valor, beneficiário ou gatilho diferem",
            );
            let valid = match (v.kind, get(index)) {
                (chain::TRIGGER_EARLY_WARNING, Some(Record::Outlook(o))) => {
                    o.triggered && o.header.region == v.subject && o.header.issued_at == rec.event_at
                }
                (chain::TRIGGER_FLOW_ALERT, Some(Record::Forecast(f))) => {
                    f.alert.is_some() && f.station_id == v.subject && f.issued_at == rec.event_at
                }
                _ => false,
            };
            check(&mut problems, valid, "gatilho inválido para o cofre");
        }
        check(&mut problems, v.payouts as usize == payouts.len() && v.total_paid == sum, "totais do cofre não batem com os pagamentos");
        vaults.push(VaultAudit { name, vault: addr.to_string(), payouts: payouts.len(), total_paid: sum, problems });
    }

    // 7. Completeness: records the ledger has no file for.
    for (i, r) in decoded.iter().enumerate() {
        if matches!(r, Record::Batch(_) | Record::Forecast(_) | Record::PublicAlert(_) | Record::Outlook(_)) && !seen.contains(&(i as u64)) {
            unmatched.push(format!("registro {i} ({}) sem arquivo no ledger", records::kind(r)));
        }
    }

    let count = |p: &dyn Fn(&ForecastAudit) -> bool| forecasts.iter().filter(|f| p(f)).count();
    let summary = BTreeMap::from([
        ("records", stored.len()),
        ("unmatched", unmatched.len()),
        ("batches", batches.len()),
        ("batches_with_problems", batches.iter().filter(|b| !b.problems.is_empty()).count()),
        ("forecasts", forecasts.len()),
        ("forecasts_with_problems", count(&|f| !f.problems.is_empty())),
        ("forecasts_observed", count(&|f| f.observation_slot.is_some())),
        ("forecasts_recorded_before_observation", count(&|f| f.recorded_before_observation == Some(true))),
        ("forecasts_inside_p10_p90", count(&|f| f.inside_p10_p90 == Some(true))),
        ("alerts", count(&|f| f.alert.is_some())),
        ("alerts_recorded_before_observation", count(&|f| f.alert.as_ref().is_some_and(|a| a.recorded_before_observation == Some(true)))),
        ("public_alerts", public_alerts.len()),
        ("public_alerts_with_problems", public_alerts.iter().filter(|a| !a.problems.is_empty()).count()),
        ("outlooks", outlooks.len()),
        ("outlooks_with_problems", outlooks.iter().filter(|o| !o.problems.is_empty()).count()),
        ("early_warnings", outlooks.iter().filter(|o| o.triggered).count()),
        ("vaults", vaults.len()),
        ("vault_payouts", vaults.iter().map(|v| v.payouts).sum()),
        ("vaults_with_problems", vaults.iter().filter(|v| !v.problems.is_empty()).count()),
    ]);
    let ok = ["unmatched", "batches_with_problems", "forecasts_with_problems", "public_alerts_with_problems", "outlooks_with_problems", "vaults_with_problems"]
        .iter()
        .all(|k| summary[k] == 0);
    for b in batches.iter().filter(|b| !b.problems.is_empty()) {
        eprintln!("lote {}: {:?}", b.batch_id, b.problems);
    }
    for f in forecasts.iter().filter(|f| !f.problems.is_empty()) {
        eprintln!("previsão {} +{}h: {:?}", f.issue_day, f.horizon_hours, f.problems);
    }
    for a in public_alerts.iter().filter(|a| !a.problems.is_empty()) {
        eprintln!("aviso {}: {:?}", a.alert_id, a.problems);
    }
    for o in outlooks.iter().filter(|o| !o.problems.is_empty()) {
        eprintln!("perspectiva {}: {:?}", o.day, o.problems);
    }
    for v in vaults.iter().filter(|v| !v.problems.is_empty()) {
        eprintln!("cofre {}: {:?}", v.name, v.problems);
    }
    for u in &unmatched {
        eprintln!("{u}");
    }
    for (k, v) in &summary {
        eprintln!("{k:<40} {v}");
    }
    let report = Report {
        program: chain::PROGRAM_ID.to_string(),
        registry: chain::registry_pda().to_string(),
        accumulator,
        batches,
        forecasts,
        public_alerts,
        outlooks,
        vaults,
        unmatched,
        summary,
    };
    std::fs::write(args.ledger.join("audit.json"), serde_json::to_string_pretty(&report)?)?;
    anyhow::ensure!(ok, "a auditoria encontrou divergências");
    eprintln!("auditoria OK: {}", args.ledger.join("audit.json").display());
    Ok(())
}
