//! Bake **handless** copies of the GoldenEye weapon GLBs.
//!
//! The `native/assets/weapons/<w>/gun.glb` files are ripped **first-person
//! viewmodels**: the gun *plus Bond's hand*, authored as ~8 separate sub-meshes
//! (one glTF node → one mesh → one texture each). That hand is correct in the
//! player's first-person view, but wrong on an enemy — the whole GLB is parented
//! to a hunter's hand bone, so Bond's hand floats beside the hunter's own hand
//! (see the `enemy-weapon-hand-artifact` recon).
//!
//! There is no semantic "hand" node or material — materials are named `m4`…`m11`
//! by texture index. The hand is identified by its **base-color texture image**:
//! six consecutive skin textures `tempImgEd0701`..`tempImgEd0706` (visibly a
//! hand — fingers, knuckles — average colour ~`[100,88,68]`, a brown flesh tone).
//! This set appears in exactly the pistols (pp7/dd44/magnum/gold/silver/golden/
//! pp7-silencer) and the detonator, and is ABSENT from every rifle/shotgun/laser/
//! mine — whose brown textures are wooden furniture (foregrips, stocks), NOT skin.
//! Filtering by this exact image set therefore strips the hand everywhere it
//! exists while leaving wood grips, revolver cylinders, env-map bodies and gun
//! metal untouched.
//!
//! **Strip mechanic:** [`engine::assets::textured_model::load`] renders geometry
//! by walking the scene graph (`scene → node.children() → node.mesh()`). So a
//! hand node is neutralised by (a) removing its index from every parent's
//! `children` array (and any scene root list) — making it unreachable in the
//! walk — and (b) deleting its `mesh` reference as a belt-and-suspenders. Node
//! indices are never renumbered (only *entries* are dropped from `children`
//! lists), so accessors/meshes/buffer bytes stay valid and the BIN chunk is
//! copied verbatim. Same GLB JSON-chunk surgery the skeletal clip loader uses to
//! patch `9729 → "LINEAR"` (see `engine::skeletal::clip::patch_interpolation`).
//!
//! The library [`strip_hand`] is used by the `strip_hands` bin (which writes the
//! `gun_handless.glb` files) and by tests; the enemy weapon library loads the
//! handless variant with a fallback to `gun.glb` (see `world::World::new`).

use std::collections::HashSet;

const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"

/// The six base-color texture images that make up Bond's first-person hand skin.
/// A node whose mesh samples any of these is the hand (not the gun). See the
/// module docs for how this set was identified and verified.
pub const HAND_SKIN_IMAGES: &[&str] = &[
    "tempImgEd0701",
    "tempImgEd0702",
    "tempImgEd0703",
    "tempImgEd0704",
    "tempImgEd0705",
    "tempImgEd0706",
];

/// Whether a base-color image name belongs to the Bond hand skin.
pub fn is_hand_image(name: &str) -> bool {
    HAND_SKIN_IMAGES.contains(&name)
}

/// Resolve which weapon GLB an **enemy** should load, given a weapon's player
/// `gun_path` (e.g. `"pp7/gun.glb"`): the handless variant (`gun_handless.glb`)
/// when one exists, else the original `gun.glb`. `exists(rel)` reports whether a
/// resolved relative asset path is present on disk. Only the pistols + detonator
/// ship a handless variant; every other weapon has no baked-in hand, so this
/// gracefully falls back. The player's first-person viewmodel never calls this —
/// it keeps loading `gun.glb` (the hand is wanted there).
pub fn enemy_gun_path(gun_path: &str, exists: impl Fn(&str) -> bool) -> String {
    let handless = gun_path.replace("gun.glb", "gun_handless.glb");
    if handless != gun_path && exists(&handless) {
        handless
    } else {
        gun_path.to_string()
    }
}

/// The result of stripping a weapon GLB: the rewritten bytes plus how many scene
/// nodes (hand sub-meshes) were removed. `removed == 0` means the GLB had no hand
/// geometry (a rifle/shotgun/etc.), so no handless variant is needed.
pub struct StripResult {
    pub bytes: Vec<u8>,
    pub removed: usize,
}

