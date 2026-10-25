//! Auditoria independente: confronta o ledger local com as contas on-chain.
//!
//! Para cada lote: a raiz Merkle recalculada das leituras, o encadeamento e os assinantes.
//! Para cada previsão: quantis, `model_sha256`, `features_root` recalculada, o lote em que
//! se baseou e, pelo slot, que foi gravada antes do lote que contém a observação prevista.
//! Para cada alerta: P90 ≥ limiar e gravação também antes da observação.
//! Para cada aviso oficial: o SHA-256 do conteúdo guardado bate com a conta on-chain.
//! Para cada perspectiva de chuva: o conteúdo guardado reproduz o cabeçalho pela regra
//! publicada, o hash e o resultado da regra batem com a conta, e ela foi gravada antes de o
//! primeiro dia da janela ser ancorado.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anchor_lang::prelude::Pubkey;
use anyhow::{Context, Result};
use clap::Parser;
use cota_core::{merkle, Reading};
use cota_oracle::{chain, outlook, rpc::Rpc};
use serde::Serialize;
use serde_json::Value;

#[derive(Parser)]
#[command(about = "Confere o ledger do Cota contra a cadeia")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8899")]
    rpc: String,
    #[arg(long, default_value = "data/ledger")]
    ledger: PathBuf,
    #[arg(long, default_value = "ml/models/outlook-rule.json")]
    outlook_rule: PathBuf,
}

