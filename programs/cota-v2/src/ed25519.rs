//! Reads the Ed25519 precompile instruction through introspection (Instructions sysvar).
//!
//! The precompile has already rejected the transaction if any signature was invalid. Here we only
//! find out *which* keys signed *which* message.
//!
//! Instruction data layout:
//! ```text
//! [0]      num_signatures: u8
//! [1]      padding
//! [2..]    num_signatures × Ed25519SignatureOffsets (14 bytes, u16 LE per field):
//!          signature_offset, signature_instruction_index,
//!          public_key_offset, public_key_instruction_index,
//!          message_data_offset, message_data_size, message_instruction_index
//! [...]    pubkeys, signatures and message, at the offsets above
//! ```

use anchor_lang::prelude::*;
use solana_instructions_sysvar::get_instruction_relative;

use crate::error::CotaError;

const OFFSETS_START: usize = 2;
const OFFSETS_SIZE: usize = 14;
const PUBKEY_SIZE: usize = 32;
/// Index meaning "data in this same instruction".
const THIS_INSTRUCTION: u16 = u16::MAX;

/// Requires the instruction right before this one to be the Ed25519 precompile signing
/// `message` and returns (bitmap of the members who signed, how many), already checking the
/// threshold.
pub fn committee_signers(instructions: &AccountInfo, members: &[Pubkey], threshold: u8, message: &[u8]) -> Result<(u8, u8)> {
    // Relative index, not absolute: the Ed25519 instruction must come right before this one.
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

/// Returns the public keys that signed `expected_message`.
///
/// Fails if any signature covers another message or if any offset points outside
/// this instruction (which would allow swapping the verified message for another).
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

    /// One signature over `msg`, with every index set to "this instruction".
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
        // Index fields: signature (+2), pubkey (+6), message (+12).
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
