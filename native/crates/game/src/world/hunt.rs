//! HUNT-phase runtime on `World`: breakable-door build/breach and the
//! per-frame enemy/door render meshes.

use super::*;

/// Locomotion band (clip index 0=idle,1=walk,2=jog,3=run) for a speed (m/s),
/// matching the JS `_playLocomotion` thresholds.
pub(crate) fn band_for_speed(speed: f32) -> usize {
    if speed >= anim_set::SPEED_RUN {
        3
    } else if speed >= anim_set::SPEED_JOG {
        2
    } else if speed > 0.0 {
        1
    } else {
        0
    }
}

impl World {
    /// A box mesh for the hunter at its current position (meters), for the
    /// renderer's entity pass. `None` when no hunter is active.
    pub fn enemy_mesh(&self) -> Option<CpuMesh> {
        // B5: when the skinned character loaded, IT is the hunter (drawn via the
        // skinned pipeline in `character_pose`), so no placeholder box. The box
        // remains only as a fallback when the model failed to load.
        if self.char_model.is_some() {
            return None;
        }
        let e = self.enemy.as_ref()?;
        // Capsule-sized box: 0.5 × 1.5 × 0.5 m, centered above the feet.
        let c = e.pos + Vec3::new(0.0, 0.75, 0.0);
        let polys = csg::box_polygons([c.x, c.y, c.z], [0.25, 0.75, 0.25]);
        let (p, n, i) = csg::polygons_to_mesh(&polys);
        Some(CpuMesh::from_csg(&p, &n, &i))
    }

    /// The B1 skinned character's CPU model, for one-time GPU upload at startup.
    /// `None` if the asset failed to load.
    pub fn character_model(&self) -> Option<&SkinnedModel> {
        self.char_model.as_ref()
    }

    /// B3 demo: cycle the locomotion band idle → walk → jog → run → idle. Bound
    /// to `L`. Crossfades to the band's clip over 0.15 s (JS `crossFadeFrom`).
    /// Also revives the character if it was in a death pose.
    pub fn cycle_char_speed(&mut self) {
        self.char_dead = false;
        self.demo_band = (self.demo_band + 1) % LOCO_SPEEDS.len();
        if let Some(anim) = &mut self.char_anim {
            anim.play(self.demo_band, 0.15);
        }
        log::info!(
            "locomotion band {} ({:.1} m/s)",
            self.demo_band,
            LOCO_SPEEDS[self.demo_band]
        );
    }

    /// B4 demo: fire the standing rifle one-shot (with its FIRE_TIMING window),
    /// returning to the current locomotion when done. Suppressed while dead.
    pub fn char_fire(&mut self) {
        if self.char_dead {
            return;
        }
        let band = self.demo_band;
        if let Some(anim) = &mut self.char_anim {
            anim.play_once(CHAR_FIRE_IDX, 0.15, Some(band), Some(anim_set::FIRE_WINDOW));
            log::info!("fire");
        }
    }

    /// B4 demo: play a random hit reaction, returning to locomotion when done.
    pub fn char_hit(&mut self) {
        if self.char_dead {
            return;
        }
        let idx = CHAR_HIT_START + self.rand_below(anim_set::HIT_CLIPS.len());
        let band = self.demo_band;
        if let Some(anim) = &mut self.char_anim {
            anim.play_once(idx, 0.1, Some(band), None);
            log::info!("hit ({})", anim_set::HIT_CLIPS[idx - CHAR_HIT_START]);
        }
    }

    /// B4 demo: play a random death (clamps on the last frame — body stays down;
    /// press `L` to revive).
    pub fn char_death(&mut self) {
        if self.char_dead {
            return;
        }
        let death_start = CHAR_HIT_START + anim_set::HIT_CLIPS.len();
        let pick = self.rand_below(anim_set::DEATH_CLIPS.len());
        let idx = death_start + pick;
        self.char_dead = true;
        if let Some(anim) = &mut self.char_anim {
            anim.play_once(idx, 0.15, None, None);
            log::info!("death ({}) — press L to reset", anim_set::DEATH_CLIPS[pick]);
        }
    }

