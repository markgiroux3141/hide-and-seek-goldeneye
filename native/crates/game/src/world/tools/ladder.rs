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

    /// Scroll the height of the ladder about to be placed, in metres.
    pub fn adjust_ladder_height(&mut self, step: f32) {
        if !self.ladder_tool {
            return;
        }
        self.ladder_height = (self.ladder_height + step * 0.5).clamp(1.0, 20.0);
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
        // Face out of the wall: the outward normal of the picked face.
        let yaw = match (sel.axis, sel.side) {
            (Axis::X, Side::Max) => std::f32::consts::FRAC_PI_2,
            (Axis::X, Side::Min) => -std::f32::consts::FRAC_PI_2,
            (Axis::Z, Side::Max) => 0.0,
            _ => std::f32::consts::PI,
        };
        Some((Vec3::new(hit.x, base_y, hit.z), yaw))
    }

    /// Recompute the ghost and return it as a preview mesh.
    pub fn update_ladder_preview(&mut self) -> Option<CpuMesh> {
        if !self.ladder_tool {
            return None;
        }
        self.ladder_preview = self.resolve_ladder_placement();
        let (pos, yaw) = self.ladder_preview?;
        let l = Ladder { height: self.ladder_height, ..Ladder::default() };
        let (min, max) = ladder_volume(pos, yaw, &l);
        Some(crate::world::geom::boxes_mesh(&[[
            min.x / WORLD_SCALE,
            min.y / WORLD_SCALE,
            min.z / WORLD_SCALE,
            (max.x - min.x) / WORLD_SCALE,
            (max.y - min.y) / WORLD_SCALE,
            (max.z - min.z) / WORLD_SCALE,
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

    /// How many ladders the level has.
    pub fn ladder_count(&self) -> usize {
        self.ecs.world().query::<&Ladder>().iter().count()
    }

    /// Every ladder's climb volume as a world AABB (metres) — what the character
    /// controller tests the player against each step.
    pub(crate) fn ladder_volumes(&self) -> Vec<(Vec3, Vec3)> {
        self.ecs
            .world()
            .query::<(&Transform, &Ladder)>()
            .iter()
            .map(|(t, l)| ladder_volume(t.pos, t.rot.to_euler(EulerRot::YXZ).0, l))
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
    let b = pos + half_w + out * LADDER_DEPTH + Vec3::Y * l.height;
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
        assert!((max.y - min.y - 3.0).abs() < 1e-5, "height runs up Y");
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
        world.adjust_ladder_height(4.0); // 3.0 → 5.0 m
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
        assert!((h - 5.0).abs() < 1e-4, "and keeps its authored height, got {h}");
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

    /// A ladder is climbed without a grab key: standing in the volume attaches you.
    /// A climb you have to *ask* for is one you fumble while being shot at.
    #[test]
    fn standing_in_the_volume_attaches_without_a_key() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        let feet = world.player_pos().expect("player exists");
        world.character.as_mut().unwrap().set_ladders(vec![(
            feet - Vec3::new(1.0, 0.0, 1.0),
            feet + Vec3::new(1.0, 4.0, 1.0),
        )]);
        let mut input = InputState::default();
        input.pointer_locked = true; // no keys held at all
        world
            .character
            .as_mut()
            .unwrap()
            .apply_move(1.0 / 60.0, &input, &mut world.physics);
        assert!(
            world.character.as_ref().unwrap().is_climbing(),
            "attached on contact, with nothing pressed"
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
