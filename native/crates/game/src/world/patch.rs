//! **Face patches** — multi-brush face selection, derived on demand.
//!
//! # The problem
//!
//! A room is not one brush. A hall with a step down in it is two subtract carves, a
//! hall with three steps is four, and the freeform draw tool emits one per rectangle
//! of a concave footprint. That decomposition is invisible while you author it and
//! unavoidable in the model — [`Brush`] is an axis-aligned *box*, so any room that
//! isn't a box has to be several — but it leaks the moment you want to edit the room
//! as a room. Raising the ceiling of a four-carve hall meant four separate selections
//! and four pushes, and cutting a hole in a floor that spanned two carves was not
//! expressible at all.
//!
//! # Why not merge the brushes
//!
//! The obvious fix — let the author fuse several brushes into one — cannot express
//! the target case. Two boxes merge into a box only when their union *is* a box, and
//! a room with a step is exactly the shape where it isn't. Letting a `Brush` hold a
//! *set* of boxes instead would reach the CSG fold, `face_owner`, the zone
//! classifier, nav, persistence, region hashing and the hard six-slot
//! [`Brush::face_tex`] array — for no new expressive power, since the fold already
//! unions overlapping brushes anyway.
//!
//! # What this is instead
//!
//! A **patch**: the set of faces coplanar with the selected one and reachable through
//! the same room. It is *derived on every use and never stored*. That is the decision
//! the whole feature rests on. A stored multi-selection goes stale on delete, on undo,
//! on a recluster, and on any resize that breaks coplanarity, and every one of those
//! is an editor that silently moves geometry the author didn't pick. `World::selected`
//! stays exactly what it was — one anchor face — and the patch is recomputed from it,
//! so it cannot be stale.
//!
//! # The two jobs a patch does, which are not the same job
//!
//! * A **full-face** push/pull loops over the patch and resizes every member.
//! * A **sub-face** carve uses the patch only for its *bounding rect*, so the sub-rect
//!   can be scrolled out past the anchor brush's own edge. The carve itself is still a
//!   single box, and the CSG does not know or care that the box spans two brushes —
//!   which is why a hole across a seam needs no new geometry code at all, only
//!   permission to draw a bigger rectangle.
//!
//! # Cost
//!
//! Deriving instead of storing means the per-frame highlight path recomputes a patch
//! two or three times a frame, and `find_room_brushes` is O(n^2) in a region's brush
//! count. Measured rather than assumed: **3.1 us per derivation over the 76-brush
//! facility_2**, so ~10 us a frame against a 16 ms budget. There is no cache and none
//! is wanted — a cached patch is a patch that can go stale, which is the one failure
//! mode this design exists to rule out. See `tests::patch_derivation_cost`.
//!
//! Patch scope is off by default and toggled with `0`: having a whole room's ceiling
//! move when you meant one bay is a bad surprise, so it is opt-in, and the highlight
//! draws the real member faces rather than their bounding box so the scope is never
//! invisible.

// ─── Related prior art, deliberately NOT shared (yet) ────────────────────────
//
// `tools::draw::coplanar_face_group` answers almost this same question for the
// freeform draw tool, and got there first. The two differ in three ways, and two of
// them are on purpose:
//
// * **Frames.** Draw includes them ("a doorway's threshold is genuinely part of the
//   floor you're standing on"); a patch excludes them, because raising a ceiling must
//   not reach through a door into the next room. Both are right for their tool.
// * **Result.** Draw keeps the individual rects and masks what it builds to the real
//   surface; a patch keeps ids (it has to resize the brushes) and uses only the union
//   as a bound. See `patch_bounds` on why the overhang is allowed.
// * **Contiguity.** Draw flood-fills on *in-plane rect* adjacency; a patch reuses the
//   3D `find_room_brushes` walk and then filters by coplanarity. Draw's is the more
//   directly correct question, and a patch can in principle admit a coplanar face that
//   touches the room only somewhere off-plane. Not observed in practice, and unifying
//   them means reconciling the frame rule above, so it is a follow-up rather than a
//   drive-by refactor of a shipped tool.

use super::*;

/// Coplanarity tolerance, in WT.
///
/// Deliberately **tight** — brush coordinates are authored on a grid, so faces that
/// belong to one plane share it exactly and this only absorbs float slop. It is the
/// same 1e-4 `brushes_touching` uses for edge adjacency, and it is emphatically *not*
/// the picker's `PLANE_EPS` (0.15): that one is raycast slop, and reusing it here
/// would sweep in a face genuinely a tenth of a unit off and then force it coplanar on
/// the first push — silently moving geometry the author never selected.
const COPLANAR_EPS: f32 = 1e-4;

