//! Canonical environmental reading. Everything that goes into a Merkle tree is a `Reading`.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// Where the data comes from. The discriminant is written in Borsh, so the order cannot change.
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
    /// Public milestone typed in from a document (e.g. an ANA/SGB technical note), with the PDF's
    /// hash.
    PublicRecord = 4,
}

/// Measured quantity. Each variable has a fixed unit, see [`Variable::unit`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
#[borsh(use_discriminant = true)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Variable {
    /// Rain accumulated over the previous hour.
    Precipitation = 0,
    /// River level (stage) relative to `datum_id`.
    RiverStage = 1,
    /// Discharge (measured or modelled).
    RiverDischarge = 2,
}

/// Unit of the value *before* the x1000 scaling.
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

/// One observation from one source, at one instant, for one station.
///
/// `value_milli` is the value in the unit of [`Variable::unit`] multiplied by 1000
/// (rain 12.4 mm -> 12_400; level 3.69 m -> 3_690). Integers avoid floating-point
/// divergence between Python, Rust and TypeScript.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, BorshSerialize, BorshDeserialize, Serialize, Deserialize,
)]
pub struct Reading {
    pub station_id: String,
    pub variable: Variable,
    pub ts_utc: i64,
    pub source: Source,
    pub value_milli: i64,
    /// Vertical reference of the level (e.g. "IBGE-1788A"); `None` for rain and discharge.
    pub datum_id: Option<String>,
    /// SHA-256 of the source's raw payload (CSV line, JSON body), so the hash can be recomputed later.
    #[serde(with = "hex32")]
    pub raw_payload_sha256: [u8; 32],
}

impl Reading {
    /// Canonical bytes (Borsh). This is exactly what becomes a leaf in the Merkle tree.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("Borsh de Reading não falha")
    }

    /// Converts a decimal value to the x1000 scale, rounding to the nearest.
    pub fn to_milli(value: f64) -> i64 {
        (value * 1000.0).round() as i64
    }

    pub fn value(&self) -> f64 {
        self.value_milli as f64 / 1000.0
    }
}

/// Serializes `[u8; 32]` as a hex string in JSON (Borsh stays binary).
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
