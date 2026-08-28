//! Ladder tool (`J`): place a climbable ladder on a wall face.
//!
//! A ladder is an ECS entity — [`Transform`] + [`Ladder`] — placed by crosshair face
//! pick, exactly as a spawn pad is placed on the floor. Its geometry is entirely derived
//! from the transform: the base sits at the transform position, it rises along +Y for
//! `height`, and its facing is the transform's yaw. That means the existing translate and
//! rotate gizmos author it for free, with no bespoke editing path.
//!
//! # Player-only, on purpose
//!
//! Hunters cannot use ladders (`DESIGN_VENTS_LADDERS.md` §4, Option A). The grid climbs
//! one cell between neighbours, and letting it climb a storey is precisely the invariant
//! the `nav::STAIR_STEP` comment is an essay about not relaxing casually. So a floor
//! reachable only by ladder is a **nav island** — accepted deliberately, and reported by
//! the NAV tab as intent rather than as a fault.
//!
//! That report is not decoration. Islands are the documented cause of the worst
//! performance bug this project has had, and an authored object stranded on one is
//! unreachable to everything. The tab already separates "nobody can get there" from "the
//! player can, hunters cannot"; ladders join the second list.

use super::super::*;
use crate::ecs::components::Ladder;
use crate::ecs::{ComponentData, EntityData, Transform};

/// How far out from the wall face the climb volume reaches, metres. Generous enough to
/// catch a player pressed near the ladder rather than exactly on it — a climb you can
/// miss by 10 cm is a climb you miss while being shot at.
const LADDER_DEPTH: f32 = 0.6;

/// How far the **climb** volume reaches above the ladder's drawn top, metres.
///
/// The case this exists for is the normal one: a ladder whose top is level with the floor
/// it serves. Without an overshoot the player tops out with their feet *at* that floor's
/// height, which is not clear of it — so they detach, immediately fall the fraction back
/// into the volume, re-attach, climb, detach, and bob at the lip forever.
///
/// Half a metre of extra climb puts the feet above the ledge before letting go, so the
/// step off the wall lands on it. The rails are drawn to the authored height, not to
/// this — you climb a little past the top of a ladder in life too.
const LADDER_OVERSHOOT: f32 = 0.5;

impl World {
    /// Whether the ladder tool is armed.
    pub fn is_ladder_tool(&self) -> bool {
        self.ladder_tool
    }

