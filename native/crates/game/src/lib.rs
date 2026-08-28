//! BUILD & HIDE — the game crate.
//!
//! Owns everything domain-specific: the authored [`world`] (CSG rooms + the
//! BUILD/HUNT loop + authoring tools), weapon [`combat`], the [`enemy`] hunter
//! and player [`character`] controllers, and the winit event loop in [`app`]
//! that maps input to game actions. All rendering, physics, CSG, nav, skinning,
//! and asset loading come from the `engine` crate (a one-way dependency).

pub mod app;
pub mod character;
pub mod combat;
pub mod ecs;
pub mod economy;
pub mod enemy;
pub mod gamepad;
pub mod hud;
pub mod levelgen;
pub mod pdsim;
pub mod props;
/// Crate-private: the middle-mouse authoring menu is a front-end over the app's own
/// actions, not part of the game crate's surface.
pub(crate) mod radial;
pub mod shop;
pub mod theme_editor;
pub mod theme_review;
pub mod turret;
pub mod world;

/// Launch the game: open the window and run the event loop.
pub fn run() {
    app::run();
}
