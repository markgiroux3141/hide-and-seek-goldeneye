//! **SPIKE: you against Perfect Dark's simulants, on Complex.**
//!
//! The third spike, joining the other two rather than growing either of them:
//! [`crate::pd_guns`] (Joanna's first-person guns, movement and HUD) supplies the
//! player, and [`crate::pd_spike`] (the Combat Simulator bots) supplies the
//! enemies. Both stay runnable on their own (`pd_range`, `pd_arena`); this module
//! owns only the glue a real match needs between them:
//!
//! * one world: Complex's collision tiles drive both the bots' movement and the
//!   player's (the ported `bondwalk.c`), and the guns' shots, projectiles and
//!   explosions ([`crate::pd_guns::range::Range::for_stage`]);
//! * the player as a chr in the bot match ([`crate::pd_spike::chr::Chr::player`]):
//!   bots see, target, collide with and shoot it;
//! * hits both ways: the player's weapons report [`crate::pd_guns::sim::ChrHit`]s,
//!   the bots' shots report [`crate::pd_spike::sim::PlayerHit`]s, and [`fight`]
//!   applies each side's half of PD's `chr_damage`;
//! * death and respawn for the player (`player_die`, `player_choose_spawn_location`).
//!
//! Ground rules are the other two spikes': port functions (with `file:line`),
//! PD's units, and say so at the call site when substituting.
//!
//! Module map:
//! * [`fight`] — the headless match: frame order, syncing, damage, death.
//! * [`health`] — the damage flash, PD's health bar and the death fades.
//! * [`bg`] — Complex's textured display geometry (`bg_ref.seg`, exported by
//!   `tools/pd-assets/pd_bg.py`) as a PD model the guns' renderer draws.
//! * [`app`] — the `pd_complex` window; [`snapshot`] — offscreen PNGs of the
//!   player's view (`pd_complex_snapshot`).

pub mod app;
pub mod bg;
pub mod fight;
pub mod health;
pub mod snapshot;

#[cfg(test)]
mod tests;
