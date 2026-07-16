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
pub mod enemy;
pub mod world;

/// Launch the game: open the window and run the event loop.
pub fn run() {
    app::run();
}
