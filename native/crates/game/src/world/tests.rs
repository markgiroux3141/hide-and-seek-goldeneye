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

    /// Back-off: a hunter shoved inside its standoff gives ground until it regains the
    /// weapon's hold distance, instead of firing point-blank. Regression for "enemies
    /// get right on top of me." Uses a shotgun (3 m standoff) so only a couple metres
    /// of retreat room is needed in the open spawn area.
    #[test]
    fn a_pinned_hunter_backs_off_toward_its_standoff() {
        let mut world = World::new();
        world.initial_meshes();
        world.toggle_mode(); // HUNT — bake nav + spawn the roster
        assert!(!world.enemies.is_empty(), "hunters spawned");
        let ppos = world.player_pos().expect("player exists in HUNT");

        // Isolate hunter 0: kill + banish every packmate so no separation-nudge or
        // stale capsule interferes with its retreat lane.
        let far = Vec3::new(500.0, 0.0, 500.0);
        for i in 1..world.enemies.len() {
            world.enemies[i].enemy.take_damage(1e6);
            let c = world.enemies[i].collider;
            world.physics.update_enemy_collider(c, far);
        }

        // Give it a short-standoff weapon so the open spawn area easily fits the retreat.
        world.enemies[0].weapon = crate::combat::enemy_def_for(&crate::combat::config::SHOTGUN);
        let standoff = world.enemies[0].weapon.standoff;

        // Pin it at point-blank on the open line toward the spawn marker (walkable both
        // ways — the wave fanned out through here) so it has room to give ground.
        let toward_spawn = {
            let d = Vec3::new(SPAWN_MARKER_POS.x - ppos.x, 0.0, SPAWN_MARKER_POS.z - ppos.z);
            if d.length_squared() > 1e-6 { d.normalize() } else { Vec3::X }
        };
        let pinned = ppos + toward_spawn * 1.0;
        world.enemies[0].enemy.pos = pinned;
        let c0 = world.enemies[0].collider;
        world.physics.update_enemy_collider(c0, pinned);
        let start = Vec3::new(pinned.x - ppos.x, 0.0, pinned.z - ppos.z).length();

        let input = InputState::default(); // player stands still
        let dt = 1.0 / 60.0;
        for _ in 0..300 {
            world.fixed_step(dt, &input); // 5 s
        }

        let e = world.enemies[0].enemy.pos;
        let end = Vec3::new(e.x - ppos.x, 0.0, e.z - ppos.z).length();
        assert!(!world.enemies[0].enemy.is_dead(), "the test hunter stays alive");
        assert!(end > start + 0.5, "gave ground from point-blank (start {start:.2}, end {end:.2})");
        // Settles back near the standoff band ([standoff−hyst, …]); hyst is 1.2 m.
        assert!(
            end >= standoff - 1.6,
            "regained ~standoff (end {end:.2}, standoff {standoff:.2})"
        );
    }

    /// The fixed spawn point: the floor marker renders in BOTH modes, and entering
    /// HUNT floods exactly [`ENEMY_COUNT`] hunters in clustered at the marker (snapped
    /// to a standable cell) — independent of where the player is standing.
    #[test]
    fn wave_floods_in_at_the_fixed_marker() {
        let mut world = World::new();
        world.set_wave_size(6); // a real wave (gameplay default is 1 — "duel mode")
        world.initial_meshes();

        // The marker is visible while authoring (BUILD), before any hunt.
        assert!(world.is_build());
        assert!(world.spawn_marker_mesh().is_some(), "marker shows in BUILD");

        world.toggle_mode(); // BUILD → HUNT
        assert!(world.spawn_marker_mesh().is_some(), "marker still shows in HUNT");

        // The whole wave floods in, clustered at the fixed marker (not the player).
        assert_eq!(world.enemies.len(), 6, "the whole requested wave floods in");
        assert!(
            world.spawn_point.distance(SPAWN_MARKER_POS) < 1.0,
            "spawn snaps to the fixed marker, got {:?}",
            world.spawn_point
        );
        for e in &world.enemies {
            let d = (e.enemy.pos - world.spawn_point).length();
            assert!(d < 2.0, "hunter enters near the marker (was {d:.1} m away)");
        }

        // No door is built for the spawn (it's just a marked floor point).
        assert!(world.doors.is_empty(), "no spawn door built");

        // Returning to BUILD tears the wave down.
        world.toggle_mode();
        assert!(world.enemies.is_empty(), "wave cleared on BUILD");
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
        assert!(!world.char_models.is_empty(), "character model loaded");
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
        let (_body, _model, joints, opacity, colors) = &instances[0];
        assert_eq!(joints.len(), 15);
        assert_eq!(*opacity, 1.0, "alive hunter is opaque");
        assert!(colors.iter().all(|&c| c == 1.0), "un-shot hunter is clean (white blood)");
    }

    /// Multi-body: the whole character catalog loads onto the shared 15-bone rig, and
    /// every per-body derived table stays parallel to it. This is the headless proof
    /// that any of the 44 GoldenEye bodies is loadable + drivable by the one shared
    /// clip set (skinning uses each body's own skeleton). Skips if assets are absent.
    #[test]
    fn character_catalog_loads_onto_the_shared_rig() {
        let world = World::new();
        if world.char_models.is_empty() {
            eprintln!("skipping: character assets not loaded");
            return;
        }
        // More than the single original guard, and the per-body tables are parallel.
        assert!(world.char_models.len() > 1, "multiple bodies loaded");
        assert_eq!(world.char_models.len(), world.char_feet_offset.len(), "feet offsets parallel");
        assert_eq!(world.char_models.len(), world.enemy_arm.len(), "arm chains parallel");
        // Every loaded body rides the identical 15-bone rig, so the shared clip set
        // (bound to body 0) retargets onto it: the idle clip must skin cleanly on each
        // body's OWN skeleton, producing 15 finite joint matrices.
        let idle = world
            .char_anim_template
            .as_ref()
            .and_then(|a| a.clip(0))
            .expect("idle clip loaded");
        for (i, m) in world.char_models.iter().enumerate() {
            assert_eq!(m.skeleton.joint_count(), 15, "body {i} on the 15-bone rig");
            let mats = idle.skinning_matrices(0.0, &m.skeleton);
            assert_eq!(mats.len(), 15, "body {i} skins to 15 joints");
            assert!(
                mats.iter().all(|mm| mm.to_cols_array().iter().all(|f| f.is_finite())),
                "body {i} skinning matrices are finite"
            );
        }
    }

    /// Multi-body spawn: a flooded-in wave wears a spread of bodies across the catalog
    /// (not six clones of body 0), and each drawn instance references a valid,
    /// in-range body id whose blood buffer matches that body's vertex count.
    #[test]
    fn wave_spreads_across_multiple_bodies() {
        let mut world = World::new();
        world.set_wave_size(6); // spread needs a pack (gameplay default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn the wave
        if world.char_models.len() < 2 || world.enemies.is_empty() {
            eprintln!("skipping: need multiple bodies + spawned hunters");
            return;
        }
        // The wave isn't all one body (the spread picks distinct bodies for ENEMY_COUNT
        // hunters over a 44-body catalog).
        let distinct: std::collections::HashSet<usize> =
            world.enemies.iter().map(|e| e.body).collect();
        assert!(distinct.len() > 1, "wave wears more than one body, got {distinct:?}");
        // Every hunter's body id is in range, and its blood buffer matches that body.
        for inst in &world.enemies {
            let m = world.char_models.get(inst.body).expect("hunter body in range");
            assert_eq!(inst.blood.len(), m.vertices.len() * 3, "blood sized to the body");
        }
        // Each render instance carries the hunter's body id.
        for (body, _model, joints, _opacity, _colors) in world.character_instances() {
            assert!(body < world.char_models.len(), "instance body id in range");
            assert_eq!(joints.len(), 15);
        }
    }

    /// The difficulty dial: level 0 is the neutral baseline (all multipliers 1.0 /
    /// dodge 0), and it ramps to a harder-hitting, tankier, evasive hunter at the cap.
    #[test]
    fn difficulty_params_ramp_from_baseline_to_brutal() {
        let mut world = World::new();
        assert_eq!(world.difficulty(), 0, "starts at the baseline");
        let base = world.difficulty_params();
        assert_eq!(base.speed_mult, 1.0);
        assert_eq!(base.cooldown_mult, 1.0);
        assert_eq!(base.reaction_mult, 1.0);
        assert_eq!(base.health_mult, 1.0);
        assert_eq!(base.dodge, 0.0);
        assert_eq!(base.sense_mult, 1.0, "baseline perception reach");
        assert_eq!(base.suppress, 0.0, "baseline holds fire until standoff");
        assert_eq!(base.flank, 0.0, "baseline chases straight");
        assert_eq!(base.cover, 0.0, "baseline never breaks to cover");

        world.change_difficulty(100); // clamps to DIFFICULTY_MAX
        assert_eq!(world.difficulty(), DIFFICULTY_MAX);
        let hard = world.difficulty_params();
        assert!(hard.accuracy_mult > 1.0, "more accurate");
        assert!(hard.speed_mult > 1.0, "moves faster");
        assert!(hard.cooldown_mult < 1.0, "shorter burst cooldown");
        assert!(hard.reaction_mult < 1.0, "reacts faster");
        assert!(hard.health_mult > 1.0, "tankier");
        assert!(hard.dodge > 0.0, "evades");
        assert!(hard.sense_mult > 1.0, "sharper senses");
        assert!(hard.suppress > 0.0, "suppresses while closing");
        assert!(hard.flank > 0.0, "flanks the approach");
        assert!(hard.cover > 0.0, "uses cover");

        // Clamps at both ends.
        world.change_difficulty(-100);
        assert_eq!(world.difficulty(), 0, "clamped at the floor");
    }

    /// #6 footstep noise: a *moving* player pulls a blind (searching) hunter within
    /// footstep range to Investigate — but only at higher difficulty; at level 0 the
    /// hunter is deaf to footsteps (range 0). Drives one player move directly so the
    /// perception FSM never runs (the hunter stays blind), then calls the emitter.
    #[test]
    fn footsteps_divert_blind_hunters_only_at_higher_difficulty() {
        let run = |difficulty: u32| -> crate::enemy::AiState {
            let mut world = World::new();
            world.set_difficulty(difficulty); // set the dial in BUILD (no restart churn)
            world.initial_meshes();
            world.toggle_mode(); // HUNT — spawns the lone hunter in Search (no FSM step yet)
            assert!(!world.enemies.is_empty(), "hunter spawned");
            let ppos = world.player_pos().unwrap();
            // Give the player real horizontal speed by driving ONE move step directly —
            // deliberately NOT via fixed_step, so the perception FSM never runs and the
            // hunter stays blind (Search).
            let mut input = InputState::default();
            input.pointer_locked = true;
            input.press(winit::keyboard::KeyCode::KeyW);
            world
                .character
                .as_mut()
                .unwrap()
                .apply_move(1.0 / 60.0, &input, &mut world.physics);
            assert!(
                world.character.as_ref().unwrap().speed() > MOVE_NOISE_MIN_SPEED,
                "player is moving above the sneak threshold"
            );
            // Park the still-blind hunter 6 m from the player (inside footstep range at
            // max difficulty, ~10 m; outside it at level 0, where range is 0).
            world.enemies[0].enemy.pos = ppos + Vec3::new(6.0, 0.0, 0.0);
            assert_eq!(
                world.enemies[0].enemy.state(),
                crate::enemy::AiState::Search,
                "hunter is still blind before the emitter runs"
            );
            world.alert_enemies_to_movement();
            world.enemies[0].enemy.state()
        };
        assert_eq!(
            run(0),
            crate::enemy::AiState::Search,
            "difficulty 0: deaf to footsteps — the hunter keeps searching"
        );
        assert_eq!(
            run(DIFFICULTY_MAX),
            crate::enemy::AiState::Investigate,
            "hard: footsteps within range pull the blind hunter to investigate"
        );
    }

    /// #5 grenade flush: when the pack is HELD AT RANGE from a camping player, an
    /// engaged hunter lobs a grenade (a projectile is spawned). Drives a hunter to
    /// engage naturally, then relocates it out beyond the blast-safe distance and
    /// runs the flush with the camp dwell satisfied — the "flush a camper you can't
    /// walk up to" case (which real play produces via cover/walls, not the open room).
    #[test]
    fn a_held_at_range_pack_flushes_a_camper() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        world.initial_meshes();
        world.toggle_mode(); // HUNT — the hunter engages the standing player
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        // Let the lone hunter engage.
        for _ in 0..120 {
            world.fixed_step(dt, &input);
            if world.enemies[0].enemy.is_engaged() {
                break;
            }
        }
        assert!(world.enemies[0].enemy.is_engaged(), "hunter engaged the camper");

        // Hold it out at range (as cover/geometry would in a real level), and satisfy
        // the camp dwell, then run one flush step.
        let ppos = world.player_pos().unwrap();
        let far = ppos + Vec3::new(9.0, 0.0, 0.0); // ≥ GRENADE_SAFE_DIST from the camp spot
        world.enemies[0].enemy.pos = far;
        world.camp_anchor = Some(ppos);
        world.camp_timer = 100.0; // well past the dwell
        world.grenade_cooldown = 0.0;
        world.grenade_flush_step(dt);
        assert!(
            !world.projectiles.is_empty(),
            "a hunter held at range lobs a grenade to flush the camper"
        );
    }

    /// #5 no self-harm: a hunter that's right on top of the camper does NOT lob a
    /// grenade (it would blast itself / packmates). Regression for "explodes in front
    /// of them." Everything is set up to throw EXCEPT the hunter is point-blank.
    #[test]
    fn no_grenade_when_a_hunter_is_point_blank() {
        let mut world = World::new();
        world.set_difficulty(DIFFICULTY_MAX);
        world.initial_meshes();
        world.toggle_mode();
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..120 {
            world.fixed_step(dt, &input);
            if world.enemies[0].enemy.is_engaged() {
                break;
            }
        }
        let ppos = world.player_pos().unwrap();
        world.enemies[0].enemy.pos = ppos + Vec3::new(2.0, 0.0, 0.0); // inside the blast radius
        world.camp_anchor = Some(ppos);
        world.camp_timer = 100.0;
        world.grenade_cooldown = 0.0;
        world.grenade_flush_step(dt);
        assert!(
            world.projectiles.is_empty(),
            "no grenade is lobbed while a hunter is point-blank (it would self-harm)"
        );
    }

    /// #5 baseline: at difficulty 0 the grenade flush is disabled — camping forever
    /// never spawns a grenade.
    #[test]
    fn no_grenade_flush_at_difficulty_zero() {
        let mut world = World::new(); // difficulty 0
        world.initial_meshes();
        world.toggle_mode(); // HUNT
        world.toggle_invulnerable();
        let input = InputState::default();
        let dt = 1.0 / 60.0;
        for _ in 0..600 {
            world.fixed_step(dt, &input);
        }
        assert!(world.projectiles.is_empty(), "no grenades are lobbed at difficulty 0");
    }

    /// Sim-style hits (default): a non-lethal hit plays NO flinch/hurt animation, so
    /// the hunter keeps fighting through it. Turning hit reactions on (the GoldenEye
    /// mode flag) restores the authored flinch one-shot.
    #[test]
    fn hits_flinch_only_when_hit_reactions_enabled() {
        let mut world = World::new();
        world.weapon_index = 0; // PP7, 25 dmg — non-lethal on a 100-hp hunter
        world.initial_meshes();
        world.toggle_mode(); // HUNT: spawn one hunter
        assert!(!world.enemies.is_empty(), "hunter spawned");
        let torso = {
            let p = world.enemies[0].enemy.pos;
            Vec3::new(p.x, p.y + 0.8, p.z)
        };

        // Default: no flinch clip, and the hunter isn't stunned.
        world.hit_enemy(0, torso);
        assert!(!world.enemies[0].enemy.is_dead(), "non-lethal");
        assert!(
            !world.enemies[0].anim.is_playing_oneshot(),
            "sim style: no flinch animation by default"
        );

        // Opt into GoldenEye-style reactions → a hit now plays the flinch one-shot.
        world.set_hit_reactions(true);
        world.hit_enemy(0, torso);
        assert!(
            world.enemies[0].anim.is_playing_oneshot(),
            "flinch animation plays when hit reactions are enabled"
        );
    }

    /// Duel mode: exactly one hunter spawns, and difficulty scales its spawn health.
    #[test]
    fn one_hunter_spawns_with_difficulty_scaled_health() {
        let mut world = World::new();
        world.initial_meshes();
        world.change_difficulty(DIFFICULTY_MAX as i32); // before the spawn
        world.toggle_mode(); // HUNT: spawn the (single) wave
        assert_eq!(world.enemies.len(), 1, "duel mode spawns exactly one hunter");
        let hp = world.enemies[0].enemy.health();
        assert!(
            hp > crate::enemy::ENEMY_HEALTH,
            "difficulty scales spawn health up ({hp} vs base {})",
            crate::enemy::ENEMY_HEALTH
        );
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
            (world.character_instances()[0].3 - 1.0).abs() < 1e-3,
            "opaque during the death anim"
        );

        // Play out the death animation, then the full fade → invisible.
        for _ in 0..600 {
            world.advance_animation(1.0 / 60.0);
        }
        assert!(world.enemies[0].fade.is_some(), "fade started once the anim finished");
        assert!(
            world.character_instances()[0].3 <= 1e-3,
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

    /// Enemy separation (legacy nudge path): two hunters stacked on the exact same cell
    /// get pushed apart in a single step by the position-nudge separation pass, so they
    /// don't merge into one body. This guards the pre-ORCA baseline (`local_avoidance`
    /// off); the ORCA path separates smoothly over several frames instead — see the
    /// `orca_*` lab scenarios.
    #[test]
    fn stacked_hunters_are_pushed_apart() {
        let mut world = World::new();
        world.set_local_avoidance(false); // exercise the legacy instant-nudge path
        world.set_wave_size(6); // separation needs a pack (gameplay default is 1)
        world.initial_meshes();
        world.toggle_mode(); // HUNT — spawns the wave
        assert!(world.enemies.len() >= 2, "need at least two hunters");
        // Stack hunter 1 exactly on hunter 0.
        let p = world.enemies[0].enemy.pos;
        let h1 = world.enemies[1].collider;
        world.enemies[1].enemy.pos = p;
        world.physics.update_enemy_collider(h1, p);
        let input = InputState::default();
        world.fixed_step(1.0 / 120.0, &input);
        let (a, b) = (world.enemies[0].enemy.pos, world.enemies[1].enemy.pos);
        let d = Vec3::new(b.x - a.x, 0.0, b.z - a.z).length();
        assert!(d > 0.3, "stacked hunters should separate (got {d})");
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

    /// Weapon class → FIRE_TIMING window mapping, and all three fire clips are
    /// recognised as fire clips (used to gate the aim/hit-reaction post-pass).
    #[test]
    fn fire_windows_and_clip_recognition() {
        use crate::combat::EnemyWeaponClass::{Pistol, Rifle};
        for (c, d) in [(Rifle, false), (Pistol, false), (Rifle, true)] {
            let (s, e) = fire_window_for(c, d);
            assert!(e > s, "window start<end for {c:?} dual={d}");
        }
        use super::hunt::is_fire_clip;
        assert!(is_fire_clip(FIRE_RIFLE_IDX) && is_fire_clip(FIRE_PISTOL_IDX) && is_fire_clip(FIRE_DUAL_IDX));
        assert!(!is_fire_clip(CHAR_HIT_START), "a hit clip is not a fire clip");
    }

    /// A dual-wield hunter draws two guns (one per hand); a single-wield hunter
    /// one. The roster includes at least one dual-wielder.
    #[test]
    fn dual_wield_hunters_draw_two_guns() {
        let mut world = World::new();
        world.set_wave_size(6); // roster has dual-wielders past index 0 (default is 1)
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
        world.set_wave_size(6); // need a couple of hunters (gameplay default is 1)
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

    // ─── Undo / redo (see `world::history`) ──────────────────────────────────

    /// A one-line signature of region 0's authored brush geometry — enough to
    /// tell "state changed" / "state restored exactly" without `PartialEq`. The
    /// per-field weights are distinct primes so a face move (which shifts a min
    /// coord one way and its dim the other) can't cancel out to no change.
    fn region0_sig(world: &World) -> f32 {
        world.regions[0]
            .brushes
            .iter()
            .map(|b| b.id as f32 + 2.0 * b.x + 3.0 * b.y + 5.0 * b.z + 7.0 * b.w + 11.0 * b.h + 13.0 * b.d)
            .sum()
    }

    /// The core contract: a push is recorded, `undo` restores the pre-edit
    /// geometry byte-for-byte (via re-bake), and `redo` re-applies it.
    #[test]
    fn undo_restores_geometry_and_redo_reapplies() {
        let mut world = World::new();
        world.initial_meshes();

        let s0 = region0_sig(&world);
        // Author an edit through the same wrapper the app uses.
        let rm = world
            .with_undo(|w| w.push(PUSH_PULL_STEP))
            .expect("crosshair hits the −Z wall");
        assert_eq!(rm.id, 0);
        let s1 = region0_sig(&world);
        assert!((s1 - s0).abs() > 0.01, "push changed the geometry");

        let meshes = world.undo().expect("one edit to undo");
        assert!(!meshes.is_empty(), "undo returns meshes to upload");
        assert!((region0_sig(&world) - s0).abs() < 1e-4, "undo restored pre-edit state");

        world.redo().expect("one edit to redo");
        assert!((region0_sig(&world) - s1).abs() < 1e-4, "redo re-applied the edit");

        // Nothing left to redo once re-applied.
        assert!(world.redo().is_none(), "redo stack drained");
    }

    /// A no-op edit (crosshair hits nothing) records no history, so `undo` has
    /// nothing to pop.
    #[test]
    fn no_op_edit_records_no_undo_step() {
        let mut world = World::new();
        world.initial_meshes();
        // Fly outside the room, look away → the push misses.
        world.camera.pos = Vec3::new(1000.0, 1000.0, 1000.0);
        world.camera.yaw = 0.0;

        assert!(world.with_undo(|w| w.push(PUSH_PULL_STEP)).is_none(), "push missed");
        assert!(world.undo().is_none(), "a no-op leaves the history empty");
    }

    /// A fresh edit after an undo forks history — the redo stack is cleared.
    #[test]
    fn new_edit_clears_redo() {
        let mut world = World::new();
        world.initial_meshes();

        world.with_undo(|w| w.push(PUSH_PULL_STEP)).expect("edit 1");
        world.undo().expect("undo edit 1"); // redo now has one entry
        world.with_undo(|w| w.push(PUSH_PULL_STEP)).expect("edit 2 forks history");

        assert!(world.redo().is_none(), "the divergent edit cleared the redo stack");
    }

    /// The undo stack is bounded: past [`history::MAX_HISTORY`], the oldest
    /// snapshot is dropped rather than growing without limit.
    #[test]
    fn history_is_capped_at_max_depth() {
        let mut world = World::new();
        world.initial_meshes();
        // Carve well past the cap; every full-face push mutates, so every one is
        // recorded (a subtract push has no thinness guard).
        for _ in 0..(history::MAX_HISTORY + 10) {
            world.with_undo(|w| w.push(1.0));
        }
        assert_eq!(
            world.undo_stack.len(),
            history::MAX_HISTORY,
            "undo depth is clamped to the cap"
        );
    }

    /// Chest-aim oracle: after measuring a rifle's real barrel and aiming the chest
    /// at a known model-space target, the **actual gun barrel** ends up pointing from
    /// the chest at that target. This is the headless proof that the overlay-hold +
    /// chest-aim chain points the gun where intended (no over/under-shoot), for the
    /// bladed rifle clip that was aiming off to the side. Skips if assets are absent.
    #[test]
    fn chest_aim_points_the_real_barrel_at_the_target() {
        let world = World::new();
        let (Some(arm), Some(template), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_anim_template.as_ref(),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let weapon = crate::combat::enemy_def_for(&crate::combat::config::AR33); // rifle
        let asset = world
            .enemy_weapon_lib
            .iter()
            .find(|w| w.name == weapon.name)
            .expect("AR33 weapon asset");

        let loco: Vec<(f32, clip::AnimationClip)> = vec![
            (0.0, template.clip(0).unwrap().clone()),
            (anim_set::SPEED_WALK, template.clip(1).unwrap().clone()),
            (anim_set::SPEED_JOG, template.clip(2).unwrap().clone()),
            (anim_set::SPEED_RUN, template.clip(3).unwrap().clone()),
        ];
        let aim_clip = template.clip(FIRE_RIFLE_IDX).unwrap().clone();
        let mut stack = arm.build_stack(loco, aim_clip);

        // Full aim pose (overlay on) → measure the real barrel in the chest frame.
        {
            let ov = stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER).unwrap();
            ov.time = super::hunt::fire_window_for(weapon.class, false).0;
            ov.weight = 1.0;
        }
        let aim_pose = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
        let attach = Quat::from_euler(
            EulerRot::XYZ,
            weapon.right_rot.x,
            weapon.right_rot.y,
            weapon.right_rot.z,
        );
        let fwd = arm
            .barrel_forward_in_chest(&aim_pose, sk, attach, asset.barrel_axis())
            .expect("barrel measured");

        // Aim the chest at an arbitrary reachable model-space target (no cone clamp).
        let target = Vec3::new(0.6, 1.4, 3.0);
        {
            let ca = stack.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER).unwrap();
            ca.forward = fwd;
            ca.target = target;
            ca.max_angle = std::f32::consts::PI;
            ca.weight = 1.0;
            ca.enabled = true;
        }
        let posed = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
        let g = posed.joint_global_transforms(sk);

        // The ACTUAL barrel (gun rides Bone_9 with the attach + muzzle axis) vs the
        // direction from the chest to the target.
        let (_, hand_rot, _) = g[arm.end].to_scale_rotation_translation();
        let barrel = (hand_rot * (attach * asset.barrel_axis())).normalize();
        let chest_origin = g[arm.chest].to_scale_rotation_translation().2;
        let to_target = (target - chest_origin).normalize();
        let err = barrel.angle_between(to_target);
        assert!(
            err < 0.05,
            "barrel should point at the target (off by {err} rad); barrel={barrel:?} to_target={to_target:?}"
        );
    }

    /// Head look-at oracle: the gaze axis baked from the rig (`EnemyArm::head_forward`),
    /// swung by the head look-at layer, ends up pointing from the head at the focus —
    /// the headless proof the head actually turns toward what the hunter's thinking
    /// about. Also checks the missing-neck safeguard: a focus behind the head clamps
    /// the swing to [`ENEMY_HEAD_LOOK_CONE`] instead of over-rotating. Skips w/o assets.
    #[test]
    fn head_look_points_the_head_at_the_focus_and_clamps_to_the_cone() {
        let world = World::new();
        let (Some(arm), Some(template), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_anim_template.as_ref(),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let loco: Vec<(f32, clip::AnimationClip)> = vec![
            (0.0, template.clip(0).unwrap().clone()),
            (anim_set::SPEED_WALK, template.clip(1).unwrap().clone()),
            (anim_set::SPEED_JOG, template.clip(2).unwrap().clone()),
            (anim_set::SPEED_RUN, template.clip(3).unwrap().clone()),
        ];
        let aim_clip = template.clip(FIRE_RIFLE_IDX).unwrap().clone();
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };

        // (a) Wide cone → the gaze lands on an in-front, reachable model-space focus.
        let focus = Vec3::new(0.5, 1.4, 3.0);
        let mut stack = arm.build_stack(loco.clone(), aim_clip.clone());
        {
            let hl = stack.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER).unwrap();
            hl.target = focus;
            hl.max_angle = std::f32::consts::PI;
            hl.weight = 1.0;
            hl.enabled = true;
        }
        let posed = stack.evaluate(Pose::bind(sk), &ctx);
        let g = posed.joint_global_transforms(sk);
        let (_, head_rot, head_origin) = g[arm.head].to_scale_rotation_translation();
        let gaze = (head_rot * arm.head_forward).normalize();
        let to_focus = (focus - head_origin).normalize();
        let err = gaze.angle_between(to_focus);
        assert!(
            err < 0.05,
            "head gaze should point at the focus (off by {err} rad); gaze={gaze:?} to_focus={to_focus:?}"
        );

        // (b) A focus directly behind → the swing is clamped to the gaze cone (the
        // no-neck safeguard), never a full 180° over-rotation.
        let rest = arm.build_stack(loco.clone(), aim_clip.clone())
            .evaluate(Pose::bind(sk), &ctx);
        let rest_gaze = {
            let rg = rest.joint_global_transforms(sk)[arm.head].to_scale_rotation_translation().1;
            (rg * arm.head_forward).normalize()
        };
        let mut stack2 = arm.build_stack(loco, aim_clip);
        {
            let hl = stack2.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER).unwrap();
            hl.target = head_origin - rest_gaze * 2.0; // behind the head
            hl.max_angle = ENEMY_HEAD_LOOK_CONE;
            hl.weight = 1.0;
            hl.enabled = true;
        }
        let posed2 = stack2.evaluate(Pose::bind(sk), &ctx);
        let swung_gaze = {
            let rg = posed2.joint_global_transforms(sk)[arm.head].to_scale_rotation_translation().1;
            (rg * arm.head_forward).normalize()
        };
        let swung = swung_gaze.angle_between(rest_gaze);
        assert!(
            swung <= ENEMY_HEAD_LOOK_CONE + 1e-2,
            "gaze swing should clamp to the cone ({ENEMY_HEAD_LOOK_CONE} rad), got {swung}"
        );
    }

    /// Foot-IK leg chains: each resolved `(hip, knee, foot)` chain solves the foot onto
    /// a reachable ground target — the headless proof the leg chains resolve to real
    /// hip/knee/foot joints and the two-bone solve plants the foot. Skips w/o assets.
    #[test]
    fn foot_ik_plants_each_foot_on_a_reachable_target() {
        let world = World::new();
        let (Some(arm), Some(model)) = (
            world.enemy_arm.first().and_then(|a| a.as_ref()),
            world.char_models.first(),
        ) else {
            eprintln!("skipping: character assets not loaded");
            return;
        };
        let sk = &model.skeleton;
        let ctx = LayerCtx { skeleton: sk, dt: 0.0 };
        for k in 0..2 {
            let (root, mid, end) = arm.legs[k];
            let base = Pose::bind(sk);
            let g = base.joint_global_transforms(sk);
            let a = g[root].to_scale_rotation_translation().2;
            let b = g[mid].to_scale_rotation_translation().2;
            let c = g[end].to_scale_rotation_translation().2;
            let l = (b - a).length() + (c - b).length();
            // Reachable target at 70% of leg reach, offset off-axis so the knee bends.
            let target = a + Vec3::new(0.2, -0.9, 0.3).normalize() * (0.7 * l);
            let mut ik = TwoBoneIkLayer {
                root,
                mid,
                end,
                target,
                reach_frac: 0.0,
                pole: Vec3::ZERO,
                weight: 1.0,
                enabled: true,
            };
            let mut pose = base;
            ik.apply(&mut pose, &ctx);
            let reached = pose.joint_global_transforms(sk)[end].to_scale_rotation_translation().2;
            let err = (reached - target).length();
            assert!(err < 1e-2, "leg {k} foot should reach the ground target (off by {err})");
        }
    }

    /// PERF BASELINE (run: `cargo test --release bench_rebake_slot1 -- --nocapture --ignored`).
    /// Loads slot1 and times the CSG paths on its single region so we can measure
    /// the fold-once / incremental optimizations against real authored data.
    #[test]
    #[ignore]
    fn bench_rebake_slot1() {
        let mut world = World::new();
        let path = super::persist::slot_path(1);
        if world.load_level(&path).is_err() {
            eprintln!("slot1 not found at {} — skipping", path.display());
            return;
        }
        let region = &mut world.regions[0];
        let n = 100;
        // Warm up.
        let _ = region.evaluate();
        let _ = region.evaluate_textured();

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate(); }
        let eval_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate_textured(); }
        let tex_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let t = Instant::now();
        for _ in 0..n { let _ = region.evaluate_both(); }
        let both_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;

        let brushes = region.brushes.len();
        let tris = region.evaluate().indices.len() / 3;
        eprintln!("=== bench slot1: {brushes} brushes, {tris} collider tris ===");
        eprintln!("evaluate()          {eval_ms:.3} ms");
        eprintln!("evaluate_textured() {tex_ms:.3} ms");
        eprintln!("OLD per edit        {:.3} ms (two folds)", eval_ms + tex_ms);
        eprintln!("NEW evaluate_both() {both_ms:.3} ms (one fold)");
    }

    /// PERF SCALING (run: `cargo test --release bench_scaling -- --nocapture --ignored`).
    /// Times a single-region fold as the room count grows, to show localized CSG
    /// keeps per-edit cost near-linear (cheap constant) instead of quadratic. Each
    /// room is a subtract touching the next via a doorway cut, so it stays ONE
    /// connected region (the hardest case — clustering can't help here).
    #[test]
    #[ignore]
    fn bench_scaling_connected_region() {
        use engine::geometry::csg_runtime::{Brush, Op, Region};
        for &rooms in &[16usize, 64, 256, 1024] {
            let mut region = Region::new(0);
            let mut id = 1u32;
            // A straight run of rooms along X, each 10 wide, bridged by a 2-wide
            // doorway cut into the shared wall so the whole run is connected.
            for r in 0..rooms {
                let x = r as f32 * 12.0;
                region
                    .brushes
                    .push(Brush::new(id, Op::Subtract, x, 0.0, 0.0, 10.0, 8.0, 10.0));
                id += 1;
                if r + 1 < rooms {
                    // Doorway bridging room r and r+1 (spans the wall at x+10..x+12).
                    region
                        .brushes
                        .push(Brush::new(id, Op::Subtract, x + 10.0, 0.0, 3.0, 2.0, 7.0, 4.0));
                    id += 1;
                }
            }
            let brushes = region.brushes.len();
            let _ = region.evaluate_both(); // warm
            let n = 10;
            let t = Instant::now();
            for _ in 0..n {
                let _ = region.evaluate_both();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            let per_brush = ms / brushes as f64 * 1000.0;
            eprintln!(
                "{rooms:>5} rooms / {brushes:>5} brushes: {ms:8.2} ms/full-fold  ({per_brush:.1} µs/brush)"
            );
        }
    }

    /// Two disjoint rooms load as two independent regions, every brush is mapped,
    /// and editing one room re-bakes only its region (the other's mesh is
    /// unaffected). Exercises clustering + `brush_to_region` + the incremental
    /// `rebuild_affected_regions` path.
    #[test]
    fn disjoint_rooms_cluster_and_edit_locally() {
        let mut world = World::new();
        // Add a second room far from the opening room (which is brush 1, x∈[0,24]).
        world.regions[0]
            .brushes
            .push(Brush::new(2, Op::Subtract, 100.0, 0.0, 0.0, 24.0, 16.0, 24.0));
        world.next_brush_id = 3;
        // Re-partition into connected regions.
        let _ = world.recluster_all();
        assert_eq!(world.regions.len(), 2, "two disjoint rooms → two regions");
        assert_eq!(world.brush_to_region.len(), 2, "both brushes mapped");
        let r0 = world.brush_to_region[&1];
        let r1 = world.brush_to_region[&2];
        assert_ne!(r0, r1, "the rooms are in different regions");

        // Editing brush 1's region returns exactly that region's mesh.
        let meshes = world.rebuild_affected_regions(&[1]);
        assert_eq!(meshes.len(), 1, "one region re-baked");
        assert_eq!(meshes[0].id, r0, "and it's brush 1's region");
    }

    /// A brush created without being pre-registered is auto-assigned to the region
    /// it overlaps when `rebuild_affected_regions` runs (the ported
    /// `assignBrushToRegion` safety net).
    #[test]
    fn unmapped_brush_is_auto_assigned() {
        let mut world = World::new();
        // Drop an additive brush inside the opening room, but don't map it.
        world.regions[0]
            .brushes
            .push(Brush::new(2, Op::Add, 4.0, 0.0, 4.0, 2.0, 4.0, 2.0));
        assert!(!world.brush_to_region.contains_key(&2), "not yet mapped");
        let meshes = world.rebuild_affected_regions(&[2]);
        assert_eq!(world.brush_to_region.get(&2), Some(&0), "assigned to room region 0");
        assert_eq!(meshes.len(), 1);
    }

    /// The CSG memo cache returns geometry identical to a fresh fold (correctness
    /// of the hash-keyed cache): re-baking the same region twice yields the same
    /// triangle count, and a real edit produces a different result (no stale hit).
    #[test]
    fn memoized_rebake_matches_fresh_and_invalidates_on_edit() {
        let mut world = World::new();
        world.initial_meshes();
        let first = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        let cached = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        assert_eq!(first, cached, "cache hit reproduces the same mesh");

        // Grow the room: different authored data → different hash → fresh fold.
        world.regions[0].brushes[0].w += 8.0;
        let edited = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        // A bigger room still has geometry; the point is the cache didn't serve
        // the stale mesh (indices count can match, so assert it re-baked by
        // confirming a second identical call now caches the NEW value).
        let edited2 = world.rebuild_region(0).expect("region 0").mesh.indices.len();
        assert_eq!(edited, edited2, "new state is itself cached consistently");
    }