/// Strip Bond's hand from a weapon GLB using the built-in [`HAND_SKIN_IMAGES`]
/// filter. See [`strip_nodes_by_image`] for the mechanics.
pub fn strip_hand(glb: &[u8]) -> Result<StripResult, String> {
    strip_nodes_by_image(glb, is_hand_image)
}

/// Remove every scene node whose mesh references a base-color texture image for
/// which `is_hand(name)` returns true. Node indices are preserved; only entries
/// in `children`/scene-`nodes` lists are dropped and the hand nodes' `mesh`
/// references cleared. The BIN chunk is copied verbatim.
pub fn strip_nodes_by_image(
    glb: &[u8],
    is_hand: impl Fn(&str) -> bool,
) -> Result<StripResult, String> {
    let mut chunks = split_chunks(glb)?;
    let json_idx = chunks
        .iter()
        .position(|(t, _)| *t == CHUNK_JSON)
        .ok_or("GLB has no JSON chunk")?;

    let mut json: serde_json::Value = serde_json::from_slice(&chunks[json_idx].1)
        .map_err(|e| format!("GLB JSON parse: {e}"))?;

    // ── Resolve, per node, the base-color image names its mesh samples, so we can
    // flag hand nodes. Read everything up front (immutable) to avoid aliasing the
    // `Value` while we mutate it below.
    let hand_nodes = find_hand_nodes(&json, &is_hand);
    let removed = hand_nodes.len();

    if removed > 0 {
        // (a) Drop hand indices from every node's `children` list.
        if let Some(nodes) = json.get_mut("nodes").and_then(|n| n.as_array_mut()) {
            for node in nodes.iter_mut() {
                if let Some(children) = node.get_mut("children").and_then(|c| c.as_array_mut()) {
                    children.retain(|c| !c.as_u64().is_some_and(|i| hand_nodes.contains(&(i as usize))));
                }
            }
        }
        // (b) Drop hand indices from every scene's root `nodes` list (defensive —
        // the weapon hand is always a child of the "Weapon" root, not a root).
        if let Some(scenes) = json.get_mut("scenes").and_then(|s| s.as_array_mut()) {
            for scene in scenes.iter_mut() {
                if let Some(roots) = scene.get_mut("nodes").and_then(|n| n.as_array_mut()) {
                    roots.retain(|c| !c.as_u64().is_some_and(|i| hand_nodes.contains(&(i as usize))));
                }
            }
        }
        // (c) Clear the `mesh` reference on each hand node so it draws nothing even
        // if some tool reaches it by another path.
        if let Some(nodes) = json.get_mut("nodes").and_then(|n| n.as_array_mut()) {
            for &i in &hand_nodes {
                if let Some(node) = nodes.get_mut(i).and_then(|n| n.as_object_mut()) {
                    node.remove("mesh");
                }
            }
        }
    }

    let mut new_json =
        serde_json::to_vec(&json).map_err(|e| format!("GLB JSON reserialize: {e}"))?;
    // glTF chunks are 4-byte aligned; the JSON chunk pads with spaces (0x20).
    while new_json.len() % 4 != 0 {
        new_json.push(b' ');
    }
    chunks[json_idx].1 = new_json;

    Ok(StripResult {
        bytes: reassemble(&chunks),
        removed,
    })
}

