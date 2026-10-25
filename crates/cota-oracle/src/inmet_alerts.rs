//! Avisos meteorológicos ativos do INMET, transformados em `PublicAlertHeader`s.
//!
//! A API só devolve os avisos vigentes ou futuros; depois que expiram, somem. Por isso o
//! conteúdo canônico de cada versão fica guardado no ledger e o hash dele, on-chain.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use chrono::{FixedOffset, NaiveDateTime, TimeZone};
use cota_core::{public_alert::content_hash, PublicAlertHeader};
use serde_json::Value;

pub const URL: &str = "https://apiprevmet3.inmet.gov.br/avisos/ativos";
pub const ISSUER: &str = "inmet";
/// Campos que não descrevem o aviso: o ícone (imagem base64) e o id interno do banco.
const IGNORED: &[&str] = &["icone", "id"];

#[derive(Debug, Clone)]
pub struct ObservedAlert {
    pub header: PublicAlertHeader,
    /// JSON canônico: os bytes cujo SHA-256 é `header.content_sha256`.
    pub content: String,
    pub description: String,
    pub severity_label: String,
    pub states: String,
}

pub fn fetch(http: &reqwest::blocking::Client) -> Result<Value> {
    // O servidor do INMET recusa clientes sem User-Agent de navegador.
    http.get(URL)
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0")
        .send()
        .and_then(|r| r.error_for_status())
        .context("consultando avisos do INMET")?
        .json()
        .context("resposta do INMET não é JSON")
}

/// JSON com chaves ordenadas, sem espaços: a mesma entrada gera os mesmos bytes em qualquer nó.
pub fn canonical_json(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{}:", Value::String(k.clone()));
                canonical_json(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => {
            let _ = write!(out, "{other}");
        }
    }
}

fn severity(label: &str) -> u8 {
    match label {
        "Perigo Potencial" => 1,
        "Perigo" => 2,
        "Grande Perigo" => 3,
        _ => 0,
    }
}

/// "2024-04-29 10:00" no horário de Brasília (UTC−3, sem horário de verão desde 2019).
fn brasilia(text: &str) -> Result<i64> {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").with_context(|| format!("data inválida: {text}"))?;
    let tz = FixedOffset::west_opt(3 * 3600).expect("fuso válido");
    Ok(tz.from_local_datetime(&naive).single().context("data ambígua")?.timestamp())
}

/// Avisos que citam algum dos `states` (nomes por extenso, como na API).
pub fn parse(body: &Value, states: &[String]) -> Result<Vec<ObservedAlert>> {
    let Some(groups) = body.as_object() else { bail!("formato inesperado da API do INMET") };
    let mut out: Vec<ObservedAlert> = Vec::new();
    for alert in groups.values().filter_map(Value::as_array).flatten() {
        let estados = alert["estados"].as_str().unwrap_or_default();
        if !estados.split(',').any(|e| states.iter().any(|s| s == e.trim())) {
            continue;
        }
        let mut obj = alert.as_object().context("aviso não é objeto")?.clone();
        for k in IGNORED {
            obj.remove(*k);
        }
        obj.insert("issuer".into(), Value::String(ISSUER.into()));
        let mut content = String::new();
        canonical_json(&Value::Object(obj), &mut content);
        let content_sha256 = content_hash(&content);
        // O mesmo aviso pode aparecer em "hoje" e em "futuro".
        if out.iter().any(|a| a.header.content_sha256 == content_sha256) {
            continue;
        }

        let label = alert["severidade"].as_str().unwrap_or_default().to_string();
        out.push(ObservedAlert {
            header: PublicAlertHeader {
                issuer: ISSUER.into(),
                alert_id: alert["codigo"].as_str().context("aviso sem codigo")?.to_string(),
                content_sha256,
                severity: severity(&label),
                onset: brasilia(alert["inicio"].as_str().context("aviso sem inicio")?)?,
                expires: brasilia(alert["fim"].as_str().context("aviso sem fim")?)?,
            },
            content,
            description: alert["descricao"].as_str().unwrap_or_default().to_string(),
            severity_label: label,
            states: estados.to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "hoje": [{
                "id": 1, "icone": "data:image/png;base64,AAA",
                "codigo": "urn:oid:2.49.0.0.76.0.2024.100.1",
                "descricao": "Acumulado de Chuva", "severidade": "Grande Perigo",
                "inicio": "2024-04-29 10:00", "fim": "2024-04-30 10:00",
                "estados": "Santa Catarina,Rio Grande do Sul", "riscos": ["Grandes alagamentos."]
            }, {
                "id": 2, "codigo": "urn:oid:2.49.0.0.76.0.2024.101.1", "descricao": "Baixa Umidade",
                "severidade": "Perigo Potencial", "inicio": "2024-04-29 10:00", "fim": "2024-04-29 17:00",
                "estados": "Ceará"
            }],
            "futuro": [{
                "id": 1, "icone": "data:image/png;base64,AAA",
                "codigo": "urn:oid:2.49.0.0.76.0.2024.100.1",
                "descricao": "Acumulado de Chuva", "severidade": "Grande Perigo",
                "inicio": "2024-04-29 10:00", "fim": "2024-04-30 10:00",
                "estados": "Santa Catarina,Rio Grande do Sul", "riscos": ["Grandes alagamentos."]
            }]
        })
    }

    #[test]
    fn filters_state_dedupes_and_converts_time() {
        let alerts = parse(&sample(), &["Rio Grande do Sul".to_string()]).unwrap();
        assert_eq!(alerts.len(), 1);
        let a = &alerts[0];
        assert_eq!(a.header.severity, 3);
        // 10:00 em Brasília = 13:00 UTC.
        assert_eq!(a.header.onset, 1714395600);
        assert_eq!(a.header.expires - a.header.onset, 86_400);
        assert!(!a.content.contains("icone") && a.content.contains("\"issuer\":\"inmet\""));
    }

    #[test]
    fn canonical_json_ignores_key_order() {
        let a = json!({"b": 1, "a": {"y": [1, "ç"], "x": null}});
        let b: Value = serde_json::from_str(r#"{"a":{"x":null,"y":[1,"ç"]},"b":1}"#).unwrap();
        let (mut sa, mut sb) = (String::new(), String::new());
        canonical_json(&a, &mut sa);
        canonical_json(&b, &mut sb);
        assert_eq!(sa, sb);
        assert_eq!(sa, r#"{"a":{"x":null,"y":[1,"ç"]},"b":1}"#);
    }

    #[test]
    fn any_change_in_content_changes_the_hash() {
        let mut body = sample();
        let before = parse(&body, &["Rio Grande do Sul".to_string()]).unwrap()[0].header.content_sha256;
        body["hoje"][0]["fim"] = json!("2024-04-30 18:00");
        body["futuro"][0]["fim"] = json!("2024-04-30 18:00");
        let after = parse(&body, &["Rio Grande do Sul".to_string()]).unwrap()[0].header.content_sha256;
        assert_ne!(before, after);
    }
}
