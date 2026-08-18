//! Spawn-pad placement (the object panel's SPAWNS tab): arm a pad for placement,
//! track a floor ghost under the cursor, and drop it as an authored ECS entity.
//!
//! A pad is [`Transform`](crate::ecs::Transform) +
//! [`SpawnPoint`](crate::ecs::SpawnPoint) and deliberately **no**
//! [`Renderable`](crate::ecs::Renderable) — exactly the shape a point light has (see
//! [`super::light`]), which is what lets it ride the level file's existing `entities`
//! collection with no format change and stay out of every prop path (draw list,
//! colliders, nav). Selection + the gizmo are shared with the prop gizmo
//! (`world::tools::prop_gizmo`), which gives a pad a synthetic pick box like a light's
//! but — unlike a light — keeps **rotate** enabled, because a pad's yaw *is* its
//! authored spawn facing.
//!
//! This is the third placeable after props and lights and follows them rather than
//! inventing a third way. Undo comes for free: `world::history` already snapshots the
//! authored entity set.

use super::super::*;
use crate::ecs::{ComponentData, EntityData, SpawnPoint, Transform};

/// Half-extent (m) of a pad's floor square — its marker, its placement ghost, and
/// (scaled up a little) the synthetic pick box the gizmo selects against.
pub(crate) const PAD_MARKER_HALF: f32 = 0.6;

/// The pad marker's flat colour: the same bright red the old fixed marker used, so a
/// level authored around the fixed ingress reads the same once pads replace it.
const PAD_MARKER_COLOR: [f32; 3] = [0.95, 0.12, 0.12];
/// A selected pad's marker colour (brightened, so the gizmo target is obvious).
const PAD_SELECTED_COLOR: [f32; 3] = [1.0, 0.65, 0.35];

/// How far along its facing a pad's direction nub sits, and the nub's half-extent —
/// a small box out front so the authored yaw is visible on the floor. Rotating the
/// pad with the gizmo swings this nub, which is the whole point of showing it.
const PAD_NUB_DIST: f32 = 0.85;
const PAD_NUB_HALF: f32 = 0.16;

impl World {
    /// Whether the spawn-pad placement tool is armed.
    pub fn is_placing_spawn_point(&self) -> bool {
        self.spawn_tool
    }

    /// Arm/toggle spawn-pad placement, BUILD only. Re-arming disarms; cancels any
    /// other armed tool/selection so the authoring tools stay mutually exclusive.
    pub fn arm_spawn_point_placement(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.spawn_tool {
            self.spawn_tool = false;
            self.spawn_preview = None;
            return;
        }
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.clear_draw_state();
        self.selected = None;
        self.prop_tool = None;
        self.prop_preview_pos = None;
        self.light_tool = false;
        self.light_preview_pos = None;
        self.spawn_tool = true;
        self.spawn_preview = None;
    }

    /// Disarm spawn-pad placement (Esc / Q / panel close).
    pub fn cancel_spawn_point_placement(&mut self) {
        self.spawn_tool = false;
        self.spawn_preview = None;
    }