    /// xorshift64 → an index in `[0, n)`. A demo/random pick, not a statistical
    /// roll (combat can bring `rand` if it ever needs a real distribution).
    pub(crate) fn rand_below(&mut self, n: usize) -> usize {
        let mut x = self.char_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.char_rng = x;
        (x % n.max(1) as u64) as usize
    }

    /// Advance the character each render frame (JS `mixer.update(delta)` cadence):
    /// walk the demo circle at the current band's speed, face the direction of
    /// travel, keep the clip selection in sync, and step the crossfade mixer.
    pub fn advance_animation(&mut self, dt: f32) {
        if self.char_anim.is_none() {
            return;
        }

        // B5: in HUNT the model IS the hunter — position/facing/locomotion band
        // come from the nav/AI-driven enemy (the model is purely visual, mirroring
        // the JS separation of movement from the mixer). In BUILD it's the demo
        // viewer paced around a circle by the `L`/`Z`/`N`/`M` keys.
        let hunt = !self.is_build() && self.enemy.is_some();
        if hunt {
            let e = self.enemy.as_ref().unwrap();
            let (pos, heading, speed) = (e.pos, e.heading(), e.speed());
            self.char_pos = pos;
            // `heading` is the hunter's facing (travel dir while chasing, toward
            // the player while alert/attack/cooldown), so aim it always — the model
            // faces the player while shooting even though it's stopped. Model faces
            // +Z at yaw 0 → yaw = atan2(vx, vz).
            self.char_yaw = heading.x.atan2(heading.z);
            // Death fade: hold the corpse fully opaque THROUGH the death animation,
            // then fade opacity 1→0 over FADE_DURATION once the clip has clamped on
            // its last frame (`oneshot_finished`). Starting it at the moment of death
            // faded the body out during the animation.
            if self.char_dead {
                let finished = self
                    .char_anim
                    .as_ref()
                    .map(|a| a.oneshot_finished())
                    .unwrap_or(true);
                if finished {
                    let t = self.enemy_fade.get_or_insert(0.0);
                    *t = (*t + dt).min(FADE_DURATION);
                }
            }
            // Drive locomotion — but DON'T stomp a hit/death one-shot (Track A).
            // Dead → the death clamp holds (char_dead). Mid hit-reaction → let the
            // one-shot play; it auto-returns to a loop, then locomotion resumes.
            // Otherwise select the band from the hunter's speed (chase 2.8 → walk;
            // stopped/stunned/breaching → idle).
            let anim = self.char_anim.as_mut().unwrap();
            if !self.char_dead && !anim.is_playing_oneshot() {
                anim.play(band_for_speed(speed), 0.15);
            }
        } else {
            // BUILD demo: stand still during a one-shot / death, else pace the
            // circle at the current band's speed facing the travel tangent.
            let oneshot = self.char_anim.as_ref().unwrap().is_playing_oneshot();
            let speed = if !oneshot && !self.char_dead {
                LOCO_SPEEDS[self.demo_band]
            } else {
                0.0
            };
            if speed > 0.0 {
                self.demo_angle =
                    (self.demo_angle + speed / DEMO_RADIUS * dt).rem_euclid(std::f32::consts::TAU);
                let (s, c) = self.demo_angle.sin_cos();
                self.char_pos = DEMO_CENTER + Vec3::new(DEMO_RADIUS * c, 0.0, DEMO_RADIUS * s);
                self.char_yaw = (-s).atan2(c);
            }
        }

        // Advance the clocks + crossfade (clip selection happens above / on
        // keypress — re-`play`ing here would be fine now but is unnecessary).
        let anim = self.char_anim.as_mut().unwrap();
        anim.update(dt);
        let open = anim.fire_window_open();
        if open != self.char_fire_open {
            self.char_fire_open = open;
            log::info!("{}", if open { "  fire window OPEN — shot" } else { "  fire window closed" });
        }
    }

