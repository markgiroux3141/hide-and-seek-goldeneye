//! **Face painting** — the PAINT tab's two operations: explain the surface under
//! the cursor, and override its texture.
//!
//! # Why a face and not a triangle
//!
//! The feature was asked for as "select a triangle and give it a texture", and
//! that is the right *gesture* but the wrong unit to store. A triangle has no
//! identity that survives the next keystroke: the CSG fold regenerates the whole
//! soup on every edit, [`engine::render::uv_zones`] then splits it at door frames
//! and at the wall band, and the builder finally sorts the result by (scheme,
//! zone). Triangle #4713 is a different triangle after any edit anywhere in the
//! region, and after a plain reload if brush order moved.
//!
//! A brush *face* — `(brush id, axis, side)` — survives all of it, is already what
//! the classifier keys on, and is already what a click resolves to. So the click
//! picks a triangle and the override lands on the face that produced it.
//!
//! # Why the probe re-classifies instead of guessing
//!
//! The obvious way to answer "which face is this?" is the editor's existing
//! [`World::pick_face_hit_from`] — raycast, then match the hit point against brush
//! face planes. That is a *different algorithm* from the one the classifier uses to
//! decide a triangle's texture (`face_owner`: nearest face plane to the triangle
//! **centroid**, smaller brush wins ties). Where the two disagree — which is
//! exactly the situation a repair tool exists for — painting the face the picker
//! named would change nothing visible, because the classifier is reading a
//! different brush.
//!
//! So the probe runs the real classifier over the one triangle the ray hit, and
//! reports what *it* concluded. Paint then writes the override onto that face. The
//! tool and the renderer cannot disagree about which face a pixel came from.

use engine::geometry::csg_runtime::{Axis, FaceTex, Side, WORLD_SCALE};
use engine::geometry::structures::ColumnInputs;
use engine::render::uv_zones::{self, OwnerCandidate, ZonedBuilder};

use super::*;

/// Everything the PAINT tab knows about the surface under the cursor.
#[derive(Clone, Debug)]
pub struct FaceProbe {
    /// Where the ray met the surface (world metres).
    pub point: Vec3,
    pub region_id: u32,
    /// The face's outward axis, as the classifier resolved it.
    pub axis: Axis,
    /// Which end of that axis the owning face sits on. `None` when nothing owns
    /// the triangle (it fell through to the default theme).
    pub side: Option<Side>,
    /// The owning brush, or `None` for an unowned triangle (the region shell).
    pub brush_id: Option<u32>,
    /// The theme this surface actually renders with, override included.
    pub scheme: usize,
    /// The zone slot it actually renders with.
    pub zone: u8,
    /// Whether that came from a painted override rather than the owner's theme.
    pub overridden: bool,
    /// How many pieces the classifier cut this one fold triangle into.
    pub fragments: usize,
    /// How many *distinct themes* those pieces landed in. Anything above 1 means
    /// this single triangle spans a theme boundary — the shape the wrong-texture
    /// defect takes.
    pub distinct_schemes: usize,
    /// Every brush face that competed to own this triangle, with the numbers the
    /// decision turned on. Sorted nearest-first.
    pub candidates: Vec<OwnerCandidate>,
    /// Why this surface can't be painted, or `None` if it can.
    pub blocked: Option<&'static str>,
}

impl FaceProbe {
    /// The face this probe would paint, if it can be painted.
    pub fn target(&self) -> Option<(u32, Axis, Side)> {
        if self.blocked.is_some() {
            return None;
        }
        Some((self.brush_id?, self.axis, self.side?))
    }

    /// A short human label for the face — "brush 42 +X".
    pub fn face_label(&self) -> String {
        match (self.brush_id, self.side) {
            (Some(id), Some(side)) => {
                let sign = if side == Side::Max { '+' } else { '-' };
                let axis = match self.axis {
                    Axis::X => 'X',
                    Axis::Y => 'Y',
                    Axis::Z => 'Z',
                };
                format!("brush {id} {sign}{axis}")
            }
            _ => "no owner".to_string(),
        }
    }
}

