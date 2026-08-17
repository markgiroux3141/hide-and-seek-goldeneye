//! `World` lifecycle: fly-cam look, the fixed-step sim loop, the view
//! projection, the BUILD↔HUNT toggle, and the spawn floor probe.

use super::*;
use crate::combat::attack_anim;
use engine::render::camera::apply_look_delta;
use engine::sim::avoidance;
use glam::Vec2;

/// Measure the **real gun barrel** direction, in the chest's local frame, for the fire
/// clip currently installed on `stack`'s aim overlay, held at `hold_time`.
///
/// This is the number the chest-aim layer swings against, and it has to be measured
/// rather than assumed: the clip is hand-authored, the gun rides a hand bone through an
/// attach rotation, and every body has its own bone lengths — see
/// [`EnemyArm::barrel_forward_in_chest`]. The overlay is forced to full weight for the
/// sample and dropped back to zero afterwards, because a hunter is not aiming at spawn
/// and `advance_animation` owns that weight.
fn measure_barrel_forward(
    stack: &mut LayeredAnimator,
    arm: &EnemyArm,
    sk: &engine::skeletal::Skeleton,
    attach: Quat,
    barrel: Vec3,
    hold_time: f32,
) -> Option<Vec3> {
    if let Some(ov) = stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
        ov.time = hold_time;
        ov.weight = 1.0;
    }
    let pose = stack.evaluate(Pose::bind(sk), &LayerCtx { skeleton: sk, dt: 0.0 });
    if let Some(ov) = stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
        ov.weight = 0.0;
    }
    arm.barrel_forward_in_chest(&pose, sk, attach, barrel)
}

impl World {
    /// Apply mouse-look — once per rendered frame, so aim is decoupled from the
    /// fixed sim rate. In HUNT, holding RMB switches to GoldenEye free-aim: the
    /// mouse floats the crosshair within a circular boundary and only pans the
    /// camera once the crosshair is pinned at the rim; releasing springs it back
    /// to center. `dt` drives that spring.
    pub fn look(&mut self, input: &mut InputState, dt: f32) {
        match self.mode {
            Mode::Build => {
                self.aiming = false;
                self.camera.apply_look(input);
            }
            Mode::Hunt => {
                let (dx, dy) = input.take_mouse_delta();
                self.aiming = input.pointer_locked && input.mouse_right_down();
                if !input.pointer_locked {
                    return; // delta already drained so a re-lock doesn't jump
                }
                if input.mouse_right_down() {
                    // Free-aim: move the floating crosshair; rim overflow pans view.
                    let (ax, ay, pan_dx, pan_dy) = super::combat::resolve_aim(self.aim_x, self.aim_y, dx, dy);
                    self.aim_x = ax;
                    self.aim_y = ay;
                    if let Some(c) = self.character.as_mut() {
                        (c.yaw, c.pitch) = apply_look_delta(c.yaw, c.pitch, pan_dx, pan_dy);
                    }
                } else {
                    // Normal look; crosshair springs back to center.
                    if let Some(c) = self.character.as_mut() {
                        (c.yaw, c.pitch) = apply_look_delta(c.yaw, c.pitch, dx, dy);
                    }
                    let k = (AIM_RETURN_SPRING * dt).min(1.0);
                    self.aim_x += (0.0 - self.aim_x) * k;
                    self.aim_y += (0.0 - self.aim_y) * k;
                }
            }
        }
    }

    /// Drive HUNT look / aim / move from the USB-N64 gamepad this frame (the
    /// GoldenEye "solitaire" scheme), replacing [`Self::look`] while a pad is the
    /// active input. Ported from `GamepadManager.poll`:
    ///   * **Aim mode** (L or R held): the stick springs the crosshair toward a
    ///     target offset (∝ stick position, clamped to the [`AIM_MAX_RANGE`] circle);
    ///     pushing past [`PAD_AIM_TURN_THRESHOLD`] pans the camera at the rim.
    ///   * **Normal mode**: stick Y = analog forward/back, stick X = camera yaw; the
    ///     crosshair springs back to center.
    /// C-Up/C-Down (`pitch_axis`, −1 = up … +1 = down) tilts the view either way.
    /// `sx, sy` are the radially-deadzoned stick axes (screen convention: +y = down).
    pub fn gamepad_look(
        &mut self,
        dt: f32,
        sx: f32,
        sy: f32,
        aim_mode: bool,
        pitch_axis: f32,
        input: &mut InputState,
    ) {
        // Gamepad control is HUNT-only; BUILD fly authoring stays keyboard+mouse.
        if self.mode != Mode::Hunt || !input.pointer_locked {
            input.set_analog_move(0.0, 0.0);
            self.aiming = false;
            return;
        }
        self.aiming = aim_mode;
        if aim_mode {
            input.set_analog_move(0.0, 0.0);
            // Spring the crosshair toward the stick's target offset, then clamp it
            // to the circular aim boundary. `PAD_PITCH_SIGN` flips the vertical.
            let tx = sx * AIM_MAX_RANGE;
            let ty = PAD_PITCH_SIGN * -sy * AIM_MAX_RANGE;
            let k = (PAD_AIM_SPRING * dt).min(1.0);
            self.aim_x += (tx - self.aim_x) * k;
            self.aim_y += (ty - self.aim_y) * k;
            let mag = (self.aim_x * self.aim_x + self.aim_y * self.aim_y).sqrt();
            if mag > AIM_MAX_RANGE && mag > 1e-6 {
                self.aim_x *= AIM_MAX_RANGE / mag;
                self.aim_y *= AIM_MAX_RANGE / mag;
            }
            // Past the threshold, the pinned crosshair pans the camera.
            let sm = (sx * sx + sy * sy).sqrt();
            if sm > PAD_AIM_TURN_THRESHOLD {
                let overflow = (sm - PAD_AIM_TURN_THRESHOLD) / (1.0 - PAD_AIM_TURN_THRESHOLD);
                let (nx, ny) = (sx / sm, sy / sm);
                if let Some(c) = self.character.as_mut() {
                    // The pan must pitch the SAME way the crosshair aims — both use
                    // `PAD_PITCH_SIGN`, so they never fight. `apply_look_delta` does
                    // `pitch -= dy`, so the base `+ny` makes stick-up pitch up.
                    (c.yaw, c.pitch) = apply_look_delta(
                        c.yaw,
                        c.pitch,
                        nx * overflow * PAD_AIM_TURN_SPEED * dt,
                        PAD_PITCH_SIGN * ny * overflow * PAD_AIM_TURN_SPEED * dt,
                    );
                }
            }
        } else {
            // Normal: analog forward from stick Y (−sy = push-up-is-forward), yaw
            // from stick X; crosshair springs back to center.
            input.set_analog_move(0.0, -sy);
            if sx != 0.0 {
                if let Some(c) = self.character.as_mut() {
                    (c.yaw, c.pitch) =
                        apply_look_delta(c.yaw, c.pitch, sx * PAD_TURN_SPEED * dt, 0.0);
                }
            }
            let k = (AIM_RETURN_SPRING * dt).min(1.0);
            self.aim_x += (0.0 - self.aim_x) * k;
            self.aim_y += (0.0 - self.aim_y) * k;
        }
        // C-Up / C-Down pitch, either mode (same vertical sign as the stick aim).
        if pitch_axis != 0.0 {
            if let Some(c) = self.character.as_mut() {
                (c.yaw, c.pitch) = apply_look_delta(
                    c.yaw,
                    c.pitch,
                    0.0,
                    PAD_PITCH_SIGN * pitch_axis * PAD_C_LOOK_SPEED * dt,
                );
            }
        }
    }

