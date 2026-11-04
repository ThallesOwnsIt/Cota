//! `cota-ingest`: downloads the sources, normalizes them into `Reading`s and writes the dataset.
//!
//! Output in `--out`:
//! - `readings.jsonl`: one `Reading` per line (what the nodes sign)
//! - `readings.csv`:   flat version for analysis/ML
//! - `manifest.json`:  window, per-station counts and SHA-256 of the files

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{DateTime, NaiveDate};
use clap::Parser;
use cota_ingest::{config, end_of_day, http_client, ingest, start_of_day, StationReport};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(about = "Ingestion of Cota's hydrometeorological sources")]
struct Args {
    /// Station catalog.
    #[arg(long, default_value = "config/stations.toml")]
    config: PathBuf,
    /// First day (UTC), inclusive.
    #[arg(long)]
    from: NaiveDate,
    /// Last day (UTC), inclusive.
    #[arg(long)]
    to: NaiveDate,
    /// Cache of the downloaded raw files.
    #[arg(long, default_value = "data/raw")]
    cache: PathBuf,
    /// Output directory.
    #[arg(long)]
    out: PathBuf,
    /// Accepts stations missing in some year (training history).
    #[arg(long)]
    lenient: bool,
}

#[derive(Serialize)]
struct Manifest {
    from_utc: i64,
    to_utc: i64,
    reading_count: usize,
    stations: Vec<StationReport>,
    files: Vec<(String, String)>,
    attribution: &'static [&'static str],
}

fn main() -> Result<()> {
    let args = Args::parse();
    let stations = config::load(&args.config)?;
    let (from, to) = (start_of_day(args.from), end_of_day(args.to));

    let (readings, reports) = ingest(&http_client()?, &stations, &args.cache, from, to, args.lenient)?;
    std::fs::create_dir_all(&args.out)?;

    let mut jsonl = String::new();
    let mut csv = String::from("station_id,variable,source,ts_utc,iso_utc,value,value_milli,datum_id,raw_payload_sha256\n");
    for r in &readings {
        jsonl.push_str(&serde_json::to_string(r)?);
        jsonl.push('\n');
        let iso = DateTime::from_timestamp(r.ts_utc, 0).expect("ts válido").format("%Y-%m-%dT%H:%M:%SZ");
        writeln!(
            csv,
            "{},{},{},{},{},{},{},{},{}",
            r.station_id,
            serde_json::to_value(r.variable)?.as_str().unwrap_or_default(),
            serde_json::to_value(r.source)?.as_str().unwrap_or_default(),
            r.ts_utc,
            iso,
            r.value(),
            r.value_milli,
            r.datum_id.as_deref().unwrap_or(""),
            hex::encode(r.raw_payload_sha256),
        )?;
    }

    let mut files = Vec::new();
    for (name, content) in [("readings.jsonl", jsonl), ("readings.csv", csv)] {
        std::fs::File::create(args.out.join(name))?.write_all(content.as_bytes())?;
        files.push((name.to_string(), hex::encode(Sha256::digest(content.as_bytes()))));
    }

    for r in &reports {
        eprintln!(
            "{:<16} {:<18} {:>6} leituras  {:>4} faltantes de {:>5}",
            r.station_id,
            serde_json::to_value(r.source)?.as_str().unwrap_or_default(),
            r.readings,
            r.missing,
            r.expected
        );
    }
    let manifest = Manifest {
        from_utc: from,
        to_utc: to,
        reading_count: readings.len(),
        stations: reports,
        files,
        attribution: &["INMET - Instituto Nacional de Meteorologia", "Weather data by Open-Meteo.com (CC BY 4.0), GloFAS/Copernicus"],
    };
    std::fs::write(args.out.join("manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
    eprintln!("{} leituras gravadas em {}", readings.len(), args.out.display());
    Ok(())
}
