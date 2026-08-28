//! Vent tool (`U`, "dUct"): carve a crawlspace duct network the player enters crouched.
//!
//! # Why this is its own tool rather than a hole preset
//!
//! The hole tool cuts a rectangle *through a wall*, one WT deep, and opens it into a
//! 1 WT protoroom so it doesn't dead-end in solid. A duct is the opposite shape: a fixed,
//! small cross-section dragged a long way through solid, turning corners. Sizing is not
//! the author's business here — it is a *constraint* (see [`VENT_BORE`]) — and length is.
//!
//! # The segment model
//!
//! The first click picks a face with the crosshair, exactly like an opening, and drives
//! the duct **into** that face. Every click after that continues the network from where
//! the last segment ended, in whatever axis direction the author is **looking** — snapped
//! to the nearest of ±X/±Y/±Z. So a duct is driven rather than assembled: look along the
//! wall and click to run, look down and click to drop a shaft.
//!
//! Reversing straight back down the duct you just cut is refused, because it carves
//! nothing and would silently walk the cursor backwards through the network.
//!
//! Each segment is **one `Op::Subtract` brush** wearing the vent theme. That is all the
//! texturing there is to it: `uv_zones::face_owner` hands each triangle to the smallest
//! brush whose face plane it lies on, and inside a duct that is the duct — so its
//! surfaces take the vent scheme without a zone or classifier change. (The design doc
//! originally routed this through the unused "tunnel" zone 4; owning the scheme is
//! cheaper, leaves zone 4 free, and lets a duct floor differ from its walls.)

use super::super::*;

/// Duct cross-section, WT. **1.0 m, and the ceiling on it is the whole safety story.**
///
/// `nav::AGENT_HEIGHT_CELLS` is 6, so a cell needs 1.5 m of headroom to be *standable*.
/// A bore under that contains no standable cell, is in no walkable component, and A\*
/// therefore cannot route a hunter through it — the duct is hunter-proof by construction
/// rather than by a rule someone has to remember to apply.
///
/// At 6 WT that inverts silently: the "vent" becomes a low corridor hunters walk down.
/// [`VENT_BORE_MAX`] is the guard, and there is a test on it.
pub(crate) const VENT_BORE: f32 = 4.0;

/// The largest bore that still keeps hunters out (5 WT = 1.25 m, one cell clear of
/// standable). Nothing may raise [`VENT_BORE`] past this.
pub(crate) const VENT_BORE_MAX: f32 = 5.0;

/// Rule 1, enforced by the compiler rather than by a test that has to be run.
///
/// A bore that reaches `nav::AGENT_HEIGHT_CELLS` stops being a vent — hunters can stand
/// in it, so A\* routes them down it — and nothing else in the codebase would notice.
/// This is the cheapest possible place to catch that: raising [`VENT_BORE`] past the
/// ceiling fails the build.
const _: () = assert!(VENT_BORE <= VENT_BORE_MAX);
const _: () = assert!(VENT_BORE_MAX < engine::sim::nav::AGENT_HEIGHT_CELLS as f32);

/// Segment length limits, WT.
///
/// The minimum is **one bore**, which is also the default: a push adds a cube, so the
/// duct grows by exactly the section you are looking at rather than by an arbitrary run.
/// That makes the tool predictable — one click is one unit of duct — and it is the right
/// granularity for turning a corner, since a segment shorter than the bore cannot clear
/// the corner it starts from anyway.
///
/// The maximum is a runaway-scroll guard, not a design limit: scroll up for a long
/// straight run when you want one.
const VENT_LEN_MIN: f32 = VENT_BORE;
const VENT_LEN_MAX: f32 = 60.0;
/// Length a fresh segment starts at — one bore, i.e. a cube.
pub(crate) const VENT_LEN_DEFAULT: f32 = VENT_BORE;

/// The duct network being carved: where the open end is, and which way it was heading.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VentRun {
    /// Centre of the duct's open end, WT — where the next segment starts.
    pub cursor: Vec3,
    /// Unit axis direction the last segment travelled, WT.
    pub dir: Vec3,
    pub region_id: u32,
    pub scheme: usize,
    /// Shared `group` id for every brush in this network, so the whole duct is
    /// recognisable as one authored thing (same convention as the draw tool).
    pub group: u32,
    pub segments: u32,
}

