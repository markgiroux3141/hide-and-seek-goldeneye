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
            self.clear_draw_state();
            self.opening_tool = Some(kind);
            self.selected = None;
            // The face pick is gone, so the sub-face carve it was growing must go too.
            // Beyond tidiness: a live `active` op makes `patch_ids` collapse to the
            // anchor (an in-progress carve is deliberately not a room), which would
            // quietly cap this tool back to one brush.
            self.reset_subface();
            if kind == OpeningKind::Hole {
                self.hole_w = HOLE_WIDTH;
                self.hole_h = HOLE_HEIGHT;
            }
            self.opening_preview = self.resolve_opening_placement();
        }
        None
    }

    /// What the armed opening tool is doing, for the BUILD status strip — either
    /// "ready" or the reason it is refusing.
    ///
    /// The refusal half is the point. This tool previews by re-picking the crosshair
    /// face every frame, so when it declines it simply draws nothing, and every reason
    /// it might decline for looks identical from the outside: a floor under the
    /// crosshair, a face too small for the opening, a ray that reached no authorable
    /// surface. Naming them turns "the ghost is broken" into "you are aiming at the
    /// floor".
    pub fn opening_status(&self) -> Option<String> {
        let kind = self.opening_tool?;
        let name = match kind {
            OpeningKind::Door => {
                if self.door_double {
                    "DOOR (double)"
                } else {
                    "DOOR"
                }
            }
            OpeningKind::Hole => "HOLE",
        };
        Some(match (self.opening_preview.is_some(), self.opening_refusal.as_deref()) {
            (true, _) => format!("{name}  click to cut"),
            (false, Some(why)) => format!("{name}  {why}"),
            (false, None) => format!("{name}  aim at a wall"),
        })
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
    ///
    /// Returns **every** rebuilt region mesh, not one.
    ///
    /// This used to hand back a single `Option<RegionMesh>` via
    /// `rebuild_affected_regions(..).into_iter().next()`, which is lossless only while
    /// the edit stays inside one region. It doesn't: a carve that touches two regions
    /// **merges** them, `assign_brush_to_region` bails, and the whole level reclusters
    /// into fresh ids — so the result is one mesh per surviving region *plus an empty
    /// mesh for every id that just stopped existing*, and those empties are how the
    /// renderer is told to drop the old geometry. Keeping only the first left the dead
    /// regions on screen: the pre-cut rooms kept drawing (no opening visible from
    /// either side) with the new merged region painted over them.
    pub fn confirm_opening(&mut self) -> Vec<RegionMesh> {
        if self.opening_tool.is_none() {
            return Vec::new();
        }
        self.opening_tool = None;
        let placement = self.opening_preview.take().or_else(|| self.resolve_opening_placement());
        match placement {
            Some(p) => self.cut_opening(p),
            None => Vec::new(),
        }
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
        // While the *door* tool is armed the same scroll toggles single ↔ double width,
        // rather than free-sizing: a doorway only has two useful widths, and a double is
        // exactly two single leaves (see `DOOR_WIDTH_DOUBLE`).
        if self.opening_tool == Some(OpeningKind::Door) {
            if du != 0.0 {
                self.door_double = du > 0.0;
            }
            return;
        }
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
        self.opening_refusal = None;
        let kind = self.opening_tool?;
        if self.mode != Mode::Build {
            return None;
        }
        let Some((sel, hit_wt)) = self.pick_face_hit() else {
            // Either the ray reached nothing, or it landed somewhere no *brush* face
            // explains — the shell's own skin, most often, which is not authorable.
            self.opening_refusal = Some("nothing under the crosshair".into());
            return None;
        };
        if kind == OpeningKind::Door && sel.axis == Axis::Y {
            // Doors go in walls only (JS rejects axis 'y'). Worth saying out loud: aim
            // low at a wall from far enough back and the ray clips the **floor** first,
            // so the ghost vanishes as you retreat and returns as you close in — which
            // reads as a distance bug rather than as "you are pointing at the floor".
            self.opening_refusal =
                Some("that is a floor/ceiling - doors go in walls only".into());
            return None;
        }
        let Some(region) = self.regions.iter().find(|r| r.id == sel.region_id) else {
            self.opening_refusal = Some("that surface's region is gone".into());
            return None;
        };
        let Some(brush) = region.brushes.iter().find(|b| b.id == sel.brush_id).copied() else {
            self.opening_refusal = Some("that surface's brush is gone".into());
            return None;
        };
        let position = brush.face_pos(sel.axis, sel.side);

        // Face UV bounds (JS `getFaceUVInfo`): the two axes orthogonal to the face
        // normal. The opening must fit within them.
        //
        // With patch scope on these are the bounds of the whole coplanar **patch**, not
        // of the one brush the crosshair happens to be over — which is what lets a hole
        // span a floor built from several carves. A room is decomposed into boxes for
        // reasons the author never sees, and before this the hole tool silently capped
        // itself at whichever box you were standing on. The cut itself is unchanged: it
        // was always a single frame box, and a box does not care that it crosses a seam.
        let (u_axis, v_axis) = sel.axis.orthogonals();
        let Some([u_min, u_max, v_min, v_max]) = self.patch_bounds(sel) else {
            self.opening_refusal = Some("could not measure that face".into());
            return None;
        };
        let (face_w, face_h) = (u_max - u_min, v_max - v_min);

        let (w, h) = match kind {
            OpeningKind::Door => (
                if self.door_double { DOOR_WIDTH_DOUBLE } else { DOOR_WIDTH },
                DOOR_HEIGHT,
            ),
            OpeningKind::Hole => (self.hole_w.min(face_w), self.hole_h.min(face_h)),
        };
        if face_w < w || face_h < h || w < 1.0 || h < 1.0 {
            self.opening_refusal = Some(format!(
                "that face is {face_w:.0}x{face_h:.0} WT - this opening needs {w:.0}x{h:.0}"
            ));
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
            scheme: brush.scheme,
        })
    }

    /// Cut the opening at a resolved placement (JS `confirmHolePlacement`): a frame
    /// subtract through the face + a 1-WT protoroom subtract just beyond, so it
    /// opens into navigable space, not solid. A door's frame is `door`-marked
    /// (breakable at HUNT); a hole's isn't.
    ///
    /// Both carves inherit the pierced wall's theme, which the JS did *not* do — it
    /// left them on the default. That default is what the author then sees, because
    /// the reveal surfaces lie on the frame's own faces (so `uv_zones` reads the
    /// frame's scheme for the per-theme doorframe zones 5/6), and because the
    /// protoroom is the seed the next push grows the room beyond into. Cutting a
    /// doorway out of a themed room and pushing it out therefore produced a
    /// default-themed room every time.
    pub(crate) fn cut_opening(&mut self, p: OpeningPlacement) -> Vec<RegionMesh> {
        let t = WALL_THICKNESS;
        // Frame carve: 1 WT deep along the face normal, at the face plane.
        let frame_a = if p.side == Side::Max { p.position } else { p.position - t };
        let mut frame = make_wall_brush(
            self.next_brush_id, p.axis, frame_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        frame.door = p.kind == OpeningKind::Door;
        frame.frame = true; // opening reveal → tunnel zones (5/6) in uv_zones
        frame.scheme = p.scheme;
        self.next_brush_id += 1;

        // Protoroom carve: 1 WT deep just beyond the frame.
        let proto_a = if p.side == Side::Max { p.position + t } else { p.position - 2.0 * t };
        let mut proto = make_wall_brush(
            self.next_brush_id, p.axis, proto_a, t, p.u_axis, p.u0, p.w, p.v_axis, p.v0, p.h,
        );
        proto.scheme = p.scheme;
        self.next_brush_id += 1;

        let frame_id = frame.id;
        let proto_id = proto.id;
        let Some(region) = self.regions.iter_mut().find(|r| r.id == p.region_id) else {
            return Vec::new();
        };
        region.brushes.push(frame);
        region.brushes.push(proto);
        log::info!("{:?} cut in region {} at {:?} {:?}", p.kind, p.region_id, p.axis, p.side);
        // Incremental where it can be — but a doorway cut between two *separate*
        // regions (two rooms drawn apart by the room plan tool, say) merges them, and
        // that path reclusters the level and returns a mesh per region plus a clear
        // per dead id. All of them have to reach the renderer.
        self.rebuild_affected_regions(&[frame_id, proto_id])
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
    pub fn confirm_door(&mut self) -> Vec<RegionMesh> {
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

/// The opening tool's **refusal reasons**.
///
/// This tool previews by re-picking the crosshair face every frame, so when it
/// declines it simply draws nothing — and every reason it might decline for looks
/// identical from the outside. The floor case in particular reads as a *distance*
/// bug: aim low at a wall and back away, and past some distance the ray clips the
/// floor first, so the ghost vanishes as you retreat and returns as you close in.
#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// The booted room, with the door tool armed and the camera in the middle of it.
    fn armed_in_room() -> World {
        let mut w = World::new();
        w.initial_meshes();
        w.door_tool_key();
        w.camera.pos = Vec3::new(12.0, 8.0, 12.0) * WORLD_SCALE;
        w.camera.yaw = std::f32::consts::PI; // +Z
        w.camera.pitch = 0.0;
        w
    }

    /// Aiming level at a wall places a door and says so.
    #[test]
    fn a_wall_in_the_crosshair_reports_ready() {
        let mut w = armed_in_room();
        assert!(w.update_door_preview().is_some(), "the ghost is drawn");
        let status = w.build_status().expect("the strip has a line");
        assert!(status.starts_with("DOOR"), "names the tool: {status}");
        assert!(status.contains("click to cut"), "and says it is ready: {status}");
    }

    /// **The distance illusion.** Steep enough and far enough back, the crosshair ray
    /// reaches the floor before the wall — so the ghost disappears as you retreat. The
    /// tool must name that, because "no ghost at 20 WT, ghost at 4 WT" on one wall is
    /// otherwise indistinguishable from that wall being broken.
    #[test]
    fn aiming_down_from_far_back_hits_the_floor_and_says_so() {
        let mut w = armed_in_room();
        w.camera.pitch = -30f32.to_radians();

        // Close to the wall: the ray still reaches it.
        w.camera.pos = Vec3::new(12.0, 8.0, 22.0) * WORLD_SCALE;
        assert!(w.update_door_preview().is_some(), "up close the wall is still in reach");

        // Backed off: the same aim now lands on the floor first.
        w.camera.pos = Vec3::new(12.0, 8.0, 4.0) * WORLD_SCALE;
        assert!(w.update_door_preview().is_none(), "from back here the ray clips the floor");
        let status = w.build_status().expect("armed, so the strip has a line");
        assert!(
            status.contains("floor/ceiling") && status.contains("walls only"),
            "the strip explains the floor, rather than leaving a silent gap: {status}"
        );
    }

    /// Aimed at nothing — outside the level, pointing away — the strip says so rather
    /// than leaving the author to wonder. This also covers a ray that lands on the
    /// shell'''s own skin, which is no longer drawn and resolves to no brush face: it
    /// would otherwise be an invisible surface refusing for invisible reasons.
    #[test]
    fn aiming_at_nothing_reports_nothing_under_the_crosshair() {
        let mut w = World::new();
        w.initial_meshes();
        w.door_tool_key();
        // Well outside the level, looking further out.
        w.camera.pos = Vec3::new(12.0, 8.0, -60.0) * WORLD_SCALE;
        w.camera.yaw = 0.0; // -Z, away from the room
        w.camera.pitch = 0.0;
        assert!(w.update_door_preview().is_none(), "there is nothing out here");
        let status = w.build_status().expect("armed");
        assert!(
            status.contains("nothing under the crosshair"),
            "the strip names the empty aim: {status}"
        );
    }

    /// The strip is silent when no tool is armed — it must not become permanent chrome.
    #[test]
    fn the_strip_says_nothing_when_no_tool_is_armed() {
        let mut w = World::new();
        w.initial_meshes();
        assert!(w.build_status().is_none(), "nothing armed, nothing to say");
        w.door_tool_key();
        assert!(w.build_status().is_some(), "armed");
        w.cancel_door();
        assert!(w.build_status().is_none(), "disarmed again");
    }

    /// A refusal must not outlive the thing it was refusing about, or the strip ends up
    /// reporting a stale reason while the ghost is plainly on screen.
    #[test]
    fn a_refusal_clears_as_soon_as_the_aim_is_good_again() {
        let mut w = armed_in_room();
        w.camera.pitch = -30f32.to_radians();
        w.camera.pos = Vec3::new(12.0, 8.0, 4.0) * WORLD_SCALE;
        assert!(w.update_door_preview().is_none());
        assert!(w.build_status().unwrap().contains("floor/ceiling"));

        w.camera.pitch = 0.0;
        assert!(w.update_door_preview().is_some(), "level again, so the wall is back");
        assert!(
            w.build_status().unwrap().contains("click to cut"),
            "and the stale reason is gone"
        );
    }
}
