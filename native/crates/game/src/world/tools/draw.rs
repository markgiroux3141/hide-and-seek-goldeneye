//! Freeform 90°-snapped draw tool: click out an arbitrary rectilinear outline on a
//! room surface, close the loop, then extrude it out of the face or inset it into
//! the face.
//!
//! This is not a new CSG operation — it is a better *selection primitive*. The
//! sub-face push/pull in [`super::super::editing`] already turns a rectangle on a face
//! into `Op::Add` (a protrusion) or `Op::Subtract` (a carve) via
//! [`World::create_sub_face_brush`]; all that was missing was a way to select
//! something other than one scroll-sized rectangle. So the outline here replaces the
//! rectangle and *nothing downstream changes*: the fold, the UV classifier, nav,
//! picking, region clustering, undo and the save format all run untouched.
//!
//! **Why the outline is decomposed into rectangles.** The BSP kernel's mesh output,
//! [`csg::polygons_to_mesh`], fan-triangulates, which is only valid for convex
//! polygons. An L- or U-shaped footprint is concave, so pushing it through as a single
//! N-gon prism yields both garbage triangles and an unreliable BSP split. Teaching
//! `Brush` about extruded polygons instead would mean touching triangulation,
//! `contains()` for nav, the AABB early-reject, the classifier's `face_owner`,
//! `pick_face_hit` and serde — the whole stack. Decomposing in the tool costs a
//! zero-line engine diff, and because every segment is 90°-snapped and grid-aligned
//! the partition is **exact**, not an approximation. The brushes stay ordinary AABBs.
//!
//! The decomposition is a rasterize-then-merge (see [`rect_decompose`]) rather than a
//! sweep-line partition: grid alignment makes rasterizing exact, concave corners need
//! no special handling, and it is a fraction of the code.

use super::super::*;

/// Max extrusion depth in WT the scroll wheel will reach, either direction. Generous
/// — this is a guard against a runaway scroll, not a design limit.
const DRAW_DEPTH_MAX: f32 = 32.0;

/// Half-width (WT) of the ghost outline's bands, and the half-size of the little
/// square drawn at each committed corner.
const GHOST_BAND_HALF: f32 = 0.07;
const GHOST_CORNER_HALF: f32 = 0.16;

/// How far off the surface the ghost quads sit, in WT. The outline sits proud of its own
/// tint so it always reads on top of it (both pipelines have depth writes off, so this is
/// about blend-order legibility, not z-fighting).
const OUTLINE_NUDGE: f32 = 0.06;
const TINT_NUDGE: f32 = 0.03;

/// Cell budget for [`rect_decompose`]. An outline's bounding box is rasterized at 1
/// WT, so this is a ~512×512 WT footprint — far past anything hand-drawable, and it
/// keeps a mis-projected outline from allocating wildly.
const DRAW_MAX_CELLS: usize = 1 << 18;

impl World {
    // ─── Arming / teardown ───────────────────────────────────────────────────

    /// Whether the draw tool is armed (the app routes clicks/scroll/ghost to it and
    /// Esc backs out of its phases).
    pub fn is_draw_tool(&self) -> bool {
        self.draw_phase.is_some()
    }

    /// Whether the tool is in its depth step, so the app routes the scroll wheel to
    /// the signed extrusion depth instead of the sub-face selection.
    pub fn is_draw_sizing(&self) -> bool {
        self.draw_phase == Some(DrawPhase::Depth)
    }

    /// Draw tool key (`Q`): arm/toggle. Arming disarms the other modal authoring
    /// tools and drops any face selection, so the crosshair tools stay mutually
    /// exclusive.
    pub fn draw_tool_key(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.draw_phase.is_some() {
            self.clear_draw_state();
            log::info!("draw tool off");
            return;
        }
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.selected = None;
        self.draw_phase = Some(DrawPhase::Idle);
        self.draw_depth = 0.0;
        self.draw_candidate = 0;
        log::info!("draw tool armed — click a room surface to drop the first corner");
    }

    /// Disarm the draw tool (Esc at the top of the ladder / pointer release / mode
    /// switch).
    pub fn cancel_draw(&mut self) {
        self.clear_draw_state();
    }

    /// Clear every scrap of draw state, turning the tool off.
    pub(crate) fn clear_draw_state(&mut self) {
        self.draw_phase = None;
        self.draw_face = None;
        self.draw_verts.clear();
        self.draw_cursor = None;
        self.draw_rects.clear();
        self.draw_depth = 0.0;
        self.draw_candidate = 0;
    }

    /// Esc while the draw tool is armed: walk one rung back down the ladder — depth
    /// step → outline, outline → drop the last corner, last corner → the idle tool.
    /// Returns whether the Esc was consumed; `false` lets the app fall through to
    /// releasing the pointer (which disarms the tool wholesale).
    pub fn draw_escape(&mut self) -> bool {
        match self.draw_phase {
            Some(DrawPhase::Depth) => {
                // Reopen the outline. The vertices are kept — the closing click never
                // pushed a duplicate of the first one — so re-closing is one click.
                self.draw_rects.clear();
                self.draw_depth = 0.0;
                self.draw_phase = Some(DrawPhase::Drawing);
                log::info!("draw: outline reopened");
                true
            }
            Some(DrawPhase::Drawing) => {
                self.draw_verts.pop();
                if self.draw_verts.is_empty() {
                    self.draw_face = None;
                    self.draw_cursor = None;
                    self.draw_phase = Some(DrawPhase::Idle);
                }
                true
            }
            // Nothing drawn yet: let the pointer release disarm us, matching the
            // platform tool's idle behaviour.
            Some(DrawPhase::Idle) | None => false,
        }
    }

    // ─── Clicks ──────────────────────────────────────────────────────────────

    /// Handle a left-click while the draw tool is armed, dispatching on the phase.
    /// Returns **every** rebuilt region mesh for the app to upload.
    ///
    /// A `Vec` and not the `Option<RegionMesh>` the other tools return: this tool adds
    /// N brushes at once, across a footprint deliberately drawn up against walls, so
    /// it routinely bridges brushes that were in separate regions.
    /// [`World::rebuild_affected_regions`] answers that with a full recluster and hands
    /// back a mesh per region (plus empty ones for ids that vanished) — dropping all
    /// but the first, as the single-mesh tools do, would leave stale geometry drawn.
    pub fn draw_click(&mut self) -> Vec<RegionMesh> {
        match self.draw_phase {
            Some(DrawPhase::Idle) => {
                self.draw_begin();
                Vec::new()
            }
            Some(DrawPhase::Drawing) => {
                self.draw_place_vertex();
                Vec::new()
            }
            Some(DrawPhase::Depth) => self.confirm_draw(),
            None => Vec::new(),
        }
    }

    /// First click: freeze the face under the crosshair and drop the first corner.
    fn draw_begin(&mut self) {
        let Some(face) = self.selected_draw_face() else {
            log::info!("draw: aim at a room surface (floor, ceiling or wall) to start");
            return;
        };
        let Some(v) = project_to_face(&face, self.camera.pos, self.camera.forward()) else {
            return;
        };
        self.draw_face = Some(face);
        self.draw_verts.clear();
        self.draw_verts.push(v);
        self.draw_cursor = Some(v);
        self.draw_phase = Some(DrawPhase::Drawing);
        log::info!(
            "draw: corner at ({}, {}) — click to add corners, click the first corner to close (Esc undoes one)",
            v.0,
            v.1
        );
    }

    /// Subsequent click: commit the previewed corner, or close the loop when it lands
    /// back on the first one.
    fn draw_place_vertex(&mut self) {
        let Some(cand) = self.draw_cursor else { return };
        let Some(&last) = self.draw_verts.last() else { return };
        if cand == last {
            return; // the crosshair hasn't left the last corner
        }
        // A rectilinear loop needs at least 4 corners, so anything shorter landing on
        // the first one is a stray click, not a close.
        let closing = self.draw_verts.len() >= 4 && cand == self.draw_verts[0];
        if segment_self_intersects(&self.draw_verts, cand, closing) {
            log::info!("draw: that segment would cross the outline — pick another corner");
            return;
        }
        if !closing {
            self.draw_verts.push(cand);
            return;
        }
        let Some(face) = self.draw_face.clone() else { return };
        // Clip to the real surface. Corners were only clamped to the group's bounding
        // box, so an outline drawn across an L-shaped surface's missing corner would
        // otherwise carve into solid rock.
        let rects = rect_decompose_where(&self.draw_verts, |u, v| face.covers_cell(u, v));
        if rects.is_empty() {
            log::info!("draw: that outline encloses no area on the surface — nothing to build");
            return;
        }
        let drawn: i32 = rect_decompose(&self.draw_verts)
            .iter()
            .map(|&(_, _, w, h)| w * h)
            .sum();
        let kept: i32 = rects.iter().map(|&(_, _, w, h)| w * h).sum();
        if kept < drawn {
            log::info!(
                "draw: {} of {drawn} WT² fell outside the surface and was trimmed",
                drawn - kept
            );
        }
        log::info!(
            "draw: outline closed ({} corners → {} rect(s)) — scroll to set depth (up = out of the face, down = into it), click to build",
            self.draw_verts.len(),
            rects.len()
        );
        self.draw_rects = rects;
        // Start at a 1 WT extrusion rather than 0 so the click straight after closing
        // already builds something sensible; scrolling down runs through 0 into insets.
        self.draw_depth = 1.0;
        self.draw_phase = Some(DrawPhase::Depth);
    }

    /// Scroll the signed extrusion depth (depth step only): up protrudes out of the
    /// face, down sinks into it, passing through 0 (which builds nothing).
    pub fn adjust_draw_depth(&mut self, step: f32) {
        if self.draw_phase != Some(DrawPhase::Depth) {
            return;
        }
        self.draw_depth = (self.draw_depth + step).clamp(-DRAW_DEPTH_MAX, DRAW_DEPTH_MAX);
    }

