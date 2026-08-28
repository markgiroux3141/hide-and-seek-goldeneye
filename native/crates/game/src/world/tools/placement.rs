//! Additive placement tools (pillar + brace): arm/confirm/cancel, scroll
//! sizing, ghost preview, and the brush resolvers.

use super::super::*;

impl World {
    // ─── Placement tools: pillar (column) + brace (arch) ─────────────────────

    /// Whether a placement tool (pillar/brace) is armed. The app draws its ghost
    /// and routes a left-click confirm + scroll sizing while this is true.
    pub fn is_placing(&self) -> bool {
        self.place_tool.is_some()
    }

    /// Arm/toggle a placement tool, BUILD only. Same key again disarms; a
    /// different tool switches. Cancels any armed opening tool.
    pub(crate) fn arm_place(&mut self, kind: PlaceKind) {
        if self.mode != Mode::Build {
            return;
        }
        if self.place_tool == Some(kind) {
            self.place_tool = None;
        } else {
            self.opening_tool = None;
            self.opening_preview = None;
            self.clear_platform_state();
            self.clear_draw_state();
            self.selected = None;
            self.prop_tool = None;
            self.prop_preview_pos = None;
            self.place_tool = Some(kind);
        }
    }

    /// Pillar tool key (`P`): arm/toggle the floor→ceiling column.
    pub fn pillar_tool_key(&mut self) {
        self.arm_place(PlaceKind::Pillar);
    }

    /// Brace tool key (`R`): arm/toggle the 3-brush wall arch.
    pub fn brace_tool_key(&mut self) {
        self.arm_place(PlaceKind::Brace);
    }

    /// Cancel an armed placement tool (Esc / pointer release).
    pub fn cancel_place(&mut self) {
        self.place_tool = None;
    }

    /// Scroll-size the armed placement tool: pillars use `da` (square size);
    /// braces use `da` (width along the wall) and `db` (depth into the room).
    /// Clamped to the tool's bounds.
    pub fn adjust_place_size(&mut self, da: f32, db: f32) {
        match self.place_tool {
            Some(PlaceKind::Pillar) => {
                self.pillar_size = (self.pillar_size + da).clamp(PILLAR_MIN, PILLAR_MAX);
            }
            Some(PlaceKind::Brace) => {
                if da != 0.0 {
                    self.brace_width = (self.brace_width + da).clamp(BRACE_MIN, BRACE_MAX);
                }
                if db != 0.0 {
                    self.brace_depth = (self.brace_depth + db).clamp(BRACE_MIN, BRACE_MAX);
                }
            }
            None => {}
        }
    }

    /// The ghost mesh for the armed placement tool (each frame while arming), or
    /// `None` if the crosshair isn't on a valid face. Drawn via the highlight
    /// pipeline (translucent boxes).
    pub fn update_place_preview(&mut self) -> Option<CpuMesh> {
        match self.place_tool? {
            PlaceKind::Pillar => {
                let boxes = self.resolve_pillar()?;
                Some(boxes_mesh(&[boxes]))
            }
            PlaceKind::Brace => {
                let boxes = self.resolve_brace()?;
                Some(boxes_mesh(&boxes))
            }
        }
    }

    /// Confirm the armed placement (left-click): add the pillar's single brush or
    /// the brace's three brushes to the region and re-evaluate. Returns the
    /// changed region's mesh, or `None`.
    pub fn confirm_place(&mut self) -> Option<RegionMesh> {
        match self.place_tool? {
            PlaceKind::Pillar => {
                let (region_id, look, b) = self.resolve_pillar_placed()?;
                self.place_tool = None;
                let brush = self.push_add_brush(region_id, look, b)?;
                log::info!("pillar placed in region {region_id} (brush {brush})");
                self.rebuild_affected_regions(&[brush]).into_iter().next()
            }
            PlaceKind::Brace => {
                let (region_id, look, boxes) = self.resolve_brace_placed()?;
                self.place_tool = None;
                let mut ids = Vec::new();
                for b in boxes {
                    if let Some(id) = self.push_add_brush(region_id, look, b) {
                        ids.push(id);
                    }
                }
                log::info!("brace placed in region {region_id}");
                self.rebuild_affected_regions(&ids).into_iter().next()
            }
        }
    }

    /// Push an `Op::Add` brush (WT AABB `[x,y,z,w,h,d]`) into a region; returns its id.
    ///
    /// `look` is the `(scheme, floor_y)` of the room brush the placement was resolved
    /// against, and the new brush wears both — as the JS `confirmPillarPlacement` /
    /// `confirmBracePlacement` did. Left at `Brush::new`'s defaults a pillar dropped
    /// into a themed room came out in the default theme, and with its wall UVs
    /// anchored to its own base rather than the room's floor, so its texture split sat
    /// at a different height to the walls around it.
    pub(crate) fn push_add_brush(
        &mut self,
        region_id: u32,
        look: (usize, f32),
        b: [f32; 6],
    ) -> Option<u32> {
        let id = self.next_brush_id;
        let mut brush = Brush::new(id, Op::Add, b[0], b[1], b[2], b[3], b[4], b[5]);
        (brush.scheme, brush.floor_y) = look;
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        region.brushes.push(brush);
        self.next_brush_id += 1;
        Some(id)
    }