/// How far off a shared plane the three corners of a triangle may sit and still
/// count as axis-aligned (metres). Fold triangles come off axis-aligned brushes so
/// they are exact; this only absorbs float noise.
const PLANAR_EPS: f32 = 1e-4;

impl World {
    /// Explain the surface an explicit ray lands on: which brush face the
    /// classifier attributed it to, what it renders as, and who else was in the
    /// running. See the module docs for why this re-classifies rather than
    /// reusing the face picker.
    ///
    /// Ray-aimed rather than crosshair-aimed for the same reason the theme picker
    /// is: an open side panel frees the mouse cursor, which leaves the camera's
    /// crosshair frozen wherever it last pointed.
    pub fn probe_surface(&mut self, origin: Vec3, dir: Vec3) -> Option<FaceProbe> {
        let hit = self.physics.raycast(origin, dir, 100.0)?;
        let point = hit.point;

        let blocked_probe = |note: &'static str| FaceProbe {
            point,
            region_id: 0,
            axis: Axis::Y,
            side: None,
            brush_id: None,
            scheme: engine::render::textures::default_scheme(),
            zone: 0,
            overridden: false,
            fragments: 0,
            distinct_schemes: 0,
            candidates: Vec::new(),
            blocked: Some(note),
        };

        let Some(region_id) = self.physics.region_of_collider(hit.collider) else {
            return Some(blocked_probe("not level geometry"));
        };
        let Some(tri) = self.physics.hit_triangle(&hit) else {
            return Some(blocked_probe("no triangle behind this hit"));
        };
        let region = self.regions.iter().find(|r| r.id == region_id)?;

        // A fold triangle is always axis-aligned (brushes are AABBs). A sloped one
        // is stair or ramp geometry appended to the collider after the fold, which
        // no brush face owns and no override can reach.
        let n = tri_normal(tri);
        let dom = dominant_axis_index(n);
        if (tri[0][dom] - tri[1][dom]).abs() > PLANAR_EPS
            || (tri[0][dom] - tri[2][dom]).abs() > PLANAR_EPS
        {
            return Some(blocked_probe("stair or ramp surface — paint its room instead"));
        }

        // Run the real classifier over just this triangle, so what the probe reports
        // is what the bake produced rather than a second opinion about it.
        let brushes = region.brush_infos();
        let pos: Vec<f32> = tri.iter().flat_map(|v| [v.x, v.y, v.z]).collect();
        let mut b = ZonedBuilder::new();
        // The probe inputs have to be the bake's, or the probe explains a band the
        // bake did not draw: the band anchor is probed per triangle from the region's
        // brushes plus whichever platforms the level lets count as floors.
        let cols = ColumnInputs::new(&region.brushes, self.band_platforms());
        uv_zones::classify_soup(
            &mut b,
            &pos,
            &[0, 1, 2],
            &brushes,
            engine::render::textures::default_scheme(),
            &cols,
            &engine::render::textures::cornice_table(),
        );
        let mesh = b.finish();

        let fragments = mesh.indices.len() / 3;
        let distinct_schemes = mesh
            .groups
            .iter()
            .map(|g| g.scheme)
            .collect::<std::collections::BTreeSet<_>>()
            .len();

        // Which emitted fragment covers the point the ray actually struck? That
        // fragment's group is the (scheme, zone) the pixel under the cursor drew with.
        let (scheme, zone, frag) = match fragment_at(&mesh, point, dom) {
            Some(x) => x,
            // The point sits on a fragment edge and no containment test claimed it —
            // fall back to the whole triangle, which is the same answer everywhere it
            // isn't straddling.
            None => (
                mesh.groups.first().map(|g| g.scheme as usize).unwrap_or(0),
                mesh.groups.first().map(|g| g.zone).unwrap_or(0),
                tri,
            ),
        };