    /// Commit the drawn shape: add the decomposed brushes to the face's region and
    /// re-bake. Returns every changed region mesh (see [`draw_click`](Self::draw_click)).
    pub(crate) fn confirm_draw(&mut self) -> Vec<RegionMesh> {
        let Some(face) = self.draw_face.clone() else {
            return Vec::new();
        };
        let mut brushes = self.draw_brushes(&face);
        if brushes.is_empty() {
            log::info!("draw: depth is 0 — scroll to set one");
            return Vec::new();
        }
        let op = brushes[0].op;
        let depth = self.draw_depth.abs();
        // Bail before touching the id allocator, so a vanished region can't leave a gap
        // in the brush ids.
        if !self.regions.iter().any(|r| r.id == face.region_id) {
            return Vec::new();
        }

        // The group id is the first brush's id: brush ids are unique and monotonic, so
        // that is collision-free without a second allocator to thread through
        // snapshot / save / load.
        let group = self.next_brush_id;
        let mut ids = Vec::with_capacity(brushes.len());
        for b in brushes.iter_mut() {
            b.id = self.next_brush_id;
            b.group = group;
            self.next_brush_id += 1;
            ids.push(b.id);
        }
        let Some(region) = self.regions.iter_mut().find(|r| r.id == face.region_id) else {
            return Vec::new();
        };
        region.brushes.extend(brushes.iter().copied());

        log::info!(
            "draw: {} {} brush(es) as group {group} in region {} ({:?} face, depth {depth} WT)",
            brushes.len(),
            if op == Op::Add { "extruded" } else { "inset" },
            face.region_id,
            face.axis,
        );
        // Enemies move on grid-nav with no gravity and climb at most one WT between
        // cells (`nav::MAX_STEP`), so a taller raised floor is scenery to a hunter but
        // a step to the player. That's a design choice, not a bug — but surface it, or
        // the author can't tell they just made a player-only shortcut.
        if op == Op::Add && face.axis == Axis::Y && face.side == Side::Min && depth > 1.0 {
            log::info!(
                "draw: {depth} WT step — hunters climb 1 WT, so this is player-only unless you add stairs"
            );
        }

        // Stay armed, back at the idle phase, rather than disarming as the one-shot
        // placement tools do. Drawing is a throughput tool — the large bases this
        // exists for mean authoring many raised sections in a row, and needing Q
        // between each would fight that. Q toggles off; Esc from idle disarms.
        self.draw_face = None;
        self.draw_verts.clear();
        self.draw_cursor = None;
        self.draw_rects.clear();
        self.draw_depth = 0.0;
        self.draw_phase = Some(DrawPhase::Idle);
        self.rebuild_affected_regions(&ids)
    }

    // ─── Geometry (shared by the ghost and the commit) ────────────────────────

    /// The brushes the current outline + depth would build — one per decomposed rect,
    /// fully configured but with `id`/`group` unassigned.
    ///
    /// Deliberately the *single* source for both the depth-step ghost and the commit,
    /// so the preview cannot drift from what actually lands. Empty at depth 0.
    fn draw_brushes(&self, face: &DrawFace) -> Vec<Brush> {
        let d = self.draw_depth.abs();
        if d <= 0.0 || self.draw_rects.is_empty() {
            return Vec::new();
        }
        let op = if self.draw_depth > 0.0 { Op::Add } else { Op::Subtract };
        // Face-anchored min corner along the normal — the same anchor
        // `create_sub_face_brush` uses, so a drawn extrude lands exactly where a
        // sub-face pull would. For a room void, "into the room" is −axis off a Max face
        // and +axis off a Min face, which is why `Op::Add` (protrude inward) and
        // `Op::Subtract` (carve outward through the surface) anchor on opposite sides.
        let a = match op {
            Op::Subtract => {
                if face.side == Side::Max {
                    face.position
                } else {
                    face.position - d
                }
            }
            Op::Add => {
                if face.side == Side::Max {
                    face.position - d
                } else {
                    face.position
                }
            }
        };
        let mut brushes: Vec<Brush> = self
            .draw_rects
            .iter()
            .map(|&(u, v, w, h)| {
                let mut b = make_wall_brush(
                    0,
                    face.axis,
                    a,
                    d,
                    face.u_axis,
                    u as f32,
                    w as f32,
                    face.v_axis,
                    v as f32,
                    h as f32,
                );
                b.op = op;
                b.scheme = face.scheme;
                b
            })
            .collect();

        // **One** wall-texture floor anchor for the whole shape, not per-brush.
        // `uv_zones::face_owner` attributes each triangle to the smallest brush whose
        // face plane it lies on and reads *that* brush's `floor_y` as the wall-UV
        // origin. Left at the per-brush default (`Brush::new` sets `floor_y = y`), an
        // L-shaped alcove drawn on a wall would give its rects different anchors and
        // texture-shift them against each other — a visible seam along an internal
        // decomposition boundary the author never drew. The lowest brush wins, which
        // for a floor extrude or a pit is the surface itself.
        let floor_y = brushes.iter().map(|b| b.y).fold(f32::INFINITY, f32::min);
        for b in brushes.iter_mut() {
            b.floor_y = floor_y;
        }
        brushes
    }

    /// Every room surface the crosshair's hit point lies on, best first.
    ///
    /// [`World::pick_face_hit`] commits to `Axis::dominant(hit.normal)` and so returns
    /// exactly one face. That's right when you're aiming at the middle of a wall, but on
    /// the **edge** where two faces meet it is whichever normal the physics engine
    /// happened to report, and on a **corner** it is one of three — arbitrary either way,
    /// and invisible to the author. So enumerate all of them and let them choose.
    ///
    /// Ordered so that physics' own answer (the dominant axis, nearest plane) is index 0.
    /// The cycle is therefore purely additive: without a scroll the tool picks exactly
    /// what it picked before this existed.
    fn candidate_faces(&mut self) -> Vec<FaceCandidate> {
        if self.mode != Mode::Build {
            return Vec::new();
        }
        let Some(hit) = self.physics.raycast(self.camera.pos, self.camera.forward(), 100.0) else {
            return Vec::new();
        };
        let hit_wt = hit.point / WORLD_SCALE;
        let dominant = Axis::dominant(hit.normal);
        // The same tolerances `pick_face_hit_from` uses: tight on the plane, lenient
        // in-rect.
        const PLANE_EPS: f32 = 0.15;
        const RECT_EPS: f32 = 0.15;

        let mut found: Vec<(FaceCandidate, u8, f32)> = Vec::new();
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let (u_axis, v_axis) = axis.orthogonals();
            let (hit_a, hit_u, hit_v) = (
                axis.component(hit_wt),
                u_axis.component(hit_wt),
                v_axis.component(hit_wt),
            );
            for side in [Side::Min, Side::Max] {
                // The nearest brush whose (axis, side) face plane the hit lies on.
                let mut best: Option<(FaceCandidate, f32)> = None;
                for region in &self.regions {
                    for b in &region.brushes {
                        // Room voids only, matching the pillar/brace tools: "out of the
                        // face" is defined relative to a room's interior, and the scheme
                        // + floor anchor this inherits are a room's.
                        if b.op != Op::Subtract {
                            continue;
                        }
                        let d = (b.face_pos(axis, side) - hit_a).abs();
                        if d > PLANE_EPS {
                            continue;
                        }
                        let (u0, u1) = (b.min(u_axis), b.min(u_axis) + b.dim(u_axis));
                        let (v0, v1) = (b.min(v_axis), b.min(v_axis) + b.dim(v_axis));
                        if hit_u < u0 - RECT_EPS
                            || hit_u > u1 + RECT_EPS
                            || hit_v < v0 - RECT_EPS
                            || hit_v > v1 + RECT_EPS
                        {
                            continue;
                        }
                        if best.as_ref().map(|(_, bd)| d < *bd).unwrap_or(true) {
                            best = Some((
                                FaceCandidate { region_id: region.id, brush: *b, axis, side },
                                d,
                            ));
                        }
                    }
                }
                if let Some((c, d)) = best {
                    found.push((c, u8::from(axis != dominant), d));
                }
            }
        }
        found.sort_by(|a, b| (a.1, a.2.to_bits()).cmp(&(b.1, b.2.to_bits())));
        found.into_iter().map(|(c, _, _)| c).collect()
    }

    /// Expand a candidate into the frozen [`DrawFace`] the tool draws on — resolving its
    /// whole coplanar group, not just the brush that was hit (see
    /// [`coplanar_face_group`]).
    fn face_from_candidate(&self, c: &FaceCandidate) -> Option<DrawFace> {
        let region = self.regions.iter().find(|r| r.id == c.region_id)?;
        let (u_axis, v_axis) = c.axis.orthogonals();
        let rects = coplanar_face_group(&region.brushes, &c.brush, c.axis, c.side, u_axis, v_axis);
        Some(DrawFace {
            region_id: c.region_id,
            axis: c.axis,
            side: c.side,
            position: c.brush.face_pos(c.axis, c.side),
            u_axis,
            v_axis,
            u_min: rects.iter().map(|r| r[0]).min()?,
            u_max: rects.iter().map(|r| r[1]).max()?,
            v_min: rects.iter().map(|r| r[2]).min()?,
            v_max: rects.iter().map(|r| r[3]).max()?,
            rects,
            scheme: c.brush.scheme,
        })
    }

    /// The surface the tool would draw on right now — the crosshair's candidates with the
    /// author's cycle offset applied. `None` if the crosshair isn't on a room surface.
    /// Silent: it runs every frame from the preview, so the "aim somewhere else" message
    /// belongs to the click path.
    pub(crate) fn selected_draw_face(&mut self) -> Option<DrawFace> {
        let candidates = self.candidate_faces();
        if candidates.is_empty() {
            return None;
        }
        let i = self.draw_candidate % candidates.len();
        self.face_from_candidate(&candidates[i])
    }

    /// Whether the tool is waiting for its first corner, so the app routes the scroll
    /// wheel to cycling candidate surfaces rather than anything else.
    pub fn is_draw_choosing_face(&self) -> bool {
        self.draw_phase == Some(DrawPhase::Idle)
    }

    /// Cycle which candidate surface the first corner lands on (scroll wheel, before the
    /// first click). A no-op where the crosshair is unambiguous, which is most of the
    /// time — only edges (2) and corners (3) offer a choice.
    pub fn cycle_draw_face(&mut self, steps: f32) {
        if self.draw_phase != Some(DrawPhase::Idle) {
            return;
        }
        let n = self.candidate_faces().len();
        if n <= 1 {
            return;
        }
        // Wrap in both directions without going negative on a usize.
        let step = if steps > 0.0 { 1 } else { n - 1 };
        self.draw_candidate = (self.draw_candidate + step) % n;
        if let Some(f) = self.selected_draw_face() {
            log::info!(
                "draw: surface {} of {n} — the {:?} {:?} face",
                self.draw_candidate + 1,
                f.side,
                f.axis
            );
        }
    }

    /// The surface-tint mesh: a cool translucent wash over the whole coplanar surface the
    /// tool is on, drawn under the yellow outline. This is what makes an edge or corner
    /// pick legible — otherwise which of the two or three meeting faces you're about to
    /// draw on is invisible until the first segment goes down the wrong plane.
    pub fn draw_surface_tint_mesh(&mut self) -> Option<CpuMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let face = match self.draw_phase? {
            // Idle tracks the crosshair (and the cycle); once drawing, the surface is
            // frozen and the tint confirms which one was committed to.
            DrawPhase::Idle => self.selected_draw_face()?,
            DrawPhase::Drawing | DrawPhase::Depth => self.draw_face.clone()?,
        };
        let bands: Vec<[f32; 4]> = face
            .rects
            .iter()
            .map(|&[u0, u1, v0, v1]| [u0 as f32, u1 as f32, v0 as f32, v1 as f32])
            .collect();
        Some(face_bands_mesh(&face, &bands, TINT_NUDGE))
    }

    // ─── Ghost preview ───────────────────────────────────────────────────────

    /// Recompute the draw ghost from the crosshair (each frame while armed) and return
    /// it, or `None` when there's nothing to show. Also refreshes `draw_cursor`, so
    /// this is what a click reads — exactly as the opening tool's preview feeds its
    /// confirm.
    pub fn update_draw_preview(&mut self) -> Option<CpuMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        match self.draw_phase? {
            DrawPhase::Idle => {
                let face = self.selected_draw_face()?;
                let v = project_to_face(&face, self.camera.pos, self.camera.forward())?;
                self.draw_cursor = Some(v);
                Some(face_bands_mesh(&face, &[corner_band(v)], OUTLINE_NUDGE))
            }
            DrawPhase::Drawing => {
                let face = self.draw_face.clone()?;
                let last = *self.draw_verts.last()?;
                // Axis-lock the previewed corner to whichever in-plane axis the
                // crosshair travelled further along. *That* is the 90° snap — there is
                // no separate snapping step.
                self.draw_cursor = project_to_face(&face, self.camera.pos, self.camera.forward())
                    .map(|raw| axis_lock(last, raw));
                let mut bands = Vec::new();
                for pair in self.draw_verts.windows(2) {
                    bands.push(segment_band(pair[0], pair[1]));
                }
                if let Some(cursor) = self.draw_cursor {
                    bands.push(segment_band(last, cursor));
                    bands.push(corner_band(cursor));
                }
                for &v in &self.draw_verts {
                    bands.push(corner_band(v));
                }
                Some(face_bands_mesh(&face, &bands, OUTLINE_NUDGE))
            }
            DrawPhase::Depth => {
                let face = self.draw_face.clone()?;
                let brushes = self.draw_brushes(&face);
                if brushes.is_empty() {
                    // Depth 0 — show the flat footprint so the shape stays visible.
                    let bands: Vec<[f32; 4]> = self
                        .draw_rects
                        .iter()
                        .map(|&(u, v, w, h)| {
                            [u as f32, (u + w) as f32, v as f32, (v + h) as f32]
                        })
                        .collect();
                    return Some(face_bands_mesh(&face, &bands, OUTLINE_NUDGE));
                }
                let boxes: Vec<[f32; 6]> = brushes
                    .iter()
                    .map(|b| [b.x, b.y, b.z, b.w, b.h, b.d])
                    .collect();
                Some(boxes_mesh(&boxes))
            }
        }
    }
}

