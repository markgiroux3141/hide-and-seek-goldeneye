//! Rendering subsystem — the wgpu pipeline and everything it consumes.
//!
//! [`renderer`] owns the GPU device, pipelines, and per-frame draw; [`mesh`] is
//! the CPU→GPU mesh bridge; [`textures`] the scheme registry + BMP assets;
//! [`uv_zones`] the post-CSG UV assignment; [`camera`] the view. Shaders live
//! under `render/shaders/` and are embedded via `include_str!` from `renderer`.

pub mod camera;
pub mod mesh;
pub mod renderer;
pub mod textures;
pub mod uv_zones;
