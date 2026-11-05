pub mod admin;
pub mod records;
pub mod vault;

pub use admin::*;
pub use records::*;
pub use vault::*;

use anchor_lang::prelude::*;

use crate::{error::CotaError, events::RecordAppended, mmr, state::{Record, Registry}};

/// Appends `record` to the accumulator and emits it. Returns its index.
pub fn append_record(registry: &mut Registry, record: Record) -> Result<u64> {
    let mut data = Vec::new();
    record.serialize(&mut data)?;
    let leaf = mmr::hash_leaf(&data);
    let index = registry.leaf_count;
    registry.leaf_count = mmr::append(&mut registry.peaks, index, leaf).ok_or(CotaError::AccumulatorFull)?;
    emit!(RecordAppended { index, leaf, record });
    Ok(index)
}