    /// Ladder tool key (`J`): arm/toggle.
    pub fn ladder_tool_key(&mut self) -> Option<RegionMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        if self.ladder_tool {
            self.cancel_ladder();
        } else {
            self.place_tool = None;
            self.clear_platform_state();
            self.clear_draw_state();
            self.cancel_opening();
            self.cancel_vent();
            self.ladder_tool = true;
            self.selected = None;
            log::info!("ladder: armed — aim at a wall and click; scroll sets height");
        }
        None
    }

    pub fn cancel_ladder(&mut self) {
        self.ladder_tool = false;
        self.ladder_preview = None;
    }

    /// Scroll the height of the ladder about to be placed, one [`LADDER_HEIGHT_STEP`]
    /// per click.
    ///
    /// The result is **snapped** to the step rather than merely offset by it, so a height
    /// always lands on the grid however it got there — a value carried over from a
    /// previous placement, or a default that was not a multiple, cannot leave every
    /// subsequent scroll half a cell off.
    pub fn adjust_ladder_height(&mut self, step: f32) {
        if !self.ladder_tool {
            return;
        }
        let h = self.ladder_height + step * LADDER_HEIGHT_STEP;
        let snapped = (h / LADDER_HEIGHT_STEP).round() * LADDER_HEIGHT_STEP;
        self.ladder_height = snapped.clamp(LADDER_HEIGHT_MIN, LADDER_HEIGHT_MAX);
    }

    /// Height (m) the next ladder will be placed at.
    pub fn ladder_height(&self) -> f32 {
        self.ladder_height
    }

    /// Resolve a placement from the crosshair: the base of the ladder on the picked wall,
    /// and the yaw that faces out of that wall.
    ///
    /// Walls only. A ladder on a floor or ceiling is not a thing you can climb, and
    /// silently placing an unusable one is worse than refusing.
    pub(crate) fn resolve_ladder_placement(&mut self) -> Option<(Vec3, f32)> {
        if self.mode != Mode::Build || !self.ladder_tool {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit()?;
        if sel.axis == Axis::Y {
            return None;
        }
        let hit = hit_wt * WORLD_SCALE;
        // The base sits on the floor beneath the hit point, so scrolling height grows the
        // ladder upward from the ground rather than from wherever the crosshair landed.
        let region = self.regions.iter().find(|r| r.id == sel.region_id)?;
        let brush = *region.brushes.iter().find(|b| b.id == sel.brush_id)?;
        let base_y = brush.floor_y * WORLD_SCALE;
        // Face out of the wall — **away** from the solid, into the room the player
        // stands in.
        //
        // The sign is the same trap the vent tool hit. `Side::Max` means a carve at this
        // face extends along **+axis** (see `cut_opening`), i.e. +axis is *into* the
        // solid; so the direction out of it is −axis. Getting this backwards buries the
        // ladder inside the wall, where it is both unclimbable and — since the rails are
        // drawn geometry, not a texture — completely invisible.
        let yaw = match (sel.axis, sel.side) {
            (Axis::X, Side::Max) => -std::f32::consts::FRAC_PI_2, // out = −X
            (Axis::X, Side::Min) => std::f32::consts::FRAC_PI_2,  // out = +X
            (Axis::Z, Side::Max) => std::f32::consts::PI,         // out = −Z
            _ => 0.0,                                             // out = +Z
        };
        Some((Vec3::new(hit.x, base_y, hit.z), yaw))
    }

    /// Recompute the ghost and return it as a preview mesh.
    ///
    /// Drawn from [`ladder_boxes`] — the same rails and rungs the level will actually
    /// get — rather than from the climb volume, so what you line up is what you place.
    pub fn update_ladder_preview(&mut self) -> Option<CpuMesh> {
        if !self.ladder_tool {
            return None;
        }
        self.ladder_preview = self.resolve_ladder_placement();
        let (pos, yaw) = self.ladder_preview?;
        let l = Ladder { height: self.ladder_height, ..Ladder::default() };
        let s = WORLD_SCALE;
        let (min, max) = ladder_plate(pos, yaw, &l);
        Some(crate::world::geom::boxes_mesh(&[[
            min.x / s,
            min.y / s,
            min.z / s,
            (max.x - min.x) / s,
            (max.y - min.y) / s,
            (max.z - min.z) / s,
        ]]))
    }

    /// Place the previewed ladder (left-click).
    pub fn confirm_ladder(&mut self) -> bool {
        if !self.ladder_tool {
            return false;
        }
        let Some((pos, yaw)) = self.ladder_preview.or_else(|| self.resolve_ladder_placement())
        else {
            return false;
        };
        self.record_undo();
        let id = self.ecs.alloc_id();
        let e = self.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::from_rotation_y(yaw).to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::Ladder {
                    height: self.ladder_height,
                    width: Ladder::default().width,
                },
            ],
        });
        self.selected_prop = Some(e);
        self.prop_gizmo_drag = None;
        self.ladder_tool = false;
        self.ladder_preview = None;
        log::info!(
            "placed a {:.1} m ladder at {pos:?} (now {} authored) — player-only; \
             check the NAV tab if it is the only way up",
            self.ladder_height,
            self.ladder_count(),
        );
        true
    }

    /// The most recently placed ladder's `(base, yaw)` — test hook, so the facing test
    /// can assert against the pose that was actually stored rather than re-deriving it.
    #[cfg(test)]
    pub(crate) fn ladder_preview_for_test(&self) -> (Vec3, f32) {
        let (t, _) = self
            .ecs
            .world()
            .query::<(&Transform, &Ladder)>()
            .iter()
            .next()
            .map(|(t, l)| (*t, *l))
            .expect("a ladder is placed");
        (t.pos, t.rot.to_euler(EulerRot::YXZ).0)
    }

    /// How many ladders the level has.
    pub fn ladder_count(&self) -> usize {
        self.ecs.world().query::<&Ladder>().iter().count()
    }

    /// Every ladder as `(min, max, outward normal)` in world metres — what the character
    /// controller tests the player against each step.
    ///
    /// The normal rides along because topping out has to step the player *off the wall*
    /// onto the ledge, and an AABB alone cannot say which way that is.
    pub(crate) fn ladder_volumes(&self) -> Vec<(Vec3, Vec3, Vec3)> {
        self.ecs
            .world()
            .query::<(&Transform, &Ladder)>()
            .iter()
            .map(|(t, l)| {
                let yaw = t.rot.to_euler(EulerRot::YXZ).0;
                let (min, max) = ladder_volume(t.pos, yaw, l);
                (min, max, Vec3::new(yaw.sin(), 0.0, yaw.cos()))
            })
            .collect()
    }

    /// Whether an authored entity is a ladder — the prop gizmo needs this for its
    /// synthetic pick box, since a ladder has no mesh bounds.
    pub(crate) fn entity_is_ladder(&self, e: hecs::Entity) -> bool {
        self.ecs.world().entity(e).map(|r| r.has::<Ladder>()).unwrap_or(false)
    }

    /// A ladder's world AABB — its climb volume, which is also its click-pick box.
    pub(crate) fn ladder_aabb(&self, e: hecs::Entity) -> Option<(Vec3, Vec3)> {
        let r = self.ecs.world().entity(e).ok()?;
        let t = *r.get::<&Transform>()?;
        let l = *r.get::<&Ladder>()?;
        Some(ladder_volume(t.pos, t.rot.to_euler(EulerRot::YXZ).0, &l))
    }
}

