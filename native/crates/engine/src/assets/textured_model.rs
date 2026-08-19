//! A loaded static textured model — the generic render asset behind any
//! multi-node, multi-material glTF/GLB that carries `POSITION / NORMAL /
//! TEXCOORD_0` with embedded base-color textures (the N64-style weapon/prop
//! GLBs). Positions are baked through the node hierarchy; the model is split
//! into per-texture [`TexturedPrimitive`]s so each material's skin renders.
//!
//! This is domain-free: the engine renderer uploads it verbatim
//! ([`crate::render::renderer::Renderer::upload_viewmodel`]). Game-specific
//! loading policy (which GLB, which material filter) lives with the caller —
//! e.g. the game's weapon viewmodel.

use glam::{Mat4, Vec3};

use crate::render::mesh::TexVertex;
use crate::skeletal::gltf_skin::{to_rgba8, TexImage};

/// A contiguous index range sharing one base-color texture (one glTF primitive).
pub struct TexturedPrimitive {
    pub index_start: u32,
    pub index_count: u32,
    /// Index into [`TexturedModel::images`], or `None` for an untextured
    /// material (drawn with a white fallback).
    pub image: Option<usize>,
    /// Index into [`TexturedModel::images`] of the model's **environment/reflection**
    /// map, or `None` for a non-metallic model. Populated for every primitive of a
    /// GoldenEye metallic gun (one that has an `*EnvMapping*` material) — the shiny
    /// gold/silver/chrome guns. Their base-color textures are mostly BLACK (the
    /// metal was meant to be filled by an environment reflection), so the renderer
    /// samples THIS texture by the surface normal (matcap-style) and adds it, which
    /// turns the black metal gold/silver. Plain guns leave it `None`.
    pub emissive: Option<usize>,
}

/// A loaded static textured model: one shared vertex/index buffer split into
/// per-texture [`TexturedPrimitive`]s, plus the decoded images.
pub struct TexturedModel {
    pub vertices: Vec<TexVertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<TexturedPrimitive>,
    pub images: Vec<TexImage>,
}

impl TexturedModel {
    /// Concatenate `other` into `self`, rebasing its indices, primitive ranges, and
    /// image references so the combined result draws as one model. Used to consolidate
    /// a GoldenEye **primary + secondary** pair — the same object split into an OPAQUE
    /// primary GLB and an alpha-cutout (`alphaMode = MASK`) secondary GLB (glass,
    /// chain-link, grates) — into a single prop, so the caller never sees the split.
    /// The two keep their own textures (props aren't atlased); the cutout renders via
    /// the prop shader's alpha `discard`, which is a no-op on the opaque primary.
    pub fn append(&mut self, other: TexturedModel) {
        let v_off = self.vertices.len() as u32;
        let i_off = self.indices.len() as u32;
        let img_off = self.images.len();
        self.vertices.extend(other.vertices);
        self.images.extend(other.images);
        self.indices.extend(other.indices.into_iter().map(|i| i + v_off));
        for mut p in other.primitives {
            p.index_start += i_off;
            p.image = p.image.map(|x| x + img_off);
            p.emissive = p.emissive.map(|x| x + img_off);
            self.primitives.push(p);
        }
    }
}

/// Which primitives an `*EnvMapping*` material's reflection map is bound to.
///
/// The two asset families genuinely differ, and picking one rule for both makes
/// half of them wrong:
///
/// * a GoldenEye metallic gun tags **one** material `EnvMapping` while the whole
///   weapon is meant to be shiny — its other materials are near-black metal
///   waiting for a reflection, so the map goes on [`EnvScope::WholeModel`];
/// * a Perfect Dark gun mixes env-mapped metal with ordinary painted skins on the
///   same model (the Shotgun has one env texture and twenty-six real ones), so
///   the same rule adds a bright sheen over its wood and its decals and washes
///   the gun out — measured on screen, not feared in the abstract.
///   [`EnvScope::PerMaterial`] binds it only where the author asked for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvScope {
    /// Every primitive reflects (GoldenEye's shiny guns).
    WholeModel,
    /// Only primitives whose own material is `*EnvMapping*` (Perfect Dark's).
    PerMaterial,
}

