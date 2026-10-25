use anchor_lang::prelude::*;

use crate::{constants::*, error::CotaError, events::MemberRotated, state::Committee};

/// Troca a chave de um nó (chave vazada ou operador substituído). A posição no
/// bitmap de assinantes é mantida.
#[derive(Accounts)]
pub struct RotateMember<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [COMMITTEE_SEED],
        bump = committee.bump,
        has_one = authority @ CotaError::Unauthorized
    )]
    pub committee: Account<'info, Committee>,
}

pub fn handle_rotate_member(ctx: Context<RotateMember>, old: Pubkey, new: Pubkey) -> Result<()> {
    let committee = &mut ctx.accounts.committee;
    let index = committee
        .members
        .iter()
        .position(|m| *m == old)
        .ok_or(CotaError::MemberNotFound)?;
    require!(!committee.members.contains(&new), CotaError::InvalidCommittee);

    committee.members[index] = new;
    emit!(MemberRotated { index: index as u8, old, new });
    Ok(())
}
