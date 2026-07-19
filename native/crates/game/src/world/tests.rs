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

    /// A3 milestone: on HUNT the hunter spawns watching the player, runs its
    /// perception FSM (detects → alert → chase into range → attack), fires its
    /// rifle inside the animation's FIRE_TIMING window, and damages the stationary
    /// player. Drives a full render-frame loop (sim + animation + enemy combat).
    #[test]
    fn hunter_perceives_chases_and_shoots_the_player() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // bake nav + spawn the hunter roster watching the player
        assert!(!world.enemies.is_empty(), "hunters spawned");
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH, "player starts at full health");

        let input = InputState::default(); // player stands still, in the guards' view
        let dt = 1.0 / 60.0;
        let mut damaged = false;
        for _ in 0..600 {
            // up to 10 s of frames
            world.fixed_step(dt, &input);
            world.advance_animation(dt);
            world.enemy_combat_step(dt);
            if world.player_health() < PLAYER_MAX_HEALTH {
                damaged = true;
                break;
            }
        }
        assert!(damaged, "a hunter should perceive, close in, and shoot the player");
        // At least one hunter engaged (left idle) — the perception FSM ran.
        assert!(
            world
                .enemies
                .iter()
                .any(|e| e.enemy.state() != crate::enemy::AiState::Idle),
            "a hunter should have engaged, not all stayed idle"
        );
    }

    /// B5: in HUNT the animated model *is* each hunter — the placeholder box is
    /// gone and there is one skinned instance per hunter, each a real posed
    /// skinning set (opaque while alive).
    #[test]
    fn hunter_drives_the_animated_model_not_a_box() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn hunter roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        assert!(world.char_model.is_some(), "character model loaded");
        // The placeholder box is suppressed (the model is the hunter).
        assert!(world.enemy_mesh().is_none(), "box replaced by the model");

        // Step the hunters, then advance the animation driver.
        let input = InputState::default();
        for _ in 0..30 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        world.advance_animation(1.0 / 60.0);

        // One skinned instance per hunter; each a real 15-joint pose, opaque alive.
        let instances = world.character_instances();
        assert_eq!(instances.len(), world.enemies.len(), "one instance per hunter");
        let (_, joints, opacity, colors) = &instances[0];
        assert_eq!(joints.len(), 15);
        assert_eq!(*opacity, 1.0, "alive hunter is opaque");
        assert!(colors.iter().all(|&c| c == 1.0), "un-shot hunter is clean (white blood)");
    }

    /// Track A: four PP7 hits kill a hunter — it takes damage each shot, and the
    /// lethal shot drops the hitscan capsule (a corpse can't be shot) and puts the
    /// model into its death state. The fade then drives the character's opacity
    /// 1 → 0 once the death animation finishes.
    #[test]
    fn four_shots_kill_the_hunter_then_it_fades_out() {
        let mut world = World::new();
        world.weapon_index = 0; // pin PP7 (25 dmg hitscan); the default start weapon is dev-set elsewhere
        world.initial_meshes();
        world.toggle_mode(); // HUNT: bake nav + spawn hunter roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        let h = world.enemies[0].collider;
        assert!(world.physics.is_enemy_collider(h), "hunter has a hitscan capsule");

        // Torso-height impacts (×1 damage), so PP7's 25 dmg lands cleanly.
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        // Three non-lethal hits on hunter 0 (PP7, 25 dmg, 100 hp → 75/50/25).
        for expect in [75.0, 50.0, 25.0] {
            world.hit_enemy(0, torso);
            let e = &world.enemies[0];
            assert!(!e.enemy.is_dead(), "still alive at {expect} hp");
            assert_eq!(e.enemy.health(), expect);
            assert!(e.fade.is_none(), "no death fade while alive");
        }

        // The fourth (lethal) hit.
        world.hit_enemy(0, torso);
        assert!(world.enemies[0].enemy.is_dead(), "dead after 4 PP7 shots");
        assert!(
            !world.physics.is_enemy_collider(h),
            "the corpse's capsule is removed — can't shoot a corpse"
        );
        // The fade does NOT start until the death animation finishes: the body
        // stays fully opaque while the death clip plays.
        assert!(world.enemies[0].fade.is_none(), "fade not armed at the moment of death");
        assert!(
            (world.character_instances()[0].2 - 1.0).abs() < 1e-3,
            "opaque during the death anim"
        );

        // Play out the death animation, then the full fade → invisible.
        for _ in 0..600 {
            world.advance_animation(1.0 / 60.0);
        }
        assert!(world.enemies[0].fade.is_some(), "fade started once the anim finished");
        assert!(
            world.character_instances()[0].2 <= 1e-3,
            "faded to invisible after the animation"
        );
    }

    /// Track A: a shot that lands on the hunter's capsule damages it and spawns NO
    /// wall spark; a shot that misses the hunter and hits a wall spawns a spark and
    /// deals no damage. Exercises the real fire path (trigger → cast → branch).
    #[test]
    fn shooting_the_hunter_damages_it_a_wall_hit_sparks() {
        let mut world = World::new();
        world.weapon_index = 0; // pin PP7 (hitscan) — the default start weapon is dev-set to the launcher
        world.initial_meshes();
        world.toggle_mode(); // HUNT

        // Move every other hunter far off so only hunter 0 can be on the ray.
        for i in 1..world.enemies.len() {
            let h = world.enemies[i].collider;
            let far = Vec3::new(500.0 + i as f32 * 5.0, 0.0, 500.0);
            world.enemies[i].enemy.pos = far;
            world.physics.update_enemy_collider(h, far);
        }

        // Put hunter 0 directly on the player's look ray ~1.5 m ahead (inside the
        // 6 m room, before any wall), with its capsule centred on the ray.
        let (eye, fwd) = {
            let c = world.character.as_ref().unwrap();
            (c.eye(), c.forward())
        };
        let centre = eye + fwd * 1.5;
        let feet = centre - Vec3::new(0.0, ENEMY_HALF_HEIGHT + ENEMY_RADIUS, 0.0);
        let h0 = world.enemies[0].collider;
        world.enemies[0].enemy.pos = feet;
        world.physics.update_enemy_collider(h0, feet);
        let hp0 = world.enemies[0].enemy.health();

        // Fire once (a fresh edge = one semi-auto shot).
        let mut input = InputState::default();
        input.pointer_locked = true;
        input.set_mouse_left(true);
        world.combat_step(1.0 / 60.0, &input);
        assert!(
            world.enemies[0].enemy.health() < hp0,
            "shooting the hunter damages it"
        );
        assert!(world.sparks.is_empty(), "an enemy hit spawns no wall spark");

        // Move hunter 0 far off the ray too, then fire again (release → fresh pull)
        // so the shot flies past into a wall → a spark, no further damage.
        let hp1 = world.enemies[0].enemy.health();
        let away = Vec3::new(100.0, 0.0, 100.0);
        world.enemies[0].enemy.pos = away;
        world.physics.update_enemy_collider(h0, away);
        input.set_mouse_left(false);
        world.combat_step(1.0 / 60.0, &input); // release resets the edge
        input.set_mouse_left(true);
        world.combat_step(1.0 / 60.0, &input); // fresh pull → shot into the wall
        assert!(!world.sparks.is_empty(), "a wall hit spawns a spark");
        assert_eq!(
            world.enemies[0].enemy.health(),
            hp1,
            "the wall shot dealt no damage to the (moved-away) hunter"
        );
    }

    /// Track A: a killed hunter stops moving — its death freezes the nav-driven
    /// chase (dead `update` is a no-op), so the corpse holds position.
    #[test]
    fn a_dead_hunter_stops_chasing() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        // Kill hunter 0 outright (torso hits, ×1 damage).
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };
        for _ in 0..4 {
            world.hit_enemy(0, torso);
        }
        assert!(world.enemies[0].enemy.is_dead());
        let rest = world.enemies[0].enemy.pos;
        let input = InputState::default();
        for _ in 0..240 {
            world.fixed_step(1.0 / 120.0, &input);
        }
        let after = world.enemies[0].enemy.pos;
        assert!(
            (after - rest).length() < 1e-4,
            "the corpse should not move (was {rest:?}, now {after:?})"
        );
    }

    /// P5: player damage subtracts from health (armor-first, but armor 0 here),
    /// arms the red flash + HUD pop, and kills at 0 → the death state.
    #[test]
    fn player_damage_subtracts_health_and_dies() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT (player alive, full health)
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH);

        world.take_player_damage(8.0);
        assert_eq!(world.player_health(), 92.0, "8 dmg off 100");
        assert!(world.damage_flash() > 0.0, "damage armed the red flash");
        assert!(world.hud_alpha() > 0.0, "damage popped the health HUD");
        assert!(!world.is_player_dead());

        world.take_player_damage(1000.0); // lethal
        assert_eq!(world.player_health(), 0.0, "health floors at 0");
        assert!(world.is_player_dead(), "0 health → dead");

        // Restart resets health + returns to BUILD.
        world.restart_after_death();
        assert!(!world.is_player_dead());
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH);
        assert!(world.is_build(), "restart drops back to BUILD");
    }

    /// A3: in HUNT the hunters carry weapons — each gun's world clip transform
    /// resolves (a hand bone is found + the pose is posed); a dead hunter drops its
    /// gun, so once every hunter is down there are no weapon draws.
    #[test]
    fn hunters_carry_weapons_in_hunt() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        assert!(!world.enemy_weapon_lib().is_empty(), "weapon assets loaded");
        assert!(
            !world.enemy_weapon_draws(1.6).is_empty(),
            "live hunters' guns have world transforms"
        );
        // Kill every hunter (8 PP7 hits each is plenty).
        let n = world.enemies.len();
        for i in 0..n {
            let torso = {
                let p = world.enemies[i].enemy.pos;
                Vec3::new(p.x, p.y + 0.8, p.z)
            };
            for _ in 0..8 {
                world.hit_enemy(i, torso);
            }
        }
        assert!(
            world.enemy_weapon_draws(1.6).is_empty(),
            "dead hunters drop their guns"
        );
    }

    /// Weapon class → fire clip + window mapping: pistol/rifle pick distinct clips,
    /// and the dual flag overrides the class to the dual clip. Each maps to a real
    /// FIRE_TIMING window, and all three fire clips are recognised as fire clips.
    #[test]
    fn fire_clip_selection_by_class_and_dual() {
        use crate::combat::EnemyWeaponClass::{Pistol, Rifle};
        assert_eq!(fire_clip_index(Rifle, false), FIRE_RIFLE_IDX);
        assert_eq!(fire_clip_index(Pistol, false), FIRE_PISTOL_IDX);
        assert_eq!(fire_clip_index(Pistol, true), FIRE_DUAL_IDX, "dual overrides class");
        assert_eq!(fire_clip_index(Rifle, true), FIRE_DUAL_IDX);
        for (c, d) in [(Rifle, false), (Pistol, false), (Rifle, true)] {
            let (s, e) = fire_window_for(c, d);
            assert!(e > s, "window start<end for {c:?} dual={d}");
        }
        assert!(is_fire_clip(FIRE_RIFLE_IDX) && is_fire_clip(FIRE_DUAL_IDX));
        assert!(!is_fire_clip(CHAR_HIT_START), "a hit clip is not a fire clip");
    }

    /// A dual-wield hunter draws two guns (one per hand); a single-wield hunter
    /// one. The roster includes at least one dual-wielder.
    #[test]
    fn dual_wield_hunters_draw_two_guns() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the roster
        let expected: usize = world
            .enemies
            .iter()
            .filter(|e| !e.enemy.is_dead())
            .map(|e| 1 + e.dual as usize)
            .sum();
        assert_eq!(
            world.enemy_weapon_draws(1.6).len(),
            expected,
            "one gun per hunter, two for a dual-wielder"
        );
        assert!(world.enemies.iter().any(|e| e.dual), "roster includes a dual-wielder");
    }

    /// Hit zones scale damage by impact height: a head-height shot does ×4 (PP7
    /// 25×4 = 100 → a one-shot kill), a leg-height shot does ×0.6 (15 dmg), and the
    /// hit paints persistent blood on the body.
    #[test]
    fn hit_zones_scale_damage_by_impact_height() {
        let mut world = World::new();
        world.weapon_index = 0; // pin PP7 (25 dmg hitscan) — zone multipliers are relative to it
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the roster
        assert!(world.enemies.len() >= 2, "roster spawned at least two hunters");

        // Head-height impact on hunter 0 → ×4 → lethal in one PP7 shot.
        let head = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 1.2, p.z)
        };
        world.hit_enemy(0, head);
        assert!(world.enemies[0].enemy.is_dead(), "a headshot one-shots with the PP7");

        // Leg-height impact on hunter 1 → ×0.6 → 15 dmg, and it paints blood.
        let leg = {
            let p = world.enemies[1].enemy.pos;
            Vec3::new(p.x, p.y + 0.3, p.z)
        };
        world.hit_enemy(1, leg);
        assert_eq!(world.enemies[1].enemy.health(), 100.0 - 15.0, "a leg shot does 0.6×");
        // Painting reddens vertices near the impact → some g/b channels drop below 1.
        assert!(
            world.enemies[1].blood.iter().any(|&c| c < 0.999),
            "the hit painted blood onto the body"
        );
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

    // NB: the door-breach tests (panel-blocks-player, hunter-breaches-to-catch)
    // and their `two_rooms_joined_by_a_door` fixture were removed when door
    // breach/blocking was disabled (2026-07-16, see `World::build_doors`). Restore
    // them from git history when the breach system is re-enabled.

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