/// Rung spacing up a ladder, metres.
const RUNG_SPACING: f32 = 0.35;
/// Half-thickness of the ladder plate, metres — it is artwork on a plane, so this only
/// has to be enough to give the ghost and the pick box something to be.
const PLATE_HALF: f32 = 0.03;
/// Scroll granularity for ladder height: **one wall thickness** (1 WT = 0.25 m).
///
/// Expressed as `WALL_THICKNESS` rather than as 0.25 because that is the reason for the
/// number — it is the editor's base unit, the depth of every wall the ladder is fixed to,
/// and the step every other tool moves in. It was 0.5 m, which is two cells, and made it
/// impossible to land a ladder exactly on a ledge that sat on an odd cell.
const LADDER_HEIGHT_STEP: f32 = WALL_THICKNESS * WORLD_SCALE;
/// A ladder shorter than this is a step; taller than this is a runaway-scroll guard.
/// Both are whole multiples of the step, so the clamp cannot knock a height off-grid.
const LADDER_HEIGHT_MIN: f32 = 4.0 * WALL_THICKNESS * WORLD_SCALE;
const LADDER_HEIGHT_MAX: f32 = 80.0 * WALL_THICKNESS * WORLD_SCALE;

/// How far the plate stands off the wall face, metres — enough that the geometry behind
/// them never z-fights through.
const RAIL_STANDOFF: f32 = 0.07;

/// Ladders are drawn geometry, not a texture, and **that is not a stopgap.**
///
/// The library has no ladder: it was extracted from GoldenEye level *surfaces*, and
/// GoldenEye built its ladders out of geometry. Searched three ways to be sure — the
/// `alpha_key_black` list (one entry, `railing`), dark high-contrast images with periodic
/// structure, and an explicit two-rails-plus-regular-rungs score. Every survivor was
/// signage, a window, a grille or a pipe frame.
///
/// So the rails and rungs below *are* the ladder. Coloured as galvanised metal rather
/// than the marker-yellow they started as, since unlike a spawn pad this is a real object
/// in the world and wants to read as one.
const LADDER_COLOR: [f32; 3] = [0.60, 0.62, 0.65];
/// The rungs, a touch brighter than the rails so the ladder reads as rungs at distance
/// instead of as a flat panel.
const LADDER_RUNG_COLOR: [f32; 3] = [0.74, 0.76, 0.79];
const LADDER_SELECTED_COLOR: [f32; 3] = [1.0, 0.9, 0.3];

impl World {
    /// Append every ladder's rails and rungs to the **structures** mesh.
    ///
    /// They used to be drawn on the flat-colour marker overlay beside the spawn pads,
    /// which is why they read as pale cut-outs rather than as objects: that channel is
    /// unlit and untextured, which is right for an authoring marker and wrong for a thing
    /// that is really there. The structures mesh is the one procedural geometry already
    /// goes through — platform slabs, stair treads, railings — and it is lit and
    /// textured, so a ladder now takes light from the room and wears Surface's metal.
    ///
    /// Railings are the precedent to follow here: thin cosmetic planes emitted into
    /// their own zone, on the same builder.
    pub(crate) fn append_ladders(&self, b: &mut ZonedBuilder) {
        let scheme = engine::render::textures::ladder_scheme();
        let w = |p: Vec3| [p.x / WORLD_SCALE, p.y / WORLD_SCALE, p.z / WORLD_SCALE];
        for (t, l) in self.ecs.world().query::<(&Transform, &Ladder)>().iter() {
            let yaw = t.rot.to_euler(EulerRot::YXZ).0;
            for (c, uv) in ladder_quads(t.pos, yaw, l) {
                b.emit_quad_uv([w(c[0]), w(c[1]), w(c[2]), w(c[3])], uv, scheme, 0);
            }
        }
    }
}