        let centroid_wt = [
            (frag[0].x + frag[1].x + frag[2].x) / 3.0 / WORLD_SCALE,
            (frag[0].y + frag[1].y + frag[2].y) / 3.0 / WORLD_SCALE,
            (frag[0].z + frag[1].z + frag[2].z) / 3.0 / WORLD_SCALE,
        ];
        let candidates = uv_zones::owner_candidates(&brushes, centroid_wt, dom);
        let winner = candidates.iter().find(|c| c.chosen).copied();

        let axis = match dom {
            0 => Axis::X,
            1 => Axis::Y,
            _ => Axis::Z,
        };
        Some(FaceProbe {
            point,
            region_id,
            axis,
            side: winner.map(|c| c.side),
            brush_id: winner.map(|c| c.brush_id),
            scheme,
            zone,
            overridden: winner.map(|c| c.overridden).unwrap_or(false),
            fragments,
            distinct_schemes,
            candidates,
            blocked: winner
                .is_none()
                .then_some("no brush owns this surface — it is the region shell"),
        })
    }

    /// Paint (or with `None`, clear) the texture override on the face an explicit
    /// ray lands on. Returns the re-baked region mesh, or `None` if the ray hit
    /// nothing paintable — which is also what makes this safe to wrap in
    /// [`with_undo`](Self::with_undo): no change, no checkpoint.
    pub fn paint_face_along(&mut self, origin: Vec3, dir: Vec3, tex: Option<FaceTex>) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let probe = self.probe_surface(origin, dir)?;
        let (brush_id, axis, side) = probe.target()?;
        self.set_face_tex(brush_id, axis, side, tex)
    }

    /// Set one brush face's texture override and re-bake its region. A no-op
    /// (returning `None`) when the face already carries exactly this override, so
    /// dragging the cursor over an already-painted face doesn't churn the undo
    /// stack or the CSG cache.
    pub fn set_face_tex(
        &mut self,
        brush_id: u32,
        axis: Axis,
        side: Side,
        tex: Option<FaceTex>,
    ) -> Option<RegionMesh> {
        let region = self
            .regions
            .iter_mut()
            .find(|r| r.brushes.iter().any(|b| b.id == brush_id))?;
        let brush = region.brushes.iter_mut().find(|b| b.id == brush_id)?;
        if brush.face_tex(axis, side) == tex {
            return None;
        }
        brush.set_face_tex(axis, side, tex);
        match tex {
            Some(t) => log::info!(
                "face paint: brush {brush_id} {axis:?}/{side:?} -> {} (zone {:?})",
                engine::render::textures::scheme_name(t.scheme),
                t.zone
            ),
            None => log::info!("face paint: brush {brush_id} {axis:?}/{side:?} cleared"),
        }
        // Safe to narrow: an already-mapped brush id, and a repaint moves no
        // geometry, so this cannot recluster. See the note in `editing::set_scheme_at`.
        self.rebuild_affected_regions(&[brush_id]).into_iter().next()
    }

    /// A translucent quad over one brush face, for the PAINT tab's highlight.
    ///
    /// The whole face, not the triangle the ray hit: the override applies to the
    /// face, so showing the triangle would promise a precision the tool does not
    /// have (and the triangle is an artifact of the fold, which is the reason none
    /// of this is keyed to one).
    pub fn face_highlight_mesh(&self, brush_id: u32, axis: Axis, side: Side) -> Option<CpuMesh> {
        let brush = self
            .regions
            .iter()
            .flat_map(|r| r.brushes.iter())
            .find(|b| b.id == brush_id)?;
        let (u_axis, v_axis) = axis.orthogonals();
        let u0 = brush.min(u_axis);
        let v0 = brush.min(v_axis);
        Some(self.face_quad_mesh(
            axis,
            side,
            brush.face_pos(axis, side),
            u_axis,
            v_axis,
            u0,
            u0 + brush.dim(u_axis),
            v0,
            v0 + brush.dim(v_axis),
        ))
    }

    /// The override currently on a face, if any.
    pub fn face_tex(&self, brush_id: u32, axis: Axis, side: Side) -> Option<FaceTex> {
        self.regions
            .iter()
            .flat_map(|r| r.brushes.iter())
            .find(|b| b.id == brush_id)
            .and_then(|b| b.face_tex(axis, side))
    }

    /// How many faces in the level carry a painted override.
    pub fn face_tex_count(&self) -> usize {
        self.regions
            .iter()
            .flat_map(|r| r.brushes.iter())
            .map(|b| b.face_tex_count())
            .sum()
    }

    /// Clear every painted override in the level, re-baking whatever regions held
    /// one. Returns the meshes to re-upload (empty if there was nothing to clear).
    pub fn clear_all_face_tex(&mut self) -> Vec<RegionMesh> {
        let mut touched: Vec<u32> = Vec::new();
        for region in self.regions.iter_mut() {
            for b in region.brushes.iter_mut() {
                if b.face_tex_count() > 0 {
                    b.face_tex = [None; 6];
                    touched.push(b.id);
                }
            }
        }
        if touched.is_empty() {
            return Vec::new();
        }
        log::info!("face paint: cleared {} override(s)", touched.len());
        self.rebuild_affected_regions(&touched)
    }
}

