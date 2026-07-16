//! Simulation subsystem — the physical/spatial world the gameplay runs on.
//!
//! [`physics`] wraps Rapier (colliders + character controller + ray queries);
//! [`nav`] is the baked WT-cell nav grid + A*. The game-side controllers that
//! ride on these (player capsule, enemy pathfinder) live in the `game` crate.

pub mod nav;
pub mod physics;
