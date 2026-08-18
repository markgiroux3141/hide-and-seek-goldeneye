//! Armed authoring tools — one submodule permodal tool. Each adds its
//! `impl World` methods (arm / query / preview / scroll / confirm / cancel).

mod gizmo;
mod light;
mod opening;
mod placement;
mod platform;
mod prop;
mod prop_gizmo;
// `pub(crate)` only so the respawn/scoreboard test modules can reuse its `place_pad` /
// `big_room` fixtures — a level with authored pads is the precondition for both.
pub(crate) mod spawn_point;
mod stairs;
