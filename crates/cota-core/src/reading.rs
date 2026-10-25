//! Leitura ambiental canônica. Tudo o que entra numa árvore Merkle é um `Reading`.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// Origem do dado. O discriminante é gravado em Borsh, então a ordem não pode mudar.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
#[borsh(use_discriminant = true)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Source {
    Inmet = 0,
    OpenMeteoGlofas = 1,
    Ana = 2,
    Cemaden = 3,
    /// Marco público digitado de um documento (ex.: nota técnica ANA/SGB), com o hash do PDF.
    PublicRecord = 4,
}

/// Grandeza medida. Cada variável tem uma unidade fixa, ver [`Variable::unit`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
#[borsh(use_discriminant = true)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Variable {
    /// Chuva acumulada na hora anterior.
    Precipitation = 0,
    /// Nível do rio (cota) relativo ao `datum_id`.
    RiverStage = 1,
    /// Vazão (medida ou modelada).
    RiverDischarge = 2,
}

/// Unidade do valor *antes* da escala x1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Millimeter,
    Meter,
    CubicMeterPerSecond,
}

impl Variable {
    pub fn unit(self) -> Unit {
        match self {
            Variable::Precipitation => Unit::Millimeter,
            Variable::RiverStage => Unit::Meter,
            Variable::RiverDischarge => Unit::CubicMeterPerSecond,
        }
    }
}

/// Uma observação de uma fonte, num instante, para uma estação.
///
/// `value_milli` é o valor na unidade de [`Variable::unit`] multiplicado por 1000
/// (chuva 12,4 mm -> 12_400; nível 3,69 m -> 3_690). Inteiros evitam divergência
/// de ponto flutuante entre Python, Rust e TypeScript.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
pub struct Reading {
    pub station_id: String,
    pub variable: Variable,
    pub ts_utc: i64,
    pub source: Source,
    pub value_milli: i64,
    /// Referência vertical do nível (ex.: "IBGE-1788A"); `None` para chuva e vazão.
    pub datum_id: Option<String>,
    /// SHA-256 do payload bruto da fonte (linha do CSV, corpo JSON), para refazer o hash depois.
    #[serde(with = "hex32")]
    pub raw_payload_sha256: [u8; 32],
}

impl Reading {
    /// Bytes canônicos (Borsh). É exatamente isso que vira folha na árvore Merkle.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("Borsh de Reading não falha")
    }

    /// Converte um valor decimal para a escala x1000, arredondando ao mais próximo.
    pub fn to_milli(value: f64) -> i64 {
        (value * 1000.0).round() as i64
    }

    pub fn value(&self) -> f64 {
        self.value_milli as f64 / 1000.0
    }
}

/// Serializa `[u8; 32]` como string hexadecimal em JSON (Borsh continua binário).
pub mod hex32 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(&text, &mut out).map_err(D::Error::custom)?;
        Ok(out)
    }
}
