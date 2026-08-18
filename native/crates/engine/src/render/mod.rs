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

#[cfg(test)]
mod shader_tests {
    /// Every `.wgsl` we ship parses and validates.
    ///
    /// Shaders are the one part of the renderer no other test touches: they are
    /// compiled by `create_shader_module` at startup, so a typo is a panic on
    /// launch and a green suite says nothing about it. `naga` here is the same
    /// front-end wgpu runs, at the same major version, so passing this is passing
    /// the real check rather than a lookalike.
    #[test]
    fn every_shader_compiles() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/render/shaders");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).expect("shaders dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("wgsl") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read shader");
            let module = naga::front::wgsl::parse_str(&src)
                .unwrap_or_else(|e| panic!("{}: {}", path.display(), e.emit_to_string(&src)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
            checked += 1;
        }
        assert!(checked >= 10, "only {checked} shaders found — did the path move?");
    }
}
