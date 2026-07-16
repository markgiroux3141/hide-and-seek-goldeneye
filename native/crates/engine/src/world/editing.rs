//! Core CSG editing on `World`: push/pull (full-face + sub-face), the
//! sub-face carve/extrude machinery, per-room retexture flood-fill, and
//! region re-bake.

use super::*;

impl World {
    /// Select the face under the crosshair (left-click). Returns `true` if a
    /// face was hit. The selection persists and drives push/pull + the highlight.
    /// Picking a *different* face resets sub-face sizing and any active carve
    /// (JS `selectFaceAtCrosshair`).
    /// Retexture the room under the crosshair (JS `retextureRoom`): flood-fill from
    /// the picked face's owner brush across connected subtract brushes, **stopping
    /// at door/hole frames**, and set every reached brush (and any stair whose
    /// voids they include) to `scheme`. So a door bounds a room — the room beyond
    /// keeps its own scheme. Re-bakes and returns the region mesh, or `None` if the
    /// crosshair isn't on a retexturable room face.
    pub fn set_scheme_at_crosshair(&mut self, scheme: usize) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, _) = self.pick_face_hit()?;
        let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
        let start = region.brushes.iter().find(|b| b.id == sel.brush_id).copied()?;
        // A frame face isn't a room (JS returns) — don't let a doorway retexture.
        if start.frame {
            return None;
        }
        let room_ids = find_room_brushes(&start, &region.brushes);
        for b in region.brushes.iter_mut() {
            if room_ids.contains(&b.id) {
                b.scheme = scheme;
            }
        }
        // Stairs carved in this room re-scheme with it (their tread mesh follows).
        for s in region.stairs.iter_mut() {
            if s.void_ids.iter().any(|id| room_ids.contains(id)) {
                s.scheme = scheme;
            }
        }
        log::info!(
            "room retexture: region {} -> {} ({} brush(es))",
            sel.region_id,
            crate::textures::SCHEMES[scheme].name,
            room_ids.len()
        );
        self.rebuild_region(sel.region_id)
    }

    /// Clear sub-face selection sizing + any in-progress carve, and drop any
    /// pending stair op (it was anchored to the old face). Mirrors the resets in
    /// JS `selectFaceAtCrosshair`.
    pub(crate) fn reset_subface(&mut self) {
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.active = None;
        self.pending_stair = None;
    }

    /// Scroll-wheel handler (JS `adjustSelectionSize`): shrink/grow the sub-rect
    /// on the selected face. `du`/`dv` are ±1 WT steps (scroll = U, Shift = V).
    /// Starts from full size, clamps to `[1, faceSize]`, and cancels any active
    /// carve so the next push spawns fresh.
    pub fn adjust_selection_size(&mut self, du: f32, dv: f32) {
        let Some(info) = self.selected_face_info() else { return };
        if du != 0.0 {
            if self.sel_size_u <= 0.0 {
                self.sel_size_u = info.u_size;
            }
            self.sel_size_u = (self.sel_size_u + du).clamp(1.0, info.u_size);
        }
        if dv != 0.0 {
            if self.sel_size_v <= 0.0 {
                self.sel_size_v = info.v_size;
            }
            self.sel_size_v = (self.sel_size_v + dv).clamp(1.0, info.v_size);
        }
        self.active = None;
    }

    /// The sub-rect `[u0, u1, v0, v1]` to carve. Uses the crosshair-tracked
    /// bounds if the preview has run this frame, else a face-centered fallback
    /// (JS `ensureSelectionBounds`).
    pub(crate) fn ensure_selection_bounds(&mut self) -> Option<[f32; 4]> {
        if let Some(b) = self.sel_bounds {
            return Some(b);
        }
        let info = self.selected_face_info()?;
        let s_u = if self.sel_size_u <= 0.0 { info.u_size } else { self.sel_size_u.min(info.u_size) };
        let s_v = if self.sel_size_v <= 0.0 { info.v_size } else { self.sel_size_v.min(info.v_size) };
        let u0 = info.u_min + ((info.u_size - s_u) / 2.0).round();
        let v0 = info.v_min + ((info.v_size - s_v) / 2.0).round();
        let b = [u0, u0 + s_u, v0, v0 + s_v];
        self.sel_bounds = Some(b);
        Some(b)
    }

    /// Push the selected face inward (JS `pushSelectedFace`). Full-face → resize
    /// the brush directly (whole wall moves). Sub-face (a sub-rect scrolled in) →
    /// carve a subtract brush over the sub-rect, growing deeper on repeat.
    /// Returns the changed region's mesh, or `None`.
    pub fn push(&mut self, step: f32) -> Option<RegionMesh> {
        let sel = self.resolve_selection()?;

        if self.is_full_face() {
            let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
            let brush = region.brushes.iter_mut().find(|b| b.id == sel.brush_id)?;
            brush.push_face(sel.axis, sel.side, step);
            self.active = None;
            return self.rebuild_region(sel.region_id);
        }

        // Sub-face carve: grow the active push brush, or spawn one over the rect.
        if matches!(self.active, Some(a) if a.op == SubOp::Push) {
            self.grow_active_brush(step);
        } else {
            let id = self.create_sub_face_brush(Op::Subtract, step)?;
            self.active = Some(ActiveOp { brush_id: id, op: SubOp::Push, side: sel.side });
        }
        self.selected = self.active_outward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.rebuild_region(sel.region_id)
    }

    /// Pull the selected face outward (JS `pullSelectedFace`). Full-face → shrink
    /// the brush directly (no-op if too thin). Sub-face → extend an additive brush
    /// (a protrusion) over the sub-rect, growing on repeat.
    pub fn pull(&mut self, step: f32) -> Option<RegionMesh> {
        let sel = self.resolve_selection()?;

        // Continue an active pull first (JS ordering).
        if matches!(self.active, Some(a) if a.op == SubOp::Pull) {
            self.grow_active_brush(step);
            self.selected = self.active_inward_face();
            return self.rebuild_region(sel.region_id);
        }

        if self.is_full_face() {
            let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
            let brush = region.brushes.iter_mut().find(|b| b.id == sel.brush_id)?;
            if !brush.pull_face(sel.axis, sel.side, step) {
                log::info!("pull: brush {} too thin along {:?} — no-op", sel.brush_id, sel.axis);
                return None;
            }
            self.active = None;
            return self.rebuild_region(sel.region_id);
        }

        // Sub-face extend.
        let id = self.create_sub_face_brush(Op::Add, step)?;
        self.active = Some(ActiveOp { brush_id: id, op: SubOp::Pull, side: sel.side });
        self.selected = self.active_inward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.rebuild_region(sel.region_id)
    }

    /// Spawn a sub-face brush over the current sub-rect (JS `createSubFaceBrush`):
    /// `depth` deep along the face normal, anchored at the face plane. A subtract
    /// carves inward from the face; an add protrudes outward. Returns its id.
    pub(crate) fn create_sub_face_brush(&mut self, op: Op, depth: f32) -> Option<u32> {
        let bounds = self.ensure_selection_bounds()?;
        let sel = self.selected?;
        let info = self.selected_face_info()?;
        let position = info.position;
        let [u0, u1, v0, v1] = bounds;
        let a = match op {
            Op::Subtract => if sel.side == Side::Max { position } else { position - depth },
            Op::Add => if sel.side == Side::Max { position - depth } else { position },
        };
        let id = self.next_brush_id;
        let mut brush = make_wall_brush(
            id, sel.axis, a, depth, info.u_axis, u0, u1 - u0, info.v_axis, v0, v1 - v0,
        );
        brush.op = op;
        let region = self.regions.iter_mut().find(|r| r.id == sel.region_id)?;
        region.brushes.push(brush);
        self.next_brush_id += 1;
        Some(id)
    }

    /// Grow the active sub-face brush by `amount` (JS `growActiveBrush`, push/pull
    /// cases). A push grows on the face side; a pull grows on the opposite side
    /// (deeper into the room). Reuses `Brush::push_face`, which encodes exactly
    /// that min/dim math.
    pub(crate) fn grow_active_brush(&mut self, amount: f32) {
        let Some(active) = self.active else { return };
        let Some(sel) = self.selected else { return };
        let grow_side = match active.op {
            SubOp::Push => active.side,
            SubOp::Pull => flip(active.side),
        };
        if let Some(brush) = self
            .regions
            .iter_mut()
            .flat_map(|r| r.brushes.iter_mut())
            .find(|b| b.id == active.brush_id)
        {
            brush.push_face(sel.axis, grow_side, amount);
        }
    }

    /// The active brush's outward face (JS `getActiveBrushOutwardFace`) — where
    /// the selection follows to after a sub-face push.
    pub(crate) fn active_outward_face(&self) -> Option<Selection> {
        let active = self.active?;
        let sel = self.selected?;
        Some(Selection {
            region_id: sel.region_id,
            brush_id: active.brush_id,
            axis: sel.axis,
            side: active.side,
        })
    }

    /// The active brush's inward face (JS `getActiveBrushInwardFace`).
    pub(crate) fn active_inward_face(&self) -> Option<Selection> {
        let active = self.active?;
        let sel = self.selected?;
        Some(Selection {
            region_id: sel.region_id,
            brush_id: active.brush_id,
            axis: sel.axis,
            side: flip(active.side),
        })
    }

    /// Build the highlight quad (in meters) for the current selection — the
    /// scrolled-in sub-rect if one exists, else the full face. Used for immediate
    /// post-edit feedback; the crosshair-tracked version is
    /// [`update_selection_preview`](Self::update_selection_preview). `None` when
    /// nothing is selected.
    pub fn selection_face_mesh(&self) -> Option<CpuMesh> {
        let sel = self.selected?;
        let info = self.selected_face_info()?;
        let [u0, u1, v0, v1] = self
            .sel_bounds
            .unwrap_or([info.u_min, info.u_max, info.v_min, info.v_max]);
        Some(self.face_quad_mesh(sel.axis, sel.side, info.position, info.u_axis, info.v_axis, u0, u1, v0, v1))
    }

    /// Recompute the selection sub-rect from the crosshair (JS csgPreviews
    /// `updateSelectionPreview`): while looking at the selected face, center a
    /// `sel_size_u × sel_size_v` rect on the crosshair (full face when unscrolled),
    /// clamp it, store the bounds, and return the ghost quad. `None` when not
    /// looking at the selected face — so the highlight hides (matching JS).
    pub fn update_selection_preview(&mut self) -> Option<CpuMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let sel = self.selected?;
        let (hit_sel, hit_wt) = self.pick_face_hit()?;
        if !same_face(Some(sel), Some(hit_sel)) {
            return None;
        }
        let info = self.selected_face_info()?;
        let s_u = if self.sel_size_u <= 0.0 { info.u_size } else { self.sel_size_u.min(info.u_size) };
        let s_v = if self.sel_size_v <= 0.0 { info.v_size } else { self.sel_size_v.min(info.v_size) };
        let u0 = (info.u_axis.component(hit_wt) - s_u / 2.0)
            .round()
            .clamp(info.u_min, info.u_max - s_u);
        let v0 = (info.v_axis.component(hit_wt) - s_v / 2.0)
            .round()
            .clamp(info.v_min, info.v_max - s_v);
        self.sel_bounds = Some([u0, u0 + s_u, v0, v0 + s_v]);
        Some(self.face_quad_mesh(sel.axis, sel.side, info.position, info.u_axis, info.v_axis, u0, u0 + s_u, v0, v0 + s_v))
    }

    /// Re-evaluate a region: rebuild its collider in place and return its mesh.
    /// Logs the bake time — the Phase 1 "does authoring feel instant?" signal.
    pub(crate) fn rebuild_region(&mut self, region_id: u32) -> Option<RegionMesh> {
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        let t0 = Instant::now();
        let mesh = region.evaluate();
        let tex = region.evaluate_textured();
        let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.physics.set_region_collider(region_id, &mesh);
        log::info!(
            "region {region_id} re-baked in {bake_ms:.2} ms ({} tris)",
            mesh.indices.len() / 3
        );
        Some(RegionMesh { id: region_id, mesh: tex })
    }
}

