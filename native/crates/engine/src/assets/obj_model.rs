//! Wavefront **OBJ + MTL** loader producing a [`TexturedModel`] — the same asset the
//! GLB loader ([`super::textured_model`]) yields, so an OBJ prop flows through the
//! identical prop render path with no special-casing downstream.
//!
//! This is the "drop a raw model in and integrate it" path: the GoldenEye Setup
//! Editor exports objects as OBJ + MTL + BMP, carrying the surface colour in the
//! material's diffuse factor (`Kd`) exactly like the GLBs carry it in glTF
//! `baseColorFactor`. We fold `Kd` into the per-vertex colour (the prop shader does
//! `texel × colour × tint`), decode the BMP `map_Kd` textures, and normalise the
//! GoldenEye units to metres. A single OBJ can hold both the opaque and the
//! alpha-cutout geometry (the editor's `g primary` / `g secondary` groups) in one
//! file, so unlike the split GLBs there is nothing to merge.
//!
//! Deliberately small: it handles the subset the editor emits — `v`, `vt`, `f`
//! (triangulated `v/vt` corners), `usemtl`, `mtllib`; and `Kd` / `map_Kd` in the
//! MTL. Normals are synthesised flat (the prop shader ignores them). Material-name
//! render hints (`EnvMapping`, `CullBoth`, `ClampS/T`) are ignored — the prop pipeline
//! is already unlit, cull-none, and repeat-wrapped.

use std::collections::HashMap;
use std::path::Path;

use glam::Vec3;

use super::textured_model::{TexturedModel, TexturedPrimitive};
use crate::render::mesh::TexVertex;
use crate::skeletal::gltf_skin::TexImage;

/// GoldenEye-units → metres. The editor exports at ~1000× scale (a prop spans
/// hundreds of units); this brings a typical object into a ~1 m range, matching the
/// metre-scale GLB props. Per-prop fine-tuning still rides the catalog `scale`.
const OBJ_IMPORT_SCALE: f32 = 0.001;

/// One parsed MTL material: its diffuse colour (folded into vertex colour) and the
/// base-colour texture filename, if any.
struct Material {
    kd: [f32; 4],
    map_kd: Option<String>,
}

/// A parsed OBJ before it becomes one or more [`TexturedModel`]s: the raw arrays plus
/// the decoded textures, shared by [`load_obj`] and [`load_obj_components`].
struct ParsedObj {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    /// `(material, [(v_idx, vt_idx); 3])` per triangle, in file order.
    faces: Vec<(String, [(usize, usize); 3])>,
    materials: HashMap<String, Material>,
    images: Vec<TexImage>,
    /// Texture filename → index into `images`.
    tex_index: HashMap<String, usize>,
}

/// Load a Wavefront OBJ (with its sibling MTL + textures) into a [`TexturedModel`],
/// normalised to metres. Textures + the MTL are resolved relative to the OBJ's own
/// directory. Errors only if the OBJ can't be read or yields no geometry; a missing
/// texture degrades to an untextured (white-fallback) primitive rather than failing.
pub fn load_obj(path: &str) -> Result<TexturedModel, String> {
    let parsed = parse_obj(path)?;
    let all: Vec<usize> = (0..parsed.faces.len()).collect();
    build_model(&parsed, &all).ok_or_else(|| format!("{path}: no drawable geometry"))
}

/// Load an OBJ split into its **connected components** — one [`TexturedModel`] per
/// island of triangles that share vertex positions — instead of one merged model.
///
/// Some GoldenEye Setup Editor exports are not assembled objects at all but *parts
/// sheets*: the editor writes each sub-part at its own local origin and leaves the
/// assembly to the game, so the file holds several disjoint pieces sitting apart in
/// model space. The sentry gun is one (six pieces on three shelves). Recovering the
/// pieces is what lets a static export become an articulated prop, since each piece
/// can then be posed by its own matrix.
///
/// Components come back in a **deterministic** order — most triangles first, ties
/// broken by earliest face in the file — because callers index into this list to say
/// which piece is which. Each model carries only the textures its own faces use.
pub fn load_obj_components(path: &str) -> Result<Vec<TexturedModel>, String> {
    let parsed = parse_obj(path)?;
    let groups = connected_components(&parsed);
    let out: Vec<TexturedModel> = groups
        .iter()
        .filter_map(|g| build_model(&parsed, g))
        .collect();
    if out.is_empty() {
        return Err(format!("{path}: no drawable geometry"));
    }
    Ok(out)
}