impl World {
    /// Whether patch scope is on (`0`).
    pub fn patch_scope(&self) -> bool {
        self.patch_scope
    }

    /// Toggle patch scope. Returns the new state.
    ///
    /// Clears any in-progress sub-face carve and the scrolled sub-rect: both were
    /// sized against the old scope's extent, so carrying them across a scope flip
    /// would grow a brush the author sized under different rules.
    pub fn toggle_patch_scope(&mut self) -> bool {
        if self.mode != Mode::Build {
            return self.patch_scope;
        }
        self.patch_scope = !self.patch_scope;
        self.reset_subface();
        // The count is only meaningful with a face picked. The crosshair tools (hole,
        // door) read the scope too but never set `selected`, so reporting "0 face(s)"
        // there would read as "the toggle did nothing" — which is exactly the wrong
        // thing to tell someone who just pressed the key.
        match (self.patch_scope, self.patch_len()) {
            (true, 0) => log::info!("selection scope: patch (aim at a face to see how many)"),
            (true, n) => log::info!("selection scope: patch ({n} face(s))"),
            (false, _) => log::info!("selection scope: single face"),
        }
        self.patch_scope
    }

    /// How many faces the current selection covers — 1 unless patch scope is on and
    /// the anchor's plane is shared. Drives the radial readout.
    pub fn patch_len(&self) -> usize {
        self.selected.map(|s| self.patch_ids(s).len()).unwrap_or(0)
    }

    /// The brush ids whose `(axis, side)` face this edit acts on.
    ///
    /// Always contains the anchor, and contains *only* the anchor when patch scope is
    /// off — which is what lets every caller run the patch path unconditionally and
    /// still behave exactly as it did before when the scope is off.
    ///
    /// A brush joins the anchor's patch when all of these hold:
    ///
    /// * it is in the same region;
    /// * it is reachable from the anchor by `find_room_brushes` — the editor's
    ///   existing notion of "the same room", which walks touching subtract carves and
    ///   stops at door/hole frames, so a patch never crosses a doorway;
    /// * its face on the same `(axis, side)` lies on the anchor's plane, within
    ///   [`COPLANAR_EPS`];
    /// * it is not a frame or a vent duct.
    ///
    /// The coplanarity rule turns out to be self-limiting in exactly the way you want.
    /// A stepped hall's carves share a *ceiling* plane but sit at different *floor*
    /// planes, so a ceiling patch is the whole hall while a floor patch is correctly
    /// just the one step you aimed at. A stairwell void's top sits at doorway height
    /// rather than room-ceiling height, so raising a ceiling never inflates it.
    pub(crate) fn patch_ids(&self, sel: Selection) -> Vec<u32> {
        let anchor_only = vec![sel.brush_id];
        if !self.patch_scope {
            return anchor_only;
        }
        // A sub-face carve in progress is its own thing, not a room. Repeated pushes
        // grow *it*; letting the room back in mid-carve would be a nasty surprise.
        // (Coplanarity already rules the room out — the carve's outward face has moved
        // off the plane it grew from — but this says so outright rather than relying on
        // that staying true.)
        if self.active.is_some() {
            return anchor_only;
        }
        let Some(region) = self.regions.iter().find(|r| r.id == sel.region_id) else {
            return anchor_only;
        };
        let Some(anchor) = region.brushes.iter().find(|b| b.id == sel.brush_id) else {
            return anchor_only;
        };
        // Patches are a *room* concept: the flood-fill walks subtract carves, so an
        // additive brush (the shell, a pulled protrusion) has no room to spread through
        // and stands alone. Frames and ducts are authored at a deliberate size and are
        // never swept along with the room around them.
        if anchor.op != Op::Subtract || anchor.frame || anchor.vent {
            return anchor_only;
        }
        let plane = anchor.face_pos(sel.axis, sel.side);
        let room = super::editing::find_room_brushes(anchor, &region.brushes);
        let ids: Vec<u32> = region
            .brushes
            .iter()
            .filter(|b| room.contains(&b.id))
            .filter(|b| b.op == Op::Subtract && !b.frame && !b.vent)
            .filter(|b| (b.face_pos(sel.axis, sel.side) - plane).abs() < COPLANAR_EPS)
            .map(|b| b.id)
            .collect();
        if ids.is_empty() {
            anchor_only
        } else {
            ids
        }
    }