/// Indices of scene nodes whose mesh samples a hand base-color image.
fn find_hand_nodes(json: &serde_json::Value, is_hand: &impl Fn(&str) -> bool) -> HashSet<usize> {
    let nodes = json.get("nodes").and_then(|n| n.as_array());
    let meshes = json.get("meshes").and_then(|m| m.as_array());
    let (Some(nodes), Some(meshes)) = (nodes, meshes) else {
        return HashSet::new();
    };

    let mut out = HashSet::new();
    for (i, node) in nodes.iter().enumerate() {
        let Some(mesh_idx) = node.get("mesh").and_then(|m| m.as_u64()) else {
            continue;
        };
        let Some(mesh) = meshes.get(mesh_idx as usize) else {
            continue;
        };
        let Some(prims) = mesh.get("primitives").and_then(|p| p.as_array()) else {
            continue;
        };
        let is_hand_node = prims.iter().any(|prim| {
            prim.get("material")
                .and_then(|m| m.as_u64())
                .and_then(|mat_idx| base_color_image_name(json, mat_idx as usize))
                .is_some_and(|name| is_hand(name))
        });
        if is_hand_node {
            out.insert(i);
        }
    }
    out
}

/// The `images[].name` of a material's base-color texture, if any.
fn base_color_image_name(json: &serde_json::Value, mat_idx: usize) -> Option<&str> {
    let tex_idx = json
        .get("materials")?
        .as_array()?
        .get(mat_idx)?
        .get("pbrMetallicRoughness")?
        .get("baseColorTexture")?
        .get("index")?
        .as_u64()?;
    let img_idx = json
        .get("textures")?
        .as_array()?
        .get(tex_idx as usize)?
        .get("source")?
        .as_u64()?;
    json.get("images")?
        .as_array()?
        .get(img_idx as usize)?
        .get("name")?
        .as_str()
}

