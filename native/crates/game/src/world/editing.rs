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
        self.set_scheme_along(None, scheme)
    }

    /// Resolve a number key to a theme, preferring **this level's** binding.
    ///
    /// Falls back to the manifest's own `key` when the level hasn't bound the digit,
    /// so a fresh level still has the built-in quick picks. A binding naming a theme
    /// this build doesn't have (pruned, or authored elsewhere) is ignored rather than
    /// silently retexturing to something else.
    pub fn scheme_for_key(&self, key: char) -> Option<usize> {
        if let Some(name) = self.theme_hotkeys.get(&key) {
            if let Some(i) = engine::render::textures::scheme_index(name) {
                return Some(i);
            }
            log::warn!("hotkey {key} is bound to unknown theme {name:?}; ignoring");
        }
        engine::render::textures::scheme_for_key(key)
    }

    /// This level's hotkey bindings (digit → theme name).
    pub fn theme_hotkeys(&self) -> &std::collections::BTreeMap<char, String> {
        &self.theme_hotkeys
    }

    /// Bind a digit to a theme, or clear it with `None`. Digits outside `1..=9` are
    /// rejected — those are the only keys the retexture handler reads.
    pub fn set_theme_hotkey(&mut self, key: char, scheme: Option<usize>) {
        if !key.is_ascii_digit() || key == '0' {
            return;
        }
        match scheme {
            Some(i) => {
                let name = engine::render::textures::scheme_name(i).to_string();
                self.theme_hotkeys.insert(key, name);
            }
            None => {
                self.theme_hotkeys.remove(&key);
            }
        }
        self.bump_revision();
    }

    /// As [`set_scheme_at_crosshair`](Self::set_scheme_at_crosshair), but aimed by an
    /// explicit `(origin, dir)` ray when one is given.
    ///
    /// The theme picker panel needs this: opening a side panel frees the mouse
    /// cursor (so the list is clickable), which leaves the camera crosshair frozen
    /// wherever it last pointed. Retexturing has to follow the *cursor* instead.
    pub fn set_scheme_along(
        &mut self,
        ray: Option<(Vec3, Vec3)>,
        scheme: usize,
    ) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, _) = match ray {
            Some((o, d)) => self.pick_face_hit_from(o, d)?,
            None => self.pick_face_hit()?,
        };
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
            engine::render::textures::scheme_name(scheme),
            room_ids.len()
        );
        self.rebuild_affected_regions(&[sel.brush_id]).into_iter().next()
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
    /// With patch scope on, `info` is the whole patch's bounding rect, so the
    /// sub-rect can be scrolled out past the anchor brush's own edge — that widened
    /// clamp is the entire mechanism behind a hole that spans a brush seam.
    pub fn adjust_selection_size(&mut self, du: f32, dv: f32) {
        let Some(info) = self.selected_patch_info() else { return };
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
        let info = self.selected_patch_info()?;
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
    ///
    /// Returns a mesh **per changed region**, not one: a patch push can span regions
    /// (connected carves that only touch do not necessarily cluster together), and
    /// returning just the first left the others rendering stale.
    pub fn push(&mut self, step: f32) -> Vec<RegionMesh> {
        let Some(sel) = self.resolve_selection() else { return Vec::new() };

        if self.is_full_face() {
            // One face when patch scope is off, the whole coplanar patch when it is on.
            // Pushing every member the same distance along the shared normal preserves
            // both coplanarity and the members' in-plane adjacency, so the patch is
            // stable under its own operation and repeated presses cannot make it drift.
            let ids = self.patch_ids(sel);
            let Some(region) = self.regions.iter_mut().find(|r| r.id == sel.region_id) else {
                return Vec::new();
            };
            let mut moved = 0usize;
            for brush in region.brushes.iter_mut().filter(|b| ids.contains(&b.id)) {
                brush.push_face(sel.axis, sel.side, step);
                moved += 1;
            }
            if moved == 0 {
                return Vec::new();
            }
            if moved > 1 {
                log::info!("push: {moved} coplanar face(s) moved {step} WT");
            }
            self.active = None;
            return self.rebuild_affected_regions(&ids);
        }

        // Sub-face carve: grow the active push brush, or spawn one over the rect.
        if matches!(self.active, Some(a) if a.op == SubOp::Push) {
            self.grow_active_brush(step);
        } else {
            let Some(id) = self.create_sub_face_brush(Op::Subtract, step) else {
                return Vec::new();
            };
            self.active = Some(ActiveOp { brush_id: id, op: SubOp::Push, side: sel.side });
        }
        self.selected = self.active_outward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        let affected = self.active.map(|a| a.brush_id).unwrap_or(sel.brush_id);
        self.rebuild_affected_regions(&[affected])
    }

    /// Pull the selected face outward (JS `pullSelectedFace`). Full-face → shrink
    /// the brush directly (no-op if too thin). Sub-face → extend an additive brush
    /// (a protrusion) over the sub-rect, growing on repeat.
    ///
    /// A patch pull is **all-or-nothing**. `pull_face` refuses a brush too thin to
    /// absorb the step, and applying it to some members but not others would leave the
    /// patch no longer coplanar — the room quietly torn along the seam, and no longer
    /// selectable as one patch to undo it by hand. So every member is checked before
    /// any is moved, and a single thin member refuses the whole edit.
    ///
    /// Returns a mesh per changed region, for the same reason [`push`](Self::push) does.
    pub fn pull(&mut self, step: f32) -> Vec<RegionMesh> {
        let Some(sel) = self.resolve_selection() else { return Vec::new() };

        // Continue an active pull first (JS ordering).
        if matches!(self.active, Some(a) if a.op == SubOp::Pull) {
            self.grow_active_brush(step);
            self.selected = self.active_inward_face();
            let affected = self.active.map(|a| a.brush_id).unwrap_or(sel.brush_id);
            return self.rebuild_affected_regions(&[affected]);
        }

        if self.is_full_face() {
            let ids = self.patch_ids(sel);
            let Some(region) = self.regions.iter().find(|r| r.id == sel.region_id) else {
                return Vec::new();
            };
            let thin: Vec<u32> = region
                .brushes
                .iter()
                .filter(|b| ids.contains(&b.id) && b.dim(sel.axis) <= step)
                .map(|b| b.id)
                .collect();
            if !thin.is_empty() {
                log::info!(
                    "pull: brush(es) {thin:?} too thin along {:?} — whole patch refused",
                    sel.axis
                );
                return Vec::new();
            }
            let Some(region) = self.regions.iter_mut().find(|r| r.id == sel.region_id) else {
                return Vec::new();
            };
            let mut moved = 0usize;
            for brush in region.brushes.iter_mut().filter(|b| ids.contains(&b.id)) {
                brush.pull_face(sel.axis, sel.side, step);
                moved += 1;
            }
            if moved == 0 {
                return Vec::new();
            }
            if moved > 1 {
                log::info!("pull: {moved} coplanar face(s) moved {step} WT");
            }
            self.active = None;
            return self.rebuild_affected_regions(&ids);
        }

        // Sub-face extend.
        let Some(id) = self.create_sub_face_brush(Op::Add, step) else { return Vec::new() };
        self.active = Some(ActiveOp { brush_id: id, op: SubOp::Pull, side: sel.side });
        self.selected = self.active_inward_face();
        self.sel_size_u = 0.0;
        self.sel_size_v = 0.0;
        self.sel_bounds = None;
        self.rebuild_affected_regions(&[id])
    }

    /// Spawn a sub-face brush over the current sub-rect (JS `createSubFaceBrush`):
    /// `depth` deep along the face normal, anchored at the face plane. A subtract
    /// carves inward from the face; an add protrudes outward. Returns its id.
    ///
    /// **It wears the theme of the face it grew out of** (JS `createSubFaceBrush`
    /// does the same). This is the whole "carve a new room off this one" path, so
    /// without it every alcove, side room and protrusion the author pushes out of a
    /// themed room came back wearing the default theme, and had to be repainted by
    /// hand. The retexture flood-fill can't rescue it either: it stops at frames, so
    /// space carved beyond a doorway is a *different* room to the one it came from
    /// and never inherits after the fact.
    pub(crate) fn create_sub_face_brush(&mut self, op: Op, depth: f32) -> Option<u32> {
        let bounds = self.ensure_selection_bounds()?;
        let sel = self.selected?;
        // Patch info, so a carve may span the patch. It still carries the *anchor's*
        // plane and theme (every patch face is coplanar), so a spanning hole is one
        // box on the right plane wearing the theme of the face that was aimed at.
        let info = self.selected_patch_info()?;
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
        brush.scheme = info.scheme;
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
        // Full-patch: outline the member faces themselves. An L-shaped room then
        // highlights as an L rather than as the box around it, so what the next push
        // will move is never a guess.
        if self.is_full_face() {
            if let Some(m) = self.patch_face_mesh() {
                return Some(m);
            }
        }
        let info = self.selected_patch_info()?;
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
        // The crosshair has to still be on the selection for the ghost to show — but
        // with patch scope on, the *patch* is the selection, so looking at a different
        // member of it still counts. Without this the highlight blinked out the moment
        // you glanced from one bay of a hall's ceiling to the next, which reads as
        // "the selection was lost" when it very much wasn't.
        let on_patch = self.patch_scope
            && hit_sel.axis == sel.axis
            && hit_sel.side == sel.side
            && self.patch_ids(sel).contains(&hit_sel.brush_id);
        if !same_face(Some(sel), Some(hit_sel)) && !on_patch {
            return None;
        }
        let info = self.selected_patch_info()?;
        let s_u = if self.sel_size_u <= 0.0 { info.u_size } else { self.sel_size_u.min(info.u_size) };
        let s_v = if self.sel_size_v <= 0.0 { info.v_size } else { self.sel_size_v.min(info.v_size) };
        let u0 = (info.u_axis.component(hit_wt) - s_u / 2.0)
            .round()
            .clamp(info.u_min, info.u_max - s_u);
        let v0 = (info.v_axis.component(hit_wt) - s_v / 2.0)
            .round()
            .clamp(info.v_min, info.v_max - s_v);
        self.sel_bounds = Some([u0, u0 + s_u, v0, v0 + s_v]);
        // Unscrolled, the rect *is* the patch bbox, so show the member faces instead
        // (see `selection_face_mesh`). `sel_bounds` is still the bbox, which is what
        // keeps `is_full_face` true and routes the next push down the patch path.
        if self.is_full_face() {
            if let Some(m) = self.patch_face_mesh() {
                return Some(m);
            }
        }
        Some(self.face_quad_mesh(sel.axis, sel.side, info.position, info.u_axis, info.v_axis, u0, u0 + s_u, v0, v0 + s_v))
    }

    /// Re-evaluate a region: rebuild its collider in place and return its mesh.
    /// Logs the bake time — the Phase 1 "does authoring feel instant?" signal.
    pub(crate) fn rebuild_region(&mut self, region_id: u32) -> Option<RegionMesh> {
        let idx = self.regions.iter().position(|r| r.id == region_id)?;
        self.regions[idx].refresh_shell();
        // Memoize by the region's authored data: undo/redo/load re-bakes only the
        // region that actually changed; unchanged regions hit the cache and skip
        // the fold (JS `wasmResultCache`).
        // Only the platforms the band probe is allowed to see — none unless the level
        // says decks are floors. Cloned rather than borrowed because `evaluate_both`
        // takes the region mutably; a handful of `Copy` slabs is nothing beside a CSG
        // fold. Hashed as well as baked, so flipping the toggle misses the memo cache.
        let platforms = self.band_platforms().to_vec();
        let key = super::regions::region_hash(&self.regions[idx], &platforms);
        let (collider, tex) = if let Some((c, t)) = self.csg_cache.get(key) {
            (c.clone(), t.clone())
        } else {
            let t0 = Instant::now();
            // One CSG fold, both outputs derived from it (was two full folds).
            let (c, t) = self.regions[idx].evaluate_both(&platforms);
            let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
            log::info!(
                "region {region_id} re-baked in {bake_ms:.2} ms ({} tris)",
                c.indices.len() / 3
            );
            self.csg_cache.insert(key, (c.clone(), t.clone()));
            (c, t)
        };
        self.physics.set_region_collider(region_id, &collider);
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
