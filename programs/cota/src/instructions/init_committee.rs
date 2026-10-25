use anchor_lang::prelude::*;

use crate::{constants::*, error::CotaError, events::CommitteeInitialized, state::Committee};

#[derive(Accounts)]
pub struct InitCommittee<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = 8 + Committee::INIT_SPACE,
        seeds = [COMMITTEE_SEED],
        bump
    )]
    pub committee: Account<'info, Committee>,
    pub system_program: Program<'info, System>,
}

pub fn validate_members(members: &[Pubkey], threshold: u8) -> Result<()> {
    require!(
        !members.is_empty()
            && members.len() <= MAX_MEMBERS
            && threshold >= 1
            && threshold as usize <= members.len(),
        CotaError::InvalidCommittee
    );
    for (i, m) in members.iter().enumerate() {
        require!(!members[..i].contains(m), CotaError::InvalidCommittee);
    }
    Ok(())
}

pub fn handle_init_committee(ctx: Context<InitCommittee>, members: Vec<Pubkey>, threshold: u8) -> Result<()> {
    validate_members(&members, threshold)?;

    let committee = &mut ctx.accounts.committee;
    committee.authority = ctx.accounts.authority.key();
    committee.members = members.clone();
    committee.threshold = threshold;
    committee.next_batch_id = 1;
    committee.last_batch_root = [0; 32];
    committee.last_t_end = i64::MIN;
    committee.bump = ctx.bumps.committee;

    emit!(CommitteeInitialized { authority: committee.authority, members, threshold });
    Ok(())
}
