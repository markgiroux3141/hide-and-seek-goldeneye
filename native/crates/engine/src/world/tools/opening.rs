//! Opening tool (door + hole): arm/confirm/cancel, the crosshair ghost
//! preview, scroll sizing, and the frame+protoroom cut.

use super::super::*;

impl World {
    // ─── Opening tools: door (fixed, breakable) + hole (arbitrary, any face) ──

    /// Whether a crosshair opening tool is armed (door or hole). The app draws
    /// the ghost and routes a left-click confirm while this is true.
    pub fn is_opening_arming(&self) -> bool {
        self.opening_tool.is_some()
    }

    /// Whether the *hole* tool specifically is armed (so the app routes scroll to
    /// hole sizing instead of sub-face selection).
    pub fn is_hole_arming(&self) -> bool {
        self.opening_tool == Some(OpeningKind::Hole)
    }

    /// Arm/toggle a crosshair opening tool, BUILD only (JS `setHoleMode`). Pressing
    /// the same tool's key again disarms; a different key switches tools. Never
    /// cuts (the cut is a left-click), so it returns `None`.
    pub(crate) fn arm_opening(&mut self, kind: OpeningKind) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        if self.opening_tool == Some(kind) {
            self.cancel_opening(); // same key again = deselect
        } else {
            // The ghost preview owns the highlight, so drop any face pick and any
            // other armed tool.
            self.place_tool = None;
            self.clear_platform_state();
            self.opening_tool = Some(kind);
            self.selected = None;
            if kind == OpeningKind::Hole {
                self.hole_w = HOLE_WIDTH;
                self.hole_h = HOLE_HEIGHT;
            }
            self.opening_preview = self.resolve_opening_placement();
        }
        None
    }

    /// Door tool key (`B`): arm/toggle the fixed breakable door.
    pub fn door_tool_key(&mut self) -> Option<RegionMesh> {
        self.arm_opening(OpeningKind::Door)
    }

    /// Hole tool key (`H`): arm/toggle the arbitrary-size opening (any face).
    pub fn hole_tool_key(&mut self) -> Option<RegionMesh> {
        self.arm_opening(OpeningKind::Hole)
    }

    /// Confirm the armed opening (left-click). Cuts at the previewed placement,
    /// falling back to a fresh crosshair resolve.
    pub fn confirm_opening(&mut self) -> Option<RegionMesh> {
        self.opening_tool?;
        self.opening_tool = None;
        let placement = self.opening_preview.take().or_else(|| self.resolve_opening_placement());
        placement.and_then(|p| self.cut_opening(p))
    }

    /// Cancel an armed opening without cutting (Esc / pointer release / mode switch).
    pub fn cancel_opening(&mut self) {
        self.opening_tool = None;
        self.opening_preview = None;
    }

    /// Recompute the ghost from the crosshair (each frame while arming) and return
    /// the ghost quad, or `None` if the crosshair isn't on a suitable face.
    pub fn update_opening_preview(&mut self) -> Option<CpuMesh> {
        self.opening_tool?;
        self.opening_preview = self.resolve_opening_placement();
        self.opening_preview.map(|p| self.opening_preview_mesh(&p))
    }

    /// Scroll-size the hole (only while the hole tool is armed): `du` widens (U),
    /// `dv` heightens (V), in ±1 WT steps, clamped to ≥1. The upper clamp to the
    /// face happens in [`resolve_opening_placement`](Self::resolve_opening_placement).
    pub fn adjust_opening_size(&mut self, du: f32, dv: f32) {
        if self.opening_tool != Some(OpeningKind::Hole) {
            return;
        }
        if du != 0.0 {
            self.hole_w = (self.hole_w + du).max(1.0);
        }
        if dv != 0.0 {
            self.hole_h = (self.hole_h + dv).max(1.0);
        }
    }

    /// Resolve an opening placement from the crosshair (JS `computeHolePreview`):
    /// the face hit → a `w × h` opening centered on the hit, clamped to the face
    /// and WT-snapped. Door: fixed 3×7, walls only. Hole: `hole_w × hole_h`
    /// (clamped to the face), any face incl. floor/ceiling. `None` if the face is
    /// unsuitable or too small.
    pub(crate) fn resolve_opening_placement(&mut self) -> Option<OpeningPlacement> {
        let kind = self.opening_tool?;
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if kind == OpeningKind::Door && sel.axis == Axis::Y {
            return None; // doors go in walls only (JS rejects axis 'y')
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let position = brush.face_pos(sel.axis, sel.side);

        // Face UV bounds (JS `getFaceUVInfo`): the two axes orthogonal to the face
        // normal. The opening must fit within them.
        let (u_axis, v_axis) = sel.axis.orthogonals();
        let (u_min, u_max) = (brush.min(u_axis), brush.min(u_axis) + brush.dim(u_axis));
        let (v_min, v_max) = (brush.min(v_axis), brush.min(v_axis) + brush.dim(v_axis));
        let (face_w, face_h) = (u_max - u_min, v_max - v_min);

        let (w, h) = match kind {
            OpeningKind::Door => (DOOR_WIDTH, DOOR_HEIGHT),
            OpeningKind::Hole => (self.hole_w.min(face_w), self.hole_h.min(face_h)),
        };
        if face_w < w || face_h < h || w < 1.0 || h < 1.0 {
            return None;
        }

        let u0 = ((u_axis.component(hit_wt) - w / 2.0).round()).clamp(u_min, u_max - w);
        let v0 = ((v_axis.component(hit_wt) - h / 2.0).round()).clamp(v_min, v_max - h);

        Some(OpeningPlacement {
            region_id: sel.region_id,
            axis: sel.axis,
            side: sel.side,
            position,
            u_axis,
            v_axis,
            u0,
            v0,
            w,
            h,
            kind,
        })
    }

    /// Cut the opening at a resolved placement (JS `confirmHolePlacement`): a frame
    /// subtract through the face + a 1-WT protoroom subtract just beyond, so it
    /// opens into navigable space, not solid. A door's frame is `door`-marked
    /// (breakable at HUNT); a hole's isn't.
    pub(crate) fn cut_opening(&mut self, p: OpeningPlacement) -> Option<RegionMesh> {
        let t = WALL_THICKNESS;
        // Frame carve: 1 WT deep along the face normal, at the face plane.
        let frame_a = if p.side == Side::Max { p.position } else { p.position - t };
        let mut frame = make_wall_brush(
            self.next_brush_id, p.axis, frame_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        frame.door = p.kind == OpeningKind::Door;
        frame.frame = true; // opening reveal → tunnel zones (5/6) in uv_zones
        self.next_brush_id += 1;

        // Protoroom carve: 1 WT deep just beyond the frame.
        let proto_a = if p.side == Side::Max { p.position + t } else { p.position - 2.0 * t };
        let proto = make_wall_brush(
            self.next_brush_id, p.axis, proto_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        self.next_brush_id += 1;

        let region = self.regions.iter_mut().find(|r| r.id == p.region_id)?;
        region.brushes.push(frame);
        region.brushes.push(proto);
        log::info!("{:?} cut in region {} at {:?} {:?}", p.kind, p.region_id, p.axis, p.side);
        self.rebuild_region(p.region_id)
    }

    /// The ghost preview quad (meters) for an opening placement — the opening rect
    /// on the face. Drawn via the translucent highlight pipeline.
    pub(crate) fn opening_preview_mesh(&self, p: &OpeningPlacement) -> CpuMesh {
        self.face_quad_mesh(
            p.axis, p.side, p.position, p.u_axis, p.v_axis, p.u0, p.u0 + p.w, p.v0, p.v0 + p.h,
        )
    }

    // ── Door-named wrappers, kept so the door tests/callers stay stable. ──

    /// Whether the *door* tool specifically is armed.
    pub fn is_door_arming(&self) -> bool {
        self.opening_tool == Some(OpeningKind::Door)
    }

    /// Confirm the armed door (delegates to the generic opening confirm).
    pub fn confirm_door(&mut self) -> Option<RegionMesh> {
        self.confirm_opening()
    }

    /// Cancel an armed door (delegates to the generic opening cancel).
    pub fn cancel_door(&mut self) {
        self.cancel_opening()
    }

    /// Recompute the door ghost (delegates to the generic opening preview).
    pub fn update_door_preview(&mut self) -> Option<CpuMesh> {
        self.update_opening_preview()
    }
}