    /// Advance movement/physics by one fixed timestep.
    pub fn fixed_step(&mut self, dt: f32, input: &InputState) {
        match self.mode {
            Mode::Build => self.camera.apply_move(dt, input),
            Mode::Hunt => {
                // On player death everything freezes behind the YOU DIED screen.
                if self.player_dead {
                    return;
                }
                let Some(c) = self.character.as_mut() else { return };
                c.apply_move(dt, input, &mut self.physics);
                let feet = c.pos;
                // The player's planar velocity — the moving-disc obstacle for ORCA.
                let player_vel = c.velocity();
                // The player's crosshair line, for the hunters' reactive aim-dodge.
                let aim_origin = c.eye();
                let aim_dir = c.forward();
                // ── ECS systems tick (scaffold seam) ──────────────────────────
                // Runs before the hunter FSM so any nav-overlay a system sets this
                // step (e.g. a door's live pathing cost) is visible when hunters
                // path below. Built from disjoint borrows of `nav`/`physics`, so it
                // coexists with the enemy loop's borrows of those fields. Inert this
                // pass (empty system list); the door/prop systems land on this seam.
                {
                    let mut ctx = crate::ecs::SystemCtx {
                        dt,
                        player_feet: feet,
                        nav: self.nav.as_mut(),
                        physics: &mut self.physics,
                        commands: Vec::new(),
                    };
                    self.ecs.run_systems(&mut ctx);
                }
                // Advance each hunter's perception FSM. Take the roster out so it
                // isn't borrowed while each FSM needs `&self.nav` + `&mut self.physics`
                // (the LOS raycast). Fire requests are collected + applied after the
                // roster is restored (`start_enemy_fire` needs `&mut self`).
                let mut enemies = std::mem::take(&mut self.enemies);
                // Pre-step positions, to measure each hunter's ACTUAL travel this step
                // (after movement AND the separation nudge) for the anti-grind gait.
                let prev_pos: Vec<Vec3> = enemies.iter().map(|e| e.enemy.pos).collect();
                // Difficulty-scaled FSM knobs (reaction/cooldown/dodge) for this step.
                let tuning = self.ai_tuning();
                // Player-visibility toggle (`N`): when invisible, hunters can't perceive
                // the player and revert to searching (dev/observe aid for the head-scan).
                let player_visible = !self.player_invisible;
                // Wall-clearance radius applied per step (0 = off) so the wide model
                // stops clipping walls; `integrate_move` reads it after the ORCA commit.
                let wall_clear_r = if self.wall_clearance { WALL_CLEARANCE_RADIUS } else { 0.0 };
                // Utility-AI decision layer on/off, applied per hunter below (like the
                // detectable + wall-clearance toggles) so it can be A/B'd against the FSM.
                let utility_on = self.utility_ai;
                // PD's knowledge rule, applied per hunter below. Gated on the hunter
                // actually carrying a simulant (`pdsim`) rather than on the lab flag, so
                // it travels with the model rather than with the mode — a GoldenEye
                // hunter in the same wave would keep its own last-known/search behaviour.
                let omniscient_on = self.pd_omniscience;
                // Difficulty dial as a fraction, for the PD simulant tier lookup.
                let dial_frac = self.difficulty_frac();
                // Everyone a simulant could shoot at, snapshotted before the loop —
                // inside it each hunter is held `&mut` and cannot read its neighbours.
                // The player is index 0 (`PdTarget::Player`); hunters follow.
                let pd_actors: Vec<pd_lab::PdActor> = {
                    let mut v = vec![pd_lab::PdActor {
                        who: pd_lab::PdTarget::Player,
                        pos: feet,
                        alive: !self.player_dead,
                        health_frac: (self.player_health / PLAYER_MAX_HEALTH).clamp(0.0, 1.0),
                        armed: true, // the player always carries a gun in this game
                        visible: false,
                    }];
                    v.extend(enemies.iter().enumerate().map(|(j, e)| pd_lab::PdActor {
                        who: pd_lab::PdTarget::Hunter(j),
                        pos: e.enemy.pos,
                        alive: !e.enemy.is_dead(),
                        health_frac: (e.enemy.health() / crate::enemy::ENEMY_HEALTH).clamp(0.0, 1.0),
                        armed: true,
                        visible: false,
                    }));
                    v
                };
                let mut fire_requests: Vec<usize> = Vec::new();
                let mut needs_target: Vec<usize> = Vec::new();
                let mut any_caught = false;
                for (i, inst) in enemies.iter_mut().enumerate() {
                    // Apply the player-visibility toggle so an invisible player can't be
                    // perceived (all LOS/proximity checks in `update` fail).
                    inst.enemy.set_detectable(player_visible);
                    // Arm the wall-clearance nudge for this step's movement commit.
                    inst.enemy.set_wall_clearance_radius(wall_clear_r);
                    // Select the decision layer (utility vs legacy FSM) for this step.
                    inst.enemy.set_utility(utility_on);
                    // Perfect Dark hunters always know where the player is (movement
                    // only — perception is untouched, see `Enemy::known_player_pos`).
                    inst.enemy.set_omniscient(omniscient_on && inst.pdsim.is_some());
                    // Is THIS hunter mid fire burst? (the JS `enemyState === 'action'`
                    // proxy the attack→cooldown transition needs). Firing is a timer
                    // now, so the hunter can move + aim through it.
                    let fire_anim = inst.fire_elapsed.is_some();
                    // Does the player's crosshair line fall on this hunter (cone + clear
                    // LOS)? Drives its reactive aim-dodge. Excludes the hunter's own
                    // capsule so it doesn't self-block the ray.
                    let aimed_at = {
                        let chest = inst.enemy.pos + Vec3::new(0.0, AIM_SENSE_CHEST_Y, 0.0);
                        let to = chest - aim_origin;
                        let d = to.length();
                        if d < 1e-3 {
                            false
                        } else {
                            let dir = to / d;
                            let clear = self
                                .physics
                                .raycast_excluding(aim_origin, dir, d, Some(inst.collider))
                                .map_or(true, |hit| (hit.point - aim_origin).length() >= d - 0.15);
                            aim_dir.dot(dir) >= AIM_SENSE_COS && clear
                        }
                    };
                    // ── Who this hunter is manoeuvring against ──
                    // The player, unless a simulant has picked a packmate. That makes
                    // hunter-on-hunter combat whole: `emit_pd_shot` already resolved the
                    // round against whoever was on the line, but the FSM only knew how
                    // to close on, hold a standoff from and take cover against the
                    // player — so a simulant that chose a packmate shot it from wherever
                    // it happened to be standing. Now the same chase / standoff / cover /
                    // search machinery runs against an arbitrary target.
                    //
                    // `pd_target` is last step's choice, because `step_simulant` runs
                    // *after* the FSM below. That is deliberate and matches PD's own
                    // ordering (`bot_tick` picks the target, then the action layer acts
                    // on it) at the cost of one frame — the alternative is running target
                    // selection twice per step.
                    let engage = match inst.pd_target {
                        Some(pd_lab::PdTarget::Hunter(j)) => pd_actors
                            .iter()
                            .find(|a| a.who == pd_lab::PdTarget::Hunter(j) && a.alive)
                            .map(|a| crate::enemy::EngageTarget::hunter(j, a.pos))
                            .unwrap_or_else(|| crate::enemy::EngageTarget::player(feet)),
                        _ => crate::enemy::EngageTarget::player(feet),
                    };
                    let step = match self.nav.as_ref() {
                        Some(nav) => inst.enemy.update(
                            dt,
                            engage,
                            feet,
                            inst.weapon.standoff,
                            tuning,
                            aimed_at,
                            nav,
                            &mut self.physics,
                            fire_anim,
                            inst.collider,
                        ),
                        None => crate::enemy::EnemyStep::default(),
                    };
                    // ── PD simulant layer (PD_LAB only) ──
                    // Runs AFTER the FSM so the FSM still owns movement, and the
                    // simulant only overrides where the weapon points and whether
                    // it fires. The sight check is a raw LOS ray of its own rather
                    // than the FSM's cone-gated perception — PD bots do the same,
                    // and borrowing the cone rule would give the aim model a
                    // perception contract it was never tuned against.
                    let mut pd_fire = None;
                    let pd_cfg = self.pd;
                    if let Some(sim) = inst.pdsim.as_mut() {
                        // Resolve sight to every candidate this simulant might pick.
                        // The model only *believes* one fresh answer per tick (the
                        // round-robin), but the caller has to be able to answer
                        // whichever slot it asks about.
                        let me = pd_lab::PdTarget::Hunter(i);
                        let mut actors = pd_actors.clone();
                        for a in &mut actors {
                            if a.who == me || !a.alive {
                                a.visible = false;
                                continue;
                            }
                            // **World geometry only.** `chr_has_los_to_chr`
                            // (`chraction.c:6533`) tests against
                            // `CDTYPE_OBJS | DOORS | PATHBLOCKER | BG | AIOPAQUE` —
                            // `CDTYPE_CHRS` is *not* in the mask, and it disables both
                            // characters' own perimeters for the cast. So another body
                            // never breaks a Perfect Dark bot's line of sight.
                            //
                            // This was `line_of_sight` (which capsules block), and the
                            // consequence only appeared once every hunter became a
                            // simulant: in a converging pack the front hunter occludes
                            // the ones behind it, so they believed they could not see the
                            // player and stood in `Attack` without firing. The AI lab
                            // measured 663 occluded frames on one hunter in a 15 s run.
                            //
                            // It also makes the sight check agree with `emit_pd_shot`,
                            // which already resolves rounds against world geometry only —
                            // a body on the line is not an obstruction there, it is the
                            // thing that gets shot.
                            a.visible = match a.who {
                                pd_lab::PdTarget::Player => player_visible,
                                pd_lab::PdTarget::Hunter(_) => true,
                            } && crate::enemy::perception_los(
                                &mut self.physics,
                                inst.enemy.pos,
                                a.pos,
                            );
                        }
                        // The playing attack animation's `angleoffset`, and only while a
                        // burst is actually running: between bursts the hunter holds its
                        // weapon on the target with its torso square, so leaving a
                        // sideways offset in force would pin the body 90° away with
                        // nothing aiming across it. Dropping it back to 0 makes the body
                        // turn home at PD's own turn rate rather than snapping.
                        let aim_offset = match inst.fire_elapsed {
                            Some(_) => inst.fire.angle_offset,
                            None => 0.0,
                        };
                        let (out, mut dbg, chosen) = pd_lab::step_simulant(
                            sim,
                            dt,
                            me,
                            inst.enemy.pos,
                            &actors,
                            dial_frac,
                            pd_cfg.difficulty.is_none(),
                            aim_offset,
                            pd_cfg.free_for_all,
                        );
                        // Body-side readouts the model can't know (see `PdDebug`).
                        dbg.id = i;
                        dbg.health = inst.enemy.health();
                        dbg.max_health = inst.enemy.max_health();
                        dbg.dead = inst.enemy.is_dead();
                        dbg.state = inst.enemy.state();
                        dbg.target_hunter = match chosen {
                            Some(pd_lab::PdTarget::Hunter(j)) => Some(j),
                            _ => None,
                        };
                        // Which of PD's per-bearing attack animations this burst picked —
                        // the direction table made visible, since "it chose the right clip"
                        // is otherwise only inspectable in a test.
                        dbg.fire_anim = inst.fire_elapsed.and_then(|_| {
                            inst.fire.slot.and_then(|s| {
                                attack_anim::ALL_ROWS.iter().find(|r| r.slot == s).map(|r| {
                                    (r.anim, r.angle_offset.to_degrees())
                                })
                            })
                        });
                        // The simulant's yaw IS the rendered facing: the aim error
                        // must be visible on the body, or none of this reads.
                        inst.render_yaw = Some(out.yaw);
                        inst.pd_debug = Some(dbg);
                        inst.pd_target = chosen;
                        pd_fire = Some(out.want_fire);
                    }

                    // (The FSM no longer moves `pos` — it only decides a preferred
                    // velocity. Movement is committed after the loop by the local-
                    // avoidance stage, which resyncs every hunter's capsule then.)
                    //
                    // Under the PD model the trigger decision belongs to the
                    // simulant, not the FSM: a simulant opens fire while still
                    // converging, which the FSM's "entered Attack" gate suppresses.
                    // The FSM's own `want_fire` is a rising edge, so the PD path has
                    // to debounce against the live burst itself.
                    let wants_fire = match pd_fire {
                        Some(pd) => pd && inst.fire_elapsed.is_none(),
                        None => step.want_fire,
                    };
                    if wants_fire {
                        fire_requests.push(i);
                    }
                    if step.needs_search_target {
                        needs_target.push(i);
                    }
                    if step.caught {
                        any_caught = true;
                    }
                }
                // Squad coordination: an engaged hunter calls nearby packmates onto
                // the player's last-known spot, so the pack converges once anyone spots
                // it (rather than some wandering off on their fan-out search).
                squad_alert(&mut enemies);
                // Local avoidance: each hunter steers around its packmates + the player
                // toward the velocity its FSM asked for (ORCA), then the resolved move is
                // committed (nav/LOS-gated + floor-snapped by `integrate_move`). This is
                // the modern replacement for the old position-nudge separation. With the
                // flag off, hunters apply their preferred velocity directly and the legacy
                // `separate_enemies` nudge runs — the pre-ORCA baseline. Either way the
                // capsules are resynced afterward so hitscan sees the new positions.
                if let Some(nav) = self.nav.as_ref() {
                    if self.local_avoidance {
                        let obstacles = [avoidance::Obstacle {
                            pos: Vec2::new(feet.x, feet.z),
                            vel: Vec2::new(player_vel.x, player_vel.z),
                            radius: PLAYER_AVOID_RADIUS,
                        }];
                        // Snapshot every live hunter as an ORCA agent (index-tagged so
                        // the resolved velocity maps back). A tiny deterministic per-index
                        // offset breaks the degenerate exactly-coincident case (ORCA can't
                        // choose a split direction for two agents sharing a point).
                        let agents: Vec<(usize, avoidance::Agent)> = enemies
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| !e.enemy.is_dead())
                            .map(|(i, e)| {
                                let p = e.enemy.pos;
                                let v = e.enemy.velocity();
                                let dv = e.enemy.desired_velocity();
                                let ang = i as f32 * 2.399_963; // golden angle
                                let jitter = Vec2::new(ang.cos(), ang.sin()) * 1e-2;
                                (
                                    i,
                                    avoidance::Agent {
                                        pos: Vec2::new(p.x, p.z) + jitter,
                                        vel: Vec2::new(v.x, v.z),
                                        pref_vel: Vec2::new(dv.x, dv.z),
                                        radius: ENEMY_RADIUS,
                                        max_speed: e.enemy.move_intent().max(AVOID_YIELD_SPEED),
                                    },
                                )
                            })
                            .collect();
                        // Solve each agent against the others + the player, then commit.
                        let resolved: Vec<(usize, Vec3)> = agents
                            .iter()
                            .map(|(i, a)| {
                                let neighbors: Vec<avoidance::Agent> = agents
                                    .iter()
                                    .filter(|(j, _)| j != i)
                                    .map(|(_, x)| *x)
                                    .collect();
                                let v =
                                    avoidance::orca_velocity(a, &neighbors, &obstacles, AVOID_HORIZONS, dt);
                                (*i, Vec3::new(v.x, 0.0, v.y))
                            })
                            .collect();
                        for (i, v) in resolved {
                            enemies[i].enemy.integrate_move(v, dt, nav);
                        }
                    } else {
                        for inst in enemies.iter_mut() {
                            let dv = inst.enemy.desired_velocity();
                            inst.enemy.integrate_move(dv, dt, nav);
                        }
                        separate_enemies(&mut enemies, nav);
                    }
                    for inst in enemies.iter() {
                        if !inst.enemy.is_dead() {
                            self.physics.update_enemy_collider(inst.collider, inst.enemy.pos);
                        }
                    }
                }
                // Anti-grind: tell each hunter how far it ACTUALLY travelled this step
                // (post-separation), so a hunter held in place by the crowd settles +
                // idles instead of walk-cycling on the spot ("manic strafing").
                for (i, inst) in enemies.iter_mut().enumerate() {
                    let a = inst.enemy.pos;
                    let b = prev_pos[i];
                    let disp = ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt();
                    inst.enemy.note_travel(disp, dt);
                }
                self.enemies = enemies;
                // Footstep noise (#6): a moving player pulls nearby blind hunters to
                // investigate (difficulty-scaled range; silent at level 0).
                self.alert_enemies_to_movement();
                // Grenade flush (#5): camp one spot too long and a hunter lobs a
                // grenade to shift you (difficulty-scaled; off at level 0).
                self.grenade_flush_step(dt);
                for i in fire_requests {
                    self.start_enemy_fire(i);
                }
                // Hand fresh fan-out points to the hunters that arrived / are stuck.
                // Done after the roster is restored so `pick_search_point` can see
                // where every other hunter is currently headed (the coordination).
                for i in needs_target {
                    let target = self.pick_search_point(i);
                    if let Some(inst) = self.enemies.get_mut(i) {
                        inst.enemy.assign_search_target(target);
                    }
                }
                // Advance any death ragdolls: step the rigid-body solver (a no-op with
                // none live), then age each corpse toward its settle → fade → despawn.
                self.physics.step_dynamics(dt);
                self.advance_ragdolls(dt);
                if any_caught && !self.caught {
                    self.caught = true;
                    log::info!("CAUGHT by a hunter!");
                }
            }
        }
    }

    /// View-projection for whichever controller is active.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        match (self.mode, self.character.as_ref()) {
            (Mode::Hunt, Some(c)) => c.view_proj(aspect),
            _ => self.camera.view_proj(aspect),
        }
    }

    /// Toggle BUILD↔HUNT (bound to `G`). Entering HUNT freezes the geometry and
    /// drops a capsule onto the floor beneath the fly-cam; leaving HUNT restores
    /// the fly-cam at the player's eye so editing can continue.
    pub fn toggle_mode(&mut self) {
        // The authoring tools are BUILD-only; a mode switch always disarms them
        // and clears any sub-face selection state.
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.reset_subface();
        // Reset the free-aim crosshair (centered, disengaged) on any mode switch.
        self.aim_x = 0.0;
        self.aim_y = 0.0;
        self.aiming = false;
        // A mode switch always ends any hunt: drop every hunter + its capsule, and
        // revive the BUILD demo model.
        self.clear_ragdolls();
        self.physics.clear_enemy_colliders();
        self.enemies.clear();
        // Fresh player-combat state each mode switch (full health, no flash/HUD).
        self.player_health = PLAYER_MAX_HEALTH;
        self.player_armor = 0.0;
        self.player_dead = false;
        self.damage_flash = 0.0;
        self.hud_show_timer = 0.0;
        match self.mode {
            Mode::Build => {
                let Some(feet) = self.floor_under(self.camera.pos) else {
                    log::warn!("HUNT: no floor beneath the camera to spawn on — staying in BUILD");
                    return;
                };
                self.character = Some(CharacterController::new(
                    feet,
                    self.camera.yaw,
                    self.camera.pitch,
                ));
                // Remember the duel start so a difficulty-change reset returns here.
                self.hunt_spawn = Some((feet, self.camera.yaw, self.camera.pitch));
                self.selected = None; // clear any authoring selection
                self.caught = false;

                // Bake the nav grid from the frozen geometry (once), seal the spawn
                // door, and flood the hunter wave in through it.
                let t0 = Instant::now();
                let mut structure_solids = self.structure_solid_boxes();
                // Placed props are solid to grid-navving enemies too: block their
                // footprint cells so hunters path around crates/furniture (enemies
                // ignore physics colliders — see the nav-vs-physics split).
                structure_solids.extend(self.prop_solid_boxes());
                match nav::bake(&mut self.regions, &structure_solids) {
                    Some(nav) => {
                        let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
                        log::info!(
                            "nav baked in {bake_ms:.2} ms ({} cells)",
                            nav.cell_count()
                        );
                        // Resolve the fixed enemy spawn point (marker → standable
                        // cell) and the fan-out search-point pool.
                        self.prepare_spawn(&nav);
                        // Decimate the walkable floor for the radar backdrop (once —
                        // it's static for the hunt).
                        self.bake_radar_cells(&nav);
                        // Flood in the wave: ENEMY_COUNT hunters clustered at the fixed
                        // spawn point, each in Search, fanning out to hunt the player.
                        if self.spawn_enemies {
                            self.spawn_wave(&nav);
                        }
                        self.nav = Some(nav);
                    }
                    None => log::warn!("nav bake produced no grid"),
                }

                // Make destructible props solid + shootable for the hunt (colliders
                // + transient Health baked from the authored entities).
                self.spawn_prop_colliders();

                self.mode = Mode::Hunt;
                log::info!("→ HUNT (spawned at {feet:?})");
            }
            Mode::Hunt => {
                if let Some(c) = self.character.take() {
                    self.camera.pos = c.pos + Vec3::new(0.0, WORLD_SCALE * 5.4, 0.0);
                    self.camera.yaw = c.yaw;
                    self.camera.pitch = c.pitch;
                }
                self.nav = None;
                self.enemies.clear();
                self.hunt_spawn = None;
                self.search_points.clear();
                self.radar_cells.clear();
                self.caught = false;
                self.sparks.clear();
                // Explosives don't survive the hunt: drop any in-flight rounds,
                // placed mines, and fading blast VFX so none leak into the next HUNT.
                self.projectiles.clear();
                self.mines.clear();
                self.blasts.clear();
                self.camp_anchor = None;
                self.camp_timer = 0.0;
                self.grenade_cooldown = 0.0;
                self.physics.clear_door_colliders();
                self.doors.clear();
                // Drop prop colliders + strip the transient prop combat state, so a
                // crate blown up this hunt returns intact to the authored level.
                self.clear_prop_colliders();
                self.mode = Mode::Build;
                log::info!("→ BUILD");
            }
        }
    }

    /// Restart the current duel WITHOUT leaving HUNT (bound to the difficulty keys):
    /// heal the player and drop them back at the hunt-start pose, clear combat VFX,
    /// then despawn + respawn the wave so it comes back fresh at the CURRENT difficulty
    /// (spawn health/lethality reflect the new level). No-op outside HUNT — in BUILD a
    /// difficulty change just updates the dial for the next hunt. Reuses the baked nav
    /// (no re-bake), so it's cheap enough to fire on every key press.
    pub fn restart_hunt(&mut self) {
        if self.mode != Mode::Hunt {
            return;
        }
        // Fresh player-combat state, and back to the duel start if we recorded it.
        self.player_health = PLAYER_MAX_HEALTH;
        self.player_armor = 0.0;
        self.player_dead = false;
        self.damage_flash = 0.0;
        self.hud_show_timer = 0.0;
        self.caught = false;
        if let Some((feet, yaw, pitch)) = self.hunt_spawn {
            self.character = Some(CharacterController::new(feet, yaw, pitch));
        }
        // Clear transient combat VFX so nothing leaks across the reset.
        self.sparks.clear();
        self.projectiles.clear();
        self.mines.clear();
        self.blasts.clear();
        // Fresh grenade-flush camp tracker for the new encounter.
        self.camp_anchor = None;
        self.camp_timer = 0.0;
        self.grenade_cooldown = 0.0;
        // Despawn the current wave and re-flood it in at the current difficulty.
        self.clear_ragdolls();
        self.physics.clear_enemy_colliders();
        self.enemies.clear();
        if self.spawn_enemies {
            if let Some(nav) = self.nav.take() {
                self.spawn_wave(&nav);
                self.nav = Some(nav);
            }
        }
    }

    /// Flood the wave in at the fixed spawn point: [`ENEMY_COUNT`] hunters clustered
    /// in a small ring around [`Self::spawn_point`] (so they don't all stack on one
    /// cell), each watching toward the player so their perception cones face where the
    /// action is, and each drawing a weapon from [`ENEMY_ROSTER`] (cycling if the count
    /// exceeds the roster). Every hunter starts in `Search` and gets a fan-out point on
    /// its first step. Skips entirely if the animation template failed to load (no
    /// clips → nothing to animate).
    fn spawn_wave(&mut self, nav: &NavWorld) {
        // **The wave draws from every loaded body, both families**, and each hunter is
        // driven by the clip template built for its own rig — Perfect Dark's set on both
        // (see `World::body_clips`). Before the roster widened, one family was chosen for
        // the whole wave because there was only one Perfect Dark template and it was bound
        // to a Perfect Dark rig.
        let bodies = self.wave_bodies();
        let (body_first, body_count) = (bodies.start, bodies.len());
        // The (at most two) templates this wave needs, cloned once each rather than once
        // per hunter, plus their gait clips. A template is identified by
        // `(is_pd_body, pd_clips)` — the rig it was bound to and which clip set it holds —
        // which is exactly what `World::body_clips` decides from a body id.
        let mut templates: Vec<((bool, bool), AnimPlayer, Option<Vec<(f32, clip::AnimationClip)>>)> =
            Vec::new();
        // Per-body index into `templates`, so the spawn loop is a lookup and does not
        // re-borrow `self` while pushing hunters.
        let mut template_of: Vec<usize> = Vec::with_capacity(body_count);
        for b in bodies.clone() {
            let Some((t, pd_clips)) = self.body_clips(b) else {
                template_of.push(usize::MAX);
                continue;
            };
            let key = (self.pd_bodies().contains(&b), pd_clips);
            let idx = match templates.iter().position(|(k, _, _)| *k == key) {
                Some(k) => k,
                None => {
                    let loco = (|| {
                        Some(vec![
                            (0.0, t.clip(0)?.clone()),
                            (anim_set::SPEED_WALK, t.clip(1)?.clone()),
                            (anim_set::SPEED_JOG, t.clip(2)?.clone()),
                            (anim_set::SPEED_RUN, t.clip(3)?.clone()),
                        ])
                    })();
                    templates.push((key, t.clone(), loco));
                    templates.len() - 1
                }
            };
            template_of.push(idx);
        }
        if templates.is_empty() {
            log::warn!("no animation template loaded — spawning no hunters");
            return;
        }
        log::info!(
            "wave draws from {body_count} bodies ({} GoldenEye + {} Perfect Dark) on {} clip template(s)",
            bodies.clone().filter(|b| !self.pd_bodies().contains(b)).count(),
            bodies.clone().filter(|b| self.pd_bodies().contains(b)).count(),
            templates.len(),
        );
        // Face the player initially (harmless: if the player's out of sight/range the
        // search FSM takes over immediately; if in view they engage, which is right).
        let watch = self.player_pos().unwrap_or(self.spawn_point);
        // Difficulty survivability: each hunter spawns with scaled health.
        let spawn_hp = crate::enemy::ENEMY_HEALTH * self.difficulty_params().health_mult;
        // ANIM_DEBUG → spawn a single AR33-rifle hunter (a two-handed weapon, to
        // check the aim/hold transfers from the one-handed pistol case) so behaviour
        // can be observed in isolation.
        let count = if self.anim_debug { 1 } else { self.wave_size };
        for i in 0..count {
            let (wcfg, dual) = if self.anim_debug {
                (crate::combat::config::AR33, false)
            } else {
                ENEMY_ROSTER[i % ENEMY_ROSTER.len()]
            };
            // Spread the wave across the whole family so a single hunt shows a varied
            // squad rather than six clones (body 0 = Karl when only one loaded).
            let body = if body_count == 0 {
                0
            } else {
                body_first + (i * body_count / count.max(1)) % body_count
            };
            // The clip template built for THIS body's rig, and whether it is the Perfect
            // Dark set (which is what gates the directional fire table and the authored
            // reactions). Both families get the PD set unless `goldeneye_clips` is on.
            let ti = template_of
                .get(body.saturating_sub(body_first))
                .copied()
                .filter(|&k| k < templates.len())
                .unwrap_or(0);
            let (_, template, loco_clips) = &templates[ti];
            let pd_family = templates[ti].0 .1;
            // This hunter starts clean (all-white blood), sized to ITS body's mesh.
            let vert_count = self.char_models.get(body).map(|m| m.vertices.len()).unwrap_or(0);
            // This body's resolved gun-arm + upper-body mask; the hunter clones its own
            // aim/recoil stack (borrowed — `EnemyArm` owns a joint-mask `Vec`, not `Copy`).
            let arm = self.enemy_arm.get(body).and_then(|a| a.as_ref());
            // Ring the cluster around the spawn point.
            let ang = i as f32 / count as f32 * std::f32::consts::TAU;
            let raw = self.spawn_point
                + Vec3::new(ang.cos(), 0.0, ang.sin()) * SPAWN_CLUSTER_RADIUS;
            let spawn = nav
                .nearest_standable(raw.x, raw.y.max(0.1), raw.z, 6)
                .unwrap_or(self.spawn_point);
            let weapon = enemy_def_for(&wcfg);
            // Sized to THIS body — a Perfect Dark hunter is 1.73 m and needs a taller
            // capsule than a 1.50 m GoldenEye one, or its head is not there to shoot.
            let (radius, half_height) = self.body_capsule(body);
            let collider = self.physics.add_enemy_collider(spawn, radius, half_height);

            // Per-hunter stack: [locomotion, upper-body aim overlay, chest-aim, recoil].
            // The overlay plays this weapon class's authored fire/aim clip on the arms
            // + chest — the hand-made two-hand rifle grip, leveled pistol, or akimbo
            // hold — held at the shot instant (the "weapon up" pose). Locomotion drives
            // the legs, so the hunter runs while holding the gun correctly.
            let aim_idx = if dual {
                FIRE_DUAL_IDX
            } else {
                match weapon.class {
                    EnemyWeaponClass::Pistol => FIRE_PISTOL_IDX,
                    EnemyWeaponClass::Rifle => FIRE_RIFLE_IDX,
                }
            };
            let skel = self.char_models.get(body).map(|m| &m.skeleton);
            let asset = self.enemy_weapon_lib.iter().find(|w| w.name == weapon.name);
            // When this hunter aims / shoots / recoils. A Perfect Dark hunter uses
            // PD's **authored** `attackanimconfig` row for the exact animation it is
            // about to play; a GoldenEye one keeps the hand-set `FIRE_TIMING` guess.
            // Both rows describe the same three animations (the two games share an
            // animation bank), so this is an A/B of authored-versus-guessed timing on
            // identical clips — which is why the choice is per family, not global.
            let fire = if pd_family {
                let cfg = attack_anim::config_for(weapon.class, dual);
                let dur = template.clip(aim_idx).map(|c| c.duration).unwrap_or(0.0);
                attack_anim::FireTiming::from_pd(cfg, dur)
            } else {
                attack_anim::FireTiming::legacy(
                    super::hunt::fire_window_for(weapon.class, dual),
                    ENEMY_CHEST_AIM_CONE,
                )
            };
            let (stack, fire_axes) = match (arm, loco_clips.clone(), template.clip(aim_idx).cloned())
            {
                (Some(a), Some(clips), Some(aim_clip)) => {
                    let mut s = a.build_stack(clips, aim_clip.clone());
                    // Freeze the overlay at the first shot — the fully-raised aim pose.
                    if let Some(ov) = s.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
                        ov.time = fire.shoot.0;
                    }
                    // The authored aim limits replace the single fallback cone.
                    if let Some(ca) = s.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER) {
                        ca.cone = fire.cone;
                    }
                    // Measure the real gun barrel (in the chest frame) at the aim pose,
                    // then enable the chest-aim so it swings the whole hold to point the
                    // barrel at the player each frame — for EVERY clip (each fire clip
                    // aims in its own direction; this corrects them all + tracks pitch).
                    let mut axes: Vec<(usize, Vec3)> = Vec::new();
                    if let (Some(sk), Some(asset)) = (skel, asset) {
                        let attach = Quat::from_euler(
                            EulerRot::XYZ,
                            weapon.right_rot.x,
                            weapon.right_rot.y,
                            weapon.right_rot.z,
                        );
                        let barrel = asset.barrel_axis();
                        if let Some(fwd) =
                            measure_barrel_forward(&mut s, a, sk, attach, barrel, fire.shoot.0)
                        {
                            axes.push((aim_idx, fwd));
                            if let Some(ca) = s.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER) {
                                ca.forward = fwd;
                                ca.enabled = true;
                            }
                        }
                        // …and once per animation in this stance's **direction table**,
                        // since a Perfect Dark hunter switches clip per burst from the
                        // bearing to its target. Measuring them all here costs a handful
                        // of pose evaluations at spawn and none mid-fight; the
                        // alternative is a hunter whose barrel axis belongs to whichever
                        // clip it last happened to play.
                        if pd_family {
                            for row in attack_anim::table_for(weapon.class, dual).rows() {
                                if axes.iter().any(|(s, _)| *s == row.slot) {
                                    continue;
                                }
                                let Some(clip) = template.clip(row.slot).cloned() else {
                                    log::warn!(
                                        "PD fire clip slot {} ({}) missing — that bearing \
                                         falls back to the forward animation",
                                        row.slot,
                                        row.anim
                                    );
                                    continue;
                                };
                                let hold = attack_anim::FireTiming::from_pd(row, clip.duration);
                                if let Some(ov) =
                                    s.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER)
                                {
                                    ov.set_clip(clip);
                                }
                                if let Some(fwd) = measure_barrel_forward(
                                    &mut s, a, sk, attach, barrel, hold.shoot.0,
                                ) {
                                    axes.push((row.slot, fwd));
                                }
                            }
                            // Restore the spawn (forward) clip + hold time — the hunter
                            // is not firing yet, and `start_enemy_fire` installs whatever
                            // the bearing asks for when it is.
                            if let Some(ov) = s.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER)
                            {
                                ov.set_clip(aim_clip);
                                ov.time = fire.shoot.0;
                            }
                        }
                    }
                    // Enable the head look-at (its gaze `forward` is baked from the rig
                    // in `build_stack`); `advance_animation` drives its target + weight,
                    // and honours the `head_look` kill-switch.
                    if let Some(hl) = s.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER) {
                        hl.enabled = true;
                    }
                    (s, axes)
                }
                _ => (LayeredAnimator::default(), Vec::new()),
            };

            self.enemies.push(EnemyInstance {
                enemy: {
                    let mut e = Enemy::new(spawn, watch);
                    e.set_max_health(spawn_hp); // difficulty survivability scaling
                    // Flank side: alternate hunters left/right so the pack surrounds the
                    // player rather than chasing single-file down one lane (#3).
                    e.set_flank_side(if i % 2 == 0 { 1.0 } else { -1.0 });
                    e
                },
                body,
                anim: template.clone(),
                weapon,
                dual,
                collider,
                fade: None,
                shot_timer: 0.0,
                burst_shot: 0,
                use_secondary: false,
                fire_elapsed: None,
                fire,
                fire_axes,
                pd_anims: pd_family,
                hit_part: None,
                thud: None,
                thud_played: [false; 2],
                muzzle_timer: 0.0,
                blood: vec![1.0f32; vert_count * 3],
                stack,
                aim_weight: 0.0,
                head_look_weight: 0.0,
                head_look_point: None,
                foot_delta: [0.0, 0.0],
                anim_speed: 0.0,
                render_yaw: None,
                final_pose: None,
                ragdoll: None,
                ragdoll_time: 0.0,
                reaction: None,
                // PD lab only: attach the simulant model. Each gets a distinct seed
                // so same-tier simulants still wander their aim individually —
                // that per-bot variation is the point of randomising the
                // convergence rate rather than the shot outcome.
                pdsim: Some(crate::pdsim::Simulant::new(
                    self.pd
                        .difficulty
                        .unwrap_or_else(|| pd_lab::tier_for_dial_frac(self.difficulty_frac())),
                    self.pd.bot_type,
                    0xA5A5_0000_u64 ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                )),
                pd_debug: None,
                pd_target: None,
            });
            log::info!(
                "hunter {i} flooded in at {spawn:?} as body {body} with {}{}",
                weapon.name,
                if dual { " (dual-wield)" } else { "" }
            );
        }
    }

    /// Pick a fan-out search point for hunter `for_idx` (it's searching and needs a
    /// target). Chooses the pooled search point **farthest from where the other
    /// hunters are already headed** — so the pack spreads out to cover different
    /// regions rather than clumping — while skipping points essentially under this
    /// hunter's feet (so it actually travels somewhere new). Falls back to the player
    /// vicinity / spawn point if the pool is empty.
    pub(crate) fn pick_search_point(&self, for_idx: usize) -> Vec3 {
        if self.search_points.is_empty() {
            return self.player_pos().unwrap_or(self.spawn_point);
        }
        let self_pos = self
            .enemies
            .get(for_idx)
            .map(|e| e.enemy.pos)
            .unwrap_or(self.spawn_point);
        // Where every *other* hunter is currently headed (its search target, or, if
        // none, its position) — we want to get away from these.
        let claimed: Vec<Vec3> = self
            .enemies
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != for_idx)
            .map(|(_, e)| e.enemy.search_target().unwrap_or(e.enemy.pos))
            .collect();

        let mut best = self.search_points[0];
        let mut best_score = f32::NEG_INFINITY;
        for &p in &self.search_points {
            if p.distance(self_pos) < 2.0 {
                continue; // don't re-pick a point we're already on top of
            }
            // Maximise the distance to the nearest already-claimed point (fan out).
            let score = claimed
                .iter()
                .map(|c| c.distance_squared(p))
                .fold(f32::INFINITY, f32::min);
            if score > best_score {
                best_score = score;
                best = p;
            }
        }
        best
    }

    /// Raycast straight down from `from` to find the floor; returns feet position.
    pub(crate) fn floor_under(&mut self, from: Vec3) -> Option<Vec3> {
        // Start a little above the camera so we don't begin inside geometry.
        let origin = from + Vec3::new(0.0, 0.1, 0.0);
        let hit = self.physics.raycast(origin, Vec3::NEG_Y, 100.0)?;
        Some(hit.point)
    }
}

