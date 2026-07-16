//! BUILD & HIDE engine — domain-agnostic runtime.
//!
//! Modules land as the game needs them (no speculative API up front). Phase 0
//! stands up windowing + renderer, glTF loading, and the Rapier link; later
//! phases add the CSG runtime subsystem, character controller, and nav.

pub mod app;
pub mod camera;
pub mod character;
pub mod csg_runtime;
pub mod enemy;
pub mod geom;
pub mod gltf_load;
pub mod input;
pub mod mesh;
pub mod nav;
pub mod physics;
pub mod renderer;
pub mod structures;
pub mod textures;
pub mod uv_zones;
pub mod world;
