//! The respawn loop — what turns a one-shot hunt into a deathmatch.
//!
//! Both sides come back [`RESPAWN_DELAY`] after dying, from the same shared spawn pool
//! (`world::spawn`). Perfect Dark's shape: `bot_spawn(chr, respawning = true)`
//! (`bot.c:262`) puts a dead simulant straight back through
//! `scenario_choose_spawn_location` with a 2-second fade-in, and in normal multiplayer
//! the player's `dostartnewlife` fires automatically once the death fade completes
//! (`player.c:4596`) rather than waiting on a button.
//!
//! ## A hunter respawns *into its own roster slot*
//!
//! This is the one thing in here that could go quietly wrong. An `EnemyInstance`'s index
//! is not a private detail of the roster — `pd_lab::PdTarget::Hunter(i)`,
//! `EnemyInstance::pd_target`, the ORCA agent tags, squad alert and every AI-lab metric
//! key off it. Pushing a fresh instance and dropping the corpse would renumber every
//! hunter after it mid-fight, silently repointing all of those. So
//! [`World::respawn_hunter`] **resets the instance in place**: a fresh [`Enemy`], a fresh
//! collider, the transient combat/animation state cleared, and everything expensive that
//! was resolved at spawn — the body, its clip template, its layered animator, the
//! measured per-clip barrel axes, the weapon — left exactly as it was. Which is also why
//! this is a reset and not a re-run of `spawn_wave`'s builder: rebuilding would re-measure
//! the barrel axes (a handful of pose evaluations) on every death, for a body whose rig
//! has not changed.
//!
//! Perfect Dark resets rather than rebuilds for the same reason, and the field-by-field
//! list below lines up with `bot_reset` + `splat_reset_chr` (`bot.c:286-287`) — the blood
//! wipe is `splat_reset_chr`.

use super::*;

impl World {
    /// Tick both sides' respawn clocks one fixed step and bring back whoever is due.
    /// Called from [`World::fixed_step`] — including while the player is dead, which is
    /// what lets the player's own clock run behind the death screen.
    pub(crate) fn respawn_step(&mut self, dt: f32) {
        // Nothing respawns once a side has won; the round-over screen is terminal until
        // `R` starts the next one.
        if self.round_over.is_some() {
            return;
        }
        if self.player_dead {
            self.player_respawn = (self.player_respawn - dt).max(0.0);
            if self.player_respawn == 0.0 {
                self.respawn_player();
            }
        }
        for i in 0..self.enemies.len() {
            let due = match self.enemies[i].respawn_timer.as_mut() {
                Some(t) => {
                    *t = (*t - dt).max(0.0);
                    *t == 0.0
                }
                None => false,
            };
            if due {
                self.respawn_hunter(i);
            }
        }
    }

    /// Arm hunter `idx`'s respawn clock. Called from `start_death`, the single funnel
    /// every hunter death goes through (bullet and blast alike).
    pub(crate) fn arm_hunter_respawn(&mut self, idx: usize) {
        // One life each (`PlayConfig::respawn` off) → the corpse stays a corpse, and the
        // round is over once the last one falls. Without that check a one-life round
        // would never end.
        if !self.respawn_enabled() {
            self.check_wipeout();
            return;
        }
        let delay = self.respawn_delay();
        if let Some(inst) = self.enemies.get_mut(idx) {
            inst.respawn_timer = Some(delay);
        }
    }