// ─── Face-plane projection + snapping ────────────────────────────────────────

/// Intersect a ray with a [`DrawFace`]'s plane and return the WT-snapped in-plane
/// `(u, v)`, clamped to the face rect. `None` when the ray is parallel to the plane or
/// the plane is behind the camera.
///
/// Ray/plane against the *frozen* plane rather than a fresh `pick_face_hit`: re-picking
/// each frame would let the outline jump to another face the moment the crosshair
/// crossed an edge, and would lose the plane entirely while aiming through a doorway.
fn project_to_face(face: &DrawFace, origin: Vec3, dir: Vec3) -> Option<(i32, i32)> {
    let n = face.axis.index();
    let o = origin.to_array();
    let d = dir.to_array();
    if d[n].abs() < 1e-6 {
        return None;
    }
    let t = (face.position * WORLD_SCALE - o[n]) / d[n];
    if t <= 0.0 {
        return None;
    }
    let p = (origin + dir * t).to_array();
    let u = (p[face.u_axis.index()] / WORLD_SCALE).round() as i32;
    let v = (p[face.v_axis.index()] / WORLD_SCALE).round() as i32;
    Some((
        u.clamp(face.u_min, face.u_max),
        v.clamp(face.v_min, face.v_max),
    ))
}

/// One face the crosshair's hit point lies on, before its coplanar group is resolved.
/// Cheap to enumerate — the flood fill only runs for the candidate actually chosen.
#[derive(Clone, Copy)]
struct FaceCandidate {
    region_id: u32,
    brush: Brush,
    axis: Axis,
    side: Side,
}

/// Every coplanar, co-facing, contiguous subtract face reachable from `start`'s, as
/// in-plane integer rects `[u0, u1, v0, v1]`. Always contains at least `start`'s own.
///
/// See also `world::patch`, which answers a near-identical question for push/pull and
/// the opening tools and deliberately differs on frames (it stops at a doorway, this
/// does not). That module's header lists all three differences; if the two are ever
/// unified, the frame rule is the thing to reconcile first.
///
/// **This is the difference between a brush's face and the surface the author sees.** A
/// room enlarged by pushing a wall out, or extended by carving an adjoining area, is two
/// or more subtract brushes whose floors are one continuous plane with an invisible seam
/// across it. Clamping the tool to the picked brush alone made it stop dead at that seam.
///
/// A brush joins the group when it is a subtract, its face on the *same side* of the
/// *same axis* lies on the same plane, and its in-plane rect overlaps or touches a member
/// already in — so contiguity is required, and a different room's floor at the same
/// height across the level is correctly excluded. Matching on `side` matters: the ceiling
/// of the room below shares a plane with this floor but is a different surface.
///
/// Opening frames are *not* excluded (unlike `find_room_brushes`, which stops at them to
/// bound a room). A doorway's threshold is genuinely part of the floor you're standing on,
/// and a raised walkway running through a door is legitimate authoring. The group can
/// therefore reach into the next room, which is fine: the union is only ever used as a
/// bound, and what gets built is masked to the real surface.
fn coplanar_face_group(
    brushes: &[Brush],
    start: &Brush,
    axis: Axis,
    side: Side,
    u_axis: Axis,
    v_axis: Axis,
) -> Vec<[i32; 4]> {
    // WT coords are grid-aligned, so a face plane match is an exact edge match up to
    // float slop — the same tolerance `brushes_touching` uses.
    const EPS: f32 = 1e-4;
    let plane = start.face_pos(axis, side);
    let rect_of = |b: &Brush| -> [i32; 4] {
        [
            b.min(u_axis).round() as i32,
            (b.min(u_axis) + b.dim(u_axis)).round() as i32,
            b.min(v_axis).round() as i32,
            (b.min(v_axis) + b.dim(v_axis)).round() as i32,
        ]
    };
    // Candidates: every subtract whose same-side face is on this plane. Cheap prefilter,
    // so the flood fill below only has to test contiguity.
    let mut pending: Vec<[i32; 4]> = brushes
        .iter()
        .filter(|b| {
            b.op == Op::Subtract && b.id != start.id && (b.face_pos(axis, side) - plane).abs() < EPS
        })
        .map(rect_of)
        .collect();

    let mut group = vec![rect_of(start)];
    // Flood fill: repeatedly absorb any candidate touching what's already in the group.
    let mut grew = true;
    while grew {
        grew = false;
        let mut i = 0;
        while i < pending.len() {
            if group.iter().any(|g| rects_overlap_or_touch(*g, pending[i])) {
                group.push(pending.swap_remove(i));
                grew = true;
            } else {
                i += 1;
            }
        }
    }
    group
}

/// Whether two in-plane integer rects overlap or merely touch along an edge. Inclusive,
/// matching `brushes_overlap_or_touch`: two floors meeting exactly at `u = 10` are one
/// continuous surface, not two.
fn rects_overlap_or_touch(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] <= b[1] && b[0] <= a[1] && a[2] <= b[3] && b[2] <= a[3]
}

/// Snap `to` onto the axis through `from`, keeping whichever in-plane axis the cursor
/// travelled further along. This is the tool's 90° lock: no segment can be diagonal,
/// which is precisely what keeps [`rect_decompose`] exact and the emitted brushes
/// axis-aligned.
pub(crate) fn axis_lock(from: (i32, i32), to: (i32, i32)) -> (i32, i32) {
    if (to.0 - from.0).abs() >= (to.1 - from.1).abs() {
        (to.0, from.1)
    } else {
        (from.0, to.1)
    }
}

// ─── Validity: no self-intersection ──────────────────────────────────────────