/// Enemy–enemy separation: nudge apart any live pair closer than
/// [`ENEMY_SEPARATION_DIST`] so hunters keep personal space instead of stacking into
/// one body / marching in unison. Positions only (enemies move on nav, not physics);
/// a nudge is applied only where it lands on standable floor (never shoves a hunter
/// off a ledge / into a wall), with the feet re-snapped to that floor. Two relaxation
/// passes keep a tight cluster stable.
fn separate_enemies(enemies: &mut [EnemyInstance], nav: &NavWorld) {
    let n = enemies.len();
    if n < 2 {
        return;
    }
    for _ in 0..2 {
        let mut nudge = vec![Vec3::ZERO; n];
        for i in 0..n {
            if enemies[i].enemy.is_dead() {
                continue;
            }
            for j in (i + 1)..n {
                if enemies[j].enemy.is_dead() {
                    continue;
                }
                let a = enemies[i].enemy.pos;
                let b = enemies[j].enemy.pos;
                let d = Vec3::new(b.x - a.x, 0.0, b.z - a.z);
                let dist = d.length();
                if dist >= ENEMY_SEPARATION_DIST {
                    continue;
                }
                // Split along the line between them; if exactly stacked, fan out on a
                // deterministic per-index angle (the golden angle) so they don't all
                // pick the same escape direction.
                let dir = if dist > 1e-4 {
                    d / dist
                } else {
                    let t = i as f32 * 2.399_963;
                    Vec3::new(t.cos(), 0.0, t.sin())
                };
                let push = (ENEMY_SEPARATION_DIST - dist) * 0.5;
                nudge[i] -= dir * push;
                nudge[j] += dir * push;
            }
        }
        for i in 0..n {
            if enemies[i].enemy.is_dead() || nudge[i] == Vec3::ZERO {
                continue;
            }
            let cand = enemies[i].enemy.pos + nudge[i];
            if let Some(fy) = nav.floor_height_at(cand.x, cand.z, cand.y + 0.25) {
                enemies[i].enemy.pos = Vec3::new(cand.x, fy, cand.z);
            }
        }
    }
}

