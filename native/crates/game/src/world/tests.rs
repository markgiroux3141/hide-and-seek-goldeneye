//! End-to-end authoring + simulation tests driving `World`'s public API (plus a
//! few module-internal helpers). This is the behavioral oracle for the port —
//! moved verbatim out of the old `world.rs`; the split changed no test logic.

use super::*;
use super::editing::find_room_brushes;

    /// Free-aim: a small mouse delta floats the crosshair inside the boundary with
    /// no camera pan; a big delta pins it to the rim and spills the rest to a pan.
    #[test]
    fn free_aim_floats_then_pans_at_the_rim() {
        use super::combat::resolve_aim;
        // Small move from center — stays inside the circle, no pan.
        let (ax, ay, pdx, pdy) = resolve_aim(0.0, 0.0, 20.0, 0.0);
        assert!((ax * ax + ay * ay).sqrt() < AIM_MAX_RANGE, "inside the boundary");
        assert!(ax > 0.0, "moved right");
        assert_eq!((pdx, pdy), (0.0, 0.0), "no camera pan inside the boundary");
        // Huge move — crosshair pinned at the rim, remainder becomes a pan.
        let (ax, ay, pdx, _pdy) = resolve_aim(0.0, 0.0, 100_000.0, 0.0);
        let mag = (ax * ax + ay * ay).sqrt();
        assert!((mag - AIM_MAX_RANGE).abs() < 1e-3, "clamped to the rim: {mag}");
        assert!(pdx > 0.0, "overflow pans the camera right");
    }

    /// End-to-end authoring loop with no GPU: build the room + collider, aim the
    /// crosshair at the −Z wall, push it, and confirm the whole pipeline fires
    /// (raycast pick → brush resize → re-evaluate → collider rebuilt → new mesh).
    /// This is the Phase 1 risk-burndown proof.
    #[test]
    fn push_carves_the_wall_the_crosshair_hits() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        assert_eq!(initial.len(), 1, "one room region");
        let tris_before = initial[0].mesh.indices.len();
        assert!(tris_before > 0, "room built geometry");

        // Camera spawns at (3,1.5,3) m looking −Z → crosshair hits the z=0 wall.
        let rm = world.push(PUSH_PULL_STEP).expect("crosshair should hit a wall");
        assert_eq!(rm.id, 0);
        assert!(!rm.mesh.indices.is_empty(), "carved room still has geometry");

        // Pulling the same wall back should also resolve a hit (loop is stable).
        assert!(world.pull(PUSH_PULL_STEP).is_some(), "pull resolves a face too");
    }

    /// Aiming at empty space (no collider along the ray) picks nothing — push is
    /// a safe no-op rather than a panic.
    #[test]
    fn looking_at_nothing_is_a_safe_noop() {
        let mut world = World::new();
        world.initial_meshes();
        // Fly far outside the room and look away from it.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.camera.yaw = 0.0;
        assert!(world.push(PUSH_PULL_STEP).is_none());
    }

    /// Entering HUNT drops a capsule that gravity settles onto the room floor
    /// (y≈0, the cavity's bottom) — it neither sinks through nor floats.
    #[test]
    fn character_settles_on_the_floor_under_gravity() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // BUILD → HUNT, spawns on the floor beneath the cam
        assert_eq!(world.mode, Mode::Hunt);
        let input = InputState::default(); // no keys, not pointer-locked

        for _ in 0..240 {
            // 2 s at 120 Hz
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player exists in HUNT");
        assert!(
            feet.y.abs() < 0.05,
            "feet should rest on the y≈0 floor, got {}",
            feet.y
        );
    }

    /// Phase 3 milestone: on HUNT the nav grid bakes, a hunter spawns across the
    /// room, and it pathfinds to a stationary player and catches them.
    #[test]
    fn hunter_pathfinds_to_and_catches_the_player() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // bakes nav + spawns hunter far from the player
        assert!(world.player_pos().is_some(), "player spawned");
        assert!(world.enemy.is_some(), "hunter spawned");

        let input = InputState::default(); // player stands still
        let mut caught = false;
        for _ in 0..1800 {
            // up to 15 s at 120 Hz
            world.fixed_step(1.0 / 120.0, &input);
            if world.is_caught() {
                caught = true;
                break;
            }
        }
        assert!(caught, "hunter should reach and catch the stationary player");
    }

    /// B5: in HUNT the animated model *is* the hunter — the placeholder box is
    /// gone and the model tracks the enemy's nav-driven position + faces its
    /// travel direction while it moves.
    #[test]
    fn hunter_drives_the_animated_model_not_a_box() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn hunter
        assert!(world.enemy.is_some(), "hunter spawned");
        assert!(world.char_model.is_some(), "character model loaded");
        // The placeholder box is suppressed (the model is the hunter).
        assert!(world.enemy_mesh().is_none(), "box replaced by the model");

        // Step the hunter, then advance the animation driver.
        let input = InputState::default();
        for _ in 0..30 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        world.advance_animation(1.0 / 60.0);

        let epos = world.enemy.as_ref().unwrap().pos;
        assert!(
            (world.char_pos - epos).length() < 1e-4,
            "model position {:?} should track the hunter {:?}",
            world.char_pos,
            epos
        );
        // Once moving, yaw faces the hunter's heading.
        let h = world.enemy.as_ref().unwrap().heading();
        if world.enemy.as_ref().unwrap().speed() > 0.0 {
            let expect = h.x.atan2(h.z);
            let d = (world.char_yaw - expect).abs();
            assert!(d < 1e-4, "yaw {} should face heading {}", world.char_yaw, expect);
        }
        // The pose is a real 15-joint skinning set.
        let (_, joints) = world.character_pose().expect("character present in HUNT");
        assert_eq!(joints.len(), 15);
    }

    /// The door tool: `B` arms a preview on the wall, a left-click cuts a
    /// `door`-marked opening. No cut happens just from arming.
    #[test]
    fn door_tool_arms_with_b_and_cuts_on_click() {
        let mut world = World::new(); // camera at (3,1.5,3) m looking −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_door_arming());

        // B arms (no geometry change).
        assert!(world.door_tool_key().is_none(), "B does not cut");
        assert!(world.is_door_arming(), "B arms the preview");
        assert!(world.update_door_preview().is_some(), "ghost previews on the wall");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "no door yet");

        // Left-click (confirm_door) cuts.
        assert!(world.confirm_door().is_some(), "click cuts the door");
        assert!(!world.is_door_arming(), "cutting disarms");
        assert!(
            world.regions[0].brushes.iter().any(|b| b.door),
            "a door-marked doorframe brush was created"
        );
    }

    /// The room retexture flood-fill stops at door/hole frames — so a room and the
    /// room beyond its door are texturable independently (issue: number keys used
    /// to change the whole level).
    #[test]
    fn room_floodfill_stops_at_frames() {
        let room = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        // Doorframe carved in the x-min wall (adjacent at x=0), marked as a frame.
        let mut frame = Brush::new(2, Op::Subtract, -1.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        frame.frame = true;
        // Protoroom just beyond the frame (adjacent at x=-1), NOT touching the room.
        let proto = Brush::new(3, Op::Subtract, -2.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        let brushes = vec![room, frame, proto];

        let ids = find_room_brushes(&room, &brushes);
        assert!(ids.contains(&1), "the room itself is in the set");
        assert!(!ids.contains(&2), "the frame bounds the room, not part of it");
        assert!(
            !ids.contains(&3),
            "flood-fill must not cross the frame into the room beyond"
        );
    }

    /// A door cut re-textures the doorway reveal as the tunnel zone (5), while the
    /// surrounding room keeps its floor/wall zones — the geometric frame-AABB
    /// classification working end-to-end through the real cut flow.
    #[test]
    fn door_cut_textures_the_reveal_as_a_tunnel_zone() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key();
        world.update_door_preview();
        world.confirm_door().expect("door cut");

        let tex = world.regions[0].evaluate_textured();
        let zones: std::collections::BTreeSet<u8> = tex.groups.iter().map(|g| g.zone).collect();
        assert!(zones.contains(&5), "doorway reveal → zone 5; got {zones:?}");
        assert!(
            zones.contains(&0) && (zones.contains(&2) || zones.contains(&3)),
            "room floor + wall zones still present: {zones:?}"
        );
    }

    /// Pressing `B` while the tool is armed toggles it back off, cutting nothing.
    #[test]
    fn pressing_b_again_deselects_the_door_tool() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.door_tool_key(); // B again → deselect
        assert!(!world.is_door_arming(), "second B turns the tool off");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "toggling off cuts nothing");
    }

    /// A door cut into an X-facing wall stays upright (height along Y, width
    /// along Z) — the regression for the 90°-rotated door.
    #[test]
    fn door_on_an_x_wall_stays_upright() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.yaw = std::f32::consts::FRAC_PI_2; // face the −X wall
        world.door_tool_key(); // arm
        assert!(world.update_door_preview().is_some(), "previews on the −X wall");
        assert!(world.confirm_door().is_some(), "cuts the door");

        let frame = world
            .regions[0]
            .brushes
            .iter()
            .find(|b| b.door)
            .expect("doorframe exists");
        assert_eq!(frame.h, DOOR_HEIGHT, "height runs vertically (Y)");
        assert_eq!(frame.d, DOOR_WIDTH, "width runs horizontally (Z)");
        assert_eq!(frame.w, WALL_THICKNESS, "1 WT thick through the wall (X)");
    }

    /// Cancelling an armed door leaves the geometry untouched.
    #[test]
    fn cancel_door_leaves_no_opening() {
        let mut world = World::new();
        world.initial_meshes();
        world.door_tool_key(); // arm
        assert!(world.is_door_arming());
        world.cancel_door();
        assert!(!world.is_door_arming());
        assert!(!world.regions[0].brushes.iter().any(|b| b.door), "cancel cuts nothing");
    }

    /// Scroll sizing clamps to [1, faceSize] and flips full ↔ sub-face.
    #[test]
    fn scroll_sizes_and_clamps_the_selection() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "picks the −Z wall");
        assert!(world.is_full_face(), "a fresh selection is full-face");

        // One scroll-down shrinks below full → sub-face.
        world.adjust_selection_size(-1.0, 0.0);
        assert!(!world.is_full_face(), "scrolling in makes it a sub-face");

        // Shrinking hard clamps at 1 (never full).
        for _ in 0..40 {
            world.adjust_selection_size(-1.0, 0.0);
        }
        assert!(!world.is_full_face());

        // Growing hard clamps back to the full face size.
        for _ in 0..40 {
            world.adjust_selection_size(1.0, 0.0);
        }
        assert!(world.is_full_face(), "grown back to full clamps at faceSize");
    }

    /// A sub-face push spawns a subtract brush sized to the sub-rect (carves a
    /// niche) rather than moving the whole wall.
    #[test]
    fn sub_face_push_carves_a_sized_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall: axis Z, side Min; u=X(24), v=Y(16)
        world.adjust_selection_size(-20.0, 0.0); // sel_size_u: 24 → 4
        world.adjust_selection_size(0.0, -10.0); // sel_size_v: 16 → 6
        assert!(!world.is_full_face());

        let before = world.regions[0].brushes.len();
        assert!(world.push(4.0).is_some(), "sub-face push rebuilds the region");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "spawned one brush");

        let sub = world.regions[0].brushes.last().unwrap();
        assert_eq!(sub.op, Op::Subtract);
        assert_eq!(sub.w, 4.0, "width = sub-rect U");
        assert_eq!(sub.h, 6.0, "height = sub-rect V");
        assert_eq!(sub.d, 4.0, "depth = push step along the normal");
        // The original room brush is untouched (whole wall didn't move).
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 24.0, "room brush unchanged by a sub-face carve");
    }

    /// A full-face push (no scroll) still resizes the wall brush in place — the
    /// Phase 1 behavior, unregressed.
    #[test]
    fn full_face_push_still_moves_the_whole_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        assert!(world.is_full_face());
        let before = world.regions[0].brushes.len();
        world.push(4.0);
        assert_eq!(world.regions[0].brushes.len(), before, "no new brush");
        let room = world.regions[0].brushes.iter().find(|b| b.id == 1).unwrap();
        assert_eq!(room.d, 28.0, "whole −Z wall pushed out by the step");
    }

    /// Repeated sub-face pushes deepen the same carve rather than stacking brushes.
    #[test]
    fn repeat_sub_face_push_grows_the_same_brush() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair();
        world.adjust_selection_size(-20.0, 0.0);
        world.adjust_selection_size(0.0, -10.0);

        world.push(4.0); // spawn the sub-face carve
        let n1 = world.regions[0].brushes.len();
        let d1 = world.regions[0].brushes.last().unwrap().d;

        world.push(4.0); // deepen it
        let n2 = world.regions[0].brushes.len();
        let d2 = world.regions[0].brushes.last().unwrap().d;

        assert_eq!(n2, n1, "repeat push grows the same brush, no new one");
        assert!(d2 > d1, "the carve deepened: {d1} → {d2}");
    }

    /// Two rooms in one region, joined ONLY through a door-marked opening in the
    /// dividing wall. Room A: x∈[0,10); Room B: x∈[11,21); the wall at x∈[10,11)
    /// is solid except where the door carves a floor-level opening. The player
    /// (camera) is placed in Room B, aligned with the door.
    fn two_rooms_joined_by_a_door() -> World {
        let mut world = World::new();
        let region = &mut world.regions[0];
        region.brushes.clear();
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 16.0, 10.0));
        region
            .brushes
            .push(Brush::new(2, Op::Subtract, 11.0, 0.0, 0.0, 10.0, 16.0, 10.0));
        // Door through the dividing wall (x∈[10,11)), floor-level, z∈[3,6).
        let mut door = Brush::new(3, Op::Subtract, 10.0, 0.0, 3.0, 1.0, 7.0, 3.0);
        door.door = true;
        region.brushes.push(door);
        world.next_brush_id = 4;
        // Player camera in Room B (meters), aligned with the door opening in z.
        world.camera.pos = Vec3::new(4.0, 1.6, 1.125);
        world
    }

    /// The intact door panel blocks the player like a wall; removing it (the
    /// breach) makes the opening passable — collision reacts with no re-bake.
    #[test]
    fn intact_door_panel_blocks_the_player_until_breached() {
        let mut world = two_rooms_joined_by_a_door();
        world.initial_meshes();
        world.toggle_mode(); // spawn player in B, hunter in A, arm the door
        assert_eq!(world.doors.len(), 1, "one door armed");
        assert_eq!(world.physics.door_collider_count(), 1, "panel collider present");

        // Face −X (yaw π/2) and walk toward Room A through the door opening.
        world.character.as_mut().unwrap().yaw = std::f32::consts::FRAC_PI_2;
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        // Short window: the door stays intact (the hunter can't breach this fast).
        for _ in 0..180 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        // Door plane is x∈[2.5,2.75] m; capsule radius 0.25 m → blocked above ~3.0.
        assert!(
            feet.x > 2.9,
            "panel should block the player at the door, got x={}",
            feet.x
        );

        // Breach the panel directly (isolate collision from the AI): the opening
        // becomes passable and the player crosses into Room A.
        let panel = world.doors[0].panel;
        world.physics.remove_door_collider(panel);
        assert_eq!(world.physics.door_collider_count(), 0, "panel removed");
        for _ in 0..300 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        assert!(
            feet.x < 2.5,
            "player should cross the breached opening into Room A, got x={}",
            feet.x
        );
    }

    /// Phase 4 thesis: a hunter walled off from the player breaches the only door
    /// on its route, then reaches the player over the SAME baked grid. The breach
    /// flips a live nav flag + drops one collider — no re-voxelization (nothing in
    /// `fixed_step` re-bakes; `nav::bake` runs only at the BUILD→HUNT toggle).
    #[test]
    fn hunter_breaches_the_only_door_to_reach_a_walled_off_player() {
        let mut world = two_rooms_joined_by_a_door();
        world.initial_meshes();
        world.toggle_mode();
        assert!(world.enemy.is_some(), "hunter spawned");
        assert_eq!(world.nav.as_ref().unwrap().door_count(), 1);
        assert!(!world.nav.as_ref().unwrap().door_broken(0), "door starts intact");
        assert_eq!(world.physics.door_collider_count(), 1);

        let input = InputState::default(); // player stands still in Room B
        let mut caught = false;
        for _ in 0..2400 {
            // up to 20 s at 120 Hz (travel + 2.5 s breach + travel)
            world.fixed_step(1.0 / 120.0, &input);
            if world.is_caught() {
                caught = true;
                break;
            }
        }
        assert!(caught, "hunter should breach the door and catch the player");
        assert!(world.nav.as_ref().unwrap().door_broken(0), "nav flag flipped by breach");
        assert!(world.doors[0].broken, "world door marked broken");
        assert_eq!(world.physics.door_collider_count(), 0, "panel collider removed by breach");
    }

    // ─── Hole tool ─────────────────────────────────────────────────────────

    /// The hole tool arms with `H`, scroll sizes it, and a click cuts an opening
    /// that is NOT door-marked (holes aren't breakable). Distinct from the door.
    #[test]
    fn hole_tool_cuts_a_non_door_opening() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();
        assert!(!world.is_opening_arming());

        world.hole_tool_key(); // arm
        assert!(world.is_opening_arming() && world.is_hole_arming(), "hole tool armed");
        assert!(!world.is_door_arming(), "not the door tool");
        assert!(world.update_opening_preview().is_some(), "ghost previews on the wall");

        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "click cuts the hole");
        assert!(!world.is_opening_arming(), "cutting disarms");
        // Frame + protoroom subtracts added, and NO brush is door-marked.
        assert_eq!(world.regions[0].brushes.len(), before + 2, "frame + protoroom");
        assert!(
            !world.regions[0].brushes.iter().any(|b| b.door),
            "a hole is not breakable (no door-marked brush)"
        );
    }

    /// A hole can be cut into a floor (axis Y) — doors can't. Scroll grows the
    /// opening, and the cut carves the floor face.
    #[test]
    fn hole_can_be_cut_into_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look almost straight down at the floor
        world.hole_tool_key(); // arm hole
        world.adjust_opening_size(2.0, 2.0); // grow to 5×5
        let p = world.resolve_opening_placement().expect("floor is a valid hole face");
        assert_eq!(p.axis, Axis::Y, "the crosshair resolved the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_opening().is_some(), "cuts a floor hole");
        assert_eq!(world.regions[0].brushes.len(), before + 2);
    }

    /// The door tool still rejects the floor (walls only) — the generalization
    /// didn't loosen the door's constraint.
    #[test]
    fn door_tool_still_rejects_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4; // look down at the floor
        world.door_tool_key(); // arm door
        assert!(world.update_opening_preview().is_none(), "no door ghost on the floor");
        assert!(world.confirm_opening().is_none(), "no door cut into the floor");
        assert!(!world.regions[0].brushes.iter().any(|b| b.door));
    }

    // ─── Pillars & braces ───────────────────────────────────────────────────

    /// The pillar tool places one additive floor→ceiling column when aimed at the
    /// floor, and rejects a wall.
    #[test]
    fn pillar_places_a_column_on_the_floor() {
        let mut world = World::new();
        world.initial_meshes();

        // Aimed at the −Z wall (default view) → pillar rejects it.
        world.pillar_tool_key();
        assert!(world.is_placing());
        assert!(world.update_place_preview().is_none(), "no pillar ghost on a wall");
        assert!(world.confirm_place().is_none(), "no pillar placed on a wall");

        // Look down at the floor → a ghost appears and a click adds one Add brush.
        world.camera.pitch = -1.4;
        assert!(world.update_place_preview().is_some(), "pillar ghost on the floor");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "pillar placed");
        assert_eq!(world.regions[0].brushes.len(), before + 1, "one additive column");
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.op, Op::Add);
        assert_eq!(col.w, PILLAR_SIZE);
        assert_eq!(col.d, PILLAR_SIZE);
        assert!(!world.is_placing(), "placing disarms after a click");
    }

    /// Scroll resizes the pillar footprint before placement.
    #[test]
    fn scroll_resizes_the_pillar() {
        let mut world = World::new();
        world.initial_meshes();
        world.camera.pitch = -1.4;
        world.pillar_tool_key();
        world.adjust_place_size(2.0, 0.0); // 2 → 4
        world.confirm_place().unwrap();
        let col = world.regions[0].brushes.last().unwrap();
        assert_eq!(col.w, 4.0, "pillar grew to the scrolled size");
    }

    /// The brace tool places three additive brushes (arch) when aimed at a wall,
    /// and rejects the floor.
    #[test]
    fn brace_places_a_three_brush_arch_on_a_wall() {
        let mut world = World::new();
        world.initial_meshes();

        // Floor → brace rejects it.
        world.camera.pitch = -1.4;
        world.brace_tool_key();
        assert!(world.update_place_preview().is_none(), "no brace ghost on the floor");
        assert!(world.confirm_place().is_none(), "no brace on the floor");

        // −Z wall → three additive brushes.
        world.camera.pitch = 0.0;
        assert!(world.update_place_preview().is_some(), "brace ghost on the wall");
        let before = world.regions[0].brushes.len();
        assert!(world.confirm_place().is_some(), "brace placed");
        assert_eq!(world.regions[0].brushes.len(), before + 3, "wall + ceiling + wall");
        assert!(
            world.regions[0].brushes.iter().rev().take(3).all(|b| b.op == Op::Add),
            "all three brace brushes are additive"
        );
    }

    /// Arming a placement tool cancels an armed opening tool (mutually exclusive).
    #[test]
    fn tools_are_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.hole_tool_key();
        assert!(world.is_opening_arming());
        world.pillar_tool_key();
        assert!(world.is_placing(), "pillar armed");
        assert!(!world.is_opening_arming(), "arming the pillar cancelled the hole");
    }

    // ─── Stairs ──────────────────────────────────────────────────────────

    /// Stairs require the selection to touch the floor: a sub-face selection
    /// scrolled up off the floor rejects the arrow key (no pending op forms).
    #[test]
    fn stairs_require_the_selection_to_touch_the_floor() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        // Shrink V to a small band and slide it up off the floor via the preview
        // (which centers the rect on the crosshair). Aim high so it clears vMin.
        world.adjust_selection_size(0.0, -12.0); // sel_size_v: 16 → 4
        world.camera.pitch = 0.5; // look up so the centered rect sits above the floor
        world.update_selection_preview();
        assert!(!world.wall_selection_touches_floor(), "raised band is off the floor");
        assert!(!world.push_stairs(StairDir::Down), "off-floor selection rejects stairs");
        assert!(!world.has_pending_stair());
    }

    /// Arrow keys accumulate a pending step counter; the opposite arrow shrinks
    /// the same op, and confirming creates two void brushes + one descriptor with
    /// the tread mesh folded into the region (more triangles than before).
    #[test]
    fn confirm_stairs_creates_voids_treads_and_descriptor() {
        let mut world = World::new();
        let initial = world.initial_meshes();
        let tris_before = initial[0].mesh.indices.len();

        world.select_at_crosshair(); // full-face −Z wall, touches floor
        assert!(world.push_stairs(StairDir::Down), "first down grows to 1 step");
        world.push_stairs(StairDir::Down); // 2
        world.push_stairs(StairDir::Down); // 3
        world.push_stairs(StairDir::Up); // opposite shrinks → 2
        assert_eq!(world.pending_stair().unwrap().0, 2, "opposite arrow shrank the op");

        let brushes_before = world.regions[0].brushes.len();
        let rm = world.confirm_stairs().expect("confirm rebuilds the region");
        assert!(!world.has_pending_stair(), "confirm clears the pending op");
        assert_eq!(
            world.regions[0].brushes.len(),
            brushes_before + 2,
            "two void brushes (stairwell + corridor)"
        );
        assert_eq!(world.regions[0].stairs.len(), 1, "one stair descriptor");
        assert!(
            rm.mesh.indices.len() > tris_before,
            "tread geometry folded into the region mesh ({} → {})",
            tris_before,
            rm.mesh.indices.len()
        );
    }

    /// Reproduce the exact live-app ordering: click to select, then the per-frame
    /// selection preview runs (as it does every RedrawRequested), THEN the arrow
    /// keys + Enter. This guards against the preview loop clobbering the selection
    /// or pending-stair state (which the other tests don't exercise).
    #[test]
    fn preview_loop_between_select_and_confirm_does_not_break_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(world.select_at_crosshair(), "click selects the −Z wall");

        // Simulate several render frames: preview updates before the user acts.
        for _ in 0..5 {
            world.update_selection_preview();
        }
        assert!(
            world.push_stairs(StairDir::Down),
            "arrow-down must still form a pending op after the preview ran"
        );
        // More frames between key presses.
        world.update_selection_preview();
        world.push_stairs(StairDir::Down);
        world.update_selection_preview();

        assert_eq!(world.pending_stair().unwrap().0, 2, "two steps pending");
        assert!(world.confirm_stairs().is_some(), "Enter confirms after previews");
        assert_eq!(world.regions[0].stairs.len(), 1);
    }

    /// Down-stairs are walkable by the hunter: the nav bake sees the treads (via
    /// the solid-box extras) and finds a path from the room floor down into the
    /// lower corridor. Also proves standable cells exist below the original floor.
    #[test]
    fn down_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..4 {
            world.push_stairs(StairDir::Down);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with stairs");
        world.regions = regions;

        // A cell below the room floor exists (the descended corridor), and a path
        // runs from the room floor down to it.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y < -0.1),
            "some standable cell sits below the original floor (descended steps)"
        );
        let top = Vec3::new(3.0, 0.1, 3.0); // room floor
        let bottom = *stand
            .iter()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a lowest cell");
        let path = nav
            .find_path(top, bottom)
            .expect("a path should run from the room floor down the stairs");
        assert!(path.len() >= 2);
        assert!(path.last().unwrap().y < -0.1, "the route reaches the lower corridor");
    }

    /// Up-stairs are walkable by the hunter: treads rise above the floor and a
    /// path runs up onto the raised corridor.
    #[test]
    fn up_stairs_are_walkable_by_nav() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall
        for _ in 0..3 {
            world.push_stairs(StairDir::Up);
        }
        world.confirm_stairs();

        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[]).expect("bake with up-stairs");
        world.regions = regions;

        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 0.1),
            "some standable cell sits above the original floor (ascended steps)"
        );
        let bottom = Vec3::new(3.0, 0.1, 3.0);
        let top = *stand.iter().max_by(|a, b| a.y.total_cmp(&b.y)).expect("a highest cell");
        let path = nav
            .find_path(bottom, top)
            .expect("a path should run up the stairs to the raised corridor");
        assert!(path.last().unwrap().y > 0.1, "the route reaches the raised corridor");
    }

    /// Down-stairs are walkable by the player: entering HUNT and walking into the
    /// stairwell, the capsule descends the treads (feet drop below the floor) and
    /// is caught by them (never falls through to the void floor). This exercises
    /// the folded tread geometry as a Rapier trimesh collider.
    #[test]
    fn player_descends_the_stairs() {
        let mut world = World::new();
        world.initial_meshes();
        world.select_at_crosshair(); // −Z wall, full face
        for _ in 0..4 {
            world.push_stairs(StairDir::Down); // 4 steps down (−1 m at the bottom)
        }
        let rm = world.confirm_stairs().expect("confirm");
        // Sanity: the tread mesh made it into the region collider.
        assert!(!rm.mesh.indices.is_empty());

        world.toggle_mode(); // BUILD → HUNT; player spawns on the room floor
        assert_eq!(world.mode, Mode::Hunt);
        world.character.as_mut().unwrap().yaw = 0.0; // face −Z, toward the stairs
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            // 5 s at 120 Hz — walk into and down the stairwell
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        assert!(
            feet.y < -0.1,
            "player should walk down the treads (feet below the floor), got y={}",
            feet.y
        );
        // Void floor is at −4 WT = −1.0 m; treads must catch the capsule above it.
        assert!(
            feet.y > -1.05,
            "player should rest on a tread, not fall through to the void floor, got y={}",
            feet.y
        );
    }

    /// Walking straight into a wall is blocked — the capsule can't tunnel
    /// through the CSG collider.
    #[test]
    fn character_cannot_walk_through_a_wall() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode();
        // Face −Z (yaw 0) toward the z=0 wall; hold W, pointer locked.
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.press(winit::keyboard::KeyCode::KeyW);

        for _ in 0..600 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().unwrap();
        // Capsule radius is 0.25 m, so it should stop before z=0, never negative.
        assert!(feet.z > 0.1, "capsule tunneled through the wall: z={}", feet.z);
    }

    // ─── Free-standing platforms + stair-runs ───────────────────────────────

    /// The default room plus a raised platform (top at y=6 WT) and a stair-run
    /// descending from its −X edge down to the floor. Structures are built into
    /// the `STRUCT_ID` mesh + collider. The platform sits at x∈[10,14], z∈[8,12];
    /// the stair-run runs along −X from x=10 down to x=4 over z∈[8,12].
    fn room_with_platform_and_stair() -> World {
        let mut world = World::new(); // 24×16×24 cavity, floor at y=0
        world.initial_meshes();
        world.platforms.push(Platform {
            id: 1,
            x: 10.0,
            y: 6.0,
            z: 8.0,
            size_x: 4.0,
            size_z: 4.0,
            thickness: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_platform_id = 2;
        world.stair_runs.push(StairRun {
            id: 1,
            from_platform: Some(1),
            to_platform: None,
            anchor_from: Anchor::Edge {
                edge: structures::Edge::XMin,
                offset: 0.5,
            },
            anchor_to: Anchor::Ground {
                x: 4.0,
                y: 0.0,
                z: 10.0,
            },
            width: 4.0,
            step_height: 1.0,
            rise_over_run: 1.0,
            grounded: false,
            railings: false,
        });
        world.next_run_id = 2;
        world.rebuild_structures();
        world
    }

    /// A platform + connecting stair-run are walkable by the hunter's grid nav:
    /// the platform top and stair treads become standable, and A* finds a route
    /// from the room floor up onto the platform. Proves `structure_solid_boxes`
    /// reaches the voxelizer (the `collectExtraSolids`/platform-box port).
    #[test]
    fn platform_and_stair_are_walkable_by_nav() {
        let mut world = room_with_platform_and_stair();

        let solids = world.structure_solid_boxes();
        assert!(!solids.is_empty(), "platform + stair produced solid boxes");
        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &solids).expect("bake with structures");
        world.regions = regions;

        // The platform top (y=6 WT = 1.5 m) yields a standable cell up there.
        let stand = nav.all_standable();
        assert!(
            stand.iter().any(|c| c.y > 1.4),
            "a standable cell sits on the raised platform (top at 1.5 m)"
        );

        // A route runs from the room floor up the stairs onto the platform top.
        let floor = Vec3::new(0.75, 0.1, 2.5); // near the bottom of the stairs
        let top = *stand
            .iter()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .expect("a highest standable cell");
        let path = nav
            .find_path(floor, top)
            .expect("A* should route up the stair-run onto the platform");
        assert!(
            path.last().unwrap().y > 1.4,
            "the route climbs onto the platform, got {:?}",
            path.last()
        );
    }

    /// Structures always wear the "simple" (blue) scheme regardless of the room's
    /// scheme, and railings emit into the dedicated railing zone (→ the
    /// transparent railing texture) rather than being classified as walls.
    #[test]
    fn structures_wear_simple_scheme_with_railings_in_their_own_zone() {
        use engine::render::textures::{RAILING_ZONE, SIMPLE_SCHEME};
        let mut world = room_with_platform_and_stair();
        world.platforms[0].railings = true;
        world.stair_runs[0].railings = true;
        let rm = world.rebuild_structures();

        // Every structure group uses the simple scheme, never a room scheme.
        let schemes: std::collections::BTreeSet<u16> =
            rm.mesh.groups.iter().map(|g| g.scheme).collect();
        assert_eq!(
            schemes,
            std::iter::once(SIMPLE_SCHEME as u16).collect(),
            "structures use only the simple scheme, got {schemes:?}"
        );
        // Slab/treads classify to floor/wall zones…
        assert!(
            rm.mesh.groups.iter().any(|g| matches!(g.zone, 0 | 2 | 3)),
            "platform/stair surfaces present (floor/wall zones)"
        );
        // …and railings land in the dedicated railing zone.
        assert!(
            rm.mesh.groups.iter().any(|g| g.zone == RAILING_ZONE),
            "railings emit into the railing zone; groups = {:?}",
            rm.mesh.groups.iter().map(|g| g.zone).collect::<Vec<_>>()
        );
    }

    /// The player capsule rests on a platform's top surface (its trimesh collider
    /// holds it): spawning the player above the platform, gravity settles it onto
    /// the slab (y≈1.5 m), not through it to the floor.
    #[test]
    fn player_capsule_rests_on_a_platform() {
        let mut world = room_with_platform_and_stair();
        // Camera above the platform centre (x=12,z=10 WT → 3.0, 2.5 m).
        world.camera.pos = Vec3::new(3.0, 2.5, 2.5);
        world.toggle_mode(); // spawns the capsule via a downward ray onto the slab
        assert_eq!(world.mode, Mode::Hunt);

        let input = InputState::default(); // stand still
        for _ in 0..360 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let feet = world.player_pos().expect("player in HUNT");
        assert!(
            feet.y > 1.4,
            "capsule should rest on the platform top (~1.5 m), got y={}",
            feet.y
        );
    }

    /// The platform tool state machine: `T` arms it, aiming at a wall places a
    /// platform on click, and it becomes selected. A second placement, connect,
    /// grounded, and delete all round-trip through the public API.
    #[test]
    fn platform_tool_places_and_edits() {
        let mut world = World::new(); // camera looks −Z at the z=0 wall
        world.initial_meshes();

        assert!(!world.is_platform_tool());
        world.platform_tool_key();
        assert!(world.is_platform_tool() && world.is_platform_placing());

        // Click while aimed at the wall → a platform is placed and selected.
        assert!(
            world.platform_click().is_some(),
            "placing a platform rebuilds the structures mesh"
        );
        assert_eq!(world.platforms.len(), 1, "one platform placed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));

        // Toggle grounded on the selection.
        assert!(world.toggle_grounded_key().is_some());
        assert!(world.platforms[0].grounded, "F grounded the platform");

        // Delete it (and it returns to the idle placement phase).
        assert!(world.delete_selected().is_some());
        assert!(world.platforms.is_empty(), "platform deleted");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Idle));
    }

    /// Arming another modal tool (door) disarms the platform tool, and vice
    /// versa — the tools stay mutually exclusive.
    #[test]
    fn platform_tool_is_mutually_exclusive() {
        let mut world = World::new();
        world.initial_meshes();
        world.platform_tool_key();
        assert!(world.is_platform_tool());
        world.door_tool_key(); // arming the door disarms the platform tool
        assert!(!world.is_platform_tool(), "door tool disarmed the platform tool");
        assert!(world.is_opening_arming());
        world.platform_tool_key(); // arming the platform disarms the door
        assert!(!world.is_opening_arming(), "platform tool disarmed the door tool");
        assert!(world.is_platform_tool());
    }

    /// The two-step connect flow: `C` arms ConnectDst; locking a destination +
    /// source edge advances to ConnectSrc; a confirm builds one run and returns to
    /// Selected; and the Esc ladder walks ConnectSrc → ConnectDst → Selected.
    #[test]
    fn connect_two_step_locks_slides_and_builds() {
        let mut world = room_with_platform_and_stair(); // platform 1 at (10,6,8)
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);

        world.connect_key();
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst));

        // Lock a ground destination + the −X source edge (what connect_lock_target
        // does from a crosshair hit), then confirm. Camera looks level (pitch 0)
        // so the slide offset resolves to the edge midpoint (0.5).
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.connect_slide_wt = 2.0;
        world.platform_phase = Some(PlatformPhase::ConnectSrc);

        // The wheel slides the attach point in 1-WT steps, clamped to the edge
        // length (platform 1 is 4 WT deep, so the XMin edge is 4 WT long).
        assert!(world.is_connect_sliding());
        world.adjust_connect_slide(1.0);
        assert_eq!(world.connect_slide_wt, 3.0, "wheel slid +1 WT");
        world.adjust_connect_slide(10.0);
        assert_eq!(world.connect_slide_wt, 4.0, "clamped to the edge length");

        let before = world.stair_runs.len();
        assert!(world.connect_confirm().is_some(), "confirm builds + rebuilds");
        assert_eq!(world.stair_runs.len(), before + 1, "one run added");
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected));
        assert!(world.connect_to.is_none() && world.connect_edge.is_none());

        // Esc ladder from a fresh ConnectSrc.
        world.connect_key();
        world.connect_to = Some(ConnectTarget::Ground { x: 4.0, y: 0.0, z: 10.0 });
        world.connect_edge = Some(Edge::XMin);
        world.platform_phase = Some(PlatformPhase::ConnectSrc);
        assert!(world.platform_escape().0, "esc consumed");
        assert_eq!(world.platform_phase, Some(PlatformPhase::ConnectDst), "src → dst");
        assert!(world.platform_escape().0);
        assert_eq!(world.platform_phase, Some(PlatformPhase::Selected), "dst → selected");
    }

    /// The gizmo shows for a selected platform, a scale-handle drag grows the
    /// footprint, a move-arrow drag repositions it, and Esc cancels a drag
    /// (restoring the transform).
    #[test]
    fn gizmo_scales_moves_and_cancels() {
        let mut world = room_with_platform_and_stair();
        world.platform_phase = Some(PlatformPhase::Selected);
        world.selected_platform = Some(1);
        assert!(world.gizmo_mesh().is_some(), "gizmo shows for a selected platform");

        // Scale +X: a large rightward drag grows the footprint.
        let size_before = world.platforms[0].size_x;
        world.gizmo_start(GizmoHandle::ScaleXMax);
        assert!(world.is_gizmo_dragging());
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(
            world.platforms[0].size_x > size_before,
            "scale handle grew size_x: {} → {}",
            size_before,
            world.platforms[0].size_x
        );
        world.gizmo_drag = None; // a click would confirm the drag

        // Move +X: drag shifts the platform; Esc cancels and restores it.
        let x_before = world.platforms[0].x;
        world.gizmo_start(GizmoHandle::MoveX);
        world.gizmo_drag_delta(400.0, 0.0);
        assert!(world.platforms[0].x > x_before, "move arrow shifted +X");
        let (consumed, mesh) = world.platform_escape();
        assert!(consumed && mesh.is_some(), "Esc cancels the drag + rebuilds");
        assert_eq!(world.platforms[0].x, x_before, "cancel restored the position");
        assert!(!world.is_gizmo_dragging());
    }