/// Whether adding a segment from the last vertex to `cand` would make the outline
/// non-simple — crossing an earlier segment, running back along one, or landing on an
/// earlier corner. `closing` marks the segment that returns to the first vertex, whose
/// touch there is legitimate.
///
/// Exact integer arithmetic: both segments are axis-aligned, so treating each as a
/// degenerate box and intersecting reduces the whole question to "is the overlap empty,
/// a single point, or longer?". A single-point overlap is a legal shared corner only at
/// the two joins the outline is allowed to have; anything longer is collinear backtrack.
pub(crate) fn segment_self_intersects(verts: &[(i32, i32)], cand: (i32, i32), closing: bool) -> bool {
    if verts.len() < 2 {
        return false;
    }
    let last = verts[verts.len() - 1];
    for i in 0..verts.len() - 1 {
        let (a, b) = (verts[i], verts[i + 1]);
        match overlap(last, cand, a, b) {
            Overlap::None => {}
            Overlap::Point(p) => {
                let joins_previous = i == verts.len() - 2 && p == last;
                let joins_first = closing && i == 0 && p == verts[0];
                if !joins_previous && !joins_first {
                    return true;
                }
            }
            Overlap::Segment => return true,
        }
    }
    false
}

/// How two axis-aligned integer segments overlap.
pub(crate) enum Overlap {
    None,
    Point((i32, i32)),
    Segment,
}

/// Intersect two axis-aligned integer segments as degenerate boxes.
pub(crate) fn overlap(a1: (i32, i32), a2: (i32, i32), b1: (i32, i32), b2: (i32, i32)) -> Overlap {
    let u0 = a1.0.min(a2.0).max(b1.0.min(b2.0));
    let u1 = a1.0.max(a2.0).min(b1.0.max(b2.0));
    let v0 = a1.1.min(a2.1).max(b1.1.min(b2.1));
    let v1 = a1.1.max(a2.1).min(b1.1.max(b2.1));
    if u0 > u1 || v0 > v1 {
        Overlap::None
    } else if u0 == u1 && v0 == v1 {
        Overlap::Point((u0, v0))
    } else {
        Overlap::Segment
    }
}

// ─── Rectilinear decomposition ───────────────────────────────────────────────

/// Decompose a closed rectilinear polygon into disjoint axis-aligned rectangles whose
/// union is exactly its interior. `verts` are integer in-plane WT corners in order,
/// implicitly closed; the result is `(u0, v0, w, h)` per rectangle.
///
/// **Rasterize, then greedily merge.** Every edge is grid-aligned, so splitting the
/// bounding box into 1 WT cells and asking which are inside is *exact* — no
/// approximation, and concave corners need no handling at all. Cell centres land on
/// half-integers, which no vertex or edge endpoint can occupy, so the even-odd crossing
/// test needs neither an epsilon nor the usual vertex-on-ray special case. The merge
/// then takes the top-left free cell, widens right, deepens down while the full band
/// stays free, and emits — which collapses an L, T, U or plus to a handful of rects.
///
/// Returns empty for a degenerate outline (fewer than 4 corners, zero area) or one
/// whose bounding box exceeds [`DRAW_MAX_CELLS`].
pub(crate) fn rect_decompose(verts: &[(i32, i32)]) -> Vec<(i32, i32, i32, i32)> {
    rect_decompose_where(verts, |_, _| true)
}

/// [`rect_decompose`] restricted to the cells `keep` admits.
///
/// The tool masks with [`DrawFace::covers_cell`], which clips a drawn shape to the real
/// coplanar surface. Vertices are only clamped to the surface group's *bounding box*, and
/// for an L-shaped group that box includes a corner of solid rock — masking here is what
/// stops an outline drawn across it from carving into (or worse, straight through) that
/// solid, without having to refuse the author's corners while they draw.
pub(crate) fn rect_decompose_where(
    verts: &[(i32, i32)],
    keep: impl Fn(i32, i32) -> bool,
) -> Vec<(i32, i32, i32, i32)> {
    if verts.len() < 4 {
        return Vec::new();
    }
    let u0 = verts.iter().map(|p| p.0).min().unwrap();
    let u1 = verts.iter().map(|p| p.0).max().unwrap();
    let v0 = verts.iter().map(|p| p.1).min().unwrap();
    let v1 = verts.iter().map(|p| p.1).max().unwrap();
    let (w, h) = ((u1 - u0) as usize, (v1 - v0) as usize);
    if w == 0 || h == 0 {
        return Vec::new();
    }
    if w * h > DRAW_MAX_CELLS {
        log::warn!("draw: outline bounding box is {w}×{h} WT — too large to decompose");
        return Vec::new();
    }

    let mut inside = vec![false; w * h];
    for j in 0..h {
        let v = v0 + j as i32;
        for i in 0..w {
            let u = u0 + i as i32;
            inside[j * w + i] =
                cell_inside(verts, u as f32 + 0.5, v as f32 + 0.5) && keep(u, v);
        }
    }

    let mut used = vec![false; w * h];
    let mut rects = Vec::new();
    for j in 0..h {
        for i in 0..w {
            let free = |idx: usize| inside[idx] && !used[idx];
            if !free(j * w + i) {
                continue;
            }
            let mut rw = 1;
            while i + rw < w && free(j * w + i + rw) {
                rw += 1;
            }
            let mut rh = 1;
            'deepen: while j + rh < h {
                for k in 0..rw {
                    if !free((j + rh) * w + i + k) {
                        break 'deepen;
                    }
                }
                rh += 1;
            }
            for jj in j..j + rh {
                for ii in i..i + rw {
                    used[jj * w + ii] = true;
                }
            }
            rects.push((u0 + i as i32, v0 + j as i32, rw as i32, rh as i32));
        }
    }
    rects
}

/// Even-odd point-in-polygon for a rectilinear outline, casting a +u ray from
/// `(cu, cv)`. Only vertical edges can cross a horizontal ray, and `cv` is always a
/// half-integer while edge endpoints are integers, so the strict comparisons below are
/// exact and no edge is ever counted twice.
fn cell_inside(verts: &[(i32, i32)], cu: f32, cv: f32) -> bool {
    let mut crossings = 0u32;
    for i in 0..verts.len() {
        let a = verts[i];
        let b = verts[(i + 1) % verts.len()];
        if a.0 != b.0 {
            continue; // horizontal edge
        }
        let (lo, hi) = if a.1 < b.1 { (a.1, b.1) } else { (b.1, a.1) };
        if cv > lo as f32 && cv < hi as f32 && (a.0 as f32) > cu {
            crossings += 1;
        }
    }
    crossings % 2 == 1
}

// ─── Ghost mesh ──────────────────────────────────────────────────────────────

/// A thin in-plane band `[u0, u1, v0, v1]` along the segment `a`→`b`.
fn segment_band(a: (i32, i32), b: (i32, i32)) -> [f32; 4] {
    let (au, av) = (a.0 as f32, a.1 as f32);
    let (bu, bv) = (b.0 as f32, b.1 as f32);
    let e = GHOST_BAND_HALF;
    [
        au.min(bu) - e,
        au.max(bu) + e,
        av.min(bv) - e,
        av.max(bv) + e,
    ]
}

/// A small in-plane square marking a corner.
fn corner_band(v: (i32, i32)) -> [f32; 4] {
    let (u, w) = (v.0 as f32, v.1 as f32);
    let e = GHOST_CORNER_HALF;
    [u - e, u + e, w - e, w + e]
}