#[derive(Serialize)]
struct BatchAudit {
    batch_id: u64,
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
    account: String,
    slot: u64,
    p10: i64,
    p50: i64,
    p90: i64,
    /// Slot do lote que contém a leitura prevista (ausente se ainda não foi ancorado).
    observation_slot: Option<u64>,
    observed_milli: Option<i64>,
    recorded_before_observation: Option<bool>,
    inside_p10_p90: Option<bool>,
    alert: Option<AlertAudit>,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct AlertAudit {
    account: String,
    slot: u64,
    threshold_milli: i64,
    recorded_before_observation: Option<bool>,
}

#[derive(Serialize)]
struct PublicAlertAudit {
    record: String,
    alert_id: String,
    slot: u64,
    committed_at: i64,
    signers: u8,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct OutlookAudit {
    day: String,
    record: String,
    rain_mean_milli: i64,
    triggered: bool,
    slot: u64,
    /// Slot do lote que ancorou o primeiro dia da janela, se já existe.
    next_batch_slot: Option<u64>,
    problems: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    program: String,
    batches: Vec<BatchAudit>,
    forecasts: Vec<ForecastAudit>,
    public_alerts: Vec<PublicAlertAudit>,
    outlooks: Vec<OutlookAudit>,
    summary: BTreeMap<&'static str, usize>,
}

fn read_dir_sorted(dir: &std::path::Path) -> Result<Vec<Value>> {
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
    let committee = chain::decode_committee(&rpc.account_data(&chain::committee_pda())?.context("comitê não encontrado")?)?;

    // Lotes.
    let mut batches = Vec::new();
    let mut by_id: BTreeMap<u64, (BatchAudit, Vec<Reading>, [u8; 32])> = BTreeMap::new();
    let mut prev = [0u8; 32];
    for e in read_dir_sorted(&args.ledger.join("batches"))? {
        let id = e["batch_id"].as_u64().context("batch_id")?;
        let readings: Vec<Reading> = serde_json::from_value(e["readings"].clone())?;
        let root = merkle::root_from_leaves(&readings.iter().map(|r| merkle::hash_leaf(&r.canonical_bytes())).collect::<Vec<_>>());
        let acc = chain::decode_batch(&rpc.account_data(&chain::batch_pda(id))?.with_context(|| format!("lote {id} não está on-chain"))?)?;

        let mut problems = Vec::new();
        check(&mut problems, root == acc.merkle_root, "raiz recalculada difere da on-chain");
        check(&mut problems, hex32(&e["header"]["merkle_root"])? == acc.merkle_root, "raiz do ledger difere da on-chain");
        check(&mut problems, acc.prev_batch_root == prev, "encadeamento quebrado");
        check(&mut problems, acc.leaf_count as usize == readings.len(), "leaf_count difere");
        let mut bitmap = 0u8;
        for s in e["signatures"].as_array().context("signatures")? {
            let pk: Pubkey = s["member"].as_str().context("member")?.parse().map_err(|_| anyhow::anyhow!("chave inválida"))?;
            if let Some(i) = committee.members.iter().position(|m| *m == pk) {
                bitmap |= 1 << i;
            }
        }
        check(&mut problems, bitmap == acc.signers, "assinantes do ledger diferem do bitmap on-chain");
        prev = acc.merkle_root;

        let audit = BatchAudit {
            batch_id: id,
            day: e["day"].as_str().unwrap_or_default().to_string(),
            t_start: acc.t_start,
            t_end: acc.t_end,
            slot: acc.slot,
            committed_at: acc.committed_at,
            signers: acc.signers,
            problems,
        };
        by_id.insert(id, (audit, readings, acc.merkle_root));
    }

    // Previsões e alertas.
    let mut forecasts = Vec::new();
    let forecast_dir = args.ledger.join("forecasts");
    let files = if forecast_dir.exists() { read_dir_sorted(&forecast_dir)? } else { vec![] };
    for f in files {
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

        for rec in f["onchain"].as_array().context("onchain")? {
            let pda: Pubkey = rec["account"].as_str().context("account")?.parse().map_err(|_| anyhow::anyhow!("conta inválida"))?;
            let acc = chain::decode_forecast(&rpc.account_data(&pda)?.with_context(|| format!("previsão {pda} não está on-chain"))?)?;
            let mut problems = Vec::new();
            check(&mut problems, acc.station_id == station, "estação difere");
            check(&mut problems, acc.based_on_batch_id == based_on, "lote-base difere");
            check(&mut problems, (acc.p10, acc.p50, acc.p90) == (rec["p10"].as_i64().unwrap_or(-1), rec["p50"].as_i64().unwrap_or(-1), rec["p90"].as_i64().unwrap_or(-1)), "quantis diferem");
            check(&mut problems, acc.model_sha256 == hex32(&f["model_sha256"])?, "model_sha256 difere");
            check(&mut problems, acc.features_root == features_root, "features_root recalculada difere");
            if let Some((base, _, root)) = by_id.get(&based_on) {
                check(&mut problems, acc.based_on_root == *root, "based_on_root não é a raiz do lote-base");
                check(&mut problems, acc.issued_at == base.t_end, "issued_at não é o fim da janela do lote-base");
            } else {
                problems.push("lote-base ausente do ledger".into());
            }

            // Lote que contém a leitura prevista.
            let target = by_id.values().find(|(b, _, _)| b.t_start <= acc.target_ts && acc.target_ts < b.t_end);
            let observation_slot = target.map(|(b, _, _)| b.slot);
            let observed = target.and_then(|(_, rs, _)| rs.iter().find(|r| r.station_id == station && r.ts_utc == acc.target_ts)).map(|r| r.value_milli);

            let alert = match rec["alert"]["account"].as_str() {
                Some(a) => {
                    let apk: Pubkey = a.parse().map_err(|_| anyhow::anyhow!("conta inválida"))?;
                    let al = chain::decode_alert(&rpc.account_data(&apk)?.with_context(|| format!("alerta {apk} não está on-chain"))?)?;
                    check(&mut problems, al.forecast == pda && al.p90 >= al.threshold_milli, "alerta inconsistente com a previsão");
                    Some(AlertAudit {
                        account: a.to_string(),
                        slot: al.slot,
                        threshold_milli: al.threshold_milli,
                        recorded_before_observation: observation_slot.map(|s| al.slot < s),
                    })
                }
                None => None,
            };

            forecasts.push(ForecastAudit {
                issue_day: f["issue_day"].as_str().unwrap_or_default().to_string(),
                horizon_hours: acc.horizon_hours,
                target_ts: acc.target_ts,
                account: pda.to_string(),
                slot: acc.slot,
                p10: acc.p10,
                p50: acc.p50,
                p90: acc.p90,
                observation_slot,
                observed_milli: observed,
                recorded_before_observation: observation_slot.map(|s| acc.slot < s),
                inside_p10_p90: observed.map(|o| acc.p10 <= o && o <= acc.p90),
                alert,
                problems,
            });
        }
    }
    batches.extend(by_id.into_values().map(|(b, _, _)| b));

    // Avisos oficiais.
    let mut public_alerts = Vec::new();
    let pa_dir = args.ledger.join("public_alerts");
    for e in if pa_dir.exists() { read_dir_sorted(&pa_dir)? } else { vec![] } {
        let header: cota_core::PublicAlertHeader = serde_json::from_value(e["header"].clone())?;
        let pda = chain::public_alert_pda(&header.content_sha256);
        let rec = chain::decode_public_alert(&rpc.account_data(&pda)?.with_context(|| format!("aviso {pda} não está on-chain"))?)?;
        let mut problems = Vec::new();
        let content = e["content"].as_str().context("content")?;
        check(&mut problems, cota_core::public_alert::content_hash(content) == rec.content_sha256, "conteúdo guardado não bate com o hash on-chain");
        check(&mut problems, rec.alert_id == header.alert_id && rec.issuer == header.issuer, "identificação difere");
        check(&mut problems, (rec.severity, rec.onset, rec.expires) == (header.severity, header.onset, header.expires), "severidade ou vigência diferem");
        check(&mut problems, rec.signer_count >= committee.threshold, "assinaturas abaixo do limiar");
        public_alerts.push(PublicAlertAudit {
            record: pda.to_string(),
            alert_id: rec.alert_id,
            slot: rec.slot,
            committed_at: rec.committed_at,
            signers: rec.signers,
            problems,
        });
    }

    // Perspectivas de chuva (alerta antecipado).
    let mut outlooks = Vec::new();
    let ol_dir = args.ledger.join("outlooks");
    let entries = if ol_dir.exists() { read_dir_sorted(&ol_dir)? } else { vec![] };
    let rule = if entries.is_empty() { None } else { Some(outlook::load_rule(&args.outlook_rule)?) };
    for e in entries {
        let rule = rule.as_ref().expect("regra carregada quando há perspectivas");
        let header: cota_core::OutlookHeader = serde_json::from_value(e["header"].clone())?;
        let pda = chain::outlook_pda(&header.region, header.issued_at);
        let rec = chain::decode_outlook(&rpc.account_data(&pda)?.with_context(|| format!("perspectiva {pda} não está on-chain"))?)?;
        let on_rule = chain::decode_outlook_rule(&rpc.account_data(&chain::outlook_rule_pda(&header.region))?.context("regra de perspectiva ausente")?)?;
        let day: chrono::NaiveDate = e["day"].as_str().context("day")?.parse()?;
        let content = e["content"].as_str().context("content")?;
        let mut problems = Vec::new();
        check(&mut problems, cota_core::public_alert::content_hash(content) == rec.content_sha256, "conteúdo guardado não bate com o hash on-chain");
        check(&mut problems, on_rule.rule_sha256 == rule.sha256, "regra local difere da fixada on-chain");
        let totals: outlook::Totals = serde_json::from_value(serde_json::from_str::<Value>(content)?["models"].clone())?;
        match outlook::summarize(rule, day, &totals) {
            Ok(re) => {
                check(&mut problems, re.content == content, "conteúdo não é reproduzível pela regra");
                check(&mut problems, re.header == header, "cabeçalho não é reproduzível a partir do conteúdo");
            }
            Err(err) => problems.push(format!("conteúdo não reproduz: {err:#}")),
        }
        check(
            &mut problems,
            (rec.rain_mean_milli, rec.rain_min_milli, rec.rain_max_milli, rec.issued_at, rec.window_end)
                == (header.rain_mean_milli, header.rain_min_milli, header.rain_max_milli, header.issued_at, header.window_end),
            "valores on-chain diferem do cabeçalho",
        );
        check(&mut problems, rec.triggered == (rec.rain_mean_milli >= rec.threshold_milli), "resultado da regra incoerente");
        check(&mut problems, rec.signer_count >= committee.threshold, "assinaturas abaixo do limiar");
        let based = chain::decode_batch(&rpc.account_data(&chain::batch_pda(rec.based_on_batch_id))?.context("lote de base ausente")?)?;
        check(&mut problems, based.t_end == rec.issued_at, "emissão não é o fim do lote de base");
        let next_batch_slot = match rpc.account_data(&chain::batch_pda(rec.based_on_batch_id + 1))? {
            Some(d) => Some(chain::decode_batch(&d)?.slot),
            None => None,
        };
        check(&mut problems, next_batch_slot.is_none_or(|s| rec.slot < s), "gravada depois do lote seguinte");
        outlooks.push(OutlookAudit {
            day: day.to_string(),
            record: pda.to_string(),
            rain_mean_milli: rec.rain_mean_milli,
            triggered: rec.triggered,
            slot: rec.slot,
            next_batch_slot,
            problems,
        });
    }

    let count = |p: &dyn Fn(&ForecastAudit) -> bool| forecasts.iter().filter(|f| p(f)).count();
    let summary = BTreeMap::from([
        ("batches", batches.len()),
        ("batches_with_problems", batches.iter().filter(|b| !b.problems.is_empty()).count()),
        ("forecasts", forecasts.len()),
        ("forecasts_with_problems", count(&|f| !f.problems.is_empty())),
        ("forecasts_observed", count(&|f| f.observation_slot.is_some())),
        ("forecasts_recorded_before_observation", count(&|f| f.recorded_before_observation == Some(true))),
        ("forecasts_inside_p10_p90", count(&|f| f.inside_p10_p90 == Some(true))),
        ("alerts", count(&|f| f.alert.is_some())),
        ("public_alerts", public_alerts.len()),
        ("public_alerts_with_problems", public_alerts.iter().filter(|a| !a.problems.is_empty()).count()),
        ("outlooks", outlooks.len()),
        ("outlooks_with_problems", outlooks.iter().filter(|o| !o.problems.is_empty()).count()),
        ("early_warnings", outlooks.iter().filter(|o| o.triggered).count()),
        ("alerts_recorded_before_observation", count(&|f| f.alert.as_ref().is_some_and(|a| a.recorded_before_observation == Some(true)))),
    ]);
    let ok = summary["batches_with_problems"] == 0 && summary["forecasts_with_problems"] == 0 && summary["public_alerts_with_problems"] == 0
        && summary["outlooks_with_problems"] == 0;
    for b in batches.iter().filter(|b| !b.problems.is_empty()) {
        eprintln!("lote {}: {:?}", b.batch_id, b.problems);
    }
    for f in forecasts.iter().filter(|f| !f.problems.is_empty()) {
        eprintln!("previsão {} +{}h: {:?}", f.issue_day, f.horizon_hours, f.problems);
    }
    for (k, v) in &summary {
        eprintln!("{k:<40} {v}");
    }

    for a in public_alerts.iter().filter(|a| !a.problems.is_empty()) {
        eprintln!("aviso {}: {:?}", a.alert_id, a.problems);
    }
    for o in outlooks.iter().filter(|o| !o.problems.is_empty()) {
        eprintln!("perspectiva {}: {:?}", o.day, o.problems);
    }
    let report = Report { program: chain::PROGRAM_ID.to_string(), batches, forecasts, public_alerts, outlooks, summary };
    std::fs::write(args.ledger.join("audit.json"), serde_json::to_string_pretty(&report)?)?;
    anyhow::ensure!(ok, "a auditoria encontrou divergências");
    eprintln!("auditoria OK: {}", args.ledger.join("audit.json").display());
    Ok(())
}