    /// Recompute the floor ghost under the cursor ray each frame while armed,
    /// returning the marker ghost mesh, or `None` when the ray misses a floor. Gated
    /// to up-facing floor faces (a pad is somewhere you *stand*, unlike a light, which
    /// can hang off a wall or ceiling). Stores the point + facing a confirm places.
    pub fn update_spawn_point_preview(&mut self, origin: Vec3, dir: Vec3) -> Option<CpuMesh> {
        if !self.spawn_tool {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit_from(origin, dir)?;
        if sel.axis != Axis::Y || sel.side != Side::Min {
            self.spawn_preview = None;
            return None;
        }
        let pos = hit_wt * WORLD_SCALE;
        self.spawn_preview = Some(pos);
        Some(pad_ghost_mesh(pos))
    }

    /// Confirm placement (left-click): author a pad entity at the ghost point, facing
    /// the way the fly-cam is looking — so you aim where a spawning player should look
    /// and drop the pad. Selects the fresh pad and **disarms**, like a light: the very
    /// next click grabs the gizmo to nudge or re-aim it. Records an undo checkpoint.
    pub fn confirm_spawn_point_placement(&mut self) -> bool {
        if !self.spawn_tool {
            return false;
        }
        let Some(pos) = self.spawn_preview else {
            return false;
        };
        self.record_undo();
        let id = self.ecs.alloc_id();
        let e = self.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::from_rotation_y(self.camera.yaw).to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::SpawnPoint,
            ],
        });
        self.selected_prop = Some(e);
        self.prop_gizmo_drag = None;
        self.spawn_tool = false;
        self.spawn_preview = None;
        log::info!("placed spawn pad at {pos:?} (now {} authored)", self.spawn_pad_count());
        true
    }

    /// Every authored spawn pad as `(position, facing yaw)`, in ECS query order.
    ///
    /// This is the whole pool — **one shared list for the player and the simulants**,
    /// as Perfect Dark has it (`g_SpawnPoints`, drawn on by both `bot_spawn` and
    /// `player_start_new_life`). Positions are raw authored metres; `prepare_spawn`
    /// resolves them to standable nav cells at G, PD's `chr_adjust_pos_for_spawn` step.
    pub(crate) fn authored_spawn_pads(&self) -> Vec<super::super::spawn::SpawnPad> {
        self.ecs
            .world()
            .query::<(&Transform, &SpawnPoint)>()
            .iter()
            .map(|(t, _)| super::super::spawn::SpawnPad {
                pos: t.pos,
                yaw: t.rot.to_euler(EulerRot::YXZ).0,
            })
            .collect()
    }

    /// How many spawn pads the level has authored (panel readout + logs).
    pub fn spawn_pad_count(&self) -> usize {
        self.ecs.world().query::<&SpawnPoint>().iter().count()
    }

    /// Whether an authored entity is a spawn pad — the prop gizmo uses this for the
    /// synthetic pick box (a pad has no mesh bounds).
    pub(crate) fn entity_is_spawn_point(&self, e: hecs::Entity) -> bool {
        self.ecs.world().entity(e).map(|r| r.has::<SpawnPoint>()).unwrap_or(false)
    }

    /// The spawn-pad markers: one flat floor square per authored pad plus a small nub
    /// showing its facing, drawn in **both** BUILD and HUNT so the level can be
    /// authored around visible ingress points (and so you can see where the pack came
    /// from mid-hunt).
    ///
    /// **No pads authored → no markers.** A marker means "a pad is here", so a level
    /// with none must show none: the fresh starting room is bare floor, and a red square
    /// at the legacy [`SPAWN_MARKER_POS`] would read as an authored pad that can't be
    /// selected, moved or deleted, and wouldn't be in the saved file.
    ///
    /// [`World::prepare_spawn`] still *falls back* to that fixed point at G so an
    /// un-authored level (an older save, the AI lab's arenas, the levelgen harness)
    /// spawns its wave somewhere rather than going quiet — Perfect Dark guards
    /// identically, `if (g_NumSpawnPoints > 0)` (`playerreset.c:398`). That fallback is a
    /// compatibility shim, not a level feature, so it warns instead of drawing itself.
    pub fn spawn_marker_mesh(&self) -> Option<ColoredMesh> {
        let pads = self.authored_spawn_pads();
        if pads.is_empty() {
            return None;
        }
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        // Highlight the selected pad so the gizmo target is unmistakable.
        let sel_pos = self
            .selected_prop
            .filter(|&e| self.entity_is_spawn_point(e))
            .and_then(|e| self.ecs.world().entity(e).ok())
            .and_then(|r| r.get::<&Transform>().map(|t| t.pos));
        for p in &pads {
            let is_sel = sel_pos.is_some_and(|s| s.distance_squared(p.pos) < 1e-6);
            let col = if is_sel { PAD_SELECTED_COLOR } else { PAD_MARKER_COLOR };
            push_pad_marker(&mut vertices, &mut indices, p.pos, Some(p.yaw), col);
        }
        Some(ColoredMesh { vertices, indices })
    }
}