/// The ladder as GoldenEye built it: **a flat plate wearing an alpha-keyed texture**,
/// not modelled rails and rungs.
///
/// `tempImgEd034C` is the real thing — Surface's `m35Transparent`, resolved out of
/// `public/existing goldeneye levels/04 - Surface1/LevelIndices.mtl`. It is 32x32 and
/// holds exactly **one rung** between two rails on a black field, so tiling it vertically
/// at [`RUNG_SPACING`] builds a ladder of any height with the rungs evenly spaced. Black
/// is keyed to transparent (`alpha_key_black` in `themes.json`), which is what makes the
/// gaps between rungs actually gaps.
///
/// This replaced two rails and N rungs of box geometry. The boxes were a stand-in for a
/// texture nobody had found yet; with the texture in hand they are strictly worse — six
/// quads per rung against one for the whole ladder, and still only an approximation of
/// the artwork.
///
/// Returned as `(corners, uvs)` in **world metres**; the caller converts to WT.
pub(crate) fn ladder_quads(pos: Vec3, yaw: f32, l: &Ladder) -> Vec<([Vec3; 4], [[f32; 2]; 4])> {
    let (sn, cs) = yaw.sin_cos();
    let out = Vec3::new(sn, 0.0, cs);
    let across = if out.x.abs() > out.z.abs() { Vec3::Z } else { Vec3::X };
    let half = across * (l.width * 0.5);
    let face = pos + out * RAIL_STANDOFF;
    let up = Vec3::Y * l.height;
    // One texture per rung, so a taller ladder gets more rungs rather than longer ones.
    let v = (l.height / RUNG_SPACING).max(1.0);

    let bl = face - half;
    let br = face + half;
    let front = [bl, br, br + up, bl + up];
    let uv = [[0.0, v], [1.0, v], [1.0, 0.0], [0.0, 0.0]];
    // Back face too, wound the other way: a ladder in a shaft gets looked at from both
    // sides, and a single quad would vanish from one of them.
    let back = [br, bl, bl + up, br + up];
    let uv_back = [[0.0, v], [1.0, v], [1.0, 0.0], [0.0, 0.0]];
    vec![(front, uv), (back, uv_back)]
}

/// The thin slab a ladder occupies, for the BUILD ghost and the click-pick box — the
/// plate's own extent, so the preview is the object and not the volume around it.
pub(crate) fn ladder_plate(pos: Vec3, yaw: f32, l: &Ladder) -> (Vec3, Vec3) {
    let (sn, cs) = yaw.sin_cos();
    let out = Vec3::new(sn, 0.0, cs);
    let across = if out.x.abs() > out.z.abs() { Vec3::Z } else { Vec3::X };
    let half = across * (l.width * 0.5);
    let face = pos + out * RAIL_STANDOFF;
    let thick = out.abs() * PLATE_HALF;
    let a = face - half - thick;
    let b = face + half + thick + Vec3::Y * l.height;
    (a.min(b), a.max(b))
}