/// A previewed segment: the WT box it would carve, plus where it would leave the cursor.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VentSeg {
    /// WT AABB `[x, y, z, w, h, d]`.
    pub aabb: [f32; 6],
    pub region_id: u32,
    pub scheme: usize,
    pub end: Vec3,
    pub dir: Vec3,
}

/// Snap a look direction to the nearest axis unit vector.
fn snap_axis(v: Vec3) -> Vec3 {
    let (ax, ay, az) = (v.x.abs(), v.y.abs(), v.z.abs());
    if ax >= ay && ax >= az {
        Vec3::new(v.x.signum(), 0.0, 0.0)
    } else if ay >= az {
        Vec3::new(0.0, v.y.signum(), 0.0)
    } else {
        Vec3::new(0.0, 0.0, v.z.signum())
    }
}

/// The WT box swept by a bore of `VENT_BORE` running `len` from `start` along `dir`.
fn segment_aabb(start: Vec3, dir: Vec3, len: f32) -> [f32; 6] {
    let half = VENT_BORE / 2.0;
    // Extent is the bore on the two axes across the run, and `len` along it.
    let ext = Vec3::new(
        if dir.x != 0.0 { len } else { VENT_BORE },
        if dir.y != 0.0 { len } else { VENT_BORE },
        if dir.z != 0.0 { len } else { VENT_BORE },
    );
    // Min corner: centred across the run, and starting at `start` along it (running
    // back from it when the direction is negative).
    let min = Vec3::new(
        if dir.x != 0.0 {
            if dir.x > 0.0 { start.x } else { start.x - len }
        } else {
            start.x - half
        },
        if dir.y != 0.0 {
            if dir.y > 0.0 { start.y } else { start.y - len }
        } else {
            start.y - half
        },
        if dir.z != 0.0 {
            if dir.z > 0.0 { start.z } else { start.z - len }
        } else {
            start.z - half
        },
    );
    [min.x, min.y, min.z, ext.x, ext.y, ext.z]
}

impl World {
    /// The theme a duct interior wears. Resolved through the texture registry rather
    /// than stored, so repointing ducts at the real GoldenEye vent texture is a
    /// `themes.json` edit with no code change.
    fn vent_scheme(&self) -> usize {
        engine::render::textures::vent_scheme()
    }

    /// Whether the vent tool is armed (the app draws the ghost and routes clicks/scroll).
    pub fn is_vent_tool(&self) -> bool {
        self.vent_tool
    }

    /// Whether a duct network is part-carved, so the next click continues it rather than
    /// picking a fresh face. Also what makes Esc mean "finish this duct" rather than
    /// "disarm".
    pub fn is_vent_running(&self) -> bool {
        self.vent_run.is_some()
    }