    /// Whole-character opacity this frame: 1 while alive, ramping 1→0 over
    /// [`FADE_DURATION`] once the hunter is killed (Track A death fade), then held
    /// at 0. Fed to the skinned shader's opacity uniform.
    pub fn character_opacity(&self) -> f32 {
        match self.enemy_fade {
            Some(t) => (1.0 - t / FADE_DURATION).clamp(0.0, 1.0),
            None => 1.0,
        }
    }

    /// The character's world placement, joint (skinning) matrices, and opacity
    /// this frame: the mixer's (possibly mid-crossfade) pose, positioned + faced
    /// by the demo/enemy mover, faded on death. `None` if the character isn't loaded.
    pub fn character_pose(&self) -> Option<(Mat4, Vec<Mat4>, f32)> {
        let m = self.char_model.as_ref()?;
        // `char_pos` is the feet position (floor y); `char_feet_offset` lifts the
        // model origin so the feet sit on that floor. In BUILD the floor is y=0;
        // in HUNT it's the hunter's nav-cell y.
        let pos = Vec3::new(
            self.char_pos.x,
            self.char_pos.y + self.char_feet_offset,
            self.char_pos.z,
        );
        let model = Mat4::from_translation(pos)
            * Mat4::from_rotation_y(self.char_yaw)
            * Mat4::from_scale(Vec3::splat(CHAR_SCALE));
        let joints = match &self.char_anim {
            Some(anim) => anim.skinning_matrices(&m.skeleton),
            None => m.skeleton.bind_pose_matrices(),
        };
        Some((model, joints, self.character_opacity()))
    }

    /// The hunter's rifle mesh (for one-time GPU upload). `None` if it failed to load.
    pub fn enemy_gun_model(&self) -> Option<&TexturedModel> {
        self.enemy_gun_model.as_ref()
    }

    /// The hunter's muzzle-flash mesh (for one-time GPU upload).
    pub fn enemy_muzzle_model(&self) -> Option<&TexturedModel> {
        self.enemy_muzzle_model.as_ref()
    }

    /// World transform of the rifle attached to the hunter's hand bone
    /// (`char_model · Bone_9_global · local_offset`, the JS `bone.add(gun)`). The
    /// offset is in GE bone-local units, converted to metres by the model's scale.
    fn enemy_weapon_world(&self) -> Option<Mat4> {
        let m = self.char_model.as_ref()?;
        let anim = self.char_anim.as_ref()?;
        let bone = m.skeleton.index_of(ENEMY_HAND_BONE)?;
        let bone_global = *anim.joint_global_transforms(&m.skeleton).get(bone)?;
        let pos = Vec3::new(
            self.char_pos.x,
            self.char_pos.y + self.char_feet_offset,
            self.char_pos.z,
        );
        let char_model = Mat4::from_translation(pos)
            * Mat4::from_rotation_y(self.char_yaw)
            * Mat4::from_scale(Vec3::splat(CHAR_SCALE));
        let offset = Mat4::from_translation(ENEMY_GUN_OFFSET)
            * Mat4::from_euler(EulerRot::XYZ, ENEMY_GUN_ROT.x, ENEMY_GUN_ROT.y, ENEMY_GUN_ROT.z);
        Some(char_model * bone_global * offset)
    }

    /// The hunter rifle's clip transform this frame (`view_proj · weapon_world`), or
    /// `None` when it shouldn't render — outside HUNT, no hunter, dead (drops the
    /// gun), or the asset failed to load. Depth-tested in the forward pass.
    pub fn enemy_weapon_transform(&self, aspect: f32) -> Option<Mat4> {
        if self.mode != Mode::Hunt
            || self.enemy.is_none()
            || self.char_dead
            || self.enemy_gun_model.is_none()
        {
            return None;
        }
        Some(self.view_proj(aspect) * self.enemy_weapon_world()?)
    }