/// Flood-fill the connected room a brush belongs to (JS `findRoomBrushes`):
/// connected subtract brushes that touch, stopping at door/hole frames. Returns
/// the set of brush ids in the room (including the start).
pub(crate) fn find_room_brushes(start: &Brush, brushes: &[Brush]) -> std::collections::HashSet<u32> {
    let mut room = std::collections::HashSet::new();
    room.insert(start.id);
    let mut queue = vec![*start];
    while let Some(cur) = queue.pop() {
        for other in brushes {
            if room.contains(&other.id) {
                continue;
            }
            if other.op != Op::Subtract || other.frame {
                continue; // frames bound the room
            }
            if brushes_touching(&cur, other) {
                room.insert(other.id);
                queue.push(*other);
            }
        }
    }
    room
}

/// Two brushes touch if they overlap on two axes and are face-adjacent on the
/// third (JS `brushesTouching`, spike line 510). WT coords are grid-aligned so
/// adjacency is an exact edge match (small epsilon for float slop).
pub(crate) fn brushes_touching(a: &Brush, b: &Brush) -> bool {
    let span = |br: &Brush, i: usize| match i {
        0 => (br.x, br.x + br.w),
        1 => (br.y, br.y + br.h),
        _ => (br.z, br.z + br.d),
    };
    const EPS: f32 = 1e-4;
    for i in 0..3 {
        let (a_min, a_max) = span(a, i);
        let (b_min, b_max) = span(b, i);
        if (a_max - b_min).abs() < EPS || (b_max - a_min).abs() < EPS {
            let mut overlap = true;
            for j in 0..3 {
                if j == i {
                    continue;
                }
                let (a0, a1) = span(a, j);
                let (b0, b1) = span(b, j);
                if a1 <= b0 || b1 <= a0 {
                    overlap = false;
                    break;
                }
            }
            if overlap {
                return true;
            }
        }
    }
    false
}
