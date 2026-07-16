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
            self.selected = None;
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
                let (region_id, b) = self.resolve_pillar_placed()?;
                self.place_tool = None;
                let brush = self.push_add_brush(region_id, b)?;
                log::info!("pillar placed in region {region_id} (brush {brush})");
                self.rebuild_region(region_id)
            }
            PlaceKind::Brace => {
                let (region_id, boxes) = self.resolve_brace_placed()?;
                self.place_tool = None;
                for b in boxes {
                    self.push_add_brush(region_id, b);
                }
                log::info!("brace placed in region {region_id}");
                self.rebuild_region(region_id)
            }
        }
    }

    /// Push an `Op::Add` brush (WT AABB `[x,y,z,w,h,d]`) into a region; returns its id.
    pub(crate) fn push_add_brush(&mut self, region_id: u32, b: [f32; 6]) -> Option<u32> {
        let id = self.next_brush_id;
        let brush = Brush::new(id, Op::Add, b[0], b[1], b[2], b[3], b[4], b[5]);
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        region.brushes.push(brush);
        self.next_brush_id += 1;
        Some(id)
    }

    /// Resolve the pillar box (WT `[x,y,z,w,h,d]`) under the crosshair, or `None`
    /// if not aimed at a floor (JS `computePillarPreview`: axis Y, side Min).
    pub(crate) fn resolve_pillar(&mut self) -> Option<[f32; 6]> {
        self.resolve_pillar_placed().map(|(_, b)| b)
    }

    /// Like [`resolve_pillar`](Self::resolve_pillar) but also returns the region id.
    pub(crate) fn resolve_pillar_placed(&mut self) -> Option<(u32, [f32; 6])> {
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
        // Snap the cursor to WT and center the (integer) footprint on it.
        let x0 = (hit_wt.x.round() - (ps / 2.0).floor()).clamp(min_x, max_x - ps);
        let z0 = (hit_wt.z.round() - (ps / 2.0).floor()).clamp(min_z, max_z - ps);
        Some((
            sel.region_id,
            [x0, min_y - e, z0, ps, (max_y - min_y) + 2.0 * e, ps],
        ))
    }

    /// Resolve the three brace boxes under the crosshair, or `None` if not aimed
    /// at a wall (JS `computeBracePreview`: axis X or Z, on a subtract brush).
    pub(crate) fn resolve_brace(&mut self) -> Option<[[f32; 6]; 3]> {
        self.resolve_brace_placed().map(|(_, boxes)| boxes)
    }

    /// Like [`resolve_brace`](Self::resolve_brace) but also returns the region id.
    pub(crate) fn resolve_brace_placed(&mut self) -> Option<(u32, [[f32; 6]; 3])> {
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
        Some((sel.region_id, boxes))
    }
}
