//! **SPIKE: Perfect Dark's first-person guns and controls, replicated literally.**
//!
//! A standalone firing range (`cargo run --release --bin pd_range`) where the
//! player's weapons are Perfect Dark's own: the articulated first-person gun
//! models with Joanna's hands, posed by PD's gun animations and driven by PD's
//! `guncmd` scripts, rendered through a port of the N64 display-list state
//! (colour combiner, blender, texgen chrome), positioned by `bondgun.c`'s sway /
//! recoil / crosshair-swivel maths, and fired through PD's hand state machine.
//! Sister spike to [`crate::pd_spike`] (the simulants); shares only its RNG and
//! angle macros ([`crate::pd_spike::pdmath`]).
//!
//! Ground rules (same as the simulant spike):
//!
//! * **Port functions, not behaviours.** Each PD function becomes a Rust function
//!   with the same name and a `file:line` citation, in PD's units — world
//!   centimetres, 60 Hz ticks with `lvupdate240` sub-ticks, gun model millimetres.
//! * **Where we must substitute, say so at the call site.** PD's room/portal
//!   collision, prop hit tests and lighting do not exist here.
//! * **The simulation never touches the GPU.** Everything under [`bgun`] and
//!   [`player`] runs headless; the window only renders and feeds input.
//!
//! Data comes from `tools/pd-assets/pd_fpgun.py all` → `native/assets/weapons/pd_fp/`.
//!
//! Module map:
//! * [`data`] — the exported JSON, typed.
//! * [`animdata`] — `lib/anim.c`'s decoder over the raw animation bytes.
//! * [`anim`] — `struct anim` + `model_set_animation` / `model_tick_anim` (+ the
//!   CHRINFO root motion the head-bob model needs).
//! * [`model`] — model instances, toggles, `model_set_matrices_with_anim`.
//! * [`pdmtx`] — PD's matrix helpers.
//! * [`gset`] — `g_Weapons`, the funcdefs and the `guncmd` scripts, typed.
//! * [`bgun`], [`bgun_state`], [`bgun_pose`] — `bondgun.c`: the hand state
//!   machine, switching/reload/ammo, and the per-frame gun pose.
//! * [`player`] — `bondmove.c` / `bondwalk.c` / `bondhead.c`: look, aim, walk, head bob.
//! * [`range`] — the stand-in world (boxes, raycast, slide collision).
//! * [`fx`] — beams, sparks, wallhits, casings.
//! * [`smoke`], [`explosions`] — `smoke.c` / `explosions.c`.
//! * [`props`], [`throw`], [`nbomb`], [`autogun`] — the weapon objects the guns
//!   spawn (`propobj.c`), the throw/fire makers (`bondgun.c`), the N-Bomb storm.
//! * [`xray`] — the Farsight's x-ray: the eraser and its BG / prop colours.
//! * [`font`], [`hud`] — PD's ROM fonts and text renderers; `bgun_draw_hud`.
//! * [`sim`] — PD's frame order and the shot path (`hands_tick_attack`), plus
//!   the combat boost and the RC-P120 cloak.
//! * [`render`] + `pdgun.wgsl` / `pdfx.wgsl` / `pdpost.wgsl` — the N64
//!   display-list state on wgpu, and PD's framebuffer effects.
//! * [`app`] — the `pd_range` window; [`snapshot`] — headless offscreen PNGs.

pub mod anim;
pub mod app;
pub mod animdata;
pub mod autogun;
pub mod bgun;
pub mod bgun_pose;
pub mod bgun_state;
pub mod data;
pub mod explosions;
pub mod font;
pub mod fx;
pub mod gset;
pub mod hud;
pub mod model;
pub mod nbomb;
pub mod pdmtx;
pub mod player;
pub mod props;
pub mod range;
pub mod render;
pub mod sim;
pub mod smoke;
pub mod snapshot;
pub mod throw;
pub mod xray;

#[cfg(test)]
mod tests;