/// Split a GLB into its `(chunk_type, chunk_data)` pairs (header validated).
fn split_chunks(bytes: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, String> {
    if bytes.len() < 12 {
        return Err("GLB too short".into());
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != GLB_MAGIC {
        return Err("not a GLB (bad magic)".into());
    }
    let mut chunks = Vec::new();
    let mut off = 12;
    while off + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let ctype = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
        let start = off + 8;
        let end = start + len;
        if end > bytes.len() {
            return Err("GLB chunk overruns file".into());
        }
        chunks.push((ctype, bytes[start..end].to_vec()));
        off = end;
    }
    Ok(chunks)
}

/// Reassemble a GLB from `(chunk_type, chunk_data)` pairs (12-byte header +
/// `[len][type][data]` per chunk). Chunk data must already be 4-byte aligned.
fn reassemble(chunks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let body_len: usize = chunks.iter().map(|(_, d)| 8 + d.len()).sum();
    let total = 12 + body_len;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes()); // glTF version 2
    out.extend_from_slice(&(total as u32).to_le_bytes());
    for (ctype, data) in chunks {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctype.to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn gun_path(w: &str) -> String {
        format!("{}/../../assets/weapons/{}/gun.glb", env!("CARGO_MANIFEST_DIR"), w)
    }

    /// Collect the base-color image names of every primitive the engine's static
    /// loader would actually render — i.e. reachable by the same scene-graph walk
    /// (`scene → children → mesh`) `textured_model::load` uses. This is the
    /// ground truth for "does the hand still load?".
    fn rendered_image_names(glb: &[u8]) -> Vec<String> {
        let (doc, _buffers, _images) =
            gltf::import_slice(glb).expect("stripped GLB is valid glTF");
        let scene = doc.default_scene().or_else(|| doc.scenes().next()).unwrap();
        let mut out = Vec::new();
        fn walk(node: gltf::Node, doc_out: &mut Vec<String>) {
            if let Some(mesh) = node.mesh() {
                for prim in mesh.primitives() {
                    if let Some(info) = prim.material().pbr_metallic_roughness().base_color_texture() {
                        if let Some(name) = info.texture().source().name() {
                            doc_out.push(name.to_string());
                        }
                    }
                }
            }
            for child in node.children() {
                walk(child, doc_out);
            }
        }
        for node in scene.nodes() {
            walk(node, &mut out);
        }
        out
    }

    /// Stripping the PP7 removes exactly the six hand sub-meshes; the loader then
    /// renders no hand image, but still renders the gun's own textures.
    #[test]
    fn strips_pp7_hand_keeps_gun() {
        let bytes = std::fs::read(gun_path("pp7")).expect("read pp7 gun.glb");
        let before = rendered_image_names(&bytes);
        assert!(
            before.iter().any(|n| is_hand_image(n)),
            "pp7 gun.glb should contain the hand before stripping"
        );

        let res = strip_hand(&bytes).expect("strip pp7");
        assert_eq!(res.removed, 6, "pp7 has six hand sub-meshes");

        let after = rendered_image_names(&res.bytes);
        assert!(
            !after.iter().any(|n| is_hand_image(n)),
            "no hand image is rendered after stripping (got {after:?})"
        );
        // The gun's own textures survive — pp7's slide (001A) and black grip (0648).
        let after_set: HashSet<&str> = after.iter().map(|s| s.as_str()).collect();
        assert!(after_set.contains("tempImgEd001A"), "pp7 slide texture kept");
        assert!(after_set.contains("tempImgEd0648"), "pp7 grip texture kept");
        assert!(!after.is_empty(), "gun geometry survives the strip");
    }

    /// The env-mapped gold gun keeps its reflective body mesh (the `EnvMapping`
    /// material's texture `tempImgEd0295`) — we don't strip the whole gun when
    /// most sub-meshes are hand.
    #[test]
    fn env_mapped_gun_keeps_body() {
        let bytes = std::fs::read(gun_path("golden-gun")).expect("read golden-gun");
        let res = strip_hand(&bytes).expect("strip golden-gun");
        assert_eq!(res.removed, 6, "golden-gun has six hand sub-meshes");
        let after = rendered_image_names(&res.bytes);
        assert!(after.iter().any(|n| n == "tempImgEd0295"), "gold body kept");
        assert!(!after.iter().any(|n| is_hand_image(n)), "hand gone");
    }

    /// The enemy-path resolver prefers a handless variant when present and falls
    /// back to `gun.glb` otherwise — and every real weapon resolves to a file that
    /// actually exists (so the enemy weapon library never silently drops a gun).
    #[test]
    fn enemy_gun_path_prefers_handless_then_falls_back() {
        assert_eq!(enemy_gun_path("pp7/gun.glb", |_| true), "pp7/gun_handless.glb");
        assert_eq!(enemy_gun_path("kf7/gun.glb", |_| false), "kf7/gun.glb");

        let asset =
            |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
        let exists = |rel: &str| std::path::Path::new(&asset(rel)).exists();
        // The pistols + detonator must resolve to their (existing) handless file.
        for w in ["pp7", "dd44", "magnum", "gold-pp7", "golden-gun", "silver-pp7", "pp7-silencer", "detonator"] {
            let p = enemy_gun_path(&format!("{w}/gun.glb"), exists);
            assert_eq!(p, format!("{w}/gun_handless.glb"), "{w} → handless");
            assert!(exists(&p), "{w} handless file exists");
        }
        // Every configured weapon resolves to a real on-disk asset.
        for cfg in crate::combat::config::WEAPONS {
            let p = enemy_gun_path(cfg.gun_path, exists);
            assert!(exists(&p), "weapon '{}' resolves to a real file ({p})", cfg.name);
        }
    }

    /// A rifle carries no Bond hand (its brown textures are wooden furniture), so
    /// stripping is a no-op — `removed == 0`, signalling "no handless variant
    /// needed" so the enemy loader falls back to `gun.glb`.
    #[test]
    fn rifle_has_no_hand_to_strip() {
        for w in ["ar33", "kf7", "rcp-90", "shotgun"] {
            let bytes = std::fs::read(gun_path(w)).unwrap_or_else(|e| panic!("read {w}: {e}"));
            let res = strip_hand(&bytes).unwrap_or_else(|e| panic!("strip {w}: {e}"));
            assert_eq!(res.removed, 0, "{w} has no hand sub-mesh");
            // Geometry is unchanged and still loads.
            assert!(!rendered_image_names(&res.bytes).is_empty(), "{w} still has geometry");
        }
    }
}