/// Partition the parsed faces into connected components, joined through **welded**
/// vertex positions: the editor duplicates a vertex per face corner, so identity of
/// index means nothing and identity of position means everything. Returns face-index
/// lists ordered largest-first, ties by earliest face — see [`load_obj_components`].
fn connected_components(p: &ParsedObj) -> Vec<Vec<usize>> {
    // Weld positions to a canonical id. Quantised to 0.1 mm, well under the coarsest
    // GoldenEye vertex spacing and well over float32 noise in the ASCII round-trip.
    const WELD: f32 = 1e-4;
    let mut canon: HashMap<[i64; 3], usize> = HashMap::new();
    let mut vid = Vec::with_capacity(p.positions.len());
    for q in &p.positions {
        let key = [
            (q[0] / WELD).round() as i64,
            (q[1] / WELD).round() as i64,
            (q[2] / WELD).round() as i64,
        ];
        let next = canon.len();
        vid.push(*canon.entry(key).or_insert(next));
    }

    // Union-find over canonical vertices.
    let mut parent: Vec<usize> = (0..canon.len()).collect();
    fn find(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    for (_mat, corners) in &p.faces {
        let ids: Vec<usize> = corners.iter().map(|&(v, _)| vid[v]).collect();
        for w in ids.windows(2) {
            let (ra, rb) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }

    // Bucket faces by root, remembering each bucket's first face for the tie-break.
    let mut order: Vec<usize> = Vec::new(); // root → bucket, in first-seen order
    let mut of_root: HashMap<usize, usize> = HashMap::new();
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    for (fi, (_mat, corners)) in p.faces.iter().enumerate() {
        let root = find(&mut parent, vid[corners[0].0]);
        let b = *of_root.entry(root).or_insert_with(|| {
            order.push(root);
            buckets.push(Vec::new());
            buckets.len() - 1
        });
        buckets[b].push(fi);
    }
    // Largest first; equal sizes keep file order, which is what `order` already is.
    let mut idx: Vec<usize> = (0..buckets.len()).collect();
    idx.sort_by(|&a, &b| buckets[b].len().cmp(&buckets[a].len()).then(a.cmp(&b)));
    idx.into_iter().map(|i| std::mem::take(&mut buckets[i])).collect()
}

/// Read + decode an OBJ, its MTL and its textures into the shared [`ParsedObj`].
fn parse_obj(path: &str) -> Result<ParsedObj, String> {
    let dir = Path::new(path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let text = std::fs::read_to_string(path).map_err(|e| format!("obj read {path}: {e}"))?;

    // Pass 1: gather positions, uvs, the mtllib name, and the face records (each face
    // carries the material active when it was declared).
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut mtllib: Option<String> = None;
    let mut cur_mat = String::new();
    // (material, [(v_idx, vt_idx); 3]) per triangle.
    let mut faces: Vec<(String, [(usize, usize); 3])> = Vec::new();

    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let p: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if p.len() == 3 {
                    positions.push([
                        p[0] * OBJ_IMPORT_SCALE,
                        p[1] * OBJ_IMPORT_SCALE,
                        p[2] * OBJ_IMPORT_SCALE,
                    ]);
                }
            }
            Some("vt") => {
                let p: Vec<f32> = it.take(2).filter_map(|s| s.parse().ok()).collect();
                if p.len() == 2 {
                    // OBJ's V origin is bottom-left; wgpu/glTF sample top-left, so flip.
                    uvs.push([p[0], 1.0 - p[1]]);
                }
            }
            Some("mtllib") => mtllib = it.next().map(|s| s.to_string()),
            Some("usemtl") => cur_mat = it.next().unwrap_or("").to_string(),
            Some("f") => {
                // Corners are `v/vt` (1-based); ignore any `/vn`. Triangulate a fan for
                // the general polygon case (the editor already emits triangles).
                let corners: Vec<(usize, usize)> = it
                    .filter_map(|tok| {
                        let mut parts = tok.split('/');
                        let v = parts.next()?.parse::<usize>().ok()?;
                        let vt = parts.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(v);
                        Some((v - 1, vt - 1))
                    })
                    .collect();
                for i in 1..corners.len().saturating_sub(1) {
                    faces.push((cur_mat.clone(), [corners[0], corners[i], corners[i + 1]]));
                }
            }
            _ => {}
        }
    }

    if faces.is_empty() {
        return Err(format!("{path}: no faces"));
    }

    // Materials (Kd + map_Kd), from the referenced MTL.
    let materials = mtllib
        .map(|lib| parse_mtl(&dir.join(lib)))
        .unwrap_or_default();

    // Decode each referenced texture once, mapping filename → image index.
    let mut images: Vec<TexImage> = Vec::new();
    let mut tex_index: HashMap<String, usize> = HashMap::new();
    for mat in materials.values() {
        if let Some(file) = &mat.map_kd {
            if !tex_index.contains_key(file) {
                if let Some(img) = load_texture(&dir.join(file)) {
                    tex_index.insert(file.clone(), images.len());
                    images.push(img);
                }
            }
        }
    }

    Ok(ParsedObj { positions, uvs, faces, materials, images, tex_index })
}

