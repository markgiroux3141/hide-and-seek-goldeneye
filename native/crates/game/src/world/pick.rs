//! Face picking + selection on `World`: crosshair raycast → dominant-axis
//! face resolve, the selected-face UV info, and full-face detection.

use super::*;

impl World {
    pub fn select_at_crosshair(&mut self) -> bool {
        if self.mode != Mode::Build {
            return false;
        }
        let picked = self.pick_face();
        let changed = !same_face(self.selected, picked);
        self.selected = picked;
        if changed {
            self.reset_subface();
        }
        self.selected.is_some()
    }

    /// The selected face resolved from state, or a fresh crosshair pick if
    /// nothing is selected yet (so `+`/`-` work without an explicit click).
    pub(crate) fn resolve_selection(&mut self) -> Option<Selection> {
        if self.mode != Mode::Build {
            return None; // no authoring during the hunt
        }
        if self.selected.is_none() {
            self.selected = self.pick_face();
        }
        self.selected
    }

    /// The selected face's U/V extent (JS `getFaceUVInfo`). `None` if nothing is
    /// selected (or the brush is gone).
    pub(crate) fn selected_face_info(&self) -> Option<FaceInfo> {
        let sel = self.selected?;
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let (u_axis, v_axis) = sel.axis.orthogonals();
        let u_min = brush.min(u_axis);
        let v_min = brush.min(v_axis);
        let u_max = u_min + brush.dim(u_axis);
        let v_max = v_min + brush.dim(v_axis);
        Some(FaceInfo {
            u_axis,
            v_axis,
            u_min,
            u_max,
            v_min,
            v_max,
            u_size: u_max - u_min,
            v_size: v_max - v_min,
            position: brush.face_pos(sel.axis, sel.side),
        })
    }

    /// Whether the current selection covers the whole face (JS `isFullFace`) —
    /// i.e. no sub-rect has been scrolled in. Push/pull then resize the brush
    /// directly instead of spawning a sub-face brush.
    pub(crate) fn is_full_face(&self) -> bool {
        match self.selected_face_info() {
            None => true,
            Some(info) => {
                (self.sel_size_u <= 0.0 || self.sel_size_u >= info.u_size)
                    && (self.sel_size_v <= 0.0 || self.sel_size_v >= info.v_size)
            }
        }
    }

    /// Raycast the crosshair against the collision world and resolve which brush
    /// face was hit (dropping the hit point). See [`pick_face_hit`](Self::pick_face_hit).
    pub(crate) fn pick_face(&mut self) -> Option<Selection> {
        self.pick_face_hit().map(|(sel, _)| sel)
    }

    /// Raycast the crosshair against the collision world and resolve which brush
    /// face was hit, plus the hit point in WT. Uses geometric matching (like JS
    /// `buildFaceMap`): find the brush face plane the hit point lies on, ignoring
    /// op-dependent normal sign. The WT hit point is what the door-cut tool
    /// centers its opening on.
    pub(crate) fn pick_face_hit(&mut self) -> Option<(Selection, Vec3)> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let hit = self.physics.raycast(origin, dir, 100.0)?;

        // Dominant axis of the surface normal.
        let axis = Axis::dominant(hit.normal);

        // Hit point in WT space.
        let hit_wt = hit.point / WORLD_SCALE;
        let hit_a = axis.component(hit_wt);
        let (u_axis, v_axis) = axis.orthogonals();
        let hit_u = u_axis.component(hit_wt);
        let hit_v = v_axis.component(hit_wt);

        // WT tolerances: on-plane match tight, in-rect containment lenient.
        const PLANE_EPS: f32 = 0.15;
        const RECT_EPS: f32 = 0.15;

        let region = &self.regions[0]; // Phase 1: single region.
        let mut best: Option<(u32, Side, f32)> = None;
        for b in &region.brushes {
            for side in [Side::Min, Side::Max] {
                let plane = b.face_pos(axis, side);
                let d = (plane - hit_a).abs();
                if d > PLANE_EPS {
                    continue;
                }
                // Hit point must lie within the face's other-axes extent.
                let (u0, u1) = (b.min(u_axis), b.min(u_axis) + b.dim(u_axis));
                let (v0, v1) = (b.min(v_axis), b.min(v_axis) + b.dim(v_axis));
                if hit_u < u0 - RECT_EPS
                    || hit_u > u1 + RECT_EPS
                    || hit_v < v0 - RECT_EPS
                    || hit_v > v1 + RECT_EPS
                {
                    continue;
                }
                if best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
                    best = Some((b.id, side, d));
                }
            }
        }

        best.map(|(brush_id, side, _)| {
            (
                Selection {
                    region_id: region.id,
                    brush_id,
                    axis,
                    side,
                },
                hit_wt,
            )
        })
    }
}

/// Whether two selections point at the same brush face.
pub(crate) fn same_face(a: Option<Selection>, b: Option<Selection>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.region_id == b.region_id && a.brush_id == b.brush_id && a.axis == b.axis && a.side == b.side
        }
        _ => false,
    }
}

/// The opposite side of an axis.
pub(crate) fn flip(side: Side) -> Side {
    match side {
        Side::Min => Side::Max,
        Side::Max => Side::Min,
    }
}
