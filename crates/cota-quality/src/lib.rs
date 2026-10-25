//! Qualidade de dados de cada lote, na ordem do briefing:
//!
//! - 6.5: regras por leitura com estado mínimo por estação (travado, salto, sem dados recentes);
//! - 6.4: concordância entre os nós por MAD (z modificado de Iglewicz & Hoaglin);
//! - 6.11: índice de confiança C_s por estação, com média móvel exponencial entre lotes.
//!
//! Tudo é determinístico sobre leituras ancoradas: qualquer pessoa com o ledger refaz os escores.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use cota_core::{Reading, Source, Variable};
use serde::{Deserialize, Serialize};

/// Pesos de C_s: disponibilidade, ausência de flags, concordância entre nós, frescor.
///
/// A disponibilidade domina: com pesos parecidos, flags e concordância somariam 0,5
/// "de graça" e a A882 (6 a 9 de 24 h por três dias, maio/2024) nunca cairia abaixo de 0,6. Divergência
/// entre nós já dispara `SourceDegraded` por conta própria (6.4).
pub const WEIGHTS: [f64; 4] = [0.7, 0.1, 0.1, 0.1];
pub const EMA_ALPHA: f64 = 0.5;
/// Abaixo disto a estação é declarada degradada (`SourceDegraded`).
pub const MIN_SCORE: f64 = 0.6;
pub const MAD_Z: f64 = 3.5;

/// Mesmos valores de `reason` no evento on-chain `SourceDegraded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Reason {
    Unavailable = 0,
    Stuck = 1,
    Jump = 2,
    Disagreement = 3,
    Stale = 4,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Limits {
    /// Horas seguidas de chuva idêntica e não nula que caracterizam sensor travado.
    #[serde(default = "default_stuck_hours")]
    pub stuck_hours_precip: u32,
    /// Dias seguidos de vazão idêntica.
    #[serde(default = "default_stuck_days")]
    pub stuck_days_discharge: u32,
    /// Chuva acima disto em uma hora é suspeita no RS (100 mm/h).
    #[serde(default = "default_max_precip")]
    pub max_precip_milli: i64,
    /// r_max por estação de vazão: |Δ ln Q| máximo entre dias (percentil 99,9 histórico).
    #[serde(default)]
    pub jump_ln: BTreeMap<String, f64>,
}

fn default_stuck_hours() -> u32 {
    6
}
fn default_stuck_days() -> u32 {
    3
}
fn default_max_precip() -> i64 {
    100_000
}

impl Limits {
    /// Lê `config/quality-limits.toml` (gerado pelo treino a partir do histórico).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("lendo {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("interpretando {}", path.display()))
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            stuck_hours_precip: default_stuck_hours(),
            stuck_days_discharge: default_stuck_days(),
            max_precip_milli: default_max_precip(),
            jump_ln: BTreeMap::new(),
        }
    }
}

/// Intervalo esperado entre leituras de cada fonte, em segundos.
pub fn cadence(source: Source) -> i64 {
    match source {
        Source::Inmet | Source::Cemaden => 3_600,
        Source::Ana => 900,
        Source::OpenMeteoGlofas | Source::PublicRecord => 86_400,
    }
}

/// Estado carregado de um lote para o seguinte (persistido pelo agregador).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub stations: BTreeMap<String, StationState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StationState {
    pub score: Option<f64>,
    pub last_ts: Option<i64>,
    pub last_value: Option<i64>,
    /// Leituras seguidas com o mesmo valor não nulo, terminando em `last_ts`.
    pub run: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flag {
    pub ts_utc: i64,
    pub reason: Reason,
    pub value_milli: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationQuality {
    pub station_id: String,
    pub expected: usize,
    pub present: usize,
    pub availability: f64,
    pub flag_rate: f64,
    pub agreement: f64,
    pub freshness: f64,
    /// C_s do lote, antes da média móvel.
    pub raw: f64,
    /// C_s suavizado: o que decide a degradação.
    pub score: f64,
    pub flags: Vec<Flag>,
    pub degraded: Option<Reason>,
}

/// Uma leitura em que um nó divergiu dos demais.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dissent {
    pub node: usize,
    pub station_id: String,
    pub ts_utc: i64,
    /// `None` = o nó não tinha esta leitura (ou tinha uma que os outros não têm).
    pub value_milli: Option<i64>,
    pub median_milli: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Agreement {
    /// Fração de nós que concordaram em todas as leituras da estação.
    pub by_station: BTreeMap<String, f64>,
    pub dissents: Vec<Dissent>,
}

fn median(sorted: &[i64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) as f64 / 2.0
    }
}

