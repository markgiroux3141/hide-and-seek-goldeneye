//! `World` lifecycle: fly-cam look, the fixed-step sim loop, the view
//! projection, the BUILD↔HUNT toggle, and the spawn floor probe.

use super::*;

impl World {
    /// Apply mouse-look — once per rendered frame, so aim is decoupled from the
    /// fixed sim rate.
    pub fn look(&mut self, input: &mut InputState) {
        match self.mode {
            Mode::Build => self.camera.apply_look(input),
            Mode::Hunt => {
                if let Some(c) = self.character.as_mut() {
                    c.apply_look(input);
                }
            }
        }
    }

    /// Advance movement/physics by one fixed timestep.
    pub fn fixed_step(&mut self, dt: f32, input: &InputState) {
        match self.mode {
            Mode::Build => self.camera.apply_move(dt, input),
            Mode::Hunt => {
                let Some(c) = self.character.as_mut() else { return };
                c.apply_move(dt, input, &mut self.physics);
                let feet = c.pos;
                // Advance the hunter toward the player over the baked grid.
                let step = match (self.nav.as_ref(), self.enemy.as_mut()) {
                    (Some(nav), Some(enemy)) => Some(enemy.update(dt, feet, nav)),
                    _ => None,
                };
                if let Some(step) = step {
                    // A blocking door: drain its hp; the breach itself flips the
                    // nav flag + drops the collider with no re-bake.
                    if let Some(di) = step.breaching {
                        self.breach_tick(di, dt);
                    }
                    if step.caught && !self.caught {
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
                            self.enemy = Some(Enemy::new(spawn));
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
