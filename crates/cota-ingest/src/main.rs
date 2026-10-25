//! `cota-ingest`: baixa as fontes, normaliza em `Reading`s e grava o conjunto de dados.
//!
//! Saída em `--out`:
//! - `readings.jsonl`: um `Reading` por linha (o que os nós assinam)
//! - `readings.csv`:   versão plana para análise/ML
//! - `manifest.json`:  janela, contagens por estação e SHA-256 dos arquivos

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
#[command(about = "Ingestão das fontes hidrometeorológicas do Cota")]
struct Args {
    /// Catálogo de estações.
    #[arg(long, default_value = "config/stations.toml")]
    config: PathBuf,
    /// Primeiro dia (UTC), inclusivo.
    #[arg(long)]
    from: NaiveDate,
    /// Último dia (UTC), inclusivo.
    #[arg(long)]
    to: NaiveDate,
    /// Cache dos arquivos brutos baixados.
    #[arg(long, default_value = "data/raw")]
    cache: PathBuf,
    /// Diretório de saída.
    #[arg(long)]
    out: PathBuf,
    /// Aceita estações ausentes em algum ano (histórico de treino).
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