/// Unit normal of a triangle given as three world-space points.
fn tri_normal(t: [Vec3; 3]) -> Vec3 {
    (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero()
}

/// Array index (`0`/`1`/`2`) of a normal's dominant axis.
fn dominant_axis_index(n: Vec3) -> usize {
    let a = n.abs();
    if a.y >= a.x && a.y >= a.z {
        1
    } else if a.x >= a.z {
        0
    } else {
        2
    }
}

/// The classified fragment covering `point`, as `(scheme, zone, corners)`.
///
/// Tests containment in the triangle's own plane (the two tangent axes), which is
/// what "the pixel under the cursor" means for a planar fragment — and is exactly
/// how the classifier itself decides frame and band membership, so the answer
/// agrees with the render by construction.
fn fragment_at(
    mesh: &engine::render::mesh::TexturedMesh,
    point: Vec3,
    dom: usize,
) -> Option<(usize, u8, [Vec3; 3])> {
    let p = [point.x, point.y, point.z];
    let (u, v) = match dom {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    };
    for g in &mesh.groups {
        let range = g.start as usize..(g.start + g.count) as usize;
        for chunk in mesh.indices[range].chunks_exact(3) {
            let c: Vec<[f32; 3]> = chunk.iter().map(|&i| mesh.vertices[i as usize].pos).collect();
            if point_in_tri_2d([c[0], c[1], c[2]], p, u, v) {
                return Some((
                    g.scheme as usize,
                    g.zone,
                    [
                        Vec3::from_array(c[0]),
                        Vec3::from_array(c[1]),
                        Vec3::from_array(c[2]),
                    ],
                ));
            }
        }
    }
    None
}

/// Point-in-triangle by sign-of-cross-product, in the `(u, v)` plane. The epsilon
/// is generous on purpose: a point on a shared edge should be claimed by one of
/// the two fragments rather than by neither.
fn point_in_tri_2d(t: [[f32; 3]; 3], p: [f32; 3], u: usize, v: usize) -> bool {
    let side = |a: [f32; 3], b: [f32; 3]| {
        (b[u] - a[u]) * (p[v] - a[v]) - (b[v] - a[v]) * (p[u] - a[u])
    };
    let d0 = side(t[0], t[1]);
    let d1 = side(t[1], t[2]);
    let d2 = side(t[2], t[0]);
    const EPS: f32 = 1e-4;
    let neg = d0 < -EPS || d1 < -EPS || d2 < -EPS;
    let pos = d0 > EPS || d1 > EPS || d2 > EPS;
    !(neg && pos)
}