    /// Resolve the pillar box (WT `[x,y,z,w,h,d]`) under the crosshair, or `None`
    /// if not aimed at a floor (JS `computePillarPreview`: axis Y, side Min).
    pub(crate) fn resolve_pillar(&mut self) -> Option<[f32; 6]> {
        self.resolve_pillar_placed().map(|(_, _, b)| b)
    }

    /// Like [`resolve_pillar`](Self::resolve_pillar) but also returns the region id and
    /// the room brush's `(scheme, floor_y)` for the placed brush to inherit.
    pub(crate) fn resolve_pillar_placed(&mut self) -> Option<(u32, (usize, f32), [f32; 6])> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if sel.axis != Axis::Y || sel.side != Side::Min {
            return None; // pillars stand on floors only
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        if brush.op != Op::Subtract {
            return None;
        }
        let ps = self.pillar_size;
        let e = BURY_EPS;
        let (min_x, max_x) = (brush.x, brush.x + brush.w);
        let (min_y, max_y) = (brush.y, brush.y + brush.h);
        let (min_z, max_z) = (brush.z, brush.z + brush.d);
        // **The footprint has to fit the brush it stands on.** Otherwise the clamps below
        // get `min > max` and `f32::clamp` *panics*, taking the editor with it. Not
        // hypothetical: a doorframe / protoroom carve is only `WALL_THICKNESS` (1 WT)
        // deep, so aiming at one with the default 2 WT pillar crashed the editor.
        // `resolve_opening_placement` already guards its own clamps the same way.
        if ps > max_x - min_x || ps > max_z - min_z {
            return None;
        }
        // Snap the cursor to WT and center the (integer) footprint on it.
        let x0 = (hit_wt.x.round() - (ps / 2.0).floor()).clamp(min_x, max_x - ps);
        let z0 = (hit_wt.z.round() - (ps / 2.0).floor()).clamp(min_z, max_z - ps);
        Some((
            sel.region_id,
            (brush.scheme, brush.floor_y),
            [x0, min_y - e, z0, ps, (max_y - min_y) + 2.0 * e, ps],
        ))
    }

    /// Resolve the three brace boxes under the crosshair, or `None` if not aimed
    /// at a wall (JS `computeBracePreview`: axis X or Z, on a subtract brush).
    pub(crate) fn resolve_brace(&mut self) -> Option<[[f32; 6]; 3]> {
        self.resolve_brace_placed().map(|(_, _, boxes)| boxes)
    }

