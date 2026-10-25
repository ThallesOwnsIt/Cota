//! Montagem dos dados da instrução do precompile Ed25519 da Solana com várias
//! assinaturas sobre a mesma mensagem.
//!
//! Layout gerado:
//! ```text
//! [n, 0] [offsets × n (14 B cada)] [mensagem] [pubkey 32 B ‖ assinatura 64 B] × n
//! ```
//! A mensagem aparece uma única vez e todas as assinaturas apontam para ela. Todos os
//! índices de instrução são `u16::MAX` ("nesta mesma instrução"), que é o que o
//! programa on-chain exige.

pub const PUBKEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;
const OFFSETS_START: usize = 2;
const OFFSETS_LEN: usize = 14;
const THIS_INSTRUCTION: u16 = u16::MAX;

/// Program ID do precompile Ed25519 (`Ed25519SigVerify111111111111111111111111111`).
pub const ED25519_PROGRAM_ID: &str = "Ed25519SigVerify111111111111111111111111111";

/// Dados da instrução para `signatures` = [(pubkey, assinatura)] sobre `message`.
pub fn ed25519_instruction_data(message: &[u8], signatures: &[([u8; PUBKEY_LEN], [u8; SIGNATURE_LEN])]) -> Vec<u8> {
    let n = signatures.len();
    assert!((1..=u8::MAX as usize).contains(&n), "entre 1 e 255 assinaturas");

    let message_offset = OFFSETS_START + n * OFFSETS_LEN;
    let first_entry = message_offset + message.len();
    let total = first_entry + n * (PUBKEY_LEN + SIGNATURE_LEN);
    assert!(total <= u16::MAX as usize, "instrução grande demais para offsets u16");

    let mut data = Vec::with_capacity(total);
    data.extend_from_slice(&[n as u8, 0]);
    for i in 0..n {
        let pubkey_offset = first_entry + i * (PUBKEY_LEN + SIGNATURE_LEN);
        let signature_offset = pubkey_offset + PUBKEY_LEN;
        for field in [
            signature_offset as u16,
            THIS_INSTRUCTION,
            pubkey_offset as u16,
            THIS_INSTRUCTION,
            message_offset as u16,
            message.len() as u16,
            THIS_INSTRUCTION,
        ] {
            data.extend_from_slice(&field.to_le_bytes());
        }
    }
    data.extend_from_slice(message);
    for (pubkey, signature) in signatures {
        data.extend_from_slice(pubkey);
        data.extend_from_slice(signature);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_points_every_signature_to_the_shared_message() {
        let msg = b"COTA";
        let data = ed25519_instruction_data(msg, &[([1; 32], [2; 64]), ([3; 32], [4; 64])]);
        let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;

        assert_eq!(data[0], 2);
        assert_eq!(data.len(), 2 + 2 * 14 + 4 + 2 * 96);
        for i in 0..2 {
            let base = 2 + i * 14;
            let (sig, pk, msg_off, msg_len) = (u16_at(base), u16_at(base + 4), u16_at(base + 8), u16_at(base + 10));
            assert_eq!(&data[msg_off..msg_off + msg_len], msg);
            assert_eq!(data[pk], [1, 3][i]);
            assert_eq!(data[sig], [2, 4][i]);
            assert_eq!(sig, pk + 32);
        }
    }
}
