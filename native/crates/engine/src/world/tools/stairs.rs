//! Arrow-key CSG stair tool: pending-op accumulate, confirm into void
//! brushes + a `StairDesc`, cancel, and the x-ray ghost preview.

use super::super::*;

impl World {
    // ─── Stair tool (arrow keys) ─────────────────────────────────────────

    /// Whether a stair op is pending (the arrow-key counter is non-empty). The
    /// app reads this to route Esc (cancel the stair vs. release the cursor) and
    /// to show the step-count message.
    pub fn has_pending_stair(&self) -> bool {
        self.pending_stair.is_some()
    }

    /// The pending stair's step count + direction, for the app's status message.
    pub fn pending_stair(&self) -> Option<(u32, StairDir)> {
        self.pending_stair.map(|p| (p.step_count, p.direction))
    }

    /// Whether the bottom of the current selection sits on the face's floor
    /// (JS `wallSelectionTouchesFloor`). Stairs must start from the floor. Only
    /// meaningful for walls (axis ≠ Y). A full-V selection implicitly touches it.
    pub(crate) fn wall_selection_touches_floor(&mut self) -> bool {
        let Some(sel) = self.selected else { return false };
        if sel.axis == Axis::Y {
            return false;
        }
        let Some(info) = self.selected_face_info() else { return false };
        if self.sel_size_v <= 0.0 {
            return true; // full-V selection reaches the floor
        }
        match self.ensure_selection_bounds() {
            Some([_, _, v0, _]) => (v0 - info.v_min).abs() < 1e-3,
            None => false,
        }
    }

    /// Arrow-key handler (JS `pushSelectedFaceAsStairs`), BUILD only. `direction`
    /// grows the pending step counter; the opposite direction shrinks it (to zero
    /// = cancel). No geometry changes until [`confirm_stairs`](Self::confirm_stairs).
    /// Requires a selected wall face whose selection touches the floor. Returns
    /// whether a pending op is now active (for the app's message).
    pub fn push_stairs(&mut self, direction: StairDir) -> bool {
        if self.mode != Mode::Build {
            log::info!("push_stairs: not in BUILD mode");
            return false;
        }
        // Auto-pick the crosshair face if nothing's selected yet (matches how
        // push/pull behave — no separate click required).
        let Some(sel) = self.resolve_selection() else {
            log::info!("push_stairs: no face under the crosshair to anchor to");
            return false;
        };
        if sel.axis == Axis::Y {
            log::info!("push_stairs: selected a floor/ceiling (axis Y) — stairs need a wall");
            return false; // floors/ceilings aren't stair anchors
        }
        let Some(info) = self.selected_face_info() else {
            log::info!("push_stairs: selected_face_info returned None");
            return false;
        };
        if !self.wall_selection_touches_floor() {
            log::info!("push_stairs: selection does not touch the floor");
            return false;
        }
        let bounds = match self.ensure_selection_bounds() {
            Some(b) => b,
            None => return false,
        };
        let [u0, u1, _v0, v1] = bounds;
        let floor = info.v_min;
        let ceil = if self.sel_size_v <= 0.0 { info.v_max } else { v1 };
        let face_pos = info.position;

        // Same-anchor test (JS): same face + same sub-rect → adjust the existing
        // counter; otherwise start a fresh 1-step op.
        let same_anchor = self.pending_stair.map(|op| {
            op.region_id == sel.region_id
                && op.axis == sel.axis
                && op.side == sel.side
                && (op.face_pos - face_pos).abs() < 1e-3
                && (op.u0 - u0).abs() < 1e-3
                && (op.u1 - u1).abs() < 1e-3
        }).unwrap_or(false);

        let new_count = match self.pending_stair {
            Some(op) if same_anchor && op.direction == direction => op.step_count + 1,
            Some(op) if same_anchor => {
                // Opposite arrow shrinks the same op; hitting zero cancels it.
                if op.step_count <= 1 {
                    self.pending_stair = None;
                    return false;
                }
                op.step_count - 1
            }
            _ => 1,
        };

        let scheme = self
            .regions
            .iter()
            .find(|r| r.id == sel.region_id)
            .and_then(|r| r.brushes.iter().find(|b| b.id == sel.brush_id))
            .map(|b| b.scheme)
            .unwrap_or(DEFAULT_SCHEME);

        self.pending_stair = Some(PendingStair {
            direction,
            step_count: new_count,
            region_id: sel.region_id,
            axis: sel.axis,
            side: sel.side,
            face_pos,
            u_axis: info.u_axis,
            u0,
            u1,
            floor,
            ceil,
            scheme,
        });
        true
    }