    /// Like [`resolve_brace`](Self::resolve_brace) but also returns the region id and
    /// the room brush's `(scheme, floor_y)` for the placed brushes to inherit.
    pub(crate) fn resolve_brace_placed(&mut self) -> Option<(u32, (usize, f32), [[f32; 6]; 3])> {
        if self.mode != Mode::Build {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if sel.axis == Axis::Y {
            return None; // braces are wall→ceiling→wall arches
        }
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        if brush.op != Op::Subtract {
            return None;
        }
        let (bw, bd, e) = (self.brace_width, self.brace_depth, BURY_EPS);
        let (ix0, ix1) = (brush.x, brush.x + brush.w);
        let (iy0, iy1) = (brush.y, brush.y + brush.h);
        let (iz0, iz1) = (brush.z, brush.z + brush.d);
        let ih = iy1 - iy0;
        // **The arch has to fit the brush it spans.** The width clamp below panics on
        // `min > max` otherwise — `f32::clamp` does, not saturate — and takes the editor
        // down. A doorframe / protoroom carve is only `WALL_THICKNESS` (1 WT) deep, so its
        // *side* face gives a 1 WT span along the arch's width axis, and the default 2 WT
        // brace overruns it: aiming at one crashed with `min = 48, max = 47`.
        //
        // The height check is a lesser cousin — with `bd >= ih` the ceiling strip would sit
        // at or below the floor, which is broken output rather than a crash, but "the tool
        // doesn't fit" is the same answer.
        let span_w = if sel.axis == Axis::X { iz1 - iz0 } else { ix1 - ix0 };
        if bw > span_w || bd >= ih {
            return None;
        }

        let boxes = if sel.axis == Axis::X {
            // Arch spans across X; U runs along Z (position from the cursor Z).
            let z0 = (hit_wt.z.round() - (bw / 2.0).floor()).clamp(iz0, iz1 - bw);
            [
                [ix0 - e, iy0 - e, z0, bd + e, ih + 2.0 * e, bw], // wall on min-X
                [ix0 - e, iy1 - bd, z0, (ix1 - ix0) + 2.0 * e, bd + e, bw], // ceiling strip
                [ix1 - bd, iy0 - e, z0, bd + e, ih + 2.0 * e, bw], // wall on max-X
            ]
        } else {
            // Arch spans across Z; U runs along X.
            let x0 = (hit_wt.x.round() - (bw / 2.0).floor()).clamp(ix0, ix1 - bw);
            [
                [x0, iy0 - e, iz0 - e, bw, ih + 2.0 * e, bd + e], // wall on min-Z
                [x0, iy1 - bd, iz0 - e, bw, bd + e, (iz1 - iz0) + 2.0 * e], // ceiling strip
                [x0, iy0 - e, iz1 - bd, bw, ih + 2.0 * e, bd + e], // wall on max-Z
            ]
        };
        Some((sel.region_id, (brush.scheme, brush.floor_y), boxes))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::*;

    /// A world holding one **1 WT thin** subtract brush at x = 48 — the shape of a
    /// doorframe / protoroom carve (`WALL_THICKNESS` deep), which is what a real level is
    /// full of. The camera sits inside it looking at a face whose in-plane span is that
    /// single tile.
    ///
    /// These coordinates reproduce the reported panic exactly: `clamp(48, 48 + 1 - 2)` →
    /// `min = 48.0, max = 47.0`.
    fn world_with_a_thin_brush() -> World {
        let mut world = World::new();
        let id = world.next_brush_id;
        world.next_brush_id += 1;
        world.regions[0]
            .brushes
            .push(Brush::new(id, Op::Subtract, 48.0, 0.0, 48.0, 1.0, 16.0, 24.0));
        world.recluster_all();
        world.initial_meshes();
        world
    }

    /// Aiming the brace at a brush too narrow for it refuses instead of panicking.
    ///
    /// `f32::clamp` **panics** on `min > max` rather than saturating, so an ill-fitting
    /// placement was not a cosmetic glitch — it killed the editor mid-session. Pressing `R`
    /// while looking at a doorframe was enough.
    #[test]
    fn a_brace_aimed_at_a_brush_narrower_than_itself_refuses_instead_of_panicking() {
        let mut world = world_with_a_thin_brush();
        // Inside the thin brush, looking down −Z at its Z-Min face. That face's width axis
        // is X, where the brush spans a single tile.
        world.camera.pos = Vec3::new(48.5, 8.0, 60.0) * WORLD_SCALE;
        world.camera.yaw = 0.0;
        world.camera.pitch = 0.0;
        world.brace_tool_key();
        assert!(world.is_placing(), "brace armed");

        let (sel, _) = world.pick_face_hit().expect("the thin brush's face");
        assert_eq!(sel.axis, Axis::Z, "aimed at a Z face, so width runs along X");
        assert!(
            world.resolve_brace().is_none(),
            "a 2 WT brace does not fit a 1 WT span — refuse, don't panic"
        );
        // The ghost and the placement both go through the same resolver.
        assert!(world.update_place_preview().is_none(), "and no ghost is drawn");
        assert!(world.confirm_place().is_none(), "and nothing is placed");
    }

    /// Same guard for the pillar, whose footprint has to fit the floor it stands on in
    /// *both* horizontal axes.
    #[test]
    fn a_pillar_aimed_at_a_floor_smaller_than_itself_refuses_instead_of_panicking() {
        let mut world = world_with_a_thin_brush();
        // Inside the thin brush, looking down at its floor: 1 WT across in X.
        world.camera.pos = Vec3::new(48.5, 8.0, 60.0) * WORLD_SCALE;
        world.camera.yaw = 0.0;
        world.camera.pitch = -1.4;
        world.pillar_tool_key();
        assert!(world.is_placing(), "pillar armed");

        let (sel, _) = world.pick_face_hit().expect("the thin brush's floor");
        assert_eq!((sel.axis, sel.side), (Axis::Y, Side::Min), "aimed at a floor");
        assert!(
            world.resolve_pillar().is_none(),
            "a 2 WT pillar does not fit a 1 WT wide floor — refuse, don't panic"
        );
        assert!(world.update_place_preview().is_none());
        assert!(world.confirm_place().is_none());
    }

    /// The guards must not have made the tools useless: both still place on a normal room
    /// wall / floor, which is the case they exist for.
    #[test]
    fn both_tools_still_place_on_an_ordinary_room_surface() {
        let mut world = World::new();
        world.initial_meshes();

        world.camera.pitch = -1.4; // the default room's floor
        world.pillar_tool_key();
        assert!(world.resolve_pillar().is_some(), "pillar fits a 24×24 floor");
        assert!(world.confirm_place().is_some(), "and places");

        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = 0.0; // the −Z wall
        world.brace_tool_key();
        assert!(world.resolve_brace().is_some(), "brace fits a 24-wide wall");
        assert!(world.confirm_place().is_some(), "and places");
    }
}