    /// The selection's U/V extent, widened to the patch's **bounding rect** when patch
    /// scope is on.
    ///
    /// This is the single seam the whole sub-face half of the feature runs through:
    /// `adjust_selection_size`, `ensure_selection_bounds` and
    /// `update_selection_preview` all clamp the sub-rect against the extent this
    /// returns, so widening it here is what lets a hole be drawn across a brush seam.
    /// `position` and `scheme` stay the **anchor's** — every face in the patch is
    /// coplanar so the plane is shared, and a carve should inherit the theme of the
    /// face you actually aimed at.
    ///
    /// The bounding rect can be larger than the faces themselves (an L-shaped room's
    /// bbox covers the notch). That is allowed rather than refused: a rect hanging over
    /// the notch carves into the solid there, which is usually the point, and is no
    /// more dangerous than pushing a single-brush sub-face too deep — which the editor
    /// has always permitted.
    pub(crate) fn selected_patch_info(&self) -> Option<FaceInfo> {
        let base = self.selected_face_info()?;
        let sel = self.selected?;
        let [u_min, u_max, v_min, v_max] = self.patch_bounds(sel)?;
        Some(FaceInfo {
            u_min,
            u_max,
            v_min,
            v_max,
            u_size: u_max - u_min,
            v_size: v_max - v_min,
            ..base
        })
    }

    /// The U/V bounding rect `[u_min, u_max, v_min, v_max]` (WT) of the patch anchored
    /// at `sel`, or just that face's own rect when the patch is a single face.
    ///
    /// Takes the anchor as an argument rather than reading `World::selected`, because
    /// the crosshair tools do not use `selected` at all — the opening tool re-picks a
    /// face every frame and had its own copy of this bounds math, clamped to one brush.
    /// One shared helper is what keeps "how wide may this get" from meaning two
    /// different things in two tools.
    pub(crate) fn patch_bounds(&self, sel: Selection) -> Option<[f32; 4]> {
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let anchor = region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let (u_axis, v_axis) = sel.axis.orthogonals();
        let mut u_min = anchor.min(u_axis);
        let mut u_max = u_min + anchor.dim(u_axis);
        let mut v_min = anchor.min(v_axis);
        let mut v_max = v_min + anchor.dim(v_axis);
        let ids = self.patch_ids(sel);
        if ids.len() > 1 {
            for b in region.brushes.iter().filter(|b| ids.contains(&b.id)) {
                u_min = u_min.min(b.min(u_axis));
                u_max = u_max.max(b.min(u_axis) + b.dim(u_axis));
                v_min = v_min.min(b.min(v_axis));
                v_max = v_max.max(b.min(v_axis) + b.dim(v_axis));
            }
        }
        Some([u_min, u_max, v_min, v_max])
    }

    /// The highlight for a whole patch: one quad per member face, merged into a single
    /// mesh (the renderer has one highlight slot).
    ///
    /// Drawn instead of the bounding rect whenever the selection is full-patch, so an
    /// L-shaped room highlights as an L and the author can see the real scope of the
    /// next push. `None` when there's nothing to draw or the patch is a single face
    /// (the ordinary one-quad path covers that).
    pub(crate) fn patch_face_mesh(&self) -> Option<CpuMesh> {
        let sel = self.selected?;
        let ids = self.patch_ids(sel);
        if ids.len() <= 1 {
            return None;
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let info = self.selected_face_info()?;
        let mut merged: Option<CpuMesh> = None;
        for b in region.brushes.iter().filter(|b| ids.contains(&b.id)) {
            let u0 = b.min(info.u_axis);
            let v0 = b.min(info.v_axis);
            let quad = self.face_quad_mesh(
                sel.axis,
                sel.side,
                info.position,
                info.u_axis,
                info.v_axis,
                u0,
                u0 + b.dim(info.u_axis),
                v0,
                v0 + b.dim(info.v_axis),
            );
            match merged.as_mut() {
                None => merged = Some(quad),
                Some(m) => {
                    let base = m.vertices.len() as u32;
                    m.vertices.extend(quad.vertices);
                    m.indices.extend(quad.indices.iter().map(|i| i + base));
                }
            }
        }
        merged
    }
}
