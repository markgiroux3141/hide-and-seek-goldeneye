//! Armed authoring tools — one submodule permodal tool. Each adds its
//! `impl World` methods (arm / query / preview / scroll / confirm / cancel).

mod draw;
/// `pub(crate)` so the ECS door system can reach the shared panel-transform helpers —
/// the draw list, the collider pose and the tick all go through the same matrix.
pub(crate) mod door;
mod gizmo;
mod light;
mod opening;
/// `pub(crate)` so the prop draw list + the panel can reach the pickup helpers, and
/// so the pickup tests' fixtures are shared like `spawn_point`'s.
pub(crate) mod pickup;
mod placement;
mod platform;
mod prop;
mod prop_gizmo;
// `pub(crate)` only so the respawn/scoreboard test modules can reuse its `place_pad` /
// `big_room` fixtures — a level with authored pads is the precondition for both.
pub(crate) mod spawn_point;
mod stairs;
mod turret;
