//! Leitura da instrução do precompile Ed25519 via introspecção (sysvar Instructions).
//!
//! O precompile já rejeitou a transação se alguma assinatura fosse inválida. Aqui só
//! descobrimos *quais* chaves assinaram *qual* mensagem.
//!
//! Layout dos dados da instrução:
//! ```text
//! [0]      num_signatures: u8
//! [1]      padding
//! [2..]    num_signatures × Ed25519SignatureOffsets (14 bytes, u16 LE cada campo):
//!          signature_offset, signature_instruction_index,
//!          public_key_offset, public_key_instruction_index,
//!          message_data_offset, message_data_size, message_instruction_index
//! [...]    pubkeys, assinaturas e mensagem, nos offsets acima
//! ```

use anchor_lang::prelude::*;
use solana_instructions_sysvar::get_instruction_relative;

use crate::error::CotaError;

const OFFSETS_START: usize = 2;
const OFFSETS_SIZE: usize = 14;
const PUBKEY_SIZE: usize = 32;
/// Índice que significa "dados nesta mesma instrução".
const THIS_INSTRUCTION: u16 = u16::MAX;

/// Exige que a instrução imediatamente anterior seja o precompile Ed25519 assinando
/// `message` e devolve (bitmap dos membros que assinaram, quantos), já checando o limiar.
pub fn committee_signers(instructions: &AccountInfo, members: &[Pubkey], threshold: u8, message: &[u8]) -> Result<(u8, u8)> {
    // Índice relativo, não absoluto: a instrução Ed25519 tem de vir logo antes desta.
    let ed_ix = get_instruction_relative(-1, instructions).map_err(|_| CotaError::MissingEd25519Instruction)?;
    require_keys_eq!(ed_ix.program_id, solana_sdk_ids::ed25519_program::ID, CotaError::MissingEd25519Instruction);

    let mut signers: u8 = 0;
    for pk in signers_of(&ed_ix.data, message)? {
        let i = members.iter().position(|m| *m == pk).ok_or(CotaError::UnknownSigner)?;
        signers |= 1 << i;
    }
    let count = signers.count_ones() as u8;
    require!(count >= threshold, CotaError::ThresholdNotMet);
    Ok((signers, count))
}

fn read_u16(data: &[u8], at: usize) -> Result<u16> {
    let bytes = data.get(at..at + 2).ok_or(CotaError::MalformedEd25519Instruction)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Retorna as chaves públicas que assinaram `expected_message`.
///
/// Falha se qualquer assinatura cobrir outra mensagem ou se algum offset apontar para
/// fora desta instrução (o que permitiria trocar a mensagem verificada por outra).
pub fn signers_of(data: &[u8], expected_message: &[u8]) -> Result<Vec<Pubkey>> {
    let count = *data.first().ok_or(CotaError::MalformedEd25519Instruction)? as usize;
    require!(count > 0, CotaError::MalformedEd25519Instruction);

    let mut signers = Vec::with_capacity(count);
    for i in 0..count {
        let base = OFFSETS_START + i * OFFSETS_SIZE;
        let sig_ix = read_u16(data, base + 2)?;
        let pk_offset = read_u16(data, base + 4)? as usize;
        let pk_ix = read_u16(data, base + 6)?;
        let msg_offset = read_u16(data, base + 8)? as usize;
        let msg_size = read_u16(data, base + 10)? as usize;
        let msg_ix = read_u16(data, base + 12)?;

        require!(
            sig_ix == THIS_INSTRUCTION && pk_ix == THIS_INSTRUCTION && msg_ix == THIS_INSTRUCTION,
            CotaError::MalformedEd25519Instruction
        );

        let message = data
            .get(msg_offset..msg_offset + msg_size)
            .ok_or(CotaError::MalformedEd25519Instruction)?;
        require!(message == expected_message, CotaError::MessageMismatch);

        let pk = data
            .get(pk_offset..pk_offset + PUBKEY_SIZE)
            .ok_or(CotaError::MalformedEd25519Instruction)?;
        signers.push(Pubkey::try_from(pk).map_err(|_| CotaError::MalformedEd25519Instruction)?);
    }
    Ok(signers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uma assinatura sobre `msg`, com todos os índices "nesta instrução".
    fn single(msg: &[u8]) -> Vec<u8> {
        let msg_off = (OFFSETS_START + OFFSETS_SIZE) as u16;
        let pk_off = msg_off + msg.len() as u16;
        let sig_off = pk_off + PUBKEY_SIZE as u16;
        let mut d = vec![1, 0];
        for f in [sig_off, THIS_INSTRUCTION, pk_off, THIS_INSTRUCTION, msg_off, msg.len() as u16, THIS_INSTRUCTION] {
            d.extend_from_slice(&f.to_le_bytes());
        }
        d.extend_from_slice(msg);
        d.extend_from_slice(&[5; PUBKEY_SIZE]);
        d.extend_from_slice(&[6; 64]);
        d
    }

    #[test]
    fn extracts_signer() {
        assert_eq!(signers_of(&single(b"m"), b"m").unwrap(), vec![Pubkey::new_from_array([5; 32])]);
    }

    #[test]
    fn rejects_offsets_into_other_instructions() {
        // Campos de índice: assinatura (+2), pubkey (+6), mensagem (+12).
        for field in [2, 6, 12] {
            let mut d = single(b"m");
            let at = OFFSETS_START + field;
            d[at..at + 2].copy_from_slice(&1u16.to_le_bytes());
            assert!(signers_of(&d, b"m").is_err(), "campo +{field} apontando para a instrução 1");
        }
    }

    #[test]
    fn rejects_other_message_and_truncated_data() {
        assert!(signers_of(&single(b"m"), b"x").is_err());
        let d = single(b"m");
        assert!(signers_of(&d[..d.len() - 70], b"m").is_err());
        assert!(signers_of(&[], b"m").is_err());
        assert!(signers_of(&[0, 0], b"m").is_err());
    }
}