    /// Vent tool key (`U`): arm/toggle. Pressing it again finishes any run in progress
    /// and disarms.
    pub fn vent_tool_key(&mut self) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        if self.vent_tool {
            self.cancel_vent();
        } else {
            self.place_tool = None;
            self.clear_platform_state();
            self.clear_draw_state();
            self.cancel_opening();
            self.vent_tool = true;
            self.vent_len = VENT_LEN_DEFAULT;
            self.vent_run = None;
            self.selected = None;
            log::info!(
                "vent: armed — click a wall/floor/ceiling to start a duct, then look and \
                 click to run it; scroll sets segment length, U or Esc finishes"
            );
        }
        None
    }

    /// Finish the duct network and disarm. Reports a duct with no second mouth, which is
    /// a pocket the player can crawl into and not get out of.
    pub fn cancel_vent(&mut self) {
        if let Some(run) = self.vent_run.take() {
            let open = self.vent_end_is_open(&run);
            if !open {
                log::warn!(
                    "vent: this duct ({} segment(s)) dead-ends in solid — it has one mouth, \
                     so the player can crawl in and not out. Run a segment out through a wall.",
                    run.segments,
                );
            } else {
                log::info!("vent: duct finished ({} segments, both ends open)", run.segments);
            }
        }
        self.vent_tool = false;
        self.vent_preview = None;
    }

    /// Whether the duct's open end came out somewhere the player can actually get to —
    /// the "does this duct have a second mouth" test.
    ///
    /// **Not solidity.** The obvious check, "is the end cell air", is wrong in a way that
    /// only shows up on a short stub: the walls are 1 WT thick, so a stub barely longer
    /// than that punches clean through into the *void outside the level*, which is not
    /// solid and would report a mouth. A duct venting into nothing is not a second exit.
    ///
    /// So the end has to be **inside some region's shell and not solid there**: inside a
    /// wall reads shut, inside a room reads open, out the back of the level reads shut.
    ///
    /// Sampled just past the cursor along the run, because the cursor sits exactly on the
    /// last carve's end plane, which is air by construction.
    fn vent_end_is_open(&self, run: &VentRun) -> bool {
        let p = run.cursor + run.dir * 0.5;
        self.regions.iter().any(|r| {
            let s = r.shell();
            let inside = p.x >= s.x
                && p.x <= s.x + s.w
                && p.y >= s.y
                && p.y <= s.y + s.h
                && p.z >= s.z
                && p.z <= s.z + s.d;
            inside && !r.solid_at(p.x, p.y, p.z)
        })
    }

    /// Scroll the next segment's length, in WT.
    pub fn adjust_vent_len(&mut self, step: f32) {
        if !self.vent_tool {
            return;
        }
        self.vent_len = (self.vent_len + step).clamp(VENT_LEN_MIN, VENT_LEN_MAX);
    }

    /// Current segment length (WT), for the HUD/panel readout.
    pub fn vent_len(&self) -> f32 {
        self.vent_len
    }

    /// Recompute the ghost for the next segment and return it as a preview mesh.
    pub fn update_vent_preview(&mut self) -> Option<CpuMesh> {
        if !self.vent_tool {
            return None;
        }
        self.vent_preview = self.resolve_vent_segment();
        self.vent_preview.map(|s| crate::world::geom::boxes_mesh(&[s.aabb]))
    }

    /// Where the next segment would go: off the crosshair face for the first one, off
    /// the run's open end (in the snapped look direction) for every one after.
    pub(crate) fn resolve_vent_segment(&mut self) -> Option<VentSeg> {
        if self.mode != Mode::Build {
            return None;
        }
        match self.vent_run {
            None => {
                let (sel, hit_wt) = self.pick_face_hit()?;
                let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
                let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
                let position = brush.face_pos(sel.axis, sel.side);
                // Into the face, i.e. deeper into the solid the crosshair struck — the
                // same sign convention `cut_opening` uses to place a frame carve, where
                // Side::Max extends along +axis from the face plane and Side::Min
                // extends back along -axis. Getting this backwards drives the duct into
                // the room instead of into the wall, which still carves (the room is
                // already air) and so fails silently.
                let mut dir = Vec3::ZERO;
                let n = if sel.side == Side::Max { 1.0 } else { -1.0 };
                match sel.axis {
                    Axis::X => dir.x = n,
                    Axis::Y => dir.y = n,
                    Axis::Z => dir.z = n,
                }
                // Centre the bore on the hit point in the two in-plane axes, rounded to
                // the grid so ducts line up with everything else the editor makes.
                let (u_axis, v_axis) = sel.axis.orthogonals();
                let mut start = Vec3::ZERO;
                let set = |p: &mut Vec3, a: Axis, val: f32| match a {
                    Axis::X => p.x = val,
                    Axis::Y => p.y = val,
                    Axis::Z => p.z = val,
                };
                set(&mut start, sel.axis, position);
                set(&mut start, u_axis, u_axis.component(hit_wt).round());
                set(&mut start, v_axis, v_axis.component(hit_wt).round());
                let len = self.vent_len;
                Some(VentSeg {
                    aabb: segment_aabb(start, dir, len),
                    region_id: sel.region_id,
                    scheme: brush.scheme,
                    end: start + dir * len,
                    dir,
                })
            }
            Some(run) => {
                let dir = snap_axis(self.camera.forward());
                // Straight back up the duct carves nothing and would rewind the cursor
                // through geometry that is already air — hold the last heading instead.
                let dir = if dir.dot(run.dir) < -0.5 { run.dir } else { dir };
                let len = self.vent_len;
                Some(VentSeg {
                    aabb: segment_aabb(run.cursor, dir, len),
                    region_id: run.region_id,
                    scheme: run.scheme,
                    end: run.cursor + dir * len,
                    dir,
                })
            }
        }
    }

    /// Break the duct out into a **protoroom** at its open end, and finish the run.
    ///
    /// This is the vent's answer to "how do I get out of here into a new room", and it is
    /// deliberately the same move the door tool already makes: `cut_opening` carves a
    /// 1 WT protoroom just beyond every doorway so the opening leads into navigable space
    /// rather than dead-ending in solid, and the author then pushes that face out to grow
    /// the room. A duct wants exactly that, minus the frame — a duct has no doorframe.
    ///
    /// Two things it does NOT inherit from the duct: the `vent` flag, because the room
    /// beyond is a room and hunters must be able to walk in it; and the vent theme, so it
    /// does not read as more ducting. It takes **the level's first theme** (`scheme_for_key('1')`,
    /// which prefers this level's own binding and falls back to the manifest's), which is
    /// the same theme a fresh room here would get.
    pub fn vent_exit_room(&mut self) -> Option<RegionMesh> {
        let run = self.vent_run?;
        // A bore-sized box one WT deep, immediately past the duct's open end.
        let a = segment_aabb(run.cursor, run.dir, WALL_THICKNESS);
        let id = self.next_brush_id;
        self.next_brush_id += 1;
        let mut brush = Brush::new(id, Op::Subtract, a[0], a[1], a[2], a[3], a[4], a[5]);
        brush.scheme = self.scheme_for_key('1').unwrap_or_else(default_scheme);
        brush.floor_y = a[1];
        let region_id = run.region_id;
        let region = self.regions.iter_mut().find(|r| r.id == region_id)?;
        region.brushes.push(brush);
        log::info!(
            "vent: opened an exit protoroom at the duct end in region {region_id} - \
             select its far face and push to grow the room"
        );
        // The duct now ends in open space, so finishing it reports two mouths.
        self.vent_run = Some(VentRun { cursor: run.cursor + run.dir * WALL_THICKNESS, ..run });
        self.cancel_vent();
        self.rebuild_affected_regions(&[id]).into_iter().next()
    }

    /// Commit the previewed segment (left-click): carve it and advance the open end.
    pub(crate) fn vent_click(&mut self) -> Option<RegionMesh> {
        if !self.vent_tool {
            return None;
        }
        let seg = self.vent_preview.take().or_else(|| self.resolve_vent_segment())?;
        if !self.regions.iter().any(|r| r.id == seg.region_id) {
            return None;
        }
        let id = self.next_brush_id;
        self.next_brush_id += 1;
        // A network's group is its first brush's id — brush ids are unique and monotonic,
        // so it needs no second allocator (the draw tool's convention).
        let group = match self.vent_run {
            Some(run) => run.group,
            None => id,
        };
        let a = seg.aabb;
        let mut brush = Brush::new(id, Op::Subtract, a[0], a[1], a[2], a[3], a[4], a[5]);
        brush.vent = true;
        brush.group = group;
        brush.scheme = self.vent_scheme();
        brush.floor_y = a[1];

        let region = self.regions.iter_mut().find(|r| r.id == seg.region_id)?;
        region.brushes.push(brush);

        self.vent_run = Some(VentRun {
            cursor: seg.end,
            dir: seg.dir,
            region_id: seg.region_id,
            scheme: seg.scheme,
            group,
            segments: self.vent_run.map(|r| r.segments + 1).unwrap_or(1),
        });
        log::info!(
            "vent: segment {} carved in region {} ({:.0} WT along {:?})",
            self.vent_run.map(|r| r.segments).unwrap_or(1),
            seg.region_id,
            self.vent_len,
            seg.dir,
        );
        self.rebuild_affected_regions(&[id]).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Rule 1.** The bore must stay under `nav::AGENT_HEIGHT_CELLS`, or a duct becomes
    /// a corridor hunters walk down and the whole feature inverts. This is the guard on
    /// the one constant that decides it.
    #[test]
    fn the_bore_is_too_short_for_a_hunter_to_stand_in() {
        assert!(
            VENT_BORE <= VENT_BORE_MAX,
            "bore {VENT_BORE} exceeds the {VENT_BORE_MAX} WT ceiling"
        );
        assert!(
            VENT_BORE_MAX < engine::sim::nav::AGENT_HEIGHT_CELLS as f32,
            "even the maximum bore ({VENT_BORE_MAX}) must leave a cell short of the \
             {} needed to stand — otherwise hunters path straight through ducts",
            engine::sim::nav::AGENT_HEIGHT_CELLS,
        );
    }

    /// **One whole texture per duct face**, which is a relationship between two numbers
    /// that live in different files.
    ///
    /// UVs reach the shader in WT (`uv_zones::vertex_uv` divides by `WORLD_SCALE`), so a
    /// theme's `repeat` counts tiles per WT — not per metre. The bore is [`VENT_BORE`]
    /// WT across, so the repeat that stretches exactly one texture over a face is
    /// `1 / VENT_BORE`. A repeat of 1.0, which is the intuitive value, tiles it four
    /// times.
    ///
    /// Pinned here because the two halves are a `themes.json` value and a Rust constant:
    /// widening the bore without touching the theme would silently re-tile every duct in
    /// every saved level, and nothing else would notice.
    #[test]
    fn vent_repeat_fits_the_bore() {
        let scheme = &engine::render::textures::schemes()[engine::render::textures::vent_scheme()];
        for zone in [0usize, 1, 2, 3] {
            let z = scheme.zones[zone]
                .unwrap_or_else(|| panic!("the vent theme defines zone {zone}"));
            assert!(
                (z.repeat * VENT_BORE - 1.0).abs() < 1e-4,
                "zone {zone}: repeat {} × bore {VENT_BORE} = {} tiles per face; want exactly 1",
                z.repeat,
                z.repeat * VENT_BORE,
            );
        }
    }

    /// **A duct's panels line up wherever it is cut**, on the floor and ceiling as much
    /// as on the walls.
    ///
    /// The bug this pins: floor/ceiling UVs were raw world `[wx, wz]` with no anchor at
    /// all (only the *vertical* axis had one, `floor_y`). So the panel grid was pinned to
    /// world zero and a duct lined up only when it happened to land on a multiple of the
    /// panel size — every other placement put the panel border across the middle of the
    /// duct floor. Invisible on a room floor, glaring in a 1 m duct whose texture is a
    /// single bordered panel.
    ///
    /// Asserted by carving the *same* duct at several deliberately un-aligned offsets and
    /// requiring every face's UVs to span exactly one whole texture, which is what
    /// "aligned" means for this art.
    #[test]
    fn duct_panels_align_wherever_the_duct_is_cut() {
        use engine::render::textures::{schemes, vent_scheme};
        let repeat = schemes()[vent_scheme()].zones[0].expect("vent zone 0").repeat;

        // Offsets chosen to be non-multiples of the bore, which is exactly the case that
        // used to drift.
        for off in [0.0f32, 1.0, 2.0, 3.0, 5.0, 7.0] {
            // A solid block with the duct bored **through** it, so all six duct faces
            // are real surfaces. (Subtracting a duct from a room's air produces no
            // geometry at all — the first version of this test did that and measured
            // nothing.)
            let mut region = Region::new(0);
            region
                .brushes
                .push(Brush::new(1, Op::Add, 0.0, 0.0, 0.0, 40.0, 24.0, 40.0));
            let mut duct = Brush::new(2, Op::Subtract, 8.0 + off, 4.0 + off, 8.0 + off,
                                      VENT_BORE, VENT_BORE, VENT_BORE);
            duct.vent = true;
            duct.floor_y = 4.0 + off;
            duct.scheme = vent_scheme();
            region.brushes.push(duct);

            let tex = region.evaluate_textured();
            // Every triangle the duct owns must have UVs that, once scaled by `repeat`,
            // land inside a single 0..1 tile — i.e. one whole panel per face.
            let mut checked = 0;
            for g in &tex.groups {
                if g.scheme as usize != vent_scheme() {
                    continue;
                }
                for i in g.start..g.start + g.count {
                    let uv = tex.vertices[i as usize].uv;
                    for c in 0..2 {
                        let t = uv[c] * repeat;
                        assert!(
                            t >= -1e-3 && t <= 1.0 + 1e-3,
                            "offset {off}: a duct vertex sits at {t:.3} tiles on axis {c}                              — the panel border is running across the face"
                        );
                    }
                    checked += 1;
                }
            }
            assert!(checked > 0, "offset {off}: the duct produced no textured geometry");
        }
    }

    /// **Enter breaks a duct out into a room you can then push.**
    ///
    /// The protoroom is a room, not more duct, and the test asserts the three ways that
    /// has to be true: it is not flagged `vent` (so hunters can walk in what grows from
    /// it), it does not wear the vent theme, and it takes the level's first theme -
    /// which is what a fresh room here would get.
    #[test]
    fn a_duct_can_open_into_a_protoroom() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.vent_tool_key();
        world.update_vent_preview();
        world.vent_click().expect("duct carved");
        assert!(world.is_vent_running(), "a run is open");

        let before = world.regions.iter().flat_map(|r| r.brushes.iter()).count();
        world.vent_exit_room().expect("the protoroom rebuilds a region");
        let after: Vec<_> =
            world.regions.iter().flat_map(|r| r.brushes.iter()).copied().collect();
        assert_eq!(after.len(), before + 1, "exactly one protoroom brush was added");

        let proto = after.last().copied().expect("the new brush");
        assert!(!proto.vent, "the room beyond a duct is a ROOM - hunters must walk in it");
        assert_ne!(
            proto.scheme,
            engine::render::textures::vent_scheme(),
            "and it must not read as more ducting"
        );
        assert_eq!(
            Some(proto.scheme),
            world.scheme_for_key('1'),
            "it takes the level's first theme, like any fresh room here"
        );
        // Opening out finishes the duct.
        assert!(!world.is_vent_tool(), "the tool disarms once the duct is out");
        assert!(!world.is_vent_running(), "and the run is closed");
    }

    /// A look direction becomes the nearest axis, and ties do not produce a zero vector.
    #[test]
    fn look_direction_snaps_to_an_axis() {
        assert_eq!(snap_axis(Vec3::new(0.9, 0.1, 0.2)), Vec3::X);
        assert_eq!(snap_axis(Vec3::new(-0.9, 0.1, 0.2)), Vec3::NEG_X);
        assert_eq!(snap_axis(Vec3::new(0.1, -0.9, 0.2)), Vec3::NEG_Y);
        assert_eq!(snap_axis(Vec3::new(0.1, 0.2, 0.9)), Vec3::Z);
        assert_eq!(
            snap_axis(Vec3::new(0.0, 0.0, 0.0)).length(),
            1.0,
            "a degenerate look still yields a unit axis, never a zero-volume carve"
        );
    }

    /// **The acceptance test for the whole feature.**
    ///
    /// Carve a real duct into the starting room's wall, bake the nav grid the hunters
    /// actually use, and assert there is no standable cell anywhere inside the bore. If
    /// this ever fails, ducts have become corridors and hunters will path down them.
    ///
    /// Asserted against the baked grid rather than against `VENT_BORE < AGENT_HEIGHT`,
    /// because the arithmetic being right is not the same claim as the voxelizer
    /// agreeing with it.
    #[test]
    fn hunters_cannot_stand_anywhere_inside_a_carved_duct() {
        let mut world = World::new();
        world.initial_meshes();
        // Aim the fly camera at the -X wall from inside the starting room and drive one
        // duct straight into it, through the real tool entry points.
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.vent_tool_key();
        assert!(world.is_vent_tool(), "the tool armed");
        assert!(world.update_vent_preview().is_some(), "it previews on the -X wall");
        let seg = world.resolve_vent_segment().expect("a segment resolves");
        world.vent_click().expect("the carve rebuilds a region");
        assert!(world.is_vent_running(), "the run is now open at the far end");

        let carved: Vec<_> =
            world.regions.iter().flat_map(|r| r.brushes.iter()).filter(|b| b.vent).collect();
        assert_eq!(carved.len(), 1, "exactly one duct brush was carved");
        assert_eq!(
            carved[0].scheme,
            engine::render::textures::vent_scheme(),
            "the duct wears the vent theme, which is what makes it look like ducting"
        );

        // Bake the grid the hunters navigate on and probe the bore.
        let mut regions = std::mem::take(&mut world.regions);
        let nav = engine::sim::nav::bake(&mut regions, &[], &[])
            .expect("the level bakes a nav grid");
        world.regions = regions;
        let a = seg.aabb;
        let mut probes = 0;
        let mut standable = 0;
        // Sample the bore on a half-cell lattice, in metres.
        let step = 0.5;
        let mut x = a[0] + 0.5;
        while x < a[0] + a[3] {
            let mut y = a[1] + 0.5;
            while y < a[1] + a[4] {
                let mut z = a[2] + 0.5;
                while z < a[2] + a[5] {
                    let m = Vec3::new(x, y, z) * WORLD_SCALE;
                    probes += 1;
                    if nav.nearest_standable(m.x, m.y, m.z, 0).is_some() {
                        standable += 1;
                    }
                    z += step;
                }
                y += step;
            }
            x += step;
        }
        assert!(probes > 0, "the probe lattice actually sampled the bore");
        assert_eq!(
            standable, 0,
            "{standable} of {probes} cells inside the duct are standable — hunters can              path into this vent, which inverts the entire feature"
        );
    }

    /// A push adds a **cube**: the default segment is exactly the bore in every axis, so
    /// one click is one predictable unit of duct rather than an arbitrary run.
    #[test]
    fn one_push_adds_a_cube_of_bore() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.vent_tool_key();
        assert_eq!(world.vent_len(), VENT_BORE, "a fresh segment is one bore long");
        world.update_vent_preview();
        world.vent_click().expect("carved");
        let b = world
            .regions
            .iter()
            .flat_map(|r| r.brushes.iter())
            .find(|b| b.vent)
            .expect("the duct brush exists");
        assert_eq!(
            [b.w, b.h, b.d],
            [VENT_BORE, VENT_BORE, VENT_BORE],
            "one push is a cube of the bore"
        );
        // Scrolling below one bore is refused — a shorter segment could not clear the
        // corner it starts from.
        world.adjust_vent_len(-10.0);
        assert_eq!(world.vent_len(), VENT_BORE, "the bore is the floor on segment length");
    }

    /// A duct that dead-ends in solid is reported, because it is a pocket the player can
    /// crawl into and not out of. The one carved above stops inside the wall.
    #[test]
    fn a_duct_that_stops_in_solid_has_only_one_mouth() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.vent_tool_key();
        // A short run stops inside the wall/void rather than breaking out.
        world.adjust_vent_len(-100.0); // clamps to VENT_LEN_MIN
        world.update_vent_preview();
        world.vent_click().expect("carved");
        let run = world.vent_run.expect("a run is open");
        assert_eq!(run.segments, 1);
        // The open-end test is what `cancel_vent` reports on; assert it directly so the
        // check is pinned independently of the log line.
        let open = world.vent_end_is_open(&run);
        assert!(
            !open,
            "a 2 WT stub that only reaches the void outside the level is not a mouth"
        );
    }

    /// A segment is bore-sized across the run and `len` along it, whichever way it goes.
    #[test]
    fn a_segment_is_bore_sized_across_and_len_along() {
        let start = Vec3::new(10.0, 4.0, 10.0);
        let a = segment_aabb(start, Vec3::X, 8.0);
        assert_eq!([a[3], a[4], a[5]], [8.0, VENT_BORE, VENT_BORE]);
        assert_eq!(a[0], 10.0, "a +X run starts at the cursor");

        // A negative run puts the box behind the cursor, not in front of it.
        let b = segment_aabb(start, Vec3::NEG_X, 8.0);
        assert_eq!([b[3], b[4], b[5]], [8.0, VENT_BORE, VENT_BORE]);
        assert_eq!(b[0], 2.0, "a -X run ends at the cursor");

        // A vertical shaft is bore-sized in X and Z.
        let c = segment_aabb(start, Vec3::NEG_Y, 6.0);
        assert_eq!([c[3], c[4], c[5]], [VENT_BORE, 6.0, VENT_BORE]);
        assert_eq!(c[1], -2.0, "a downward shaft hangs below the cursor");
    }
}
