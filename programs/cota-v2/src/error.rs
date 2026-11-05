use anchor_lang::prelude::*;

#[error_code]
pub enum CotaError {
    #[msg("Only the committee authority can run this instruction")]
    Unauthorized,
    #[msg("Invalid committee: threshold outside 1..=members, too many or duplicate members")]
    InvalidCommittee,
    #[msg("Member not found in the committee")]
    MemberNotFound,
    #[msg("Text longer than allowed")]
    StringTooLong,
    #[msg("batch_id out of sequence")]
    BatchOutOfSequence,
    #[msg("prev_batch_root does not match the latest anchored batch")]
    BrokenHashChain,
    #[msg("Invalid time window, or one overlapping the previous batch")]
    InvalidWindow,
    #[msg("The previous instruction is not an Ed25519 verification")]
    MissingEd25519Instruction,
    #[msg("Malformed Ed25519 instruction, or one pointing at another instruction's data")]
    MalformedEd25519Instruction,
    #[msg("Signed message differs from the header")]
    MessageMismatch,
    #[msg("Signature from a key that is not in the committee")]
    UnknownSigner,
    #[msg("Not enough distinct signatures for the committee threshold")]
    ThresholdNotMet,
    #[msg("No batch anchored yet")]
    NoAnchoredBatch,
    #[msg("A degradation report only applies to the most recent batch")]
    StaleBatch,
    #[msg("Unknown degradation reason or score outside 0..=1000")]
    InvalidDegradation,
    #[msg("The forecast reading is already inside an anchored batch")]
    TargetAlreadyAnchored,
    #[msg("Quantiles out of order (needs p10 <= p50 <= p90) or zero horizon")]
    InvalidForecast,
    #[msg("Official warning with an invalid validity period")]
    InvalidPublicAlert,
    #[msg("Invalid rain outlook: issued outside the latest batch, window different from the rule or too few models")]
    InvalidOutlook,
    #[msg("Invalid vault: unknown trigger, zero amount or expiry in the past")]
    InvalidVault,
    #[msg("The record is not a trigger for this vault (type, subject, or rule not fired)")]
    TriggerMismatch,
    #[msg("The trigger was recorded before the vault started accepting triggers")]
    TriggerTooEarly,
    #[msg("Still inside the cooldown of the last payout")]
    CooldownActive,
    #[msg("Vault has no balance above the rent minimum")]
    VaultEmpty,
    #[msg("Vault expired: it no longer pays")]
    VaultExpired,
    #[msg("Vault still active: the creator only recovers the balance after expiry")]
    VaultActive,
    #[msg("The record is not in the accumulator (wrong proof, or peaks changed: rebuild the proof)")]
    NotInAccumulator,
    #[msg("Accumulator full")]
    AccumulatorFull,
}