    /// Cancel a pending stair op (Esc), discarding the counter. No geometry was
    /// created yet, so nothing to undo.
    pub fn cancel_stairs(&mut self) {
        self.pending_stair = None;
    }

    /// The [`StairDesc`] a pending op would confirm into (also used for the ghost).
    pub(crate) fn pending_desc(&self) -> Option<StairDesc> {
        let op = self.pending_stair?;
        // The wall-UV anchor is the destination floor (JS descriptor `floorY`):
        // the pit floor for a down-stair, the raised floor for an up-stair.
        let floor_y = match op.direction {
            StairDir::Down => op.floor - op.step_count as f32,
            StairDir::Up => op.floor + op.step_count as f32,
        };
        Some(StairDesc {
            direction: op.direction,
            step_count: op.step_count,
            axis: op.axis,
            side: op.side,
            face_pos: op.face_pos,
            u_axis: op.u_axis,
            u0: op.u0,
            u1: op.u1,
            floor: op.floor,
            ceil: op.ceil,
            floor_y,
            scheme: op.scheme,
            void_ids: [0, 0], // filled in at confirm once brush ids are allocated
        })
    }

    /// A translucent ghost of the pending stair's steps (meters), for the highlight
    /// pipeline — immediate feedback as the arrow keys grow the op. `None` when no
    /// stair is pending.
    pub fn stair_preview_mesh(&self) -> Option<CpuMesh> {
        self.pending_desc().map(|d| d.mesh())
    }

    /// Confirm the pending stair (Enter, JS `confirmStairOp`): create the two
    /// `subtract` void brushes (stairwell + far corridor), register a [`StairDesc`]
    /// on the region (whose treads [`Region::evaluate`] folds into the mesh), and
    /// re-evaluate. Returns the changed region's mesh, or `None` if nothing pending.
    pub fn confirm_stairs(&mut self) -> Option<RegionMesh> {
        if self.pending_stair.is_none() {
            log::info!("confirm_stairs: nothing pending (press ↑/↓ on a floor-touching wall first)");
            return None;
        }
        let desc = self.pending_desc()?;
        let op = self.pending_stair.take()?;
        let sc = op.step_count as f32;
        let dir = if op.side == Side::Max { 1.0 } else { -1.0 };

        // Brush 1: the main stairwell, flush with the wall face.
        let (b1_lo, b1_hi) = if dir > 0.0 {
            (op.face_pos, op.face_pos + sc)
        } else {
            (op.face_pos - sc, op.face_pos)
        };
        let (b1_ymin, b1_ymax) = match op.direction {
            StairDir::Down => (op.floor - sc, op.ceil),
            StairDir::Up => (op.floor, op.ceil + sc),
        };
        let mut brush1 = make_stair_void(
            self.next_brush_id, op.axis, b1_lo, b1_hi, b1_ymin, b1_ymax, op.u_axis, op.u0, op.u1,
        );
        brush1.scheme = op.scheme;
        brush1.floor_y = desc.floor_y;
        self.next_brush_id += 1;

        // Brush 2: the destination corridor, 1 WT deep past the stairwell.
        let (b2_lo, b2_hi) = if dir > 0.0 {
            (op.face_pos + sc, op.face_pos + sc + 1.0)
        } else {
            (op.face_pos - sc - 1.0, op.face_pos - sc)
        };
        let (b2_ymin, b2_ymax) = match op.direction {
            StairDir::Down => (op.floor - sc, op.ceil - sc),
            StairDir::Up => (op.floor + sc, op.ceil + sc),
        };
        let mut brush2 = make_stair_void(
            self.next_brush_id, op.axis, b2_lo, b2_hi, b2_ymin, b2_ymax, op.u_axis, op.u0, op.u1,
        );
        brush2.scheme = op.scheme;
        brush2.floor_y = desc.floor_y;
        self.next_brush_id += 1;

        let mut desc = desc;
        desc.void_ids = [brush1.id, brush2.id];
        let region = self.regions.iter_mut().find(|r| r.id == op.region_id)?;
        region.brushes.push(brush1);
        region.brushes.push(brush2);
        region.stairs.push(desc);
        log::info!(
            "stairs confirmed: {} step(s) {:?} in region {}",
            op.step_count, op.direction, op.region_id
        );
        self.rebuild_region(op.region_id)
    }
}