/// Build one `CpuMesh` of in-plane quads on a face — the outline ghost. Mirrors
/// [`World::face_quad_mesh`], including the nudge toward the room interior so the
/// bands sit in front of the surface, but accumulates many quads into a single mesh
/// (the highlight pipeline takes exactly one, with culling off, so winding is moot).
fn face_bands_mesh(face: &DrawFace, bands: &[[f32; 4]], nudge: f32) -> CpuMesh {
    let a = face.position + if face.side == Side::Max { -nudge } else { nudge };
    let n = face.axis.normal();
    let mut positions = Vec::with_capacity(bands.len() * 12);
    let mut normals = Vec::with_capacity(bands.len() * 12);
    let mut indices = Vec::with_capacity(bands.len() * 6);
    for band in bands {
        let [u0, u1, v0, v1] = *band;
        let corner = |u: f32, v: f32| -> [f32; 3] {
            let mut p = [0.0f32; 3];
            p[face.axis.index()] = a;
            p[face.u_axis.index()] = u;
            p[face.v_axis.index()] = v;
            [p[0] * WORLD_SCALE, p[1] * WORLD_SCALE, p[2] * WORLD_SCALE]
        };
        let base = (positions.len() / 3) as u32;
        for c in [
            corner(u0, v0),
            corner(u1, v0),
            corner(u1, v1),
            corner(u0, v1),
        ] {
            positions.extend_from_slice(&c);
            normals.extend_from_slice(&n);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    CpuMesh::from_csg(&positions, &normals, &indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Decomposition ───────────────────────────────────────────────────────

    /// The decomposition's whole contract, asserted directly against the rasterizer:
    /// the emitted rectangles are pairwise **disjoint** and their union is **exactly**
    /// the polygon's interior — no gap, no double-cover, no spill outside.
    ///
    /// Checking against `cell_inside` rather than a hand-written expected list is what
    /// makes this meaningful for concave shapes, where the expected list is exactly the
    /// thing that's easy to get wrong by hand.
    fn assert_exact_cover(verts: &[(i32, i32)]) -> Vec<(i32, i32, i32, i32)> {
        let rects = rect_decompose(verts);
        let u0 = verts.iter().map(|p| p.0).min().unwrap();
        let u1 = verts.iter().map(|p| p.0).max().unwrap();
        let v0 = verts.iter().map(|p| p.1).min().unwrap();
        let v1 = verts.iter().map(|p| p.1).max().unwrap();

        for u in u0..u1 {
            for v in v0..v1 {
                let want = cell_inside(verts, u as f32 + 0.5, v as f32 + 0.5);
                let got = rects
                    .iter()
                    .filter(|&&(ru, rv, rw, rh)| {
                        u >= ru && u < ru + rw && v >= rv && v < rv + rh
                    })
                    .count();
                assert!(
                    got <= 1,
                    "cell ({u}, {v}) is covered {got} times — rectangles must be disjoint"
                );
                assert_eq!(
                    got == 1,
                    want,
                    "cell ({u}, {v}): covered={} but inside={want} (rects {rects:?})",
                    got == 1
                );
            }
        }
        rects
    }

    /// A plain rectangle is already convex — it must come back as itself, not split.
    #[test]
    fn a_rectangle_decomposes_to_itself() {
        let rects = assert_exact_cover(&[(0, 0), (6, 0), (6, 4), (0, 4)]);
        assert_eq!(rects, vec![(0, 0, 6, 4)], "one rect, unsplit");
    }

    /// The case that forces decomposition to exist at all: a concave L would be
    /// fan-triangulated into garbage as a single prism. Two rects, exact cover.
    #[test]
    fn an_l_shape_decomposes_into_two_exact_rectangles() {
        let l = [(0, 0), (6, 0), (6, 2), (2, 2), (2, 6), (0, 6)];
        let rects = assert_exact_cover(&l);
        assert_eq!(rects.len(), 2, "an L needs exactly two rects: {rects:?}");
        let area: i32 = rects.iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(area, 20, "6×2 bar + 2×4 leg");
    }

    /// Concave shapes with more than one prong — a U (two legs off a base) and a plus
    /// (four prongs, and the only one whose interior isn't reachable by widening a
    /// single row). Both must still cover exactly.
    #[test]
    fn multi_prong_concave_shapes_decompose_exactly() {
        let u_shape = [
            (0, 0),
            (8, 0),
            (8, 6),
            (6, 6),
            (6, 2),
            (2, 2),
            (2, 6),
            (0, 6),
        ];
        let rects = assert_exact_cover(&u_shape);
        let area: i32 = rects.iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(area, 8 * 2 + 2 * 4 + 2 * 4, "base plus two legs");

        let plus = [
            (2, 0),
            (4, 0),
            (4, 2),
            (6, 2),
            (6, 4),
            (4, 4),
            (4, 6),
            (2, 6),
            (2, 4),
            (0, 4),
            (0, 2),
            (2, 2),
        ];
        let rects = assert_exact_cover(&plus);
        let area: i32 = rects.iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(area, 20, "a plus of 1-WT arms on a 2×2 core");
    }

    /// Winding must not matter: the author draws clockwise or counter-clockwise
    /// depending on which way they walked the outline, and an even-odd crossing test is
    /// what makes both give the same interior.
    #[test]
    fn decomposition_is_winding_independent() {
        let ccw = [(0, 0), (6, 0), (6, 2), (2, 2), (2, 6), (0, 6)];
        let mut cw = ccw;
        cw.reverse();
        let a: i32 = assert_exact_cover(&ccw).iter().map(|&(_, _, w, h)| w * h).sum();
        let b: i32 = assert_exact_cover(&cw).iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(a, b, "both windings enclose the same area");
    }

    /// Degenerate outlines build nothing rather than emitting a zero-volume brush.
    #[test]
    fn degenerate_outlines_decompose_to_nothing() {
        assert!(rect_decompose(&[]).is_empty(), "no corners");
        assert!(
            rect_decompose(&[(0, 0), (4, 0), (4, 4)]).is_empty(),
            "a rectilinear loop needs 4+ corners"
        );
        assert!(
            rect_decompose(&[(0, 0), (4, 0), (4, 0), (0, 0)]).is_empty(),
            "zero height encloses no area"
        );
        assert!(
            rect_decompose(&[(3, 3), (3, 3), (3, 3), (3, 3)]).is_empty(),
            "a single repeated point encloses no area"
        );
    }

    // ─── Snapping + validity ─────────────────────────────────────────────────

    /// The 90° lock is the load-bearing constraint of the whole feature: one diagonal
    /// segment and rectilinear decomposition dies. No input may produce one.
    #[test]
    fn axis_lock_never_produces_a_diagonal() {
        for (to, want) in [
            ((5, 1), (5, 0)),  // moved further in u → keep u, snap v back
            ((1, 5), (0, 5)),  // moved further in v → keep v, snap u back
            ((3, 3), (3, 0)),  // a tie resolves to u, deterministically
            ((-4, 2), (-4, 0)),// negative directions lock the same way
        ] {
            let got = axis_lock((0, 0), to);
            assert_eq!(got, want, "axis_lock((0,0), {to:?})");
            assert!(
                got.0 == 0 || got.1 == 0,
                "{got:?} is diagonal from the origin"
            );
        }
    }

    /// The outline must stay simple. A segment that crosses an earlier one, or runs
    /// back along it, is refused — but the two joins a valid outline genuinely has (the
    /// shared corner with the previous segment, and the closing touch on the first
    /// corner) are allowed.
    #[test]
    fn self_intersection_rejects_crossings_but_allows_the_legal_joins() {
        // A three-quarter box: (0,0) → (4,0) → (4,4) → (0,4).
        let verts = vec![(0, 0), (4, 0), (4, 4), (0, 4)];

        assert!(
            !segment_self_intersects(&verts, (0, 0), true),
            "closing back onto the first corner is legal"
        );
        assert!(
            !segment_self_intersects(&verts, (0, 2), false),
            "a shorter segment that stops before any edge is legal"
        );
        assert!(
            segment_self_intersects(&verts, (0, -2), false),
            "running past the first corner crosses the opening edge"
        );
        assert!(
            segment_self_intersects(&[(0, 0), (4, 0), (4, 4)].to_vec(), (2, 0), false),
            "landing on the interior of an earlier segment is a crossing"
        );
        assert!(
            segment_self_intersects(&[(0, 0), (4, 0)].to_vec(), (2, 0), false),
            "doubling back along the previous segment is a collinear overlap"
        );
        assert!(
            !segment_self_intersects(&[(0, 0)].to_vec(), (4, 0), false),
            "the very first segment has nothing to cross"
        );
    }

    // ─── Face projection ─────────────────────────────────────────────────────

    /// A floor face for projection tests: the default room's floor, 24×24 WT, one brush.
    fn floor_face() -> DrawFace {
        DrawFace {
            region_id: 0,
            axis: Axis::Y,
            side: Side::Min,
            position: 0.0,
            u_axis: Axis::X,
            v_axis: Axis::Z,
            u_min: 0,
            u_max: 24,
            v_min: 0,
            v_max: 24,
            rects: vec![[0, 24, 0, 24]],
            scheme: default_scheme(),
        }
    }

    /// Projection snaps to the WT grid, clamps to the face rect, and refuses rays that
    /// can't reach the plane — the last one matters because the plane is frozen, so the
    /// crosshair can and will point away from it mid-draw.
    #[test]
    fn projection_snaps_clamps_and_refuses_rays_that_miss() {
        let face = floor_face();
        let s = WORLD_SCALE;
        // Straight down from above (10.4, 6, 15.6) WT → snaps to (10, 16).
        let got = project_to_face(
            &face,
            Vec3::new(10.4 * s, 6.0 * s, 15.6 * s),
            Vec3::new(0.0, -1.0, 0.0),
        );
        assert_eq!(got, Some((10, 16)), "snapped to the nearest WT corner");

        // A point beyond the face clamps into it rather than escaping the brush.
        let got = project_to_face(
            &face,
            Vec3::new(99.0 * s, 6.0 * s, -40.0 * s),
            Vec3::new(0.0, -1.0, 0.0),
        );
        assert_eq!(got, Some((24, 0)), "clamped to the face rect corner");

        // Parallel to the plane, and pointing away from it.
        assert!(
            project_to_face(&face, Vec3::new(0.0, s, 0.0), Vec3::new(1.0, 0.0, 0.0)).is_none(),
            "a ray parallel to the plane never meets it"
        );
        assert!(
            project_to_face(&face, Vec3::new(0.0, s, 0.0), Vec3::new(0.0, 1.0, 0.0)).is_none(),
            "the plane is behind the camera"
        );
    }

    // ─── Driving the real tool ───────────────────────────────────────────────

    /// Arm the tool and land the first corner by aiming the fly-cam at a real surface,
    /// so face resolution goes through `pick_face_hit` for real. Returns that corner —
    /// tests build their outline relative to it rather than assuming where it lands.
    fn begin_on_surface(world: &mut World, pitch: f32) -> (i32, i32) {
        world.initial_meshes();
        world.camera.pitch = pitch;
        world.draw_tool_key();
        assert!(world.is_draw_tool(), "tool armed");
        world.draw_click();
        assert_eq!(
            world.draw_phase,
            Some(DrawPhase::Drawing),
            "the first click resolved a surface and dropped a corner"
        );
        world.draw_verts[0]
    }

    /// Click a corner the way the app does. The previewed corner is set directly
    /// instead of aiming the camera at it: that keeps the real commit / close /
    /// decompose / undo path under test while stubbing only the ray projection, which
    /// has its own test above.
    fn click_corner(world: &mut World, v: (i32, i32)) -> Vec<RegionMesh> {
        world.draw_cursor = Some(v);
        world.with_undo_many(|w| w.draw_click())
    }

    /// Walk an outline (after the first corner) and close it.
    fn draw_outline(world: &mut World, corners: &[(i32, i32)]) {
        for &c in corners {
            click_corner(world, c);
        }
        assert_eq!(
            world.draw_phase,
            Some(DrawPhase::Depth),
            "clicking the first corner again closed the loop"
        );
    }

    /// The brushes added to the world by the last commit.
    fn added_brushes(world: &World, since_id: u32) -> Vec<Brush> {
        world
            .regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied())
            .filter(|b| b.id >= since_id)
            .collect()
    }

    /// The headline case: an L drawn on the floor and extruded becomes a raised
    /// section — several brushes, but **one** group and **one** wall-texture anchor, and
    /// additive.
    #[test]
    fn an_l_shaped_floor_extrude_emits_one_group_with_a_shared_anchor() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4); // look down at the floor
        let first_new_id = world.next_brush_id;
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov), // back to the start → close
            ],
        );
        world.adjust_draw_depth(1.0); // 1 → 2 WT tall
        let meshes = click_corner(&mut world, (ou, ov));
        assert!(!meshes.is_empty(), "the commit rebuilt at least one region");
        // A throughput tool: committing returns to idle *still armed*, ready for the
        // next shape, rather than making the author re-press Q every time.
        assert!(world.is_draw_tool(), "the tool stays armed after a commit");
        assert_eq!(world.draw_phase, Some(DrawPhase::Idle));
        assert!(world.draw_verts.is_empty(), "with the finished outline cleared");

        let added = added_brushes(&world, first_new_id);
        assert_eq!(added.len(), 2, "the L decomposed into two brushes");
        assert!(
            added.iter().all(|b| b.op == Op::Add),
            "a positive depth extrudes (Op::Add)"
        );
        let groups: std::collections::HashSet<u32> = added.iter().map(|b| b.group).collect();
        assert_eq!(groups.len(), 1, "both brushes share one group");
        let group = *groups.iter().next().unwrap();
        assert_eq!(group, first_new_id, "the group id is its first brush's id");
        assert_ne!(group, 0, "0 is reserved for ungrouped brushes");

        let anchors: std::collections::HashSet<u32> =
            added.iter().map(|b| b.floor_y.to_bits()).collect();
        assert_eq!(anchors.len(), 1, "one shared wall-texture floor anchor");
        assert!(
            added.iter().all(|b| (b.h - 2.0).abs() < 1e-6),
            "each brush is the scrolled 2 WT deep"
        );
    }

    /// Decomposition is only sound if the *internal* boundaries it invents disappear in
    /// the fold. Two adjacent extruded rects share a full face, and a leftover wall
    /// there would be a visible slab standing inside the author's shape — the failure
    /// mode that would make this whole approach untenable.
    ///
    /// The kernel does handle it (coplanar-opposed polygons route to `coplanar_back` in
    /// `split_polygon` and get clipped), but that's the fragile corner of any BSP, so
    /// pin it: the L's two rects meet at v = ov+2 over u ∈ [ou, ou+2], and no triangle
    /// may survive on that plane inside the extrusion.
    #[test]
    fn adjacent_decomposed_brushes_leave_no_internal_wall() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        world.adjust_draw_depth(1.0); // 2 WT tall
        click_corner(&mut world, (ou, ov));

        // Both spans live on the same Z plane at ov+2, inside the 2 WT extrusion:
        //   u ∈ (ou,   ou+2) — INTERNAL, the decomposition boundary. Must be empty.
        //   u ∈ (ou+2, ou+6) — EXTERNAL, the L's real step face. Must NOT be empty,
        //                      which is what proves the detector is looking in the
        //                      right place rather than passing vacuously.
        let (collider, _) = world.regions[0].evaluate_both(&[]);
        let seam_z = (ov + 2) as f32;
        let mut internal = 0;
        let mut external = 0;
        for tri in collider.indices.chunks_exact(3) {
            let p: Vec<[f32; 3]> = tri
                .iter()
                .map(|&i| collider.vertices[i as usize].pos)
                .collect();
            let c = [
                (p[0][0] + p[1][0] + p[2][0]) / 3.0 / WORLD_SCALE,
                (p[0][1] + p[1][1] + p[2][1]) / 3.0 / WORLD_SCALE,
                (p[0][2] + p[1][2] + p[2][2]) / 3.0 / WORLD_SCALE,
            ];
            let n = collider.vertices[tri[0] as usize].normal;
            let on_seam_plane = n[2].abs() > 0.9
                && (c[2] - seam_z).abs() < 0.01
                && c[1] > 0.01
                && c[1] < 1.99;
            if !on_seam_plane {
                continue;
            }
            if c[0] > ou as f32 && c[0] < (ou + 2) as f32 {
                internal += 1;
            } else if c[0] > (ou + 2) as f32 && c[0] < (ou + 6) as f32 {
                external += 1;
            }
        }
        assert!(
            external > 0,
            "the L's outward step face is missing from the fold — this test is looking \
             at the wrong plane, so its internal-wall check proves nothing"
        );
        assert_eq!(
            internal, 0,
            "an internal wall survived between the two decomposed brushes \
             ({external} triangles found on the same plane just outside the seam)"
        );
    }

    /// Scrolling the depth negative insets instead of extruding, and the carve anchors
    /// its walls to **its own** new floor — the pit case
    /// `uv_zones::a_lower_pit_anchors_its_walls_to_its_own_floor` guards from the other
    /// side.
    #[test]
    fn a_negative_depth_insets_and_anchors_to_the_new_floor() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        let first_new_id = world.next_brush_id;
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        world.adjust_draw_depth(-3.0); // 1 → -2: through neutral into an inset
        assert_eq!(world.draw_depth, -2.0);
        click_corner(&mut world, (ou, ov));

        let added = added_brushes(&world, first_new_id);
        assert_eq!(added.len(), 2);
        assert!(
            added.iter().all(|b| b.op == Op::Subtract),
            "a negative depth carves (Op::Subtract)"
        );
        // The floor was at y = 0, so a 2 WT pit bottoms out at −2 and anchors there.
        assert!(
            added.iter().all(|b| (b.y + 2.0).abs() < 1e-6),
            "the carve bottoms out 2 WT below the floor: {:?}",
            added.iter().map(|b| b.y).collect::<Vec<_>>()
        );
        assert!(
            added.iter().all(|b| (b.floor_y + 2.0).abs() < 1e-6),
            "the pit's walls anchor to the pit floor, not the room floor"
        );
    }

    /// The seam this feature would otherwise ship: an L drawn on a **wall** spans two
    /// heights, so its rects have different `y`. Left at the per-brush default, each
    /// would anchor its own wall UVs and texture-shift against its neighbour along an
    /// internal decomposition boundary the author never drew. One anchor for the shape.
    #[test]
    fn a_wall_shape_spanning_heights_still_shares_one_anchor() {
        let mut world = World::new();
        // Yaw 0 / pitch 0 looks down −Z at the far wall.
        let (ou, ov) = begin_on_surface(&mut world, 0.0);
        let first_new_id = world.next_brush_id;
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        click_corner(&mut world, (ou, ov)); // commit at the default 1 WT extrude

        let added = added_brushes(&world, first_new_id);
        assert_eq!(added.len(), 2, "the wall L decomposed into two brushes");
        let heights: std::collections::HashSet<u32> =
            added.iter().map(|b| b.y.to_bits()).collect();
        assert_eq!(
            heights.len(),
            2,
            "the two brushes really do sit at different heights (else this proves nothing)"
        );
        let anchors: std::collections::HashSet<u32> =
            added.iter().map(|b| b.floor_y.to_bits()).collect();
        assert_eq!(anchors.len(), 1, "but they share a single wall-UV anchor");
        let lowest = added.iter().map(|b| b.y).fold(f32::INFINITY, f32::min);
        assert!(
            (added[0].floor_y - lowest).abs() < 1e-6,
            "the anchor is the shape's lowest point"
        );
    }

    /// A drawn shape is an ordinary edit: undo removes the whole group in one step (not
    /// one brush at a time), redo brings it back, and it survives a save/load
    /// round-trip with its grouping intact.
    #[test]
    fn a_drawn_group_undoes_as_one_and_survives_save_load() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        let before = world.regions.iter().map(|r| r.brushes.len()).sum::<usize>();
        let first_new_id = world.next_brush_id;
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        click_corner(&mut world, (ou, ov));
        let after = world.regions.iter().map(|r| r.brushes.len()).sum::<usize>();
        assert_eq!(after, before + 2);

        assert!(world.undo().is_some(), "the commit left an undo step");
        assert_eq!(
            world.regions.iter().map(|r| r.brushes.len()).sum::<usize>(),
            before,
            "one undo removes the whole drawn group, not one rect of it"
        );
        assert!(world.redo().is_some());
        assert_eq!(
            world.regions.iter().map(|r| r.brushes.len()).sum::<usize>(),
            after,
            "redo restores it"
        );

        let path = std::env::temp_dir().join("bah_draw_group_roundtrip.json");
        world.save_level(&path).expect("save");
        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        let reloaded = added_brushes(&loaded, first_new_id);
        assert_eq!(reloaded.len(), 2, "both brushes round-trip");
        let groups: std::collections::HashSet<u32> = reloaded.iter().map(|b| b.group).collect();
        assert_eq!(groups.len(), 1, "and they still share their group");
        assert_ne!(
            *groups.iter().next().unwrap(),
            0,
            "the group id survived the file, it didn't fall back to the serde default"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Esc walks back down the ladder one rung at a time — depth step to outline, then
    /// one corner per press — rather than throwing the whole drawing away.
    #[test]
    fn the_escape_ladder_backs_out_one_rung_at_a_time() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        assert_eq!(world.draw_verts.len(), 6, "six corners, close pushed none");

        assert!(world.draw_escape(), "Esc leaves the depth step");
        assert_eq!(world.draw_phase, Some(DrawPhase::Drawing));
        assert_eq!(world.draw_verts.len(), 6, "the outline is kept, just reopened");
        assert!(world.draw_rects.is_empty(), "the decomposition is dropped");

        for want in (1..6).rev() {
            assert!(world.draw_escape());
            assert_eq!(world.draw_verts.len(), want, "one corner per Esc");
        }
        assert!(world.draw_escape(), "the last corner");
        assert_eq!(world.draw_phase, Some(DrawPhase::Idle), "back to idle, still armed");
        assert!(world.draw_face.is_none(), "and the face is released");
        assert!(
            !world.draw_escape(),
            "idle hands the Esc on, so the app releases the pointer and disarms"
        );
    }

    /// A depth of 0 builds nothing and leaves the tool where it is, so a stray click
    /// while scrolling through neutral can't drop a zero-volume brush into the level.
    #[test]
    fn a_zero_depth_commit_builds_nothing() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        let before = world.regions.iter().map(|r| r.brushes.len()).sum::<usize>();
        draw_outline(
            &mut world,
            &[
                (ou + 6, ov),
                (ou + 6, ov + 2),
                (ou + 2, ov + 2),
                (ou + 2, ov + 6),
                (ou, ov + 6),
                (ou, ov),
            ],
        );
        world.adjust_draw_depth(-1.0); // 1 → 0
        assert_eq!(world.draw_depth, 0.0);
        assert!(
            click_corner(&mut world, (ou, ov)).is_empty(),
            "nothing was rebuilt"
        );
        assert_eq!(
            world.regions.iter().map(|r| r.brushes.len()).sum::<usize>(),
            before,
            "and no brush was added"
        );
        assert!(
            world.is_draw_tool() && world.draw_phase == Some(DrawPhase::Depth),
            "the tool stays at the depth step so the author can scroll on"
        );
    }

    /// Arming another crosshair tool abandons an in-progress drawing rather than
    /// leaving orphaned state behind that a later click would act on.
    #[test]
    fn arming_another_tool_abandons_an_in_progress_drawing() {
        let mut world = World::new();
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        click_corner(&mut world, (ou + 4, ov));
        assert_eq!(world.draw_verts.len(), 2);

        world.hole_tool_key();
        assert!(!world.is_draw_tool(), "the hole tool took over");
        assert!(world.draw_verts.is_empty(), "no orphaned vertices");
        assert!(world.draw_face.is_none());

        // And the reverse: the draw tool disarms the hole tool.
        world.draw_tool_key();
        assert!(world.is_draw_tool());
        assert!(!world.is_opening_arming(), "the hole tool was disarmed");
    }

    // ─── Surfaces that span several brushes ──────────────────────────────────

    /// Enlarge the default room by carving a second subtract flush against it, the way
    /// pushing a wall out or extending a room does. The floor is then one continuous
    /// plane made of two brushes with an invisible seam at `z = 24`.
    fn extend_room(world: &mut World) {
        let id = world.next_brush_id;
        world.next_brush_id += 1;
        world.regions[0]
            .brushes
            .push(Brush::new(id, Op::Subtract, 0.0, 0.0, 24.0, 24.0, 16.0, 24.0));
        world.recluster_all();
    }

    /// The reported flaw: with the floor spanning two brushes, the drawable area must be
    /// the whole visible surface, not the brush the crosshair happened to land on.
    #[test]
    fn a_surface_spanning_two_brushes_is_drawable_end_to_end() {
        let mut world = World::new();
        extend_room(&mut world);
        world.initial_meshes();
        world.camera.pitch = -1.4;
        world.draw_tool_key();
        let face = world.selected_draw_face().expect("the floor resolves");

        assert_eq!(face.rects.len(), 2, "both floor brushes joined the group");
        assert_eq!(
            (face.v_min, face.v_max),
            (0, 48),
            "the drawable span covers both brushes, not just the picked one"
        );
        assert!(
            face.covers_cell(12, 30),
            "a cell in the *other* brush is on the surface"
        );

        // And a corner can actually be placed over there: the clamp is the union now.
        let s = WORLD_SCALE;
        let far = project_to_face(
            &face,
            Vec3::new(12.4 * s, 6.0 * s, 40.4 * s),
            Vec3::new(0.0, -1.0, 0.0),
        );
        assert_eq!(
            far,
            Some((12, 40)),
            "a corner lands in the second brush instead of being clamped to the first \
             brush's edge at v = 24"
        );
    }

    /// And the shape genuinely builds across the seam — one group of brushes spanning
    /// both floor brushes, rather than being cut off at `z = 24`.
    #[test]
    fn a_drawn_shape_builds_across_a_brush_seam() {
        let mut world = World::new();
        extend_room(&mut world);
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        let first_new_id = world.next_brush_id;

        // A 12×18 WT rectangle from the first corner, deep enough to cross the seam at
        // z = 24 (the first corner lands near z = 12, so ov + 18 is past it).
        const W: i32 = 12;
        const D: i32 = 18;
        assert!(
            ov < 24 && ov + D > 24,
            "the test rectangle must straddle the seam (ov = {ov})"
        );
        draw_outline(
            &mut world,
            &[(ou + W, ov), (ou + W, ov + D), (ou, ov + D), (ou, ov)],
        );
        click_corner(&mut world, (ou, ov)); // build at the default 1 WT

        let added = added_brushes(&world, first_new_id);
        assert!(!added.is_empty(), "the shape built");
        let z_lo = added.iter().map(|b| b.z).fold(f32::INFINITY, f32::min);
        let z_hi = added.iter().map(|b| b.z + b.d).fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (z_lo - ov as f32).abs() < 1e-3 && (z_hi - (ov + D) as f32).abs() < 1e-3,
            "the built shape spans the seam (got z {z_lo}..{z_hi}, wanted {ov}..{})",
            ov + D
        );
        let total: f32 = added.iter().map(|b| b.w * b.d).sum();
        assert!(
            (total - (W * D) as f32).abs() < 1e-3,
            "and covers the whole {W}×{D} WT drawn, not just the part before the seam \
             (got {total} WT²)"
        );
    }

    /// **A save/load round trip must not change the folded geometry.**
    ///
    /// `evaluate` folds a region's brushes in slice order, and that order is meaningful —
    /// a `Subtract` after an `Add` carves the added geometry away. At build time the order
    /// is push order (ascending id, i.e. authoring order), but a reload reclusters through
    /// `cluster_brush_indices`, a stack-based DFS whose order is neither authored nor
    /// stable. A shape extruded across a widened room's seam therefore rendered correctly
    /// until saved and reloaded, at which point the DFS put the drawn `Add` *before* the
    /// second room brush and that brush carved away everything past the seam — leaving
    /// only the part over the first brush.
    ///
    /// Fixed by restoring ascending-id order in `rebuild_from_flat`. Asserted on the
    /// triangle count because that is the actual symptom (missing surface), and it catches
    /// the whole class rather than this one shape.
    #[test]
    fn a_shape_across_a_seam_survives_a_save_load_round_trip() {
        let mut world = World::new();
        extend_room(&mut world);
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        const W: i32 = 12;
        const D: i32 = 18;
        assert!(ov < 24 && ov + D > 24, "must straddle the seam (ov = {ov})");
        draw_outline(
            &mut world,
            &[(ou + W, ov), (ou + W, ov + D), (ou, ov + D), (ou, ov)],
        );
        world.adjust_draw_depth(1.0); // 2 WT tall, so the top face is unmistakable
        click_corner(&mut world, (ou, ov));

        // How far the extrusion's top surface reaches, as built: past the seam to ov + D.
        let built_tris = world.regions[0].evaluate_both(&[]).0.indices.len() / 3;
        let built_reach = top_face_reach_z(&mut world, 2.0);
        let want = (ov + D) as f32;
        assert!(
            (built_reach - want).abs() < 0.01,
            "as built the top surface reaches z = {want} (got {built_reach})"
        );

        let path = std::env::temp_dir().join("bah_draw_seam_roundtrip.json");
        world.save_level(&path).expect("save");
        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            loaded.regions.len(),
            1,
            "the whole level is one region either way"
        );
        let loaded_reach = top_face_reach_z(&mut loaded, 2.0);
        assert!(
            (loaded_reach - want).abs() < 0.01,
            "the extrusion still reaches z = {want} after a reload (got {loaded_reach}) — \
             a short reach here is the second room brush re-folding out of order and \
             carving away everything past the seam at z = 24"
        );
        let loaded_tris = loaded.regions[0].evaluate_both(&[]).0.indices.len() / 3;
        assert_eq!(
            loaded_tris, built_tris,
            "and the whole fold is identical, not merely reaching far enough"
        );
    }

    /// How far in +Z (WT) the up-facing surface at height `y` WT reaches.
    ///
    /// Vertex **extent**, not triangle centroids: the extrusion's top is one big quad, so
    /// both its triangles have centroids well short of their own far edge — a centroid
    /// test reports the surface as absent while it is plainly there.
    fn top_face_reach_z(world: &mut World, y: f32) -> f32 {
        let mesh = world.regions[0].evaluate_both(&[]).0;
        let mut reach = f32::NEG_INFINITY;
        for tri in mesh.indices.chunks_exact(3) {
            if mesh.vertices[tri[0] as usize].normal[1] <= 0.9 {
                continue;
            }
            for &i in tri {
                let p = mesh.vertices[i as usize].pos;
                if ((p[1] / WORLD_SCALE) - y).abs() < 0.01 {
                    reach = reach.max(p[2] / WORLD_SCALE);
                }
            }
        }
        reach
    }

    /// The ordering invariant itself, stated directly: however clustering walks the
    /// brushes, a region always folds them in authoring (ascending id) order.
    #[test]
    fn reclustering_restores_ascending_brush_order() {
        let mut world = World::new();
        extend_room(&mut world);
        let (ou, ov) = begin_on_surface(&mut world, -1.4);
        draw_outline(
            &mut world,
            &[(ou + 12, ov), (ou + 12, ov + 18), (ou, ov + 18), (ou, ov)],
        );
        click_corner(&mut world, (ou, ov));

        world.recluster_all();
        for r in &world.regions {
            let ids: Vec<u32> = r.brushes.iter().map(|b| b.id).collect();
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            assert_eq!(ids, sorted, "region {} folds in authoring order", r.id);
        }
        // Undo/redo go through the same path.
        world.undo();
        world.redo();
        for r in &world.regions {
            let ids: Vec<u32> = r.brushes.iter().map(|b| b.id).collect();
            let mut sorted = ids.clone();
            sorted.sort_unstable();
            assert_eq!(ids, sorted, "still ordered after undo/redo");
        }
    }

    /// A different room's floor at the same height must NOT join the group — coplanar
    /// isn't enough, the faces have to be contiguous. Otherwise drawing in one room would
    /// range over the whole level.
    #[test]
    fn a_detached_coplanar_floor_is_a_different_surface() {
        let mut world = World::new();
        // A second room on the same floor plane but well clear of the first.
        let id = world.next_brush_id;
        world.next_brush_id += 1;
        world.regions[0]
            .brushes
            .push(Brush::new(id, Op::Subtract, 100.0, 0.0, 100.0, 24.0, 16.0, 24.0));
        world.recluster_all();
        world.initial_meshes();
        world.camera.pitch = -1.4;
        world.draw_tool_key();
        let face = world.selected_draw_face().expect("the floor resolves");

        assert_eq!(face.rects.len(), 1, "the detached room's floor stayed out");
        assert_eq!((face.u_max, face.v_max), (24, 24), "bounds are the local room's");
        assert!(!face.covers_cell(110, 110), "the far room is not this surface");
    }

    /// A ceiling and the floor above it share a plane but are opposite sides of it, so
    /// they're different surfaces. Matching on `side` is what keeps them apart.
    #[test]
    fn a_coplanar_opposite_facing_surface_is_excluded() {
        let mut world = World::new();
        // A room stacked directly on top: its floor (Y-Min at 16) is the same plane as
        // this room's ceiling (Y-Max at 16).
        let id = world.next_brush_id;
        world.next_brush_id += 1;
        world.regions[0]
            .brushes
            .push(Brush::new(id, Op::Subtract, 0.0, 16.0, 0.0, 24.0, 16.0, 24.0));
        world.recluster_all();
        let brushes = world.regions[0].brushes.clone();
        let room = brushes.iter().find(|b| b.id == 1).copied().expect("the first room");

        // The ceiling group of the lower room: Y-Max at 16.
        let group = coplanar_face_group(&brushes, &room, Axis::Y, Side::Max, Axis::X, Axis::Z);
        assert_eq!(
            group.len(),
            1,
            "the upper room's floor is the same plane but faces the other way"
        );
    }

    /// Vertices are clamped only to the group's *bounding box*, so an L-shaped surface has
    /// a corner of solid rock inside that box. A shape drawn over it is trimmed to the
    /// real surface instead of carving into the solid — which is what lets the tool give
    /// the author free rein over the bbox without risking a breach.
    #[test]
    fn a_shape_drawn_over_an_l_surfaces_missing_corner_is_trimmed() {
        let face = DrawFace {
            region_id: 0,
            axis: Axis::Y,
            side: Side::Min,
            position: 0.0,
            u_axis: Axis::X,
            v_axis: Axis::Z,
            // An L: the (10..20, 10..20) corner of the bbox is solid.
            u_min: 0,
            u_max: 20,
            v_min: 0,
            v_max: 20,
            rects: vec![[0, 20, 0, 10], [0, 10, 0, 20]],
            scheme: default_scheme(),
        };
        assert!(face.covers_cell(15, 5), "inside the bottom arm");
        assert!(face.covers_cell(5, 15), "inside the left arm");
        assert!(!face.covers_cell(15, 15), "the missing corner is not surface");

        // Draw the full 20×20 bbox square over the whole L.
        let square = [(0, 0), (20, 0), (20, 20), (0, 20)];
        let unmasked: i32 = rect_decompose(&square).iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(unmasked, 400, "the outline itself encloses the whole box");

        let masked = rect_decompose_where(&square, |u, v| face.covers_cell(u, v));
        let area: i32 = masked.iter().map(|&(_, _, w, h)| w * h).sum();
        assert_eq!(area, 300, "trimmed to the L's real 300 WT² of surface");
        for &(u, v, w, h) in &masked {
            for cu in u..u + w {
                for cv in v..v + h {
                    assert!(
                        face.covers_cell(cu, cv),
                        "rect cell ({cu}, {cv}) is off-surface"
                    );
                }
            }
        }
    }

    // ─── Edge / corner disambiguation ────────────────────────────────────────

    /// Aim the fly-cam from the room centre at a WT point on the room's surface.
    fn aim_at(world: &mut World, target: Vec3) {
        let eye = Vec3::new(12.0, 8.0, 12.0) * WORLD_SCALE;
        world.camera.pos = eye;
        let d = (target * WORLD_SCALE - eye).normalize();
        // `camera::forward_from` convention: fwd = (-sin yaw · cos pitch, sin pitch,
        // -cos yaw · cos pitch).
        world.camera.pitch = d.y.asin();
        world.camera.yaw = (-d.x).atan2(-d.z);
    }

    /// Aiming into the corner where floor and two walls meet must offer all three faces,
    /// not silently commit to whichever normal the physics engine reported. This is the
    /// ambiguity the tint + scroll cycle exist to resolve.
    #[test]
    fn an_edge_offers_two_candidate_surfaces_and_a_corner_three() {
        let mut world = World::new();
        world.initial_meshes();

        // Middle of the floor: unambiguous.
        aim_at(&mut world, Vec3::new(12.0, 0.0, 12.0));
        assert_eq!(world.candidate_faces().len(), 1, "a plain floor has one surface");

        // The floor/−X-wall edge, and the floor/−X/−Z corner.
        //
        // Aimed a hair *off* the mathematical edge and vertex, not exactly at them: a ray
        // through a trimesh's shared edge or corner vertex is degenerate and the physics
        // raycast misses outright (`pick_face_hit` returns `None` there too). 0.05 WT =
        // 1.25 cm is well inside the 0.15 WT plane tolerance, so all the meeting faces are
        // still candidates — and it is what aiming at a corner actually looks like.
        aim_at(&mut world, EDGE);
        assert_eq!(
            world.candidate_faces().len(),
            2,
            "an edge where floor meets wall offers both"
        );

        aim_at(&mut world, CORNER);
        assert_eq!(
            world.candidate_faces().len(),
            3,
            "a corner where three faces meet offers all three"
        );
    }

    /// Just off the floor/−X-wall edge, and just off the floor/−X/−Z corner (see
    /// `an_edge_offers_two_candidate_surfaces_and_a_corner_three` on why not exactly on).
    const EDGE: Vec3 = Vec3::new(0.0, 0.05, 12.0);
    const CORNER: Vec3 = Vec3::new(0.0, 0.05, 0.05);

    /// Index 0 is always what the old code would have picked, so the cycle is purely
    /// additive — the tool behaves identically until the author scrolls.
    #[test]
    fn the_first_candidate_matches_the_plain_pick() {
        let mut world = World::new();
        world.initial_meshes();
        for target in [Vec3::new(12.0, 0.0, 12.0), EDGE, CORNER] {
            aim_at(&mut world, target);
            let (plain, _) = world.pick_face_hit().expect("something under the crosshair");
            let first = world.candidate_faces()[0];
            assert_eq!(
                (first.axis, first.side),
                (plain.axis, plain.side),
                "candidate 0 is the dominant-normal pick at {target:?}"
            );
        }
    }

    /// Scrolling cycles the chosen surface, wraps both ways, and lands the first corner on
    /// whichever surface was showing.
    #[test]
    fn scrolling_cycles_the_surface_the_first_corner_lands_on() {
        let mut world = World::new();
        world.initial_meshes();
        world.draw_tool_key();
        aim_at(&mut world, EDGE); // floor/wall edge

        let all: Vec<(Axis, Side)> = world
            .candidate_faces()
            .iter()
            .map(|c| (c.axis, c.side))
            .collect();
        assert_eq!(all.len(), 2);
        assert_eq!(world.selected_draw_face().unwrap().axis, all[0].0);

        world.cycle_draw_face(1.0);
        assert_eq!(
            world.selected_draw_face().unwrap().axis,
            all[1].0,
            "one scroll up moves to the second surface"
        );
        world.cycle_draw_face(1.0);
        assert_eq!(
            world.selected_draw_face().unwrap().axis,
            all[0].0,
            "and wraps back around"
        );
        world.cycle_draw_face(-1.0);
        assert_eq!(
            world.selected_draw_face().unwrap().axis,
            all[1].0,
            "scrolling down wraps the other way without underflowing"
        );

        // The first corner commits to whatever was showing.
        world.draw_click();
        assert_eq!(
            world.draw_face.as_ref().map(|f| f.axis),
            Some(all[1].0),
            "the first corner landed on the cycled-to surface"
        );
        // And the surface is frozen from here — cycling is idle-phase only.
        world.cycle_draw_face(1.0);
        assert_eq!(
            world.draw_face.as_ref().map(|f| f.axis),
            Some(all[1].0),
            "scroll no longer re-picks the surface once drawing has started"
        );
    }

    /// The tint covers the whole coplanar surface (so it reads as "this plane"), tracks the
    /// cycle while idle, and disappears with the tool.
    #[test]
    fn the_surface_tint_covers_the_group_and_follows_the_cycle() {
        let mut world = World::new();
        extend_room(&mut world);
        world.initial_meshes();
        assert!(
            world.draw_surface_tint_mesh().is_none(),
            "no tint while the tool is off"
        );

        world.draw_tool_key();
        aim_at(&mut world, Vec3::new(12.0, 0.0, 12.0));
        let tint = world.draw_surface_tint_mesh().expect("tint on the floor");
        // Two floor brushes in the group → two quads → 8 verts, 12 indices.
        assert_eq!(world.selected_draw_face().unwrap().rects.len(), 2);
        assert_eq!(
            tint.indices.len(),
            12,
            "one quad per group member, covering the whole surface"
        );

        // On an edge the tint follows the cycle onto the other plane.
        aim_at(&mut world, EDGE);
        let before = world.selected_draw_face().unwrap().axis;
        world.cycle_draw_face(1.0);
        let after = world.selected_draw_face().unwrap().axis;
        assert_ne!(before, after, "the cycle moved to the other plane");
        assert!(
            world.draw_surface_tint_mesh().is_some(),
            "and the tint follows it"
        );

        world.cancel_draw();
        assert!(
            world.draw_surface_tint_mesh().is_none(),
            "disarming clears the tint"
        );
    }

    /// The tool refuses to start on solid geometry — "out of the face" is only
    /// meaningful relative to a room's interior, matching the pillar/brace tools.
    #[test]
    fn drawing_only_starts_on_a_room_surface() {
        let mut world = World::new();
        world.initial_meshes();
        // Aim into the void outside the level: the raycast finds nothing at all.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.draw_tool_key();
        world.draw_click();
        assert_eq!(
            world.draw_phase,
            Some(DrawPhase::Idle),
            "no surface, no first corner"
        );
        assert!(world.draw_verts.is_empty());
        assert!(
            world.update_draw_preview().is_none(),
            "and no ghost to draw"
        );
    }
}
