//! Reavalia a qualidade de todos os lotes de um ledger: `cargo run --example ledger -- data/ledger`.

use cota_core::{Reading, Source};
use cota_quality::{assess, Agreement, Limits, State};

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "data/ledger".into()));
    let limits = Limits::load(std::path::Path::new("config/quality-limits.toml"))?;
    let mut files: Vec<_> = std::fs::read_dir(dir.join("batches"))?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    files.sort();
    let mut state = State::default();
    let mut catalog: Vec<(String, Source)> = Vec::new();
    for f in files {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f)?)?;
        let readings: Vec<Reading> = serde_json::from_value(v["readings"].clone())?;
        for r in &readings {
            if !catalog.iter().any(|(id, _)| id == &r.station_id) {
                catalog.push((r.station_id.clone(), r.source));
            }
        }
        let (t0, t1) = (v["header"]["t_start"].as_i64().unwrap(), v["header"]["t_end"].as_i64().unwrap());
        for q in assess(&mut state, &limits, &catalog, t0, t1, &readings, &Agreement::default()) {
            if q.degraded.is_some() || !q.flags.is_empty() || std::env::var("ALL").is_ok_and(|s| s == q.station_id) {
                println!(
                    "{} {:<16} {:>2}/{:<2} C={:.3} (bruto {:.3}) flags={:?} degradada={:?}",
                    v["day"].as_str().unwrap(),
                    q.station_id,
                    q.present,
                    q.expected,
                    q.score,
                    q.raw,
                    q.flags.iter().map(|f| f.reason).collect::<Vec<_>>(),
                    q.degraded
                );
            }
        }
    }
    Ok(())
}