/// Squad awareness: any hunter actively engaged with the player broadcasts its
/// last-known player position to non-engaged packmates within [`SQUAD_ALERT_RANGE`],
/// which converge to investigate it. So once one hunter spots the player the pack
/// coordinates onto it, instead of some fanning off obliviously. Broadcasts the
/// *last-known* spot (not the live player), so breaking contact + repositioning still
/// loses them — coordinated, not omniscient.
fn squad_alert(enemies: &mut [EnemyInstance]) {
    let contacts: Vec<(Vec3, Vec3)> = enemies
        .iter()
        .filter(|e| !e.enemy.is_dead() && e.enemy.is_engaged())
        .filter_map(|e| e.enemy.last_known().map(|lk| (e.enemy.pos, lk)))
        .collect();
    if contacts.is_empty() {
        return;
    }
    for inst in enemies.iter_mut() {
        if inst.enemy.is_dead() || inst.enemy.is_engaged() {
            continue;
        }
        let p = inst.enemy.pos;
        let call = contacts
            .iter()
            .filter(|(cpos, _)| cpos.distance(p) < SQUAD_ALERT_RANGE)
            .min_by(|a, b| {
                a.0.distance(p)
                    .partial_cmp(&b.0.distance(p))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some((_, lk)) = call {
            inst.enemy.hear_noise(*lk);
        }
    }
}

/// Choose up to `n` spread-out spawn cells via farthest-point sampling: seed with
/// the cell farthest from the `player`, then repeatedly add the cell that maximises
/// its minimum distance to the already-chosen set. Keeps the hunters spaced apart
/// (not clustered on the single farthest cell) and away from the player's start.
///
/// **Interior bias:** prefers standable cells at least 2 WT from any wall (so the
/// wider-than-a-cell character model doesn't spawn clipping a wall / hanging in a
/// corner); falls back to all standable cells if too few interior ones exist.
/// Returns fewer than `n` when there aren't enough cells.
pub(crate) fn pick_spread_spawns(nav: &NavWorld, player: Vec3, n: usize) -> Vec<Vec3> {
    let all = nav.all_standable();
    let interior: Vec<Vec3> = all
        .iter()
        .copied()
        .filter(|c| nav.wall_clearance_cells(*c, 2) >= 2)
        .collect();
    let cells = if interior.len() >= n { interior } else { all };

    let mut chosen: Vec<Vec3> = Vec::new();
    if cells.is_empty() || n == 0 {
        return chosen;
    }
    // Seed: the standable cell farthest from the player.
    let seed = *cells
        .iter()
        .max_by(|a, b| a.distance_squared(player).total_cmp(&b.distance_squared(player)))
        .unwrap();
    chosen.push(seed);
    while chosen.len() < n && chosen.len() < cells.len() {
        // Add the cell maximising the minimum distance to the chosen set.
        let next = cells.iter().copied().max_by(|a, b| {
            let da = chosen.iter().map(|c| c.distance_squared(*a)).fold(f32::INFINITY, f32::min);
            let db = chosen.iter().map(|c| c.distance_squared(*b)).fold(f32::INFINITY, f32::min);
            da.total_cmp(&db)
        });
        match next {
            // Skip if the best remaining cell is one we already picked (all far
            // cells exhausted) — avoids stacking two hunters on one cell.
            Some(p) if !chosen.iter().any(|c| c.distance_squared(p) < 1e-6) => chosen.push(p),
            _ => break,
        }
    }
    chosen
}
