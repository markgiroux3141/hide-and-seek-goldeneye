//! BUILD & HIDE engine — the runtime the game crate drives.
//!
//! Organized by subsystem rather than a flat file list:
//! - [`platform`] — winit window helper + input state + frame clock.
//! - [`render`]   — wgpu pipelines, meshes, textures, UV zones, camera, shaders.
//! - [`geometry`] — runtime CSG, platform/stair authoring, shared math.
//! - [`sim`]      — Rapier physics + nav grid / A*.
//! - [`assets`]   — glTF/GLB loading (skinned loading lives in [`skeletal`]).
//! - [`skeletal`] — shared-skeleton skinned character animation.
//!
//! Game-specific code (the authored `world`, weapon combat, enemy/player
//! controllers, and the winit event loop) lives in the `game` crate, which
//! consumes this engine as a library — the dependency is one-way.

pub mod assets;
pub mod geometry;
pub mod platform;
pub mod render;
pub mod sim;
pub mod skeletal;
