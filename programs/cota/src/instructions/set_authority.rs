use anchor_lang::prelude::*;

use crate::{constants::*, error::CotaError, events::AuthorityChanged, state::Committee};

/// Hands the committee authority to another key (e.g. an automated operator), without
/// touching the program's upgrade authority, which stays with whoever deployed it.
#[derive(Accounts)]
pub struct SetAuthority<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [COMMITTEE_SEED],
        bump = committee.bump,
        has_one = authority @ CotaError::Unauthorized
    )]
    pub committee: Account<'info, Committee>,
}

pub fn handle_set_authority(ctx: Context<SetAuthority>, new_authority: Pubkey) -> Result<()> {
    let committee = &mut ctx.accounts.committee;
    let old = committee.authority;
    committee.authority = new_authority;
    emit!(AuthorityChanged { old, new: new_authority });
    Ok(())
}