    /// Bring hunter `idx` back **in its own slot**, at a pad drawn from the shared pool.
    ///
    /// Resets the [`Enemy`] (position, health at the *current* difficulty, clean AI
    /// state), re-adds the hitscan capsule that death removed, and clears every transient
    /// combat/animation/corpse field. Keeps the body, clip template, layered animator,
    /// measured barrel axes and weapon — see the module docs for why.
    fn respawn_hunter(&mut self, idx: usize) {
        // Where it comes back. `Spawning::Hunter(idx)` keeps its own corpse position out
        // of the occupant list, so a hunter isn't pushed away from a good pad by where it
        // happened to die.
        let pad = self.choose_spawn_pad(Spawning::Hunter(idx)).map(|(_, p)| p);
        let spawn = pad.map(|p| p.pos).unwrap_or(self.spawn_point);
        // …and don't come back standing inside whoever is there now.
        let spawn = self.spawn_clear_of_bodies(Spawning::Hunter(idx), spawn);
        // Face the player, as the wave does at G — if it's out of sight the search FSM
        // takes over immediately, and if it's in view engaging is the right response.
        let watch = self.player_pos().unwrap_or(spawn);
        // Respawn health tracks the LIVE difficulty dial, matching `restart_hunt`: turn
        // the dial mid-round and the next body in reflects it.
        let spawn_hp = crate::enemy::ENEMY_HEALTH * self.difficulty_params().health_mult;
        let (radius, half_height) = self.body_capsule(
            self.enemies.get(idx).map(|i| i.body).unwrap_or(0),
        );
        let collider = self.physics.add_enemy_collider(spawn, radius, half_height);
        // A fresh simulant at the current tier — PD's `bot_reset(chr, respawning)`. The
        // seed stays keyed to the slot so this hunter keeps its own aim personality
        // across its deaths rather than becoming a different bot each life.
        let sim = self
            .enemies
            .get(idx)
            .and_then(|i| i.pdsim.as_ref())
            .map(|_| {
                crate::pdsim::Simulant::new(
                    self.pd.difficulty.unwrap_or_else(|| {
                        pd_lab::tier_for_dial_frac(self.difficulty_frac())
                    }),
                    self.pd.bot_type,
                    0xA5A5_0000_u64 ^ (idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                )
            });

        // Any ragdoll / living-hit reaction bodies must come out of the physics sim
        // before the slot is reused, or they leak (the structs don't clean up on Drop).
        if let Some(rag) = self.enemies[idx].ragdoll.take() {
            rag.remove(&mut self.physics);
        }
        if let Some(r) = self.enemies[idx].reaction.take() {
            r.rag.remove(&mut self.physics);
        }

        let flank = if idx % 2 == 0 { 1.0 } else { -1.0 };
        // Read before the `&mut` borrow of the instance below.
        let unarmed_hunters = self.hunters_start_unarmed();
        let inst = &mut self.enemies[idx];
        inst.enemy = {
            let mut e = Enemy::new(spawn, watch);
            e.set_max_health(spawn_hp);
            e.set_flank_side(flank);
            e
        };
        inst.collider = collider;
        inst.respawn_timer = None;
        // Corpse state.
        inst.fade = None;
        inst.ragdoll_time = 0.0;
        // Weapon state — a fresh magazine, no burst in flight (`aibot->loadedammo`,
        // `timeuntilreload60`, `burstsdone` all cleared by `bot_reset`).
        //
        // Under the deathmatch rule a hunter comes back **empty-handed**, exactly as
        // the player does: dying costs it the gun it found. Which also means the gun
        // it was carrying is not kept across the respawn — it goes back to looking.
        if unarmed_hunters {
            inst.weapon = crate::combat::enemy_def_for(&crate::combat::config::UNARMED);
            inst.dual = false;
        }
        inst.loaded = inst.weapon.clip;
        inst.reserve = if inst.weapon.is_unarmed() {
            0
        } else {
            inst.weapon.clip * crate::world::tools::pickup::HUNTER_SPAWN_MAGS
        };
        inst.reload_timer = 0.0;
        inst.shot_timer = 0.0;
        inst.burst_shot = 0;
        inst.use_secondary = false;
        inst.fire_elapsed = None;
        inst.muzzle_timer = 0.0;
        // Hit bookkeeping + the blood wipe (`splat_reset_chr`, `bot.c:287`): a respawned
        // body is clean, not still wearing the stains that killed it.
        inst.hit_part = None;
        inst.thud = None;
        inst.thud_played = [false; 2];
        for c in &mut inst.blood {
            *c = 1.0;
        }
        // Animation: drop the clamped death one-shot back to the looping idle clip, so
        // `advance_animation` hands the body back to the procedural layer stack instead of
        // holding it dead on the floor. `play` also clears `return_to` / the fire window.
        inst.anim.play(0, 0.0);
        inst.render_yaw = None;
        inst.final_pose = None;
        inst.aim_weight = 0.0;
        inst.head_look_weight = 0.0;
        inst.head_look_point = None;
        inst.foot_delta = [0.0, 0.0];
        inst.anim_speed = 0.0;
        // PD layer state.
        inst.pdsim = sim;
        inst.pd_debug = None;
        inst.pd_target = None;

        log::info!("hunter {idx} respawned at {spawn:?} (slot reused)");
    }

    /// Kill the player: latch the death state, credit it, and start the respawn clock.
    /// Called from `take_player_damage` at 0 health.
    pub(crate) fn kill_player(&mut self, killer: Killer) {
        if self.player_dead {
            return;
        }
        self.player_dead = true;
        self.record_player_death(killer);
        // One life each → no clock at all, and the hunters have won.
        if !self.respawn_enabled() {
            self.player_respawn = 0.0;
            self.check_wipeout();
            log::info!("YOU DIED — one life each, so that is the round");
            return;
        }
        let delay = self.respawn_delay();
        self.player_respawn = delay;
        log::info!("YOU DIED — respawning in {delay:.0}s");
    }

    /// Bring the player back from the pool: full health, a fresh capsule pose at a chosen
    /// pad (facing the way that pad was aimed), combat feedback cleared. Keeps the hunt
    /// running — this is a respawn, not a return to BUILD.
    ///
    /// With no pads authored there is nothing to draw from, so the player returns to the
    /// pose it entered at (`hunt_spawn`), which is the fly-cam drop. That keeps the
    /// no-pads level playable instead of leaving a dead player with nowhere to go.
    pub(crate) fn respawn_player(&mut self) {
        // Back to the authored *starting* condition, not to a hard-coded full bar: a
        // level tuned as a 50-health handicap run stays one after a death.
        self.player_health = self.play_config().health;
        self.player_armor = self.play_config().armor;
        self.player_dead = false;
        self.player_respawn = 0.0;
        self.damage_flash = 0.0;
        self.hud_show_timer = 0.0;
        self.caught = false;
        // Dying costs you what you were carrying — guns, magazines and reserves —
        // and puts you back to empty hands. This is what makes the weapons on the
        // floor worth crossing the level for (`DESIGN_PICKUPS.md`).
        self.reset_loadout();
        // …and then you get your *starting* guns back, whatever the level authored them
        // to be. On a `LoadoutMode::Level` level that is the fallback sidearm (or
        // nothing, when there are pickups to find), so the pickup economy is untouched;
        // on an authored loadout it is the loadout, which is the only reading of "start
        // with a PP7" that survives your first death.
        self.apply_start_loadout();
        // The pad, then the step-aside — the level is full of bodies by now, which is the
        // whole reason a respawn needs this and the initial entry does not. Honours the
        // authored entry mode, so a camera-entry level puts you back where you dropped in.
        let entry = match self
            .entry_uses_pads()
            .then(|| self.choose_spawn_pad(Spawning::Player))
            .flatten()
        {
            Some((_, pad)) => {
                Some((self.spawn_clear_of_bodies(Spawning::Player, pad.pos), pad.yaw, 0.0))
            }
            None => self.hunt_spawn,
        };
        if let Some((feet, yaw, pitch)) = entry {
            self.character = Some(CharacterController::new(feet, yaw, pitch));
            let ladders = self.ladder_volumes();
            if let Some(c) = self.character.as_mut() {
                c.set_ladders(ladders);
            }
            log::info!("respawned at {feet:?}");
        }
    }

    /// Start a fresh round without leaving HUNT (the `R` key on the round-over screen):
    /// 0–0, everyone back in from the pool, transient combat VFX dropped. The authored
    /// level and the baked nav are untouched, so it is cheap.
    pub fn restart_round(&mut self) {
        if self.mode != Mode::Hunt {
            return;
        }
        self.reset_scores();
        self.sparks.clear();
        self.projectiles.clear();
        self.mines.clear();
        self.blasts.clear();
        self.camp_anchor = None;
        self.camp_timer = 0.0;
        self.grenade_cooldown = 0.0;
        // Restock the level. A new round is a clean slate, so the guns and ammo taken
        // during the last one are back on the floor — without this the second round of
        // a session would start stripped, since a pickup with no respawn time never
        // comes back on its own.
        self.spawn_pickups();
        // Every hunter back in, alive, wherever the pool puts it — including the ones
        // still standing (a new round is a clean slate, not a continuation).
        for i in 0..self.enemies.len() {
            if !self.enemies[i].enemy.is_dead() {
                // A living hunter's capsule is replaced by the respawn, so retire the
                // current one first or it leaks and keeps taking hits.
                let c = self.enemies[i].collider;
                self.physics.remove_enemy_collider(c);
            }
            self.respawn_hunter(i);
        }
        self.respawn_player();
        log::info!("── NEW ROUND ──");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::tools::spawn_point::tests::{big_room, place_pad};

    /// Step the sim `secs` seconds at the fixed rate with the player idle.
    fn run(world: &mut World, secs: f32) {
        let dt = 1.0 / 60.0;
        let input = InputState::default();
        for _ in 0..(secs / dt).ceil() as usize {
            world.fixed_step(dt, &input);
        }
    }

    /// A killed hunter comes back after [`RESPAWN_DELAY`] — alive, at full health, from a
    /// pad — **and it comes back in its own roster slot**, which is the trap this whole
    /// design is shaped around: the index is load-bearing for the PD target ids, the ORCA
    /// agent tags, squad alert and every AI-lab metric, so the roster must not renumber.
    #[test]
    fn a_killed_hunter_respawns_into_its_own_slot() {
        let mut world = big_room(40.0);
        world.set_wave_size(3);
        world.set_score_limit(0); // endless, so the round can't end mid-test
        for p in [
            Vec3::new(6.0, 0.0, 6.0),
            Vec3::new(34.0, 0.0, 6.0),
            Vec3::new(20.0, 0.0, 34.0),
        ] {
            place_pad(&mut world, p, 0.0);
        }
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();
        assert_eq!(world.enemies.len(), 3);

        // Identify slot 1 by something that survives a respawn, then kill it.
        let body_before = world.enemies[1].body;
        let weapon_before = world.enemies[1].weapon.name;
        let collider_before = world.enemies[1].collider;
        // One lethal shot through the real damage funnel, so `start_death` runs (and with
        // it the scoreboard credit + the respawn clock).
        let at = world.enemies[1].enemy.pos + Vec3::Y * 0.8;
        world.hit_enemy_with(1, at, 1e6, Killer::Player);
        assert!(world.enemies[1].enemy.is_dead(), "slot 1 is down");
        assert_eq!(world.enemies.len(), 3, "a death does not shrink the roster");
        assert!(world.enemies[1].respawn_timer.is_some(), "its clock is armed");

        // Not back before the delay…
        run(&mut world, RESPAWN_DELAY * 0.5);
        assert!(world.enemies[1].enemy.is_dead(), "still down mid-beat");
        // …and back after it.
        run(&mut world, RESPAWN_DELAY);
        assert_eq!(world.enemies.len(), 3, "the roster length never changed");
        let inst = &world.enemies[1];
        assert!(!inst.enemy.is_dead(), "slot 1 is alive again");
        assert_eq!(inst.enemy.health(), inst.enemy.max_health(), "full health");
        assert!(inst.respawn_timer.is_none(), "clock cleared");
        // The slot kept its identity — same body, same gun, not a fresh hunter appended.
        assert_eq!(inst.body, body_before, "same body in the same slot");
        assert_eq!(inst.weapon.name, weapon_before, "same weapon in the same slot");
        // A fresh magazine + clean transient state (PD's `bot_reset`).
        assert_eq!(inst.loaded, inst.weapon.clip, "reloaded on respawn");
        assert!(inst.fade.is_none(), "corpse fade cleared");
        assert!(inst.ragdoll.is_none(), "ragdoll torn down");
        assert!(inst.blood.iter().all(|c| *c == 1.0), "blood wiped (splat_reset_chr)");
        // Its capsule is back and it is a NEW handle (death removed the old one), so the
        // respawned body is shootable again rather than a ghost.
        assert_ne!(inst.collider, collider_before, "a fresh capsule was added");
        let pos = inst.enemy.pos;
        assert!(
            world.physics.is_enemy_collider(inst.collider),
            "the new handle is registered as an enemy capsule"
        );
        // And it came back from the pool, not where it died.
        assert!(
            world
                .spawn_pads
                .iter()
                .any(|p| p.pos.distance(pos) < 3.0),
            "respawned at a pad, got {pos:?}"
        );
    }

    /// The player respawns automatically after the same beat, without a keypress, and the
    /// hunt keeps running. Perfect Dark's normal-multiplayer behaviour (`dostartnewlife`
    /// fires on its own, `player.c:4596`) rather than coop's button press.
    #[test]
    fn the_player_respawns_automatically_and_the_hunt_continues() {
        let mut world = big_room(40.0);
        world.set_spawn_enemies(false);
        world.set_score_limit(0);
        place_pad(&mut world, Vec3::new(6.0, 0.0, 6.0), 0.0);
        place_pad(&mut world, Vec3::new(34.0, 0.0, 34.0), 0.0);
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();

        world.take_player_damage(1e6);
        assert!(world.is_player_dead(), "dead");
        assert_eq!(world.player_score().deaths, 1, "the death is scored");

        // Mid-beat: still dead, still in HUNT (the world is frozen, not ended).
        run(&mut world, RESPAWN_DELAY * 0.5);
        assert!(world.is_player_dead(), "still dead mid-beat");
        assert!(!world.is_build(), "death does not drop out of HUNT");

        // After the beat: alive, healed, at a pad — no keypress involved.
        run(&mut world, RESPAWN_DELAY);
        assert!(!world.is_player_dead(), "respawned on its own");
        assert_eq!(world.player_health(), PLAYER_MAX_HEALTH, "healed");
        assert!(!world.is_build(), "the hunt is still running");
        let p = world.player_pos().expect("player exists");
        assert!(
            world.spawn_pads.iter().any(|pad| pad.pos.distance(p) < 3.0),
            "respawned at a pad, got {p:?}"
        );
    }

    /// The loop is genuinely a loop: a hunter killed twice comes back twice, and its slot
    /// tally accumulates across its own deaths (the reason the scores live on the `World`
    /// rather than on the instance a respawn rebuilds).
    #[test]
    fn respawn_repeats_and_the_slot_tally_accumulates() {
        let mut world = big_room(40.0);
        world.set_wave_size(1);
        world.set_score_limit(0);
        place_pad(&mut world, Vec3::new(6.0, 0.0, 6.0), 0.0);
        place_pad(&mut world, Vec3::new(34.0, 0.0, 34.0), 0.0);
        world.camera.pos = Vec3::new(20.0, 2.0, 20.0);
        world.toggle_mode();

        for expected in 1..=3u32 {
            let at = world.enemies[0].enemy.pos + Vec3::Y * 0.8;
            world.hit_enemy_with(0, at, 1e6, Killer::Player);
            assert!(world.enemies[0].enemy.is_dead(), "down on pass {expected}");
            assert_eq!(
                world.hunter_scores()[0].deaths, expected,
                "slot 0's deaths accumulate across respawns"
            );
            assert_eq!(world.player_score().kills, expected, "the player is credited");
            run(&mut world, RESPAWN_DELAY * 1.5);
            assert!(!world.enemies[0].enemy.is_dead(), "back on pass {expected}");
        }
    }
}