/// Build one [`TexturedModel`] from a subset of a [`ParsedObj`]'s faces, or `None` if
/// the subset is empty. Vertices are deduped per `(material, v_idx, vt_idx)` so each
/// carries its material's Kd colour without cross-material bleed, and bucketing by
/// material yields one contiguous primitive per material. Only the images this subset
/// actually samples are copied in, and their indices are remapped, so splitting a
/// 14-texture parts sheet into six pieces doesn't hand each piece all 14.
fn build_model(p: &ParsedObj, face_idx: &[usize]) -> Option<TexturedModel> {
    let mut vertices: Vec<TexVertex> = Vec::new();
    let mut vert_map: HashMap<(usize, usize, usize), u32> = HashMap::new();
    let mut buckets: Vec<Vec<u32>> = Vec::new();
    let mut bucket_of: HashMap<usize, usize> = HashMap::new();
    let mut bucket_image: Vec<Option<usize>> = Vec::new();
    // Stable material ordering (first-seen within this subset).
    let mut mat_order: HashMap<&str, usize> = HashMap::new();
    // Source image index → index into this model's own `images`.
    let mut images: Vec<TexImage> = Vec::new();
    let mut image_remap: HashMap<usize, usize> = HashMap::new();

    for &fi in face_idx {
        let (mat_name, corners) = &p.faces[fi];
        let mat = p.materials.get(mat_name);
        let kd = mat.map(|m| m.kd).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let image = mat
            .and_then(|m| m.map_kd.as_ref())
            .and_then(|f| p.tex_index.get(f).copied())
            .map(|src| {
                *image_remap.entry(src).or_insert_with(|| {
                    images.push(p.images[src].clone());
                    images.len() - 1
                })
            });
        let next_key = mat_order.len();
        let mkey = *mat_order.entry(mat_name.as_str()).or_insert(next_key);
        let bucket_idx = *bucket_of.entry(mkey).or_insert_with(|| {
            buckets.push(Vec::new());
            bucket_image.push(image);
            buckets.len() - 1
        });

        // Flat face normal (unused by the prop shader; correct-enough for reuse).
        let q: Vec<Vec3> = corners
            .iter()
            .map(|&(v, _)| Vec3::from(p.positions.get(v).copied().unwrap_or([0.0; 3])))
            .collect();
        let normal = (q[1] - q[0]).cross(q[2] - q[0]).normalize_or_zero().to_array();

        for &(v, vt) in corners {
            let key = (mkey, v, vt);
            let vi = *vert_map.entry(key).or_insert_with(|| {
                vertices.push(TexVertex {
                    pos: p.positions.get(v).copied().unwrap_or([0.0; 3]),
                    normal,
                    uv: p.uvs.get(vt).copied().unwrap_or([0.0, 0.0]),
                    color: kd,
                });
                (vertices.len() - 1) as u32
            });
            buckets[bucket_idx].push(vi);
        }
    }

    // Flatten buckets → one index buffer + one primitive per material.
    let mut indices: Vec<u32> = Vec::new();
    let mut primitives: Vec<TexturedPrimitive> = Vec::new();
    for (bi, bucket) in buckets.iter().enumerate() {
        if bucket.is_empty() {
            continue;
        }
        let start = indices.len() as u32;
        indices.extend_from_slice(bucket);
        primitives.push(TexturedPrimitive {
            index_start: start,
            index_count: bucket.len() as u32,
            image: bucket_image[bi],
            emissive: None,
        });
    }

    if vertices.is_empty() {
        return None;
    }
    Some(TexturedModel { vertices, indices, primitives, images })
}

