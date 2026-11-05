//! Merkle Mountain Range: the append-only accumulator of Cota v2.
//!
//! Every record the program accepts becomes a leaf. The program stores only the peaks (one perfect
//! tree per set bit of `leaf_count`, indexed by height); this module is the reference that anyone can
//! run over the ledger to get the same peaks, and that builds the inclusion proofs.
//!
//! Hashing is the same as the batch trees (RFC 9162): leaf `H(0x00 ‖ data)`, node `H(0x01 ‖ left ‖ right)`,
//! with the older subtree on the left.

use crate::merkle::{hash_leaf, hash_node, Hash};

/// Heights the on-chain account has room for: up to 2^32 − 1 records.
pub const MAX_PEAKS: usize = 32;

/// Inclusion proof of a leaf in the peak that contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub leaf_index: u64,
    /// Siblings from the leaf up to the peak; their count is the peak's height.
    pub siblings: Vec<Hash>,
}

/// Full MMR (all leaf hashes kept), to rebuild the peaks and produce proofs.
#[derive(Debug, Clone, Default)]
pub struct Mmr {
    leaves: Vec<Hash>,
}

impl Mmr {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the record bytes; returns the leaf index.
    pub fn push(&mut self, data: &[u8]) -> u64 {
        self.push_leaf(hash_leaf(data))
    }

    pub fn push_leaf(&mut self, leaf: Hash) -> u64 {
        self.leaves.push(leaf);
        self.leaves.len() as u64 - 1
    }

    pub fn len(&self) -> u64 {
        self.leaves.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// The peaks as the program stores them: `peaks[h]` is set when bit `h` of the leaf count is set,
    /// zeros otherwise.
    pub fn peaks(&self) -> [Hash; MAX_PEAKS] {
        let mut peaks = [[0u8; 32]; MAX_PEAKS];
        for (height, start) in ranges(self.len()) {
            peaks[height] = perfect_root(&self.leaves[start as usize..start as usize + (1usize << height)]);
        }
        peaks
    }

    /// One hash for the whole accumulator: the peaks folded from the lowest (newest) to the highest
    /// (oldest), `H(0x01 ‖ older ‖ newer)`. This is what gets timestamped off-chain.
    pub fn root(&self) -> Option<Hash> {
        bag(&self.peaks(), self.len())
    }

    pub fn proof(&self, leaf_index: u64) -> Option<Proof> {
        let (height, start) = ranges(self.len()).find(|(h, s)| leaf_index >= *s && leaf_index < s + (1u64 << h))?;
        let tree = &self.leaves[start as usize..start as usize + (1usize << height)];
        let mut level: Vec<Hash> = tree.to_vec();
        let mut index = (leaf_index - start) as usize;
        let mut siblings = Vec::with_capacity(height);
        while level.len() > 1 {
            siblings.push(level[index ^ 1]);
            level = level.chunks(2).map(|p| hash_node(&p[0], &p[1])).collect();
            index /= 2;
        }
        Some(Proof { leaf_index, siblings })
    }
}

/// Binary-counter append, exactly as the program does it: carry the new leaf up while the bit at that
/// height is set. Returns the new leaf count.
pub fn append(peaks: &mut [Hash; MAX_PEAKS], leaf_count: u64, leaf: Hash) -> u64 {
    let mut carry = leaf;
    let mut height = 0;
    while (leaf_count >> height) & 1 == 1 {
        carry = hash_node(&peaks[height], &carry);
        peaks[height] = [0; 32];
        height += 1;
    }
    peaks[height] = carry;
    leaf_count + 1
}

/// Checks that `leaf` is the leaf `proof.leaf_index` of an accumulator with `leaf_count` leaves and these
/// `peaks`. Same algorithm as the program and the dashboard.
pub fn verify(peaks: &[Hash; MAX_PEAKS], leaf_count: u64, leaf: &Hash, proof: &Proof) -> bool {
    let Some((height, start)) =
        ranges(leaf_count).find(|(h, s)| proof.leaf_index >= *s && proof.leaf_index < s + (1u64 << h))
    else {
        return false;
    };
    if proof.siblings.len() != height {
        return false;
    }
    let local = proof.leaf_index - start;
    let mut node = *leaf;
    for (level, sib) in proof.siblings.iter().enumerate() {
        node = if (local >> level) & 1 == 0 { hash_node(&node, sib) } else { hash_node(sib, &node) };
    }
    node == peaks[height]
}

/// Folds the peaks into one hash (see [`Mmr::root`]). `None` for an empty accumulator.
pub fn bag(peaks: &[Hash; MAX_PEAKS], leaf_count: u64) -> Option<Hash> {
    let mut acc: Option<Hash> = None;
    for height in 0..MAX_PEAKS {
        if (leaf_count >> height) & 1 == 1 {
            acc = Some(match acc {
                None => peaks[height],
                Some(newer) => hash_node(&peaks[height], &newer),
            });
        }
    }
    acc
}

/// The perfect trees of an accumulator with `n` leaves, oldest first: (height, first leaf index).
fn ranges(n: u64) -> impl Iterator<Item = (usize, u64)> {
    let mut start = 0u64;
    (0..MAX_PEAKS).rev().filter_map(move |h| {
        if (n >> h) & 1 == 1 {
            let r = (h, start);
            start += 1u64 << h;
            Some(r)
        } else {
            None
        }
    })
}

fn perfect_root(leaves: &[Hash]) -> Hash {
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        level = level.chunks(2).map(|p| hash_node(&p[0], &p[1])).collect();
    }
    level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(i: u64) -> Vec<u8> {
        format!("record {i}").into_bytes()
    }