/// The climb volume for a ladder at `pos` facing `yaw`: a box spanning the ladder's
/// width across the wall, its height up it, and [`LADDER_DEPTH`] out from it.
///
/// Axis-aligned rather than rotated, because the climb test is a point-in-AABB check on
/// the fixed step and ladders only ever sit on axis-aligned CSG walls. A rotated box
/// would cost an oriented test for a facing the editor cannot produce.
pub(crate) fn ladder_volume(pos: Vec3, yaw: f32, l: &Ladder) -> (Vec3, Vec3) {
    let (s, c) = yaw.sin_cos();
    // Outward normal of the wall the ladder is on. The tool only ever assigns the four
    // axis yaws, so this is a unit axis vector.
    let out = Vec3::new(s, 0.0, c);
    // Across the wall is the other horizontal axis.
    let across = if out.x.abs() > out.z.abs() { Vec3::Z } else { Vec3::X };
    let half_w = across * (l.width * 0.5);
    // Two opposite corners, then min/max — cheaper to read than six signed terms, and
    // correct for all four facings without a case per direction.
    let a = pos - half_w;
    let b = pos + half_w + out * LADDER_DEPTH + Vec3::Y * (l.height + LADDER_OVERSHOOT);
    (a.min(b), a.max(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The climb volume straddles the ladder's face: it is the ladder's width across the
    /// wall, its height up it, and reaches out from the wall on the side you stand.
    #[test]
    fn a_ladders_climb_volume_stands_off_the_wall_it_is_on() {
        let l = Ladder { height: 3.0, width: 0.75 };
        // Facing +Z (a ladder on a wall whose outward normal is +Z).
        let (min, max) = ladder_volume(Vec3::new(5.0, 1.0, 2.0), 0.0, &l);
        assert!(
            (max.y - min.y - (3.0 + LADDER_OVERSHOOT)).abs() < 1e-5,
            "height runs up Y, plus the top-out overshoot"
        );
        assert!((min.y - 1.0).abs() < 1e-5, "and starts at the base");
        assert!((max.x - min.x - 0.75).abs() < 1e-5, "width runs across the wall (X)");
        assert!(max.z > 2.0 && min.z <= 2.0, "depth reaches out on the +Z side");
    }

    /// A ladder placed and saved comes back with its height, so a level does not quietly
    /// reset every ladder to the default on load.
    #[test]
    fn a_placed_ladder_survives_a_save_load_round_trip() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.ladder_tool_key();
        // Scroll off the default so the test proves the *authored* height survives, not
        // just that a default is reapplied. Derived from the step rather than written as
        // a literal — hard-coding it is what broke this test when the scroll granularity
        // changed, and the number was never the point.
        let want = world.ladder_height() + 4.0 * LADDER_HEIGHT_STEP;
        world.adjust_ladder_height(4.0);
        assert!((world.ladder_height() - want).abs() < 1e-5, "scrolled off the default");
        assert!(world.update_ladder_preview().is_some(), "previews on the wall");
        assert!(world.confirm_ladder(), "places");
        assert_eq!(world.ladder_count(), 1);

        let path = std::env::temp_dir().join("bah_ladder_roundtrip.json");
        world.save_level(&path).expect("save");
        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(loaded.ladder_count(), 1, "the ladder round-trips");
        let h = loaded
            .ecs
            .world()
            .query::<&Ladder>()
            .iter()
            .next()
            .map(|l| l.height)
            .unwrap();
        assert!((h - want).abs() < 1e-4, "and keeps its authored height {want}, got {h}");
    }

    /// **The acceptance test: a ladder actually lifts the player.**
    ///
    /// Holding W inside the climb volume gains height against gravity, and stepping out
    /// of the volume hands them back to it. Driven through the real controller so the
    /// grounded-check interaction is exercised — a ladder starting at floor level is
    /// grounded on its first rungs, and an earlier version pinned the player there.
    #[test]
    fn a_ladder_lifts_the_player_and_gravity_takes_over_at_the_top() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        let feet = world.player_pos().expect("player exists");
        // A ladder right where the player is standing, tall enough to be unmistakable.
        let c = world.character.as_mut().unwrap();
        c.set_ladders(vec![(
            feet - Vec3::new(1.0, 0.0, 1.0),
            feet + Vec3::new(1.0, 4.0, 1.0),
            Vec3::Z,
        )]);

        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);
        let start_y = feet.y;
        for _ in 0..90 {
            world.character.as_mut().unwrap().apply_move(
                1.0 / 60.0,
                &input,
                &mut world.physics,
            );
        }
        let c = world.character.as_ref().unwrap();
        assert!(c.is_climbing(), "still attached partway up");
        let climbed = c.pos.y - start_y;
        assert!(
            climbed > 1.0,
            "the player climbed against gravity (gained {climbed:.2} m)"
        );

        // Now take the ladder away — gravity resumes and they come back down.
        let top = world.character.as_ref().unwrap().pos.y;
        world.character.as_mut().unwrap().set_ladders(Vec::new());
        for _ in 0..120 {
            world.character.as_mut().unwrap().apply_move(
                1.0 / 60.0,
                &input,
                &mut world.physics,
            );
        }
        let c = world.character.as_ref().unwrap();
        assert!(!c.is_climbing(), "detached once out of the volume");
        assert!(c.pos.y < top, "and fell back down ({:.2} m from {top:.2})", c.pos.y);
    }

    /// **Standing at the foot of a ladder is not hanging on one.**
    ///
    /// This test used to assert the opposite — that merely entering the volume attached
    /// you — and that was the playtest's "stuck on it and can't get off". Attaching on
    /// contact means gravity is off and forward/back are spent on the climb axis, so a
    /// player who wandered past a ladder could only shuffle sideways out of it.
    ///
    /// There is still no grab key: pressing *up* is the intent, and that is an input the
    /// player is already making. What changed is that the intent is required.
    #[test]
    fn standing_at_the_foot_is_not_climbing_but_pressing_up_is() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        let feet = world.player_pos().expect("player exists");
        world.character.as_mut().unwrap().set_ladders(vec![(
            feet - Vec3::new(1.0, 0.0, 1.0),
            feet + Vec3::new(1.0, 4.0, 1.0),
            Vec3::Z,
        )]);
        let mut input = InputState::default();
        input.pointer_locked = true;

        // Nothing pressed: standing beside it, free to walk away.
        for _ in 0..10 {
            world.character.as_mut().unwrap().apply_move(
                1.0 / 60.0,
                &input,
                &mut world.physics,
            );
        }
        assert!(
            !world.character.as_ref().unwrap().is_climbing(),
            "idling in the volume must not grab the player"
        );

        // Press up: on it, no grab key needed.
        input.press(winit::keyboard::KeyCode::KeyW);
        world
            .character
            .as_mut()
            .unwrap()
            .apply_move(1.0 / 60.0, &input, &mut world.physics);
        assert!(
            world.character.as_ref().unwrap().is_climbing(),
            "pressing up is the grab"
        );
    }

    /// **Topping out hands the player to the ledge instead of trapping them.**
    ///
    /// The other half of "stuck on it and can't get off": the climb volume reaches the
    /// top of the ladder, so climbing to the top left the player inside it with nowhere
    /// to go. Now reaching the top detaches, steps up over the lip and pushes off the
    /// wall, and refuses to re-attach for a beat so falling back through the volume
    /// cannot grab them again.
    #[test]
    fn reaching_the_top_of_a_ladder_detaches() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        let feet = world.player_pos().expect("player exists");
        // The awkward case, and the one that bobbed: a ladder whose top is level with
        // the floor it serves. The volume is built through `ladder_volume`, so it carries
        // the top-out overshoot the same way a placed ladder does.
        let rungs_top = feet.y + 2.0;
        let l = Ladder { height: 2.0, width: 0.75 };
        let (lmin, lmax) = ladder_volume(feet, 0.0, &l);
        world.character.as_mut().unwrap().set_ladders(vec![(lmin, lmax, Vec3::Z)]);
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        let mut ever_climbed = false;
        let mut detached_high = false;
        let mut detach_y = 0.0f32;
        for _ in 0..240 {
            world.character.as_mut().unwrap().apply_move(
                1.0 / 60.0,
                &input,
                &mut world.physics,
            );
            let c = world.character.as_ref().unwrap();
            ever_climbed |= c.is_climbing();
            if ever_climbed && !c.is_climbing() && c.pos.y > feet.y + 1.0 {
                detached_high = true;
                detach_y = c.pos.y;
                break;
            }
        }
        assert!(ever_climbed, "it climbed at all");
        assert!(
            detached_high,
            "it let go at the top instead of holding on forever ({:?})",
            world.character.as_ref().unwrap().pos
        );
        // **The overshoot**: the feet clear the ladder's own top before letting go. Level
        // with it is not clear of it — that is the bob, where the player detaches, drops
        // straight back into the volume, re-grabs and climbs again forever.
        assert!(
            detach_y > rungs_top,
            "let go at {detach_y:.2} m, at or below the ladder top {rungs_top:.2} m —              the feet have to clear the ledge or the player bobs at the lip"
        );
    }

    /// **A placed ladder stands in open space, not inside the wall it is on.**
    ///
    /// The property the facing sign governs, and the one that broke: a ladder facing the
    /// wrong way is buried in solid, where it cannot be climbed and cannot be seen. So
    /// this samples against the CSG rather than asserting a yaw.
    ///
    /// It samples **close to the face** on purpose. The first version measured the middle
    /// of the climb volume and passed with the sign inverted: walls are 0.25 m and the
    /// volume reaches 0.6 m, so a wrongly-faced ladder punches clean through into the
    /// void beyond and reads as open. It has to land inside the slab to discriminate.
    #[test]
    fn a_placed_ladder_stands_in_open_space() {
        for (yaw, name) in [
            (std::f32::consts::PI, "-Z wall"),
            (0.0, "+Z wall"),
            (std::f32::consts::FRAC_PI_2, "-X wall"),
            (-std::f32::consts::FRAC_PI_2, "+X wall"),
        ] {
            let mut world = World::new();
            world.initial_meshes();
            world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
            world.camera.yaw = yaw;
            world.camera.pitch = 0.0;
            world.ladder_tool_key();
            assert!(world.update_ladder_preview().is_some(), "{name}: previews");
            assert!(world.confirm_ladder(), "{name}: places");

            let (base, placed_yaw) = world.ladder_preview_for_test();
            let out = Vec3::new(placed_yaw.sin(), 0.0, placed_yaw.cos());
            let probe = base + out * 0.15 + Vec3::Y * 1.0;
            let wt = probe / WORLD_SCALE;
            let solid = world.regions.iter().any(|r| r.solid_at(wt.x, wt.y, wt.z));
            assert!(
                !solid,
                "{name}: the ladder faces into the wall - {probe:?} is solid, so it is \
                 buried, unclimbable and invisible"
            );
        }
    }

    /// A placed ladder produces real, **lit and textured** geometry in the structures
    /// mesh, wearing the ladder theme — and it is a *plate*, not modelled rails.
    ///
    /// It used to go on the flat-colour marker overlay beside the spawn pads, which is
    /// why it read as a pale cut-out: that channel is unlit and untextured, which is
    /// right for an authoring marker and wrong for an object that is really there.
    #[test]
    fn a_placed_ladder_is_a_lit_textured_alpha_keyed_plate() {
        let mut world = World::new();
        world.initial_meshes();
        let bare = world.rebuild_structures().mesh.groups.len();

        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.ladder_tool_key();
        world.update_ladder_preview();
        assert!(world.confirm_ladder(), "places");

        let rm = world.rebuild_structures();
        let want = engine::render::textures::ladder_scheme() as u16;
        let tris: u32 = rm
            .mesh
            .groups
            .iter()
            .filter(|g| g.scheme == want)
            .map(|g| g.count / 3)
            .sum();
        assert!(
            tris > 0,
            "the ladder emitted no structure geometry (groups went {bare} -> {})",
            rm.mesh.groups.len()
        );
        // A flat plate, front and back: two quads, four triangles. This assertion used to
        // demand >= 24 — two rails and N rungs of box geometry — and that expectation
        // died the moment the real texture turned up. The boxes were standing in for
        // artwork nobody had found; `tempImgEd034C` holds the rails and the rung, so
        // modelling them again would be six quads per rung to say what one quad says.
        assert_eq!(tris, 4, "a double-sided plate, not modelled rails and rungs");
    }

    /// **The ghost is the ladder**, not the trigger volume around it.
    ///
    /// The climb volume is deliberately bigger than the object: half a metre taller for
    /// the top-out overshoot, and 0.6 m deep so you can grab it slightly off the face.
    /// Both are right for a volume and wrong as a preview, and previewing the volume is
    /// what made the placed ladder come out shorter than the ghost promised.
    ///
    /// Asserted against the geometry the level actually gets, not against numbers
    /// restated here, so the two cannot drift apart again.
    #[test]
    fn the_ghost_matches_the_geometry_it_places() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI;
        world.camera.pitch = 0.0;
        world.ladder_tool_key();
        world.adjust_ladder_height(4.0); // 3.0 -> 5.0 m, so height is not the default

        let ghost = world.update_ladder_preview().expect("previews on the wall");
        let (pos, yaw) = world.ladder_preview.expect("a placement resolved");
        let h = world.ladder_height();

        // The plate the ghost should be drawing.
        let (lo, hi) = ladder_plate(pos, yaw, &Ladder { height: h, ..Ladder::default() });

        // The ghost's own bounds, read back off the mesh it produced.
        assert!(!ghost.vertices.is_empty(), "the ghost has geometry");
        let mut gl = Vec3::splat(f32::INFINITY);
        let mut gh = Vec3::splat(f32::NEG_INFINITY);
        for v in &ghost.vertices {
            let p = Vec3::from(v.pos);
            gl = gl.min(p);
            gh = gh.max(p);
        }
        assert!(
            (gl - lo).abs().max_element() < 1e-3 && (gh - hi).abs().max_element() < 1e-3,
            "ghost spans {gl:?}..{gh:?} but the ladder is {lo:?}..{hi:?}"
        );

        // And specifically: it is the ladder's height, NOT the climb volume's.
        let (vmin, vmax) = ladder_volume(pos, yaw, &Ladder { height: h, ..Ladder::default() });
        assert!(
            (gh.y - gl.y - h).abs() < 1e-3,
            "the ghost is {:.2} m tall; the ladder is {h:.2} m",
            gh.y - gl.y
        );
        assert!(
            vmax.y - vmin.y > gh.y - gl.y + 0.4,
            "sanity: the climb volume really is the taller of the two, so this test \
             would have caught the old ghost"
        );
    }

    /// **A taller ladder gets more rungs, not longer ones.**
    ///
    /// The texture holds exactly one rung, so the vertical UV has to scale with height.
    /// Get that wrong and a 6 m ladder is one enormous stretched rung — which is the
    /// failure mode a single hard-coded UV would have.
    #[test]
    fn rung_spacing_stays_constant_as_a_ladder_gets_taller() {
        let at = |h: f32| {
            let l = Ladder { height: h, ..Ladder::default() };
            let q = ladder_quads(Vec3::ZERO, 0.0, &l);
            // The V span of the front quad is how many rungs it tiles.
            let vs: Vec<f32> = q[0].1.iter().map(|uv| uv[1]).collect();
            vs.iter().cloned().fold(f32::MIN, f32::max)
        };
        let short = at(2.0);
        let tall = at(6.0);
        assert!(
            (tall / short - 3.0).abs() < 1e-3,
            "three times the height should tile three times the rungs ({short} -> {tall})"
        );
        // And the spacing itself is the constant the geometry used to model.
        assert!(
            (2.0 / short - RUNG_SPACING).abs() < 1e-3,
            "a rung every {RUNG_SPACING} m, got every {} m",
            2.0 / short
        );
    }

    /// **Height scrolls one wall thickness at a time**, and stays on that grid.
    ///
    /// It moved in 0.5 m — two cells — which made it impossible to land a ladder exactly
    /// on a ledge sitting at an odd cell height. Snapping matters as much as the step
    /// size: offsetting alone would preserve any off-grid value forever.
    #[test]
    fn ladder_height_scrolls_by_one_wall_thickness_and_stays_on_grid() {
        let mut world = World::new();
        world.initial_meshes();
        world.ladder_tool_key();
        let start = world.ladder_height();
        world.adjust_ladder_height(1.0);
        assert!(
            (world.ladder_height() - start - LADDER_HEIGHT_STEP).abs() < 1e-5,
            "one click is one wall thickness ({LADDER_HEIGHT_STEP} m), got {}",
            world.ladder_height() - start
        );
        world.adjust_ladder_height(-1.0);
        assert!((world.ladder_height() - start).abs() < 1e-5, "and back down again");

        // An off-grid height is pulled onto the grid by the next scroll, rather than
        // carrying its offset forever.
        world.ladder_height = start + 0.07;
        world.adjust_ladder_height(1.0);
        let h = world.ladder_height();
        let cells = h / LADDER_HEIGHT_STEP;
        assert!(
            (cells - cells.round()).abs() < 1e-4,
            "{h} m is {cells} cells — heights must land on whole wall thicknesses"
        );

        // The clamps are themselves on the grid, so hitting one cannot knock it off.
        for _ in 0..500 {
            world.adjust_ladder_height(-1.0);
        }
        let lo = world.ladder_height();
        assert!((lo - LADDER_HEIGHT_MIN).abs() < 1e-5, "clamps at the minimum");
        assert!(
            ((lo / LADDER_HEIGHT_STEP) - (lo / LADDER_HEIGHT_STEP).round()).abs() < 1e-4,
            "and the minimum is itself a whole number of cells"
        );
    }

    /// Ladders go on walls. A floor or ceiling pick is refused rather than silently
    /// placing one nobody can climb.
    #[test]
    fn a_ladder_refuses_a_floor_face() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pos = Vec3::new(3.0, 2.0, 3.0);
        world.camera.pitch = -std::f32::consts::FRAC_PI_2; // straight down
        world.ladder_tool_key();
        assert!(
            world.resolve_ladder_placement().is_none(),
            "a floor is not a wall, so there is nothing to climb"
        );
    }
}