/// Concordância entre as leituras que cada nó assinou para a mesma janela (6.4).
///
/// Por chave (estação, variável, instante): z_i = 0,6745·(x_i − med)/MAD, sinaliza |z| > 3,5.
/// Com MAD = 0 (a maioria concorda exatamente), qualquer valor diferente da mediana diverge.
/// A mediana resiste a 1 nó mentiroso entre 3; a média seria arrastada.
pub fn node_agreement(nodes: &[&[Reading]]) -> Agreement {
    type Key = (String, Variable, i64);
    let mut values: BTreeMap<Key, Vec<Option<i64>>> = BTreeMap::new();
    for (i, readings) in nodes.iter().enumerate() {
        for r in *readings {
            let slot = values.entry((r.station_id.clone(), r.variable, r.ts_utc)).or_insert_with(|| vec![None; nodes.len()]);
            slot[i] = Some(r.value_milli);
        }
    }

    let mut dissenting: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    let mut dissents = Vec::new();
    for ((station, _, ts), vals) in &values {
        let flags = dissenting.entry(station.clone()).or_insert_with(|| vec![false; nodes.len()]);
        let mut present: Vec<i64> = vals.iter().flatten().copied().collect();
        present.sort_unstable();
        // A leitura existe se a maioria dos nós a tem.
        let majority_has = present.len() * 2 > nodes.len();
        let med = median(&present);
        let mut dev: Vec<i64> = present.iter().map(|x| (*x as f64 - med).abs() as i64).collect();
        dev.sort_unstable();
        let mad = median(&dev);

        for (i, v) in vals.iter().enumerate() {
            let diverges = match (v, majority_has) {
                (Some(x), true) => {
                    let d = *x as f64 - med;
                    if mad == 0.0 { d != 0.0 } else { (0.6745 * d / mad).abs() > MAD_Z }
                }
                (None, false) => false,
                _ => true,
            };
            if diverges {
                flags[i] = true;
                dissents.push(Dissent {
                    node: i,
                    station_id: station.clone(),
                    ts_utc: *ts,
                    value_milli: *v,
                    median_milli: majority_has.then_some(med.round() as i64),
                });
            }
        }
    }

    let by_station = dissenting
        .into_iter()
        .map(|(s, f)| {
            let n = f.iter().filter(|d| **d).count();
            (s, 1.0 - n as f64 / nodes.len() as f64)
        })
        .collect();
    Agreement { by_station, dissents }
}