/// Load a GLB into a [`TexturedModel`], keeping only primitives whose glTF
/// material name passes `keep` (a nameless material is offered as `""`). Pass
/// `|_| true` to keep everything. Node transforms are baked into positions
/// (these models are multi-node). Errors if nothing drawable survives the
/// filter.
///
/// Environment mapping follows [`EnvScope::WholeModel`]; see [`load_with`].
pub fn load(path: &str, keep: impl Fn(&str) -> bool + Copy) -> Result<TexturedModel, String> {
    load_with(path, keep, EnvScope::WholeModel)
}

/// [`load`], with an explicit [`EnvScope`].
pub fn load_with(
    path: &str,
    keep: impl Fn(&str) -> bool + Copy,
    env_scope: EnvScope,
) -> Result<TexturedModel, String> {
    let (doc, buffers, images) =
        gltf::import(path).map_err(|e| format!("gltf import {path}: {e}"))?;

    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or_else(|| format!("{path}: no scene"))?;

    let mut model = TexturedModel {
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        images: Vec::new(),
    };
    // Per-primitive "its own material is `*EnvMapping*`", parallel to
    // `model.primitives` — only [`EnvScope::PerMaterial`] reads it.
    let mut env_material: Vec<bool> = Vec::new();
    // Per-primitive `(vertex_start, vertex_end, is_decal)`, for `lift_decals`.
    let mut decals: Vec<(usize, usize, bool)> = Vec::new();
    for node in scene.nodes() {
        visit_node(
            node,
            Mat4::IDENTITY,
            &buffers,
            &mut model,
            keep,
            &mut env_material,
            &mut decals,
        );
    }
    if model.vertices.is_empty() {
        return Err(format!("{path}: no drawable geometry found (after material filter)"));
    }
    lift_decals(&mut model, &decals);

    // Metallic guns (gold/silver/chrome) — environment mapping. GoldenEye tags the
    // reflective surface with an `*EnvMapping*` material whose texture is the metal
    // reflection map (gold for the Golden Gun, chrome for the Magnum, …). The gun's
    // BASE textures are mostly black — the metal was meant to be filled by that
    // reflection, so unlit it renders near-black with only a few gold accents.
    // Rather than the base color, the renderer samples this reflection map by the
    // surface normal (matcap-style, see `shader_viewmodel`) and adds it, turning
    // the black metal into uniform gold/silver like the original. Point EVERY
    // primitive of such a model at the (first) EnvMapping material's texture so the
    // whole gun reflects; plain guns have no EnvMapping material and stay `None`.
    let env_image = doc
        .materials()
        .find(|m| m.name().is_some_and(|n| n.contains("EnvMapping")))
        .and_then(|m| {
            m.pbr_metallic_roughness()
                .base_color_texture()
                .map(|info| info.texture().source().index())
        });
    if let Some(env) = env_image {
        for (i, p) in model.primitives.iter_mut().enumerate() {
            if env_scope == EnvScope::WholeModel || env_material.get(i).copied().unwrap_or(false) {
                p.emissive = Some(env);
            }
        }
    }

    model.images = images.iter().map(to_rgba8).collect();
    Ok(model)
}

/// How far a decal surface is lifted off the face it sits on, in model-space metres.
///
/// Big enough to clear depth-buffer precision at any view distance the game uses, small
/// enough to be invisible: 2 mm on a 1.75 m door panel, and it survives the doorway fit
/// (a door squashed to ~0.6 still clears by better than a millimetre).
const DECAL_LIFT: f32 = 0.002;

