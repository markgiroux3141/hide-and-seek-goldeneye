//! Face picking + selection on `World`: crosshair raycast → dominant-axis
//! face resolve, the selected-face UV info, and full-face detection.

use super::*;

/// The two fields of a raycast hit that face resolution actually reads. Lets
/// [`World::face_at_hit`] take a hit without depending on the physics crate's own
/// hit type, and keeps the resolve body reading exactly as it did.
struct RayHitParts {
    point: Vec3,
    normal: Vec3,
}

impl World {
    /// Whether anything is picked right now (a face or a structure). Cheaper than
    /// asking for `selection_face_mesh`, which builds a quad to answer.
    pub fn has_selection(&self) -> bool {
        self.selected.is_some()
    }

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
            scheme: brush.scheme,
        })
    }

    /// Whether the current selection covers the whole face (JS `isFullFace`) —
    /// i.e. no sub-rect has been scrolled in. Push/pull then resize the brush
    /// directly instead of spawning a sub-face brush.
    ///
    /// With patch scope on this asks about the whole **patch**, whose extent is the
    /// bounding rect of every coplanar member: unscrolled means "move all of them",
    /// and any scrolled-in rect means "carve one box over that rect" exactly as it
    /// always did.
    pub(crate) fn is_full_face(&self) -> bool {
        match self.selected_patch_info() {
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
        self.pick_face_hit_from(origin, dir)
    }

    /// Like [`pick_face_hit`](Self::pick_face_hit) but casts an explicit ray instead
    /// of the crosshair — used by the prop tool, which picks the floor under the free
    /// mouse cursor (the panel frees the cursor, so there's no crosshair to aim).
    /// Cast for an authorable face, **looking through anything that isn't one**.
    ///
    /// The collider carries more than the level: every region is a solid shell with its
    /// rooms carved out, and that shell's outer skin is real collision geometry that no
    /// `Brush` face explains. A ray that stops on it resolves to nothing, and the tool
    /// that cast it shows no ghost, no highlight, no selection.
    ///
    /// That used to be survivable because the skin was *drawn* — aiming at it looked
    /// like aiming at a wall, because it was one. Now that it is stripped from the
    /// render mesh (`csg_runtime::strip_shell_skin`) it is invisible, so the author
    /// aims through it at a room they can plainly see and the ray dies on scaffolding
    /// in mid-air. Worst where two rooms sit a single wall apart, because that is where
    /// a neighbouring region's skin lies closest to the surface you meant to hit.
    ///
    /// So a hit that explains nothing is stepped over rather than accepted: advance
    /// just past it and cast again. Bounded, because a pathological ray down the length
    /// of a level could otherwise walk a lot of surfaces, and because giving up quietly
    /// is what the caller already handles.
    pub(crate) fn pick_face_hit_from(&mut self, origin: Vec3, dir: Vec3) -> Option<(Selection, Vec3)> {
        /// How far the ray reaches in total, in metres.
        const RANGE: f32 = 100.0;
        /// How far past an unauthorable hit to restart, in metres. Comfortably above
        /// the trimesh's own precision and far below the 1 WT (0.25 m) that separates
        /// a shell skin from the surface behind it.
        const SKIP: f32 = 0.005;
        /// How many non-face surfaces to look through before giving up. A ray meets at
        /// most a couple of shell skins in any real level; this is the runaway guard.
        const MAX_SKIPS: usize = 8;

        let mut from = origin;
        let mut range = RANGE;
        for _ in 0..MAX_SKIPS {
            let hit = self.physics.raycast(from, dir, range)?;
            if let Some(found) = self.face_at_hit(hit.point, hit.normal) {
                return Some(found);
            }
            // Nothing here explains the hit — it is shell skin or some other
            // non-authored surface. Step past it and keep going.
            let travelled = (hit.point - from).length() + SKIP;
            range -= travelled;
            if range <= 0.0 {
                return None;
            }
            from = hit.point + dir * SKIP;
        }
        None
    }

    /// Resolve one raycast hit to the brush face that explains it, or `None` when no
    /// brush face does (the shell's own skin being the common case).
    fn face_at_hit(&self, point: Vec3, normal: Vec3) -> Option<(Selection, Vec3)> {
        let hit = RayHitParts { point, normal };

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

        // Search every region's brushes for the face plane the hit lies on, and
        // keep the closest match (regions may be disjoint — the ray landed on one
        // of them, but we resolve by geometry, not by which collider was struck).
        let mut best: Option<(u32, u32, Side, f32)> = None; // (region_id, brush_id, side, dist)
        for region in &self.regions {
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
                    if best.map(|(_, _, _, bd)| d < bd).unwrap_or(true) {
                        best = Some((region.id, b.id, side, d));
                    }
                }
            }
        }

        best.map(|(region_id, brush_id, side, _)| {
            (
                Selection {
                    region_id,
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

/// Picking **through the shell's skin**.
///
/// Every region is a solid shell with its rooms carved out, and that shell's outer
/// skin is real collision geometry no `Brush` face explains. It used to be drawn, so
/// a ray stopping on it looked like aiming at a wall — because it was one. Stripping
/// it from the render mesh made it invisible, and a ray that dies on an invisible
/// surface leaves the author aiming at a room they can plainly see while every tool
/// reports nothing under the crosshair.
///
/// Two rooms a single wall apart is where it bites: that is where a neighbouring
/// region's skin lies closest to the surface you meant to hit.
#[cfg(test)]
mod scaffolding_tests {
    use super::*;

    /// The booted room (0..24 in x and z, 0..16 tall) plus a second, **disjoint** room
    /// one wall-thickness past its +Z wall — so the two are separate regions, each with
    /// its own shell, exactly as the room plan tool leaves them.
    fn two_rooms() -> World {
        let mut w = World::new();
        w.initial_meshes();
        let id = w.next_brush_id;
        w.next_brush_id += 1;
        let mut b = Brush::new(id, Op::Subtract, 4.0, 0.0, 25.0, 16.0, 16.0, 12.0);
        b.floor_y = 0.0;
        let rid = w.next_region_id;
        w.next_region_id += 1;
        let mut r = Region::new(rid);
        r.brushes.push(b);
        r.refresh_shell();
        w.regions.push(r);
        w.recluster_all();
        assert_eq!(w.regions.len(), 2, "a 1 WT gap keeps them separate regions");
        w
    }

    fn aim(w: &mut World, eye: Vec3, yaw: f32, pitch: f32) -> Option<Selection> {
        w.camera.pos = eye * WORLD_SCALE;
        w.camera.yaw = yaw;
        w.camera.pitch = pitch;
        w.pick_face_hit().map(|(s, _)| s)
    }

    /// From outside the level, looking at a room: the ray crosses two shell skins on
    /// the way in and must land on the room's far wall rather than dying on the first
    /// invisible surface.
    #[test]
    fn a_ray_from_outside_looks_through_the_skin_to_the_room_behind_it() {
        let mut w = two_rooms();
        let far_room = w.regions[1].brushes[0];
        let sel = aim(&mut w, Vec3::new(12.0, 8.0, 60.0), 0.0, 0.0)
            .expect("the room is right there, in plain sight");
        assert_eq!(sel.brush_id, far_room.id, "it picked the room, not the scaffolding");
        assert_eq!((sel.axis, sel.side), (Axis::Z, Side::Max), "its far wall");
    }

    /// Straight down from above the level — the shell's lid is the first thing the ray
    /// meets and explains nothing.
    #[test]
    fn a_ray_from_above_looks_through_the_lid_to_the_ceiling() {
        let mut w = two_rooms();
        let sel = aim(&mut w, Vec3::new(12.0, 60.0, 12.0), 0.0, -std::f32::consts::FRAC_PI_2)
            .expect("the booted room is directly below");
        assert_eq!(sel.brush_id, 1, "the booted room");
        assert_eq!((sel.axis, sel.side), (Axis::Y, Side::Max), "its ceiling");
    }

    /// **The guard that matters.** Looking through scaffolding must not become looking
    /// through *walls*: from inside a room, the wall you are facing still wins, even
    /// though another room sits one thickness behind it and the skin between them is
    /// coincident with that wall.
    #[test]
    fn looking_through_scaffolding_never_looks_through_a_real_wall() {
        let mut w = two_rooms();
        for off_deg in [-40.0f32, -20.0, 0.0, 20.0, 40.0] {
            let sel = aim(
                &mut w,
                Vec3::new(12.0, 8.0, 12.0),
                std::f32::consts::PI + off_deg.to_radians(),
                0.0,
            )
            .unwrap_or_else(|| panic!("a wall is in view at {off_deg} degrees"));
            assert_eq!(
                sel.brush_id, 1,
                "at {off_deg} degrees the pick stayed in the room the camera is in"
            );
        }
    }

    /// Aiming into empty space still resolves to nothing — the skip must not invent a
    /// face, and must terminate.
    #[test]
    fn a_ray_into_open_space_still_finds_nothing() {
        let mut w = two_rooms();
        // Outside the level, looking further out.
        assert!(
            aim(&mut w, Vec3::new(12.0, 8.0, -60.0), 0.0, 0.0).is_none(),
            "there is nothing that way"
        );
    }

    /// The door tool is the tool this was reported through: from outside, aimed at a
    /// wall, it must now offer a ghost instead of refusing.
    #[test]
    fn the_door_tool_places_on_a_wall_seen_through_the_skin() {
        let mut w = two_rooms();
        w.door_tool_key();
        w.camera.pos = Vec3::new(12.0, 8.0, 60.0) * WORLD_SCALE;
        w.camera.yaw = 0.0;
        w.camera.pitch = 0.0;
        assert!(w.update_door_preview().is_some(), "the ghost is on the wall we can see");
        assert!(
            w.build_status().unwrap().contains("click to cut"),
            "and the strip agrees: {:?}",
            w.build_status()
        );
    }

    /// When a refusal does survive, it says *where* the ray stopped rather than leaving
    /// the author to guess — the report that started this needed exactly that.
    #[test]
    fn a_surviving_refusal_names_the_surface_it_stopped_on() {
        let mut w = two_rooms();
        w.door_tool_key();
        // Into open space: nothing at all, and the strip says so in those terms.
        w.camera.pos = Vec3::new(12.0, 8.0, -60.0) * WORLD_SCALE;
        w.camera.yaw = 0.0;
        w.camera.pitch = 0.0;
        assert!(w.update_door_preview().is_none());
        assert!(
            w.build_status().unwrap().contains("pointing at nothing"),
            "an empty aim reads as empty: {:?}",
            w.build_status()
        );
    }
}