/// Avalia o lote [t_start, t_end) de cada estação do catálogo e atualiza `state`.
///
/// `readings` são as leituras ancoradas (consenso); `agreement` vem de [`node_agreement`].
pub fn assess(
    state: &mut State,
    limits: &Limits,
    stations: &[(String, Source)],
    t_start: i64,
    t_end: i64,
    readings: &[Reading],
    agreement: &Agreement,
) -> Vec<StationQuality> {
    let mut out = Vec::new();
    for (id, source) in stations {
        let step = cadence(*source);
        let mut rs: Vec<&Reading> = readings.iter().filter(|r| &r.station_id == id).collect();
        rs.sort_by_key(|r| r.ts_utc);
        let st = state.stations.entry(id.clone()).or_default();

        let mut flags = Vec::new();
        for r in &rs {
            let consecutive = st.last_ts == Some(r.ts_utc - step);
            let v = r.value_milli;
            st.run = if consecutive && st.last_value == Some(v) && v != 0 { st.run + 1 } else { 1 };

            let stuck_limit = match r.variable {
                Variable::Precipitation => limits.stuck_hours_precip,
                _ => limits.stuck_days_discharge,
            };
            let jump = match r.variable {
                Variable::Precipitation => v > limits.max_precip_milli,
                _ => match (consecutive, st.last_value, limits.jump_ln.get(id)) {
                    (true, Some(prev), Some(max)) if prev > 0 && v > 0 => (v as f64 / prev as f64).ln().abs() > *max,
                    _ => false,
                },
            };
            if v != 0 && st.run >= stuck_limit {
                flags.push(Flag { ts_utc: r.ts_utc, reason: Reason::Stuck, value_milli: v });
            } else if jump {
                flags.push(Flag { ts_utc: r.ts_utc, reason: Reason::Jump, value_milli: v });
            }
            st.last_ts = Some(r.ts_utc);
            st.last_value = Some(v);
        }

        let expected = ((t_end - t_start) / step).max(1) as usize;
        let availability = (rs.len() as f64 / expected as f64).min(1.0);
        let flag_rate = if rs.is_empty() { 0.0 } else { flags.len() as f64 / rs.len() as f64 };
        let agree = agreement.by_station.get(id).copied().unwrap_or(1.0);
        // Frescor: 1 enquanto a última leitura tem até 2 cadências; cai a zero em mais um dia.
        let freshness = match st.last_ts {
            Some(last) => {
                let age = t_end - last;
                if age <= 2 * step { 1.0 } else { (1.0 - (age - 2 * step) as f64 / 86_400.0).max(0.0) }
            }
            None => 0.0,
        };

        let parts = [availability, 1.0 - flag_rate, agree, freshness];
        let raw: f64 = parts.iter().zip(WEIGHTS).map(|(p, w)| p * w).sum();
        let score = match st.score {
            Some(prev) => EMA_ALPHA * raw + (1.0 - EMA_ALPHA) * prev,
            None => raw,
        };
        st.score = Some(score);

        let degraded = if agree < 1.0 {
            // 6.4: divergência entre nós é registrada sempre, independentemente do escore.
            Some(Reason::Disagreement)
        } else if score < MIN_SCORE {
            let stuck = flags.iter().filter(|f| f.reason == Reason::Stuck).count();
            let flag_reason = if stuck * 2 >= flags.len() { Reason::Stuck } else { Reason::Jump };
            let losses = [
                (WEIGHTS[0] * (1.0 - availability), Reason::Unavailable),
                (WEIGHTS[1] * flag_rate, flag_reason),
                (WEIGHTS[3] * (1.0 - freshness), Reason::Stale),
            ];
            losses.into_iter().max_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, r)| r)
        } else {
            None
        };

        out.push(StationQuality {
            station_id: id.clone(),
            expected,
            present: rs.len(),
            availability,
            flag_rate,
            agreement: agree,
            freshness,
            raw,
            score,
            flags,
            degraded,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn rain(station: &str, ts: i64, mm_milli: i64) -> Reading {
        Reading {
            station_id: station.into(),
            variable: Variable::Precipitation,
            ts_utc: ts,
            source: Source::Inmet,
            value_milli: mm_milli,
            datum_id: None,
            raw_payload_sha256: [0; 32],
        }
    }

    fn flow(station: &str, ts: i64, milli: i64) -> Reading {
        Reading { variable: Variable::RiverDischarge, source: Source::OpenMeteoGlofas, ..rain(station, ts, milli) }
    }

    fn day_of_rain(station: &str, day: i64, hours: i64, value: impl Fn(i64) -> i64) -> Vec<Reading> {
        (0..hours).map(|h| rain(station, day * DAY + h * 3600, value(h))).collect()
    }

    fn inmet(id: &str) -> Vec<(String, Source)> {
        vec![(id.into(), Source::Inmet)]
    }

    #[test]
    fn healthy_station_scores_one() {
        let mut st = State::default();
        let rs = day_of_rain("A801", 10, 24, |h| h % 3 * 200);
        let q = assess(&mut st, &Limits::default(), &inmet("A801"), 10 * DAY, 11 * DAY, &rs, &Agreement::default());
        assert!((q[0].score - 1.0).abs() < 1e-12);
        assert!(q[0].flags.is_empty() && q[0].degraded.is_none());
    }

    #[test]
    fn six_identical_nonzero_hours_are_stuck() {
        let mut st = State::default();
        let five = day_of_rain("A882", 10, 24, |h| if (3..8).contains(&h) { 800 } else { h * 10 });
        let q = assess(&mut st, &Limits::default(), &inmet("A882"), 10 * DAY, 11 * DAY, &five, &Agreement::default());
        assert!(q[0].flags.is_empty(), "5 h iguais ainda não é travamento");

        let mut st = State::default();
        let six = day_of_rain("A882", 10, 24, |h| if (3..9).contains(&h) { 800 } else { h * 10 });
        let q = assess(&mut st, &Limits::default(), &inmet("A882"), 10 * DAY, 11 * DAY, &six, &Agreement::default());
        assert_eq!(q[0].flags.len(), 1);
        assert_eq!(q[0].flags[0].reason, Reason::Stuck);
        // Uma flag isolada não rebaixa a fonte.
        assert!(q[0].degraded.is_none());
    }

    #[test]
    fn stuck_run_continues_across_batches() {
        let mut st = State::default();
        let lim = Limits::default();
        let a = day_of_rain("A801", 10, 24, |h| if h >= 21 { 500 } else { 0 });
        assess(&mut st, &lim, &inmet("A801"), 10 * DAY, 11 * DAY, &a, &Agreement::default());
        let b = day_of_rain("A801", 11, 24, |h| if h < 3 { 500 } else { 0 });
        let q = assess(&mut st, &lim, &inmet("A801"), 11 * DAY, 12 * DAY, &b, &Agreement::default());
        assert_eq!(q[0].flags.len(), 1, "3 h ontem + 3 h hoje = 6 h travado");
    }

    #[test]
    fn impossible_rain_and_discharge_jump() {
        let mut st = State::default();
        let rs = day_of_rain("A801", 10, 24, |h| if h == 5 { 120_000 } else { 0 });
        let q = assess(&mut st, &Limits::default(), &inmet("A801"), 10 * DAY, 11 * DAY, &rs, &Agreement::default());
        assert_eq!(q[0].flags[0].reason, Reason::Jump);

        let lim = Limits { jump_ln: [("G".to_string(), 1.0)].into(), ..Limits::default() };
        let cat = vec![("G".to_string(), Source::OpenMeteoGlofas)];
        let mut st = State::default();
        assess(&mut st, &lim, &cat, 10 * DAY, 11 * DAY, &[flow("G", 10 * DAY, 1_000_000)], &Agreement::default());
        // ln(2) = 0,69: dentro do limite.
        let q = assess(&mut st, &lim, &cat, 11 * DAY, 12 * DAY, &[flow("G", 11 * DAY, 2_000_000)], &Agreement::default());
        assert!(q[0].flags.is_empty());
        // ln(3) = 1,10: salto.
        let q = assess(&mut st, &lim, &cat, 12 * DAY, 13 * DAY, &[flow("G", 12 * DAY, 6_000_000)], &Agreement::default());
        assert_eq!(q[0].flags[0].reason, Reason::Jump);
    }

    #[test]
    fn missing_hours_degrade_after_ema_and_recover() {
        // Sequência real da A882 em 21–25/05/2024 (horas por dia), depois dois dias completos.
        let mut st = State::default();
        let lim = Limits::default();
        let cat = inmet("A882");
        let mut log = Vec::new();
        for (i, hours) in [24, 12, 7, 6, 9, 24, 24].into_iter().enumerate() {
            let day = 10 + i as i64;
            let rs = day_of_rain("A882", day, hours, |_| 0);
            let q = assess(&mut st, &lim, &cat, day * DAY, (day + 1) * DAY, &rs, &Agreement::default());
            log.push(q[0].degraded);
        }
        // A média móvel absorve um ou dois dias ruins; o terceiro seguido rebaixa a fonte.
        assert_eq!(&log[..3], &[None, None, None]);
        assert_eq!(&log[3..5], &[Some(Reason::Unavailable), Some(Reason::Unavailable)]);
        assert_eq!(&log[5..], &[None, None], "volta ao normal com dados completos");
    }

    #[test]
    fn mad_flags_the_tampering_node_only() {
        let honest = day_of_rain("A801", 10, 24, |h| h * 100);
        let mut tampered = honest.clone();
        tampered[3].value_milli += 10_000;
        let ag = node_agreement(&[&honest, &tampered, &honest]);
        assert_eq!(ag.dissents.len(), 1);
        assert_eq!(ag.dissents[0].node, 1);
        assert_eq!(ag.dissents[0].median_milli, Some(300));
        assert!((ag.by_station["A801"] - 2.0 / 3.0).abs() < 1e-12);

        let mut st = State::default();
        let q = assess(&mut st, &Limits::default(), &inmet("A801"), 10 * DAY, 11 * DAY, &honest, &ag);
        assert_eq!(q[0].degraded, Some(Reason::Disagreement));
    }

    #[test]
    fn missing_reading_in_one_node_is_dissent_but_minority_extra_too() {
        let full = day_of_rain("A801", 10, 24, |_| 0);
        let short = full[..23].to_vec();
        let ag = node_agreement(&[&full, &full, &short]);
        assert_eq!(ag.dissents.len(), 1);
        assert_eq!((ag.dissents[0].node, ag.dissents[0].value_milli), (2, None));

        let ag = node_agreement(&[&short, &short, &full]);
        assert_eq!(ag.dissents.len(), 1, "leitura extra só em 1 nó também diverge");
        assert_eq!(ag.dissents[0].node, 2);
    }

    #[test]
    fn mad_tolerates_small_spread() {
        let a = vec![flow("G", 0, 1_000_000)];
        let b = vec![flow("G", 0, 1_000_100)];
        let c = vec![flow("G", 0, 1_000_200)];
        assert!(node_agreement(&[&a, &b, &c]).dissents.is_empty());
    }
}
