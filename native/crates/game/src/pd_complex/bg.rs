//! Complex's textured display geometry: `bg_ref.seg` decoded by
//! `tools/pd-assets/pd_bg.py` into the guns spike's model JSON (materials =
//! the interpreted N64 draw state, batches = the room display lists' triangles,
//! world-space positions, one matrix), so the guns' N64 renderer draws it the
//! way it draws a gun — two-cycle combiner, 3-point filtering, the vertex
//! colours as PD's baked lighting.
//!
//! Every room is drawn every frame: PD's portal culling (`bg_tick_portals`)
//! only decides what is on screen, not how it looks. Not applied yet: the room
//! brightness (`room_highlight`, `dlights.c:1627` — the exported colours are the
//! raw base colours, i.e. brightness 255), the ocean texture animation, and the
//! camera-dependent BSP order of the translucent blocks (they draw in export
//! order).

use std::path::PathBuf;
use std::sync::Arc;

use crate::pd_guns::data::ModelFile;
use crate::pd_guns::model::ModelDef;

/// The model name the BG is registered under in the guns' model table.
pub const BG_MODEL: &str = "bg_ref";

/// `native/assets/levels/pd_bg/ref/`.
pub fn bg_dir() -> PathBuf {
    PathBuf::from(format!("{}/../../assets/levels/pd_bg/ref", env!("CARGO_MANIFEST_DIR")))
}

/// The BG's triangles in world cm (the x-ray draws them).
pub fn triangles(def: &ModelDef) -> Vec<[glam::Vec3; 3]> {
    let mut out = Vec::new();
    for b in &def.batches {
        let p = |i: u32| {
            let v = b.verts[i as usize];
            glam::Vec3::new(v[0], v[1], v[2])
        };
        for t in b.indices.chunks_exact(3) {
            out.push([p(t[0]), p(t[1]), p(t[2])]);
        }
    }
    out
}

/// Load the exported BG as a model definition whose textures live beside it.
pub fn load() -> Result<Arc<ModelDef>, String> {
    let dir = bg_dir();
    let path = dir.join("bg.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} (run tools/pd-assets/pd_bg.py ref)", path.display()))?;
    let file: ModelFile = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut def = ModelDef::from_file(file);
    def.name = BG_MODEL.to_string();
    def.tex_dir = Some(dir.join("textures"));
    Ok(Arc::new(def))
}