    #[test]
    fn incremental_append_matches_full_rebuild() {
        let mut mmr = Mmr::new();
        let mut peaks = [[0u8; 32]; MAX_PEAKS];
        let mut count = 0;
        for i in 0..300 {
            mmr.push(&data(i));
            count = append(&mut peaks, count, hash_leaf(&data(i)));
            assert_eq!(peaks, mmr.peaks(), "after {} leaves", i + 1);
        }
    }

    #[test]
    fn every_leaf_has_a_valid_proof_and_tampering_fails() {
        let mut mmr = Mmr::new();
        for i in 0..77 {
            mmr.push(&data(i));
        }
        let peaks = mmr.peaks();
        for i in 0..77 {
            let proof = mmr.proof(i).unwrap();
            assert!(verify(&peaks, 77, &hash_leaf(&data(i)), &proof), "leaf {i}");
            assert!(!verify(&peaks, 77, &hash_leaf(b"forged"), &proof));
            let wrong_index = Proof { leaf_index: (i + 1) % 77, ..proof.clone() };
            assert!(!verify(&peaks, 77, &hash_leaf(&data(i)), &wrong_index));
        }
        assert!(mmr.proof(77).is_none());
    }

    #[test]
    fn proofs_go_stale_when_peaks_merge() {
        // 3 leaves: peaks of height 1 and 0. A fourth merges everything into one peak of height 2.
        let mut mmr = Mmr::new();
        for i in 0..3 {
            mmr.push(&data(i));
        }
        let old = mmr.proof(2).unwrap();
        assert!(old.siblings.is_empty());
        mmr.push(&data(3));
        assert!(!verify(&mmr.peaks(), 4, &hash_leaf(&data(2)), &old));
        assert!(verify(&mmr.peaks(), 4, &hash_leaf(&data(2)), &mmr.proof(2).unwrap()));
    }

    #[test]
    fn root_of_a_power_of_two_is_the_merkle_root() {
        let mut mmr = Mmr::new();
        let leaves: Vec<Hash> = (0..8).map(|i| hash_leaf(&data(i))).collect();
        for l in &leaves {
            mmr.push_leaf(*l);
        }
        assert_eq!(mmr.root(), Some(crate::merkle::root_from_leaves(&leaves)));
        assert_eq!(Mmr::new().root(), None);
    }
}