/// Lift every `TopFlag` (decal) primitive clear of the surface it is coplanar with.
///
/// **Why this exists.** GoldenEye's artists authored door handles, crate stencils and
/// barrel labels as geometry sharing a plane *exactly* with the body underneath, and
/// tagged the material `TopFlag`. On the N64 that was free: the RDP had a dedicated
/// **decal z-mode** (`ZMODE_DEC`) which drew a coplanar surface on top of what was
/// already in the depth buffer instead of fighting it. Modern depth testing has no such
/// mode, so the two surfaces interleave per-pixel and shimmer.
///
/// The fix is geometric rather than a depth-bias pipeline, because a bias would have to
/// be applied *per primitive* — the body and its decal are primitives of the same mesh
/// drawn in one pass, so biasing the pipeline moves both equally and fixes nothing.
/// Splitting the prop draw by primitive to switch pipelines would be a much larger
/// change for the same result.
///
/// **Direction is derived from the model, not from vertex normals.** A decal's normals
/// may be averaged, inward, or (on the flat quads) degenerate. Instead: find which face
/// of the model's own bounding box the decal is flush against — the axis and side whose
/// gap is smallest — and push it out that way. That is exactly what "sits on the
/// surface" means, and it works for a flat quad and for a handle with thickness alike.
fn lift_decals(model: &mut TexturedModel, decals: &[(usize, usize, bool)]) {
    if !decals.iter().any(|(_, _, d)| *d) {
        return; // no decal materials — the overwhelmingly common case
    }
    // Whole-model bounds: the box whose faces the decals are stuck to.
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for v in &model.vertices {
        lo = lo.min(Vec3::from(v.pos));
        hi = hi.max(Vec3::from(v.pos));
    }

    for &(start, end, is_decal) in decals {
        if !is_decal || start >= end || end > model.vertices.len() {
            continue;
        }
        let (mut dlo, mut dhi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &model.vertices[start..end] {
            dlo = dlo.min(Vec3::from(v.pos));
            dhi = dhi.max(Vec3::from(v.pos));
        }
        // Smallest gap to a bounding-box face wins: that's the surface it lies on.
        let mut best = (f32::MAX, Vec3::ZERO);
        for axis in 0..3 {
            let to_min = (dlo[axis] - lo[axis]).abs();
            if to_min < best.0 {
                let mut d = Vec3::ZERO;
                d[axis] = -1.0;
                best = (to_min, d);
            }
            let to_max = (hi[axis] - dhi[axis]).abs();
            if to_max < best.0 {
                let mut d = Vec3::ZERO;
                d[axis] = 1.0;
                best = (to_max, d);
            }
        }
        let push = best.1 * DECAL_LIFT;
        for v in &mut model.vertices[start..end] {
            v.pos = (Vec3::from(v.pos) + push).to_array();
        }
    }
}

fn visit_node(
    node: gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut TexturedModel,
    keep: impl Fn(&str) -> bool + Copy,
    env_material: &mut Vec<bool>,
    decals: &mut Vec<(usize, usize, bool)>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    let normal_mat = Mat4::from_mat3(glam::Mat3::from_mat4(world).inverse().transpose());

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            // Material filter (e.g. keep only the flash billboards for a muzzle).
            let mat_name = prim.material().name().unwrap_or("");
            if !keep(mat_name) {
                continue;
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(n) => n.collect(),
                None => vec![[0.0, 1.0, 0.0]; positions.len()],
            };
            let uvs: Vec<[f32; 2]> = match reader.read_tex_coords(0) {
                Some(t) => t.into_f32().collect(),
                None => vec![[0.0, 0.0]; positions.len()],
            };
            // Vertex colors (glTF `COLOR_0`): the GoldenEye weapon GLBs shade a
            // palette texture per-vertex with these, so they're multiplied onto the
            // sampled texel. Absent → white (no tint).
            let colors: Vec<[f32; 4]> = match reader.read_colors(0) {
                Some(c) => c.into_rgba_f32().collect(),
                None => vec![[1.0, 1.0, 1.0, 1.0]; positions.len()],
            };
            // Material base-color FACTOR (glTF `pbrMetallicRoughness.baseColorFactor`,
            // default white). Many GoldenEye props ship a GRAYSCALE base texture and
            // carry their real colour here (e.g. the wooden crate's brown factor over
            // a gray wood-grain texture); ignoring it renders them black-and-white.
            // Fold it into the vertex colour so `texel × colour` reproduces the
            // authored surface colour. Weapons default to white here → no change.
            let factor = prim.material().pbr_metallic_roughness().base_color_factor();

            let base = out.vertices.len() as u32;
            for i in 0..positions.len() {
                let wp = world.transform_point3(Vec3::from(positions[i]));
                let n = normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
                let wn = normal_mat.transform_vector3(Vec3::from(n)).normalize_or_zero();
                let c = colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]);
                out.vertices.push(TexVertex {
                    pos: wp.to_array(),
                    normal: wn.to_array(),
                    uv: uvs.get(i).copied().unwrap_or([0.0, 0.0]),
                    color: [
                        c[0] * factor[0],
                        c[1] * factor[1],
                        c[2] * factor[2],
                        c[3] * factor[3],
                    ],
                });
            }

            let index_start = out.indices.len() as u32;
            match reader.read_indices() {
                Some(idx) => out.indices.extend(idx.into_u32().map(|i| base + i)),
                None => out
                    .indices
                    .extend((0..positions.len() as u32).map(|i| base + i)),
            }
            let index_count = out.indices.len() as u32 - index_start;

            let image = prim
                .material()
                .pbr_metallic_roughness()
                .base_color_texture()
                .map(|info| info.texture().source().index());

            out.primitives.push(TexturedPrimitive {
                index_start,
                index_count,
                image,
                // Filled in the metallic post-pass below (env reflection map).
                emissive: None,
            });
            env_material.push(mat_name.contains("EnvMapping"));
            // GoldenEye's `TopFlag` material flag = the N64 RDP's **decal** z-mode
            // (`ZMODE_DEC`): a surface authored exactly coplanar with the one under it
            // — a door handle, a crate stencil, a barrel label — which the hardware drew
            // on top instead of z-fighting with. We have no such mode, so record the
            // range now and nudge it off the surface in `lift_decals` once the whole
            // model's bounds are known.
            decals.push((
                base as usize,
                out.vertices.len(),
                mat_name.contains("TopFlag"),
            ));
        }
    }

    for child in node.children() {
        visit_node(child, world, buffers, out, keep, env_material, decals);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A metallic gun (golden-gun contains an `*EnvMapping*` material) points EVERY
    /// primitive at the SAME reflection texture (the env map), so the whole gun
    /// reflects gold — not each primitive's own (mostly black) base texture. A
    /// plain gun (pp7, no EnvMapping) gets no reflection map.
    #[test]
    fn metallic_gun_env_maps_every_primitive_others_none() {
        let asset =
            |g: &str| format!("{}/../../assets/weapons/{}/gun.glb", env!("CARGO_MANIFEST_DIR"), g);

        let gold = load(&asset("golden-gun"), |_| true).expect("load golden gun");
        let env = gold.primitives[0].emissive;
        assert!(env.is_some(), "golden-gun is metallic → has a reflection map");
        for (i, p) in gold.primitives.iter().enumerate() {
            assert_eq!(p.emissive, env, "golden-gun prim[{i}] shares the one env map");
        }

        let pp7 = load(&asset("pp7"), |_| true).expect("load pp7");
        assert!(
            pp7.primitives.iter().all(|p| p.emissive.is_none()),
            "pp7 is not metallic → no reflection map"
        );
    }

    /// GoldenEye's `TopFlag` decal surfaces (door handles, crate stencils) are authored
    /// exactly coplanar with the body beneath them — free on the N64's decal z-mode,
    /// z-fighting for us. The loader lifts them clear along the model-box face they sit
    /// on, and must leave every other primitive untouched.
    #[test]
    fn topflag_decals_are_lifted_off_the_surface_they_sit_on() {
        // `wooden_door` is the case from the playtest: two handle quads at x = ±0.075,
        // exactly on the door's own faces.
        let path = format!(
            "{}/../../assets/props/wooden_door.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        let m = load_with(&path, |_| true, EnvScope::PerMaterial).expect("loads");
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for v in &m.vertices {
            lo = lo.min(Vec3::from(v.pos));
            hi = hi.max(Vec3::from(v.pos));
        }
        // The lift pushes the handles just outside what the body alone spans, so the
        // model is now wider than the panel by about one lift on each face.
        let thickness = hi.x - lo.x;
        assert!(
            thickness > 0.1506 && thickness < 0.1506 + 4.0 * DECAL_LIFT,
            "handles lifted clear of the 0.1506 m panel without floating: {thickness}"
        );
    }

    /// A prop with no `TopFlag` material is passed through byte-identically — the lift
    /// must never perturb ordinary geometry.
    #[test]
    fn a_prop_without_decals_is_untouched() {
        let path = format!(
            "{}/../../assets/props/filing_cabinet.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        let m = load_with(&path, |_| true, EnvScope::PerMaterial).expect("loads");
        // `lift_decals` early-returns when nothing is flagged; assert the geometry still
        // has the tight, axis-aligned bounds a box prop should.
        let mut n = 0;
        for v in &m.vertices {
            if v.pos.iter().any(|c| !c.is_finite()) {
                n += 1;
            }
        }
        assert_eq!(n, 0, "no vertex was disturbed into a bad value");
        assert!(!m.vertices.is_empty());
    }
}