/// Parse the subset of an MTL we consume: `newmtl` blocks with `Kd` + `map_Kd`.
fn parse_mtl(path: &Path) -> HashMap<String, Material> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        log::warn!("mtl read failed: {}", path.display());
        return out;
    };
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("newmtl") => {
                let name = it.next().unwrap_or("").to_string();
                out.insert(name.clone(), Material { kd: [1.0; 4], map_kd: None });
                cur = Some(name);
            }
            Some("Kd") => {
                if let Some(name) = &cur {
                    let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                    if c.len() == 3 {
                        if let Some(m) = out.get_mut(name) {
                            m.kd = [c[0], c[1], c[2], 1.0];
                        }
                    }
                }
            }
            Some("map_Kd") => {
                if let Some(name) = &cur {
                    // The filename is the last token (skip any option flags).
                    if let Some(file) = line.split_whitespace().last() {
                        if let Some(m) = out.get_mut(name) {
                            m.map_kd = Some(file.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Decode a texture file into a [`TexImage`] (RGBA, top-left origin).
///
/// **Alpha matters here.** GoldenEye's semi-transparent "secondary" surfaces (glass
/// domes, screens, lens covers) ship as **32-bit BMPs whose alpha channel is the real
/// translucency** (e.g. 144/255 ≈ 56% for a glass dome). The `image` crate decodes a
/// 32-bit BI_RGB BMP as XRGB and forces alpha to 255 — silently dropping that
/// translucency. So we decode uncompressed 32-bit BMPs ourselves to keep the alpha,
/// and fall back to the `image` crate for everything else (24-bit BMP, PNG, JPEG).
fn load_texture(path: &Path) -> Option<TexImage> {
    if let Ok(bytes) = std::fs::read(path) {
        if let Some(img) = decode_bmp32(&bytes) {
            return Some(img);
        }
    }
    match image::open(path) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            Some(TexImage {
                width: rgba.width(),
                height: rgba.height(),
                rgba: rgba.into_raw(),
            })
        }
        Err(e) => {
            log::warn!("prop texture load failed {}: {e}", path.display());
            None
        }
    }
}

/// Decode an uncompressed **32-bit** BMP into top-left-origin RGBA, preserving the
/// alpha channel (the whole reason this exists — see [`load_texture`]). Returns `None`
/// for anything that isn't a 32-bit BI_RGB BMP so the caller falls back to the `image`
/// crate. Robust to short/malformed input (returns `None` rather than panicking).
fn decode_bmp32(d: &[u8]) -> Option<TexImage> {
    if d.len() < 54 || &d[0..2] != b"BM" {
        return None;
    }
    let u16le = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let i32le = |o: usize| i32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);

    let data_off = u32le(10) as usize;
    let width = i32le(18);
    let height_raw = i32le(22);
    let bpp = u16le(28);
    let compression = u32le(30);
    // Only handle straight 32-bit BI_RGB (compression 0) or BITFIELDS (3); both store
    // 4 bytes/pixel as B,G,R,A little-endian. Anything else → let `image` handle it.
    if bpp != 32 || (compression != 0 && compression != 3) || width <= 0 || height_raw == 0 {
        return None;
    }
    let w = width as usize;
    let h = height_raw.unsigned_abs() as usize;
    let top_down = height_raw < 0; // negative height = rows already top-to-bottom
    let need = data_off + w * h * 4;
    if d.len() < need {
        return None;
    }
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        let src_row = if top_down { y } else { h - 1 - y };
        for x in 0..w {
            let s = data_off + (src_row * w + x) * 4;
            let o = (y * w + x) * 4;
            rgba[o] = d[s + 2]; // R
            rgba[o + 1] = d[s + 1]; // G
            rgba[o + 2] = d[s]; // B
            rgba[o + 3] = d[s + 3]; // A (the channel `image` throws away)
        }
    }
    Some(TexImage { width: w as u32, height: h as u32, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 32-bit BMP decoder preserves the real alpha channel that the `image` crate
    /// drops — proving GoldenEye's translucent "secondary" surfaces load see-through.
    /// The sentry gun's glass-dome texture is a uniform ~56% alpha (144/255).
    #[test]
    fn decode_bmp32_keeps_the_translucent_alpha() {
        let p = format!(
            "{}/../../assets/props/sentry_gun/tempImgEd0198.bmp",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&p).expect("glass-dome bmp present");
        let img = decode_bmp32(&bytes).expect("a 32-bit BMP");
        // Every texel should carry the translucent alpha (144), not opaque 255.
        assert!(img.rgba.chunks_exact(4).all(|px| px[3] == 144));
    }

    /// A model without an OBJ still errors cleanly rather than panicking.
    #[test]
    fn missing_obj_errors() {
        assert!(load_obj("does/not/exist.obj").is_err());
        assert!(load_obj_components("does/not/exist.obj").is_err());
    }

    fn sentry_path() -> String {
        format!(
            "{}/../../assets/props/sentry_gun/sentry_gun.obj",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    /// The sentry gun splits into its six authored pieces, in the documented order,
    /// with the documented sizes.
    ///
    /// The turret rig indexes this list by position — component 0 is the barrel
    /// bundle it spins, component 1 the housing it pitches — so an ordering change
    /// would silently re-wire which piece moves how. Pinning the triangle count *and*
    /// the measured size of each piece means a re-export that renumbers or reshapes
    /// them fails here rather than in-game.
    #[test]
    fn sentry_gun_splits_into_its_six_parts() {
        let parts = load_obj_components(&sentry_path()).expect("sentry gun loads");
        // (triangles, size in metres, rounded to mm) — see tools/sentry/sentry_rig.py.
        let expect = [
            (18, [0.600, 0.208, 0.240]), // barrel bundle
            (12, [0.800, 0.500, 0.250]), // housing
            (12, [0.600, 0.200, 0.250]), // cowl + dish
            (7, [0.100, 0.400, 0.300]),  // yaw fin
            (6, [0.100, 0.100, 0.400]),  // trunnion
            (2, [0.000, 0.200, 0.250]),  // panel
        ];
        assert_eq!(parts.len(), expect.len(), "component count");
        for (i, (tris, size)) in expect.iter().enumerate() {
            let p = &parts[i];
            assert_eq!(p.indices.len(), tris * 3, "component {i} triangle count");
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for v in &p.vertices {
                for k in 0..3 {
                    lo[k] = lo[k].min(v.pos[k]);
                    hi[k] = hi[k].max(v.pos[k]);
                }
            }
            for k in 0..3 {
                assert!(
                    ((hi[k] - lo[k]) - size[k]).abs() < 1e-3,
                    "component {i} axis {k}: got {:.3} want {:.3}",
                    hi[k] - lo[k],
                    size[k]
                );
            }
        }
    }

    /// Splitting is lossless and non-duplicating: every triangle in the merged model
    /// lands in exactly one component, and each component carries only the textures
    /// its own faces sample rather than the whole sheet's.
    #[test]
    fn components_partition_the_model_and_trim_their_textures() {
        let whole = load_obj(&sentry_path()).expect("sentry gun loads");
        let parts = load_obj_components(&sentry_path()).expect("sentry gun splits");
        let split_tris: usize = parts.iter().map(|p| p.indices.len()).sum();
        assert_eq!(split_tris, whole.indices.len(), "triangles conserved");
        for (i, p) in parts.iter().enumerate() {
            let used: std::collections::HashSet<usize> =
                p.primitives.iter().filter_map(|pr| pr.image).collect();
            assert_eq!(p.images.len(), used.len(), "component {i} carries dead images");
            assert!(
                p.images.len() < whole.images.len(),
                "component {i} kept all {} sheet textures",
                whole.images.len()
            );
        }
    }
}
