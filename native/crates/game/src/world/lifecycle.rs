//! `World` lifecycle: fly-cam look, the fixed-step sim loop, the view
//! projection, the BUILD↔HUNT toggle, and the spawn floor probe.

use super::*;
use engine::render::camera::apply_look_delta;

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
                // Is a FIRE one-shot currently animating? (disambiguated from
                // hit/death by the clip index) — the JS `enemyState === 'action'`
                // proxy the FSM's attack→cooldown transition needs.
                let fire_anim = self
                    .char_anim
                    .as_ref()
                    .map(|a| a.is_playing_oneshot() && a.current_clip() == CHAR_FIRE_IDX)
                    .unwrap_or(false);
                // Advance the hunter's perception FSM. Take it out so `self.enemy`
                // isn't borrowed while the FSM needs `&self.nav` + `&mut self.physics`
                // (LOS raycast).
                if let Some(mut enemy) = self.enemy.take() {
                    let step = match self.nav.as_ref() {
                        Some(nav) => enemy.update(dt, feet, nav, &mut self.physics, fire_anim),
                        None => crate::enemy::EnemyStep::default(),
                    };
                    // Keep the hitscan capsule on the hunter each step (marks the
                    // query pipeline dirty so raycasts see the move). Skipped once
                    // dead — the collider is already gone.
                    if !enemy.is_dead() {
                        self.physics.update_enemy_collider(enemy.pos);
                    }
                    let (caught, want_fire) = (step.caught, step.want_fire);
                    self.enemy = Some(enemy);
                    // It entered attack → start a fire burst on the shared mixer.
                    if want_fire {
                        self.start_enemy_fire();
                    }
                    if caught && !self.caught {
                        self.caught = true;
                        log::info!("CAUGHT by the hunter!");
                    }
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
        // A mode switch always ends any death state: drop the hunter's capsule,
        // clear the fade, and revive the model (BUILD demo / a fresh hunt).
        self.physics.remove_enemy_collider();
        self.enemy_fade = None;
        self.char_dead = false;
        // Fresh player-combat state each mode switch (full health, no flash/HUD).
        self.player_health = PLAYER_MAX_HEALTH;
        self.player_armor = 0.0;
        self.player_dead = false;
        self.damage_flash = 0.0;
        self.hud_show_timer = 0.0;
        self.enemy_shot_timer = 0.0;
        self.enemy_muzzle_timer = 0.0;
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
                self.selected = None; // clear any authoring selection
                self.caught = false;

                // Bake the nav grid from the frozen geometry (once) and drop a
                // hunter on the standable cell farthest from the player.
                let t0 = Instant::now();
                let structure_solids = self.structure_solid_boxes();
                match nav::bake(&mut self.regions, &structure_solids) {
                    Some(mut nav) => {
                        let bake_ms = t0.elapsed().as_secs_f32() * 1000.0;
                        log::info!(
                            "nav baked in {bake_ms:.2} ms ({} cells)",
                            nav.cell_count()
                        );
                        if let Some(spawn) = nav
                            .all_standable()
                            .into_iter()
                            .max_by(|a, b| {
                                a.distance_squared(feet)
                                    .total_cmp(&b.distance_squared(feet))
                            })
                        {
                            // Spawn watching toward the player's start so the
                            // perception FSM can engage (a guard on watch).
                            self.enemy = Some(Enemy::new(spawn, feet));
                            // Track A: the hunter's hitscan capsule (moved each
                            // fixed step, removed on death / return to BUILD).
                            self.physics.set_enemy_collider(
                                spawn,
                                ENEMY_RADIUS,
                                ENEMY_HALF_HEIGHT,
                            );
                            log::info!("hunter spawned at {spawn:?}");
                        } else {
                            log::warn!("no standable cell for the hunter");
                        }
                        // Arm breakable doors as a live overlay on the frozen grid
                        // (panel colliders + nav cost). This is the only per-hunt
                        // dynamic layer; the grid itself never re-bakes.
                        self.build_doors(&mut nav);
                        self.nav = Some(nav);
                    }
                    None => log::warn!("nav bake produced no grid"),
                }

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
                self.enemy = None;
                self.caught = false;
                self.sparks.clear();
                self.physics.clear_door_colliders();
                self.doors.clear();
                self.mode = Mode::Build;
                log::info!("→ BUILD");
            }
        }
    }

    /// Raycast straight down from `from` to find the floor; returns feet position.
    pub(crate) fn floor_under(&mut self, from: Vec3) -> Option<Vec3> {
        // Start a little above the camera so we don't begin inside geometry.
        let origin = from + Vec3::new(0.0, 0.1, 0.0);
        let hit = self.physics.raycast(origin, Vec3::NEG_Y, 100.0)?;
        Some(hit.point)
    }
}