/// Append one pad marker — the floor square, plus (when a facing is given) the
/// direction nub out front along that yaw — to a colored-overlay buffer.
fn push_pad_marker(
    v: &mut Vec<engine::render::mesh::ColorVertex>,
    idx: &mut Vec<u32>,
    pos: Vec3,
    yaw: Option<f32>,
    color: [f32; 3],
) {
    let h = PAD_MARKER_HALF;
    push_colored_box(
        v,
        idx,
        Vec3::new(pos.x - h, pos.y + 0.01, pos.z - h),
        Vec3::new(pos.x + h, pos.y + 0.05, pos.z + h),
        color,
    );
    if let Some(yaw) = yaw {
        // Same convention as `camera::forward_from` at pitch 0, so the nub points
        // where a body spawned here will be looking.
        let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let c = pos + fwd * PAD_NUB_DIST;
        let n = PAD_NUB_HALF;
        push_colored_box(
            v,
            idx,
            Vec3::new(c.x - n, pos.y + 0.01, c.z - n),
            Vec3::new(c.x + n, pos.y + 0.09, c.z + n),
            color,
        );
    }
}

/// The placement ghost: a marker-sized box at `pos`, built in WT for [`boxes_mesh`]
/// (which scales WT → metres), matching the light/prop ghost path.
fn pad_ghost_mesh(pos: Vec3) -> CpuMesh {
    let s = WORLD_SCALE;
    let h = PAD_MARKER_HALF;
    boxes_mesh(&[[
        (pos.x - h) / s,
        pos.y / s,
        (pos.z - h) / s,
        (2.0 * h) / s,
        0.1 / s,
        (2.0 * h) / s,
    ]])
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Author a pad at `pos` facing `yaw` through the real placement path, leaving the
    /// tool disarmed (mirrors `tools::prop`'s test helper). Shared with the respawn and
    /// scoreboard test modules, which both need a level with pads.
    pub(crate) fn place_pad(world: &mut World, pos: Vec3, yaw: f32) {
        world.camera.yaw = yaw;
        world.spawn_tool = true;
        world.spawn_preview = Some(pos);
        assert!(world.confirm_spawn_point_placement(), "pad should place");
        world.cancel_spawn_point_placement();
    }

    /// M1's acceptance test: place 3 pads, save, load into a fresh world, get 3 back —
    /// with their positions and authored facings intact. Pads ride the level file's
    /// existing `entities` collection, so this exercises no new schema.
    #[test]
    fn three_placed_pads_survive_a_save_load_round_trip() {
        let mut world = World::new();
        place_pad(&mut world, Vec3::new(2.0, 0.0, 2.0), 0.0);
        place_pad(&mut world, Vec3::new(9.0, 0.0, 4.0), std::f32::consts::FRAC_PI_2);
        place_pad(&mut world, Vec3::new(4.0, 0.0, 11.0), -std::f32::consts::FRAC_PI_2);
        assert_eq!(world.spawn_pad_count(), 3, "three pads authored");

        let path = std::env::temp_dir().join("bah_spawn_pads_roundtrip.json");
        world.save_level(&path).expect("save");

        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        assert_eq!(loaded.spawn_pad_count(), 3, "all three pads round-trip");

        // Positions + facings survive (order is ECS query order, so compare as sets).
        let mut got: Vec<(i32, i32, i32)> = loaded
            .authored_spawn_pads()
            .iter()
            .map(|p| {
                (
                    (p.pos.x * 100.0) as i32,
                    (p.pos.z * 100.0) as i32,
                    (p.yaw.to_degrees()).round() as i32,
                )
            })
            .collect();
        got.sort_unstable();
        let mut want = vec![(200, 200, 0), (900, 400, 90), (400, 1100, -90)];
        want.sort_unstable();
        assert_eq!(got, want, "each pad keeps its position and authored yaw");

        // A pad is NOT a prop: it has no Renderable, so it stays out of the prop draw
        // list (the same guarantee point lights have).
        assert!(loaded.prop_draws(1.0).is_empty(), "a pad is not drawn as a prop");
        let _ = std::fs::remove_file(&path);
    }

    /// Placement and deletion are both undoable — pads join the authored-entity
    /// snapshot `world::history` already takes, which is the trap every placeable in
    /// this editor has hit.
    #[test]
    fn pad_placement_and_deletion_undo() {
        let mut world = World::new();
        place_pad(&mut world, Vec3::new(2.0, 0.0, 2.0), 0.0);
        place_pad(&mut world, Vec3::new(8.0, 0.0, 8.0), 0.0);
        assert_eq!(world.spawn_pad_count(), 2);

        world.undo();
        assert_eq!(world.spawn_pad_count(), 1, "undo removes the second pad");

        world.redo();
        assert_eq!(world.spawn_pad_count(), 2, "redo puts it back");

        // Delete the selected pad, then undo the delete.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &SpawnPoint)>()
            .iter()
            .next()
            .map(|(e, _)| e)
            .expect("a pad entity");
        world.selected_prop = Some(e);
        world.delete_selected_prop();
        assert_eq!(world.spawn_pad_count(), 1, "delete removes it");
        world.undo();
        assert_eq!(world.spawn_pad_count(), 2, "undo restores the deleted pad");
    }

    /// A world whose single region is a `side`-metre-square room, so pads can be placed
    /// far enough apart for Perfect Dark's 10 m distance gate to be meaningful (the
    /// default 6 m starting room is smaller than the gate).
    pub(crate) fn big_room(side_m: f32) -> World {
        let mut world = World::new();
        let wt = side_m / WORLD_SCALE;
        let mut region = Region::new(0);
        region.brushes.push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, wt, 16.0, wt));
        world.regions = vec![region];
        world.initial_meshes();
        world
    }

    /// M2's acceptance test: **the player no longer spawns under the fly-cam.** With pads
    /// authored, both the player and every hunter enter from the pool; the fly-cam is
    /// parked in a corner none of the pads occupy, so "the player is at a pad" and "the
    /// player is not where the camera was" are distinguishable.
    #[test]
    fn player_and_wave_enter_from_the_authored_pool() {
        let mut world = big_room(40.0);
        world.set_wave_size(4);
        // Three pads spread across the room…
        let pads = [
            Vec3::new(6.0, 0.0, 6.0),
            Vec3::new(34.0, 0.0, 6.0),
            Vec3::new(20.0, 0.0, 34.0),
        ];
        for p in pads {
            place_pad(&mut world, p, 0.0);
        }
        // …and the fly-cam parked well away from all of them.
        let cam = Vec3::new(20.0, 2.0, 20.0);
        world.camera.pos = cam;

        world.toggle_mode(); // BUILD → HUNT
        let player = world.player_pos().expect("player entered");

        let near_a_pad = |p: Vec3| {
            pads.iter()
                .any(|pad| (p.x - pad.x).abs() < 3.0 && (p.z - pad.z).abs() < 3.0)
        };
        assert!(
            near_a_pad(player),
            "the player entered from a pad, got {player:?}"
        );
        assert!(
            (player.x - cam.x).abs() > 3.0 || (player.z - cam.z).abs() > 3.0,
            "the player must NOT enter under the fly-cam once pads exist (cam {cam:?}, got {player:?})"
        );
        assert_eq!(world.enemies.len(), 4, "the whole wave floods in");
        for (i, e) in world.enemies.iter().enumerate() {
            assert!(
                near_a_pad(e.enemy.pos),
                "hunter {i} entered from a pad, got {:?}",
                e.enemy.pos
            );
        }
    }

    /// With no pads authored the level keeps its old behaviour exactly: the player drops
    /// in under the fly-cam. This is Perfect Dark's own guard (`if (g_NumSpawnPoints > 0)`,
    /// `playerreset.c:398`) and it is what every headless caller predating pads relies on
    /// — the AI lab's arenas seat the player by moving the camera.
    #[test]
    fn with_no_pads_the_player_still_enters_under_the_fly_cam() {
        let mut world = big_room(40.0);
        let cam = Vec3::new(12.0, 2.0, 17.0);
        world.camera.pos = cam;
        world.toggle_mode();
        let player = world.player_pos().expect("player entered");
        assert!(
            (player.x - cam.x).abs() < 0.01 && (player.z - cam.z).abs() < 0.01,
            "no pads → the fly-cam drop is preserved (cam {cam:?}, got {player:?})"
        );
        // And the wave still floods in at the legacy fixed marker rather than going
        // quiet, which is the trap the handoff called out for the AI lab.
        assert!(!world.enemies.is_empty(), "an empty pool must not mean no hunters");
        for e in &world.enemies {
            assert!(
                e.enemy.pos.distance(world.spawn_point) < 2.0,
                "hunters fall back to the fixed marker, got {:?}",
                e.enemy.pos
            );
        }
    }

    /// Perfect Dark's filter, end to end: given one pad on top of the player and one far
    /// away, a hunter entering takes the far one — **every** time, not on average. This
    /// is the property that stops a respawn dropping a body inside someone's engagement
    /// band, which the handoff flagged as what makes `AI=pd` + respawn survivable.
    ///
    /// The wave is suppressed so the player is the only occupant: with hunters also in
    /// the level and only two pads there would be no clear pad to find, and the rule
    /// would correctly return the least-bad one instead.
    #[test]
    fn a_hunter_avoids_the_pad_the_player_is_standing_on() {
        let mut world = big_room(40.0);
        world.set_spawn_enemies(false);
        place_pad(&mut world, Vec3::new(4.0, 0.0, 4.0), 0.0);
        place_pad(&mut world, Vec3::new(36.0, 0.0, 36.0), 0.0);
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();

        // Stand the player on the first pad and ask the rule where a hunter should enter.
        if let Some(c) = world.character.as_mut() {
            c.pos = Vec3::new(4.0, c.pos.y, 4.0);
        }
        for _ in 0..20 {
            let (_, pad) = world
                .choose_spawn_pad(crate::world::Spawning::Hunter(0))
                .expect("the pool yields a pad");
            assert!(
                pad.pos.x > 20.0 && pad.pos.z > 20.0,
                "a hunter takes the pad clear of the player, got {:?}",
                pad.pos
            );
        }
    }

    /// A marker means "a pad is here", so an un-authored level draws none: the fresh
    /// starting room is bare floor. Authored pads then render in BOTH modes.
    ///
    /// The old behaviour drew a square at the legacy fixed [`SPAWN_MARKER_POS`] whenever
    /// the pool was empty, which read as an authored pad you couldn't select, move,
    /// delete or find in the saved file. `prepare_spawn` still falls back to that point
    /// so a wave has somewhere to enter (see
    /// `the_fixed_fallback_still_spawns_a_wave_without_drawing_a_marker`) — it just no
    /// longer advertises itself as level content.
    #[test]
    fn markers_only_render_for_authored_pads() {
        let mut world = World::new();
        world.initial_meshes();

        assert!(
            world.spawn_marker_mesh().is_none(),
            "a level with no pads authored draws no marker at all"
        );

        place_pad(&mut world, Vec3::new(2.0, 0.0, 2.0), 0.0);
        let one = world.spawn_marker_mesh().expect("pad markers in BUILD");
        place_pad(&mut world, Vec3::new(4.0, 0.0, 4.0), 0.0);
        let two = world.spawn_marker_mesh().expect("pad markers in BUILD");
        assert!(
            two.vertices.len() > one.vertices.len(),
            "each authored pad adds its own marker"
        );

        world.toggle_mode(); // BUILD → HUNT
        assert!(!world.is_build());
        assert!(world.spawn_marker_mesh().is_some(), "markers still show in HUNT");
    }

    /// The fallback is behaviour-only: with no pads authored, entering HUNT still floods
    /// the wave in at the fixed point (so an older save or a generated level isn't dead),
    /// while nothing is drawn there. This is the pairing that makes dropping the phantom
    /// marker safe rather than a silent regression.
    #[test]
    fn the_fixed_fallback_still_spawns_a_wave_without_drawing_a_marker() {
        let mut world = World::new();
        world.set_wave_size(4);
        world.initial_meshes();
        assert_eq!(world.spawn_pad_count(), 0, "nothing authored");

        world.toggle_mode(); // BUILD → HUNT
        assert_eq!(world.enemies.len(), 4, "the wave still enters the level");
        assert!(
            world.spawn_point.distance(SPAWN_MARKER_POS) < 1.0,
            "at the fixed fallback point, got {:?}",
            world.spawn_point
        );
        assert!(
            world.spawn_marker_mesh().is_none(),
            "but the fallback draws no marker for itself"
        );
    }
}
