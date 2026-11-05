//! The program's side of the accumulator: append and inclusion check.
//! Reference implementation and proof builder: `cota_core::mmr` (tests compare both).

use solana_sha256_hasher::hashv;

use crate::constants::MAX_PEAKS;

pub fn hash_leaf(data: &[u8]) -> [u8; 32] {
    hashv(&[&[0x00], data]).to_bytes()
}

pub fn hash_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    hashv(&[&[0x01], left, right]).to_bytes()
}

/// Binary-counter append: carries the new leaf up while the bit at that height is set.
/// Returns the new leaf count, or `None` if the accumulator is full.
pub fn append(peaks: &mut [[u8; 32]; MAX_PEAKS], leaf_count: u64, leaf: [u8; 32]) -> Option<u64> {
    let mut carry = leaf;
    let mut height = 0;
    while (leaf_count >> height) & 1 == 1 {
        carry = hash_node(&peaks[height], &carry);
        peaks[height] = [0; 32];
        height += 1;
    }
    if height >= MAX_PEAKS {
        return None;
    }
    peaks[height] = carry;
    Some(leaf_count + 1)
}

/// Is `leaf` the leaf `leaf_index` of the accumulator, given the siblings up to its peak?
pub fn contains(
    peaks: &[[u8; 32]; MAX_PEAKS],
    leaf_count: u64,
    leaf: &[u8; 32],
    leaf_index: u64,
    siblings: &[[u8; 32]],
) -> bool {
    // The perfect trees, oldest (highest) first.
    let mut start = 0u64;
    for height in (0..MAX_PEAKS).rev() {
        if (leaf_count >> height) & 1 == 0 {
            continue;
        }
        let size = 1u64 << height;
        if leaf_index < start + size {
            if siblings.len() != height {
                return false;
            }
            let local = leaf_index - start;
            let mut node = *leaf;
            for (level, sib) in siblings.iter().enumerate() {
                node = if (local >> level) & 1 == 0 { hash_node(&node, sib) } else { hash_node(sib, &node) };
            }
            return node == peaks[height];
        }
        start += size;
    }
    false
}
