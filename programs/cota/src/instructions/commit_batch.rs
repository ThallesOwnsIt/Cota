use anchor_lang::prelude::*;
use solana_instructions_sysvar::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::{
    constants::*,
    ed25519,
    error::CotaError,
    events::{BatchCommitted, OracleAbsent},
    state::{BatchCommitment, BatchHeader, Committee},
};

/// Ancora um lote assinado pelo comitê. Qualquer conta pode submeter (paga as taxas);
/// a autorização vem das assinaturas Ed25519 verificadas na instrução anterior.
#[derive(Accounts)]
#[instruction(header: BatchHeader)]
pub struct CommitBatch<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(mut, seeds = [COMMITTEE_SEED], bump = committee.bump)]
    pub committee: Account<'info, Committee>,
    #[account(
        init,
        payer = submitter,
        space = 8 + BatchCommitment::INIT_SPACE,
        seeds = [BATCH_SEED, &header.batch_id.to_le_bytes()],
        bump
    )]
    pub batch: Account<'info, BatchCommitment>,
    /// CHECK: endereço conferido contra o ID do sysvar Instructions.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_commit_batch(ctx: Context<CommitBatch>, header: BatchHeader) -> Result<()> {
    let committee = &ctx.accounts.committee;

    // 1. Sequência e hash-chain: impede pular, repetir ou inserir lotes no passado.
    require!(header.batch_id == committee.next_batch_id, CotaError::BatchOutOfSequence);
    require!(header.prev_batch_root == committee.last_batch_root, CotaError::BrokenHashChain);
    require!(
        header.t_end > header.t_start && header.t_start >= committee.last_t_end,
        CotaError::InvalidWindow
    );

    // 2. Quem assinou exatamente este cabeçalho (precompile na instrução anterior) e se
    //    são membros distintos em número suficiente.
    let (signers, signer_count) = ed25519::committee_signers(
        &ctx.accounts.instructions.to_account_info(),
        &committee.members,
        committee.threshold,
        &header.signing_message(),
    )?;

    // 3. Grava o recibo e avança o comitê.
    let clock = Clock::get()?;
    let batch = &mut ctx.accounts.batch;
    batch.batch_id = header.batch_id;
    batch.t_start = header.t_start;
    batch.t_end = header.t_end;
    batch.leaf_count = header.leaf_count;
    batch.merkle_root = header.merkle_root;
    batch.station_set_hash = header.station_set_hash;
    batch.prev_batch_root = header.prev_batch_root;
    batch.signers = signers;
    batch.signer_count = signer_count;
    batch.slot = clock.slot;
    batch.committed_at = clock.unix_timestamp;
    batch.submitter = ctx.accounts.submitter.key();
    batch.bump = ctx.bumps.batch;

    let all_members: u8 = ((1u16 << committee.members.len()) - 1) as u8;
    let committee = &mut ctx.accounts.committee;
    committee.next_batch_id += 1;
    committee.last_batch_root = header.merkle_root;
    committee.last_t_end = header.t_end;

    emit!(BatchCommitted {
        batch_id: header.batch_id,
        merkle_root: header.merkle_root,
        leaf_count: header.leaf_count,
        t_start: header.t_start,
        t_end: header.t_end,
        signers,
        signer_count,
        slot: clock.slot,
    });
    if signers != all_members {
        emit!(OracleAbsent { batch_id: header.batch_id, missing: all_members & !signers });
    }
    Ok(())
}