    /// The hunter's muzzle-flash clip transform (same bone frame as the rifle),
    /// shown only while a shot's flash is active.
    pub fn enemy_muzzle_transform(&self, aspect: f32) -> Option<Mat4> {
        if self.mode != Mode::Hunt
            || self.char_dead
            || self.enemy_muzzle_model.is_none()
            || self.enemy_muzzle_timer <= 0.0
        {
            return None;
        }
        Some(self.view_proj(aspect) * self.enemy_weapon_world()?)
    }

    /// Door breach/blocking is **disabled for now** (user call, 2026-07-16: "get
    /// rid of the door breach thing … no things blocking the doors on gameplay")
    /// so the enemy-combat work can be tested without doors interfering. Doors are
    /// therefore open passages during the hunt: no panel colliders (the player
    /// walks through), no nav door cost (the hunter walks through), and no panels
    /// rendered ([`World::door_mesh`] reads the empty `doors` vec). The breach
    /// system (`Door`, `breach_tick`, the nav overlay) is left intact for a future
    /// re-enable. Called once at G→HUNT; `nav` is untouched.
    pub(crate) fn build_doors(&mut self, _nav: &mut NavWorld) {
        self.doors.clear();
    }

    /// Drain a breaching door's hp; on break, remove its panel collider and flip
    /// the live nav flag. Currently unused ([`Self::build_doors`] is a no-op while
    /// breach is disabled) but retained for the re-enable. **The thesis in code:**
    /// a built element is destroyed and both collision and nav react instantly —
    /// one collider gone, one bool flipped — with **no re-voxel/CSG re-eval**.
    #[allow(dead_code)]
    pub(crate) fn breach_tick(&mut self, di: usize, dt: f32) {
        let broke = {
            let Some(door) = self.doors.get_mut(di) else { return };
            if door.broken {
                return;
            }
            door.hp -= dt;
            if door.hp <= 0.0 {
                door.broken = true;
                Some(door.panel)
            } else {
                None
            }
        };
        if let Some(panel) = broke {
            self.physics.remove_door_collider(panel);
            if let Some(nav) = self.nav.as_mut() {
                nav.break_door(di);
            }
            log::info!(
                "DOOR {di} BREACHED — panel collider removed + nav flag flipped, no re-bake"
            );
        }
    }

    /// A combined mesh of every intact door panel (meters), for the renderer's
    /// door pass. `None` when no intact doors remain — so a breached door simply
    /// vanishes. Cheap to regenerate (a handful of boxes).
    pub fn door_mesh(&self) -> Option<CpuMesh> {
        let mut positions: Vec<f32> = Vec::new();
        let mut normals: Vec<f32> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for door in self.doors.iter().filter(|d| !d.broken) {
            let b = &door.aabb;
            let c = [
                (b.x + b.w * 0.5) * WORLD_SCALE,
                (b.y + b.h * 0.5) * WORLD_SCALE,
                (b.z + b.d * 0.5) * WORLD_SCALE,
            ];
            let half = [
                b.w * 0.5 * WORLD_SCALE,
                b.h * 0.5 * WORLD_SCALE,
                b.d * 0.5 * WORLD_SCALE,
            ];
            let polys = csg::box_polygons(c, half);
            let (p, n, i) = csg::polygons_to_mesh(&polys);
            let base = (positions.len() / 3) as u32;
            positions.extend_from_slice(&p);
            normals.extend_from_slice(&n);
            indices.extend(i.iter().map(|idx| idx + base));
        }
        if indices.is_empty() {
            return None;
        }
        Some(CpuMesh::from_csg(&positions, &normals, &indices))
    }
}
