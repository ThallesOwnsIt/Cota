//! Árvore Merkle no formato da RFC 6962 / RFC 9162 (Certificate Transparency).
//!
//! - folha:  H(0x00 || dado)
//! - nó:     H(0x01 || esquerda || direita)
//! - divisão: para n > 1 folhas, a subárvore esquerda tem k folhas, onde k é a maior
//!   potência de 2 menor que n. Não há duplicação de nós ímpares.
//!
//! Os prefixos distintos impedem apresentar um nó interno como se fosse uma folha
//! (ataque de segunda pré-imagem).

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

const LEAF_PREFIX: u8 = 0x00;
const NODE_PREFIX: u8 = 0x01;

pub fn hash_leaf(data: &[u8]) -> Hash {
    let mut h = Sha256::new();
    h.update([LEAF_PREFIX]);
    h.update(data);
    h.finalize().into()
}

pub fn hash_node(left: &Hash, right: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([NODE_PREFIX]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Hash de árvore vazia: SHA-256 da string vazia.
pub fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

/// Maior potência de 2 estritamente menor que `n` (n >= 2).
fn split_point(n: usize) -> usize {
    debug_assert!(n >= 2);
    1 << (usize::BITS - 1 - (n - 1).leading_zeros())
}

/// Raiz a partir dos hashes de folha já calculados.
pub fn root_from_leaves(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => empty_root(),
        1 => leaves[0],
        n => {
            let k = split_point(n);
            hash_node(&root_from_leaves(&leaves[..k]), &root_from_leaves(&leaves[k..]))
        }
    }
}

/// Caminho de auditoria (irmãos, da folha até a raiz) para a folha `index`.
pub fn proof(leaves: &[Hash], index: usize) -> Vec<Hash> {
    assert!(index < leaves.len(), "índice fora da árvore");
    let mut path = Vec::new();
    build_proof(leaves, index, &mut path);
    path
}

fn build_proof(leaves: &[Hash], index: usize, path: &mut Vec<Hash>) {
    let n = leaves.len();
    if n <= 1 {
        return;
    }
    let k = split_point(n);
    if index < k {
        build_proof(&leaves[..k], index, path);
        path.push(root_from_leaves(&leaves[k..]));
    } else {
        build_proof(&leaves[k..], index - k, path);
        path.push(root_from_leaves(&leaves[..k]));
    }
}

/// Verificação de prova de inclusão (RFC 9162, seção 2.1.3.2).
pub fn verify(leaf: &Hash, index: u64, tree_size: u64, path: &[Hash], root: &Hash) -> bool {
    if index >= tree_size {
        return false;
    }
    let (mut fn_, mut sn) = (index, tree_size - 1);
    let mut r = *leaf;
    for p in path {
        if sn == 0 {
            return false;
        }
        if fn_ & 1 == 1 || fn_ == sn {
            r = hash_node(p, &r);
            while fn_ & 1 == 0 && fn_ != 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            r = hash_node(&r, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    sn == 0 && r == *root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<Hash> {
        (0..n).map(|i| hash_leaf(&(i as u32).to_le_bytes())).collect()
    }

    #[test]
    fn split_point_matches_rfc() {
        assert_eq!(split_point(2), 1);
        assert_eq!(split_point(3), 2);
        assert_eq!(split_point(4), 2);
        assert_eq!(split_point(5), 4);
        assert_eq!(split_point(8), 4);
        assert_eq!(split_point(9), 8);
    }

    #[test]
    fn every_proof_verifies_for_many_sizes() {
        for n in 1..=70 {
            let ls = leaves(n);
            let root = root_from_leaves(&ls);
            for i in 0..n {
                let p = proof(&ls, i);
                assert!(verify(&ls[i], i as u64, n as u64, &p, &root), "n={n} i={i}");
            }
        }
    }

    #[test]
    fn proof_rejects_wrong_leaf_index_or_root() {
        let ls = leaves(13);
        let root = root_from_leaves(&ls);
        let p = proof(&ls, 5);
        assert!(!verify(&ls[6], 5, 13, &p, &root));
        assert!(!verify(&ls[5], 4, 13, &p, &root));
        // Um tamanho com outro formato de árvore invalida a prova. Tamanhos com o mesmo formato
        // (13 e 14 para o índice 5) não, por isso `leaf_count` faz parte do cabeçalho assinado.
        assert!(!verify(&ls[5], 5, 6, &p, &root));
        assert!(!verify(&ls[5], 5, 13, &p, &[0u8; 32]));
    }

    #[test]
    fn leaf_and_node_domains_differ() {
        let a = hash_leaf(b"x");
        let b = hash_leaf(b"y");
        let mut concat = a.to_vec();
        concat.extend_from_slice(&b);
        assert_ne!(hash_node(&a, &b), hash_leaf(&concat));
    }
}
