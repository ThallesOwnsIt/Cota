use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::CotaError,
    events::StationRegistered,
    state::{Committee, Station},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct StationArgs {
    pub station_id: String,
    pub name: String,
    pub source: u8,
    pub variable: u8,
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub datum_id: Option<String>,
}

#[derive(Accounts)]
#[instruction(args: StationArgs)]
pub struct RegisterStation<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        seeds = [COMMITTEE_SEED],
        bump = committee.bump,
        has_one = authority @ CotaError::Unauthorized
    )]
    pub committee: Account<'info, Committee>,
    #[account(
        init,
        payer = authority,
        space = 8 + Station::INIT_SPACE,
        seeds = [STATION_SEED, args.station_id.as_bytes()],
        bump
    )]
    pub station: Account<'info, Station>,
    pub system_program: Program<'info, System>,
}

pub fn handle_register_station(ctx: Context<RegisterStation>, args: StationArgs) -> Result<()> {
    require!(
        !args.station_id.is_empty()
            && args.station_id.len() <= MAX_STATION_ID_LEN
            && args.name.len() <= MAX_STATION_NAME_LEN
            && args.datum_id.as_ref().is_none_or(|d| d.len() <= MAX_DATUM_ID_LEN),
        CotaError::StringTooLong
    );

    let station = &mut ctx.accounts.station;
    station.station_id = args.station_id.clone();
    station.name = args.name;
    station.source = args.source;
    station.variable = args.variable;
    station.lat_e6 = args.lat_e6;
    station.lon_e6 = args.lon_e6;
    station.datum_id = args.datum_id.clone();
    station.registered_at = Clock::get()?.unix_timestamp;
    station.bump = ctx.bumps.station;

    emit!(StationRegistered {
        station_id: args.station_id,
        source: args.source,
        variable: args.variable,
        lat_e6: args.lat_e6,
        lon_e6: args.lon_e6,
        datum_id: args.datum_id,
    });
    Ok(())
}
