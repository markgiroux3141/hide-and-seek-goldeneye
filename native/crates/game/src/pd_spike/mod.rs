//! **SPIKE: Perfect Dark combat-simulator bots, replicated as literally as we can.**
//!
//! A standalone arena (`cargo run --release --bin pd_arena`) in which Perfect Dark
//! simulants — PD bodies, PD animations, PD's bot code — fight each other while you
//! watch with the debug overlays on. It shares nothing with the game's hunter AI on
//! purpose: that stack accumulated guard behaviour the decomp shows bots never had
//! (attack clips, hit stuns, 45° trigger cones, aim correction), and the point here
//! is to see what the real thing looks like before deciding what to keep.
//!
//! Ground rules for anything added here:
//!
//! * **Port functions, not behaviours.** Each PD function becomes a Rust function
//!   with the same name and a `file:line` citation, in PD's own units — world units
//!   are centimetres, time is 60 Hz ticks with PD's `lvupdate240` sub-tick counts —
//!   and only [`arena`] and the render code convert to metres.
//! * **Where we must substitute, say so at the call site.** PD's pad-graph routing,
//!   room system and collision tiles do not exist here; the stand-ins are marked.
//! * **The simulation never touches clip data.** It runs headless ([`sim`]); the
//!   window is only a viewer, so behaviour can be tested without a GPU.
//!
//! Module map:
//! * [`arena`] — the room, sight lines, raycasts, wall sliding.
//! * [`level_geom`] — the generic level description (floor/wall/blocker polygons,
//!   optional rooms) that PD stages and, later, editor levels are both converted to;
//!   [`pd_tiles`] — the PD adapter (collision tiles, pads, PD's waypoint graph, spawns);
//!   [`tile_level`] — PD's `lib/collision.c` primitives ported onto that geometry
//!   (wall volume/sweep tests, ground finding, sight and shot rays). The arena is
//!   converted to the same geometry ([`arena::Arena::geom`]), so both levels share it.
//! * [`anims`] — the PD animation ids a bot can play, with frame counts + loop flags.
//! * [`model`] — PD's `struct anim` engine and the pose evaluation / joint callback.
//! * [`thirdperson`] — `player_choose_third_person_animation`, the body animator.
//! * [`bot`], [`botcmd`], [`chraction`] — the brain and body, by PD source file.
//! * [`chr`] — `chrdata` / `aibot` state; [`sim`] — the match and PD frame timing.
//! * [`waypoints`] — stand-in pad graph; [`weapons`] — the guns' bot-facing stats.
//! * [`navgen`] — our waypoint graph, generated from any `LevelGeom` in PD's format;
//!   [`navcheck`] — the static checks S1-S4 that compare it with PD's;
//!   [`abtest`] — the dynamic checks D1-D4 (matches on both graphs, same seeds).
//! * [`viewer`] (+ [`camera`], [`debug_draw`], [`greybox`], [`view`]) — the window, read-only.

pub mod abtest;
pub mod anims;
pub mod arena;
pub mod bot;
pub mod botcmd;
pub mod camera;
pub mod chr;
pub mod chraction;
pub mod debug_draw;
pub mod greybox;
pub mod gunpos;
pub mod level_geom;
pub mod model;
pub mod navcheck;
pub mod navgen;
pub mod pd_nav;
pub mod pd_tiles;
pub mod pdmath;
pub mod root_y;
pub mod sim;
pub mod thirdperson;
pub mod tile_level;
pub mod view;
pub mod viewer;
pub mod walk;
pub mod waypoints;
pub mod weapons;

#[cfg(test)]
mod tests;

pub use viewer::run;
