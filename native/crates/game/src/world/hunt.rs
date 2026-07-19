//! HUNT-phase runtime on `World`: the hunter roster's per-frame animation +
//! render data (skinned poses, hand-attached weapons, muzzle flashes),
//! breakable-door build/breach, and the BUILD-phase animation-preview viewer.

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

/// The fire-clip index for a weapon class + dual flag (the `AnimPlayer` layout set
/// in `World::new`): dual → the dual clip regardless of class, else the class clip.
pub(crate) fn fire_clip_index(class: EnemyWeaponClass, dual: bool) -> usize {
    if dual {
        FIRE_DUAL_IDX
    } else {
        match class {
            EnemyWeaponClass::Pistol => FIRE_PISTOL_IDX,
            EnemyWeaponClass::Rifle => FIRE_RIFLE_IDX,
        }
    }
}

/// The FIRE_TIMING shot window for a weapon class + dual flag (seconds into the
/// fire clip). Falls back to the rifle window if a hex id is somehow missing.
pub(crate) fn fire_window_for(class: EnemyWeaponClass, dual: bool) -> (f32, f32) {
    let hex = if dual {
        "7A"
    } else {
        match class {
            EnemyWeaponClass::Pistol => "41",
            EnemyWeaponClass::Rifle => "01",
        }
    };
    anim_set::fire_window(hex).unwrap_or(anim_set::FIRE_WINDOW)
}

/// Whether a clip index is one of the (class-specific) fire clips — the
/// `enemyState === 'action'` proxy the FSM's attack→cooldown transition needs,
/// disambiguated from hit/death one-shots.
pub(crate) fn is_fire_clip(idx: usize) -> bool {
    (FIRE_RIFLE_IDX..=FIRE_DUAL_IDX).contains(&idx)
}

impl EnemyInstance {
    /// Horizontal facing yaw (model faces +Z at yaw 0 → `atan2(x, z)`).
    pub(crate) fn yaw(&self) -> f32 {
        let h = self.enemy.heading();
        h.x.atan2(h.z)
    }

    /// Whole-body opacity this frame: 1 while alive / mid death-anim, ramping 1→0
    /// over [`FADE_DURATION`] once the death animation has finished, then held at 0.
    pub(crate) fn opacity(&self) -> f32 {
        match self.fade {
            Some(t) => (1.0 - t / FADE_DURATION).clamp(0.0, 1.0),
            None => 1.0,
        }
    }
}

impl World {
    /// A combined box mesh for the hunters at their current positions (meters), for
    /// the renderer's entity pass — used ONLY as a fallback when the skinned model
    /// failed to load (otherwise the hunters ARE the skinned characters). `None`
    /// when the model loaded or no hunters are live.
    pub fn enemy_mesh(&self) -> Option<CpuMesh> {
        if self.char_model.is_some() || self.enemies.is_empty() {
            return None;
        }
        let mut positions: Vec<f32> = Vec::new();
        let mut normals: Vec<f32> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for inst in &self.enemies {
            let c = inst.enemy.pos + Vec3::new(0.0, 0.6, 0.0);
            let polys = csg::box_polygons([c.x, c.y, c.z], [0.2, 0.6, 0.2]);
            let (p, n, i) = csg::polygons_to_mesh(&polys);
            let base = (positions.len() / 3) as u32;
            positions.extend_from_slice(&p);
            normals.extend_from_slice(&n);
            indices.extend(i.iter().map(|idx| idx + base));
        }
        Some(CpuMesh::from_csg(&positions, &normals, &indices))
    }

    /// The shared skinned-character CPU model, for one-time GPU upload at startup.
    /// `None` if the asset failed to load.
    pub fn character_model(&self) -> Option<&SkinnedModel> {
        self.char_model.as_ref()
    }

    /// The enemy weapon render library (gun + muzzle meshes for the arsenal), for
    /// one-time GPU upload into the renderer's weapon library at startup.
    pub(crate) fn enemy_weapon_lib(&self) -> &[EnemyWeaponAsset] {
        &self.enemy_weapon_lib
    }

    // ─── BUILD demo viewer (animation preview) ──────────────────────────────────

    /// The weapon class the BUILD demo previews (from the cycled arsenal index).
    fn demo_weapon(&self) -> EnemyWeaponDef {
        enemy_def_for(&crate::combat::config::WEAPONS[self.demo_weapon_idx])
    }

    /// BUILD demo: cycle the previewed weapon through the arsenal (bound to `K`), so
    /// every gun + its class fire animation can be eyeballed without a hunt.
    pub fn cycle_demo_weapon(&mut self) {
        self.demo_weapon_idx = (self.demo_weapon_idx + 1) % crate::combat::config::WEAPONS.len();
        log::info!(
            "demo weapon → {} (dual={})",
            self.demo_weapon().name,
            self.demo_dual
        );
    }

    /// BUILD demo: toggle whether the previewed weapon is shown dual-wielded (`J`).
    pub fn toggle_demo_dual(&mut self) {
        self.demo_dual = !self.demo_dual;
        log::info!("demo dual-wield {}", if self.demo_dual { "ON" } else { "OFF" });
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

    /// B4 demo: fire the previewed weapon's class fire one-shot (with its
    /// FIRE_TIMING window), returning to the current locomotion when done.
    /// Suppressed while dead.
    pub fn char_fire(&mut self) {
        if self.char_dead {
            return;
        }
        let band = self.demo_band;
        let def = self.demo_weapon();
        let idx = fire_clip_index(def.class, self.demo_dual);
        let win = fire_window_for(def.class, self.demo_dual);
        if let Some(anim) = &mut self.char_anim {
            anim.play_once(idx, 0.15, Some(band), Some(win));
            log::info!("fire ({}{})", def.name, if self.demo_dual { " ×2" } else { "" });
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

    /// xorshift64 → an index in `[0, n)`. Shared by the demo picks and the Track A
    /// hit/death/pain rolls.
    pub(crate) fn rand_below(&mut self, n: usize) -> usize {
        let mut x = self.char_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.char_rng = x;
        (x % n.max(1) as u64) as usize
    }

    /// Advance every hunter's mixer (HUNT) or the demo viewer (BUILD) once per
    /// render frame (JS `mixer.update(delta)` cadence). Position/facing come from
    /// each hunter's nav/AI-driven [`Enemy`] (the model is purely visual); the demo
    /// paces a circle by the current band. Doesn't stomp a fire/hit/death one-shot.
    pub fn advance_animation(&mut self, dt: f32) {
        if !self.is_build() {
            // HUNT: each hunter animates independently from its own AI state.
            for inst in &mut self.enemies {
                // Death fade: hold the corpse opaque THROUGH the death animation,
                // then ramp opacity 1→0 once the clip has clamped (`oneshot_finished`).
                if inst.enemy.is_dead() {
                    if inst.anim.oneshot_finished() {
                        let t = inst.fade.get_or_insert(0.0);
                        *t = (*t + dt).min(FADE_DURATION);
                    }
                } else if !inst.anim.is_playing_oneshot() {
                    // Not mid fire/hit → keep the locomotion band in sync with speed.
                    inst.anim.play(band_for_speed(inst.enemy.speed()), 0.15);
                }
                inst.anim.update(dt);
            }
            return;
        }

        // BUILD demo: stand still during a one-shot / death, else pace the circle at
        // the current band's speed facing the travel tangent.
        if self.char_anim.is_none() {
            return;
        }
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
        let anim = self.char_anim.as_mut().unwrap();
        anim.update(dt);
        let open = anim.fire_window_open();
        if open != self.char_fire_open {
            self.char_fire_open = open;
            log::info!("{}", if open { "  fire window OPEN — shot" } else { "  fire window closed" });
        }
    }

    /// World transform placing a character (feet at `feet`, facing `yaw`) with the
    /// feet-seating offset + `CHAR_SCALE` — the model root the skinned pose + any
    /// bone-attached weapon are expressed under.
    fn char_transform(&self, feet: Vec3, yaw: f32) -> Mat4 {
        let pos = Vec3::new(feet.x, feet.y + self.char_feet_offset, feet.z);
        Mat4::from_translation(pos)
            * Mat4::from_rotation_y(yaw)
            * Mat4::from_scale(Vec3::splat(CHAR_SCALE))
    }

    /// Every skinned character to draw this frame as `(model, joint matrices,
    /// opacity, blood_colors)`: in BUILD the single demo viewer (clean/white), in
    /// HUNT one per live hunter (each its own mid-crossfade pose, positioned/faced by
    /// its AI, faded on death, with its accumulated per-vertex blood).
    pub fn character_instances(&self) -> Vec<(Mat4, Vec<Mat4>, f32, &[f32])> {
        let Some(m) = self.char_model.as_ref() else {
            return Vec::new();
        };
        if self.is_build() {
            let joints = match &self.char_anim {
                Some(anim) => anim.skinning_matrices(&m.skeleton),
                None => m.skeleton.bind_pose_matrices(),
            };
            vec![(
                self.char_transform(self.char_pos, self.char_yaw),
                joints,
                1.0,
                self.demo_blood.as_slice(),
            )]
        } else {
            self.enemies
                .iter()
                .map(|inst| {
                    let joints = inst.anim.skinning_matrices(&m.skeleton);
                    let model = self.char_transform(inst.enemy.pos, inst.yaw());
                    (model, joints, inst.opacity(), inst.blood.as_slice())
                })
                .collect()
        }
    }

    /// World transform of a weapon attached to a character's hand bone
    /// (`char_model · bone_global · local_offset`, the JS `bone.add(gun)`). `left`
    /// selects the left-hand (dual) bone + offset. Offsets are GE bone-local units,
    /// converted to metres by the model's scale.
    fn weapon_world(
        &self,
        anim: &AnimPlayer,
        feet: Vec3,
        yaw: f32,
        def: &EnemyWeaponDef,
        left: bool,
    ) -> Option<Mat4> {
        let m = self.char_model.as_ref()?;
        let bone_name = if left { LEFT_HAND_BONE } else { RIGHT_HAND_BONE };
        let bone = m.skeleton.index_of(bone_name)?;
        let bone_global = *anim.joint_global_transforms(&m.skeleton).get(bone)?;
        let (off, rot) = if left {
            (def.left_offset, def.left_rot)
        } else {
            (def.right_offset, def.right_rot)
        };
        let offset = Mat4::from_translation(off)
            * Mat4::from_euler(EulerRot::XYZ, rot.x, rot.y, rot.z);
        Some(self.char_transform(feet, yaw) * bone_global * offset)
    }

    /// The enemy weapon draws this frame: `(weapon name, view_proj · world)` for
    /// each gun to render. In HUNT, one per live hunter (two for a dual-wielder,
    /// left + right hand); a dead hunter drops its gun. In BUILD, the demo preview
    /// weapon on the demo character. Keyed by name so the renderer looks up the mesh.
    pub fn enemy_weapon_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4)> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        if self.is_build() {
            let Some(anim) = self.char_anim.as_ref() else { return out };
            let def = self.demo_weapon();
            if let Some(w) = self.weapon_world(anim, self.char_pos, self.char_yaw, &def, false) {
                out.push((def.name, vp * w));
            }
            if self.demo_dual {
                if let Some(w) = self.weapon_world(anim, self.char_pos, self.char_yaw, &def, true) {
                    out.push((def.name, vp * w));
                }
            }
            return out;
        }
        for inst in &self.enemies {
            if inst.enemy.is_dead() {
                continue; // drop the gun on death
            }
            if let Some(w) = self.weapon_world(&inst.anim, inst.enemy.pos, inst.yaw(), &inst.weapon, false) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&inst.anim, inst.enemy.pos, inst.yaw(), &inst.weapon, true) {
                    out.push((inst.weapon.name, vp * w));
                }
            }
        }
        // In-flight explosive projectiles that carry a GLB (the grenade rounds) ride
        // the same world-space weapon-draw path, keyed by their model name. Tumbling
        // while airborne, frozen once settled. The rocket (`model == ""`) is skipped
        // here — it shows as the procedural streak in `spark_mesh`.
        for p in &self.projectiles {
            if p.spec.model.is_empty() {
                continue;
            }
            let spin = if p.at_rest { 0.0 } else { p.age };
            let world = Mat4::from_translation(p.pos)
                * Mat4::from_euler(
                    EulerRot::XYZ,
                    spin * PROJECTILE_SPIN_X,
                    spin * PROJECTILE_SPIN_Y,
                    0.0,
                )
                * Mat4::from_scale(Vec3::splat(PROJECTILE_MODEL_SCALE));
            out.push((p.spec.model, vp * world));
        }
        // Placed mines ride the same world-space draw path, keyed by their weapon
        // name, oriented flat to the surface they're stuck to (the model's +Y up is
        // rotated onto the surface normal).
        for m in &self.mines {
            let orient = glam::Quat::from_rotation_arc(Vec3::Y, m.normal.normalize_or_zero());
            let world = Mat4::from_translation(m.pos)
                * Mat4::from_quat(orient)
                * Mat4::from_scale(Vec3::splat(MINE_MODEL_SCALE));
            out.push((m.model, vp * world));
        }
        out
    }

    /// The enemy muzzle-flash draws this frame (same bone frames as the guns),
    /// shown only while a shot's flash is active: in HUNT per live firing hunter
    /// (both hands when dual), in BUILD while the demo is inside its fire window.
    pub fn enemy_muzzle_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4)> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        if self.is_build() {
            let Some(anim) = self.char_anim.as_ref() else { return out };
            if !anim.fire_window_open() {
                return out;
            }
            let def = self.demo_weapon();
            if let Some(w) = self.weapon_world(anim, self.char_pos, self.char_yaw, &def, false) {
                out.push((def.name, vp * w));
            }
            if self.demo_dual {
                if let Some(w) = self.weapon_world(anim, self.char_pos, self.char_yaw, &def, true) {
                    out.push((def.name, vp * w));
                }
            }
            return out;
        }
        for inst in &self.enemies {
            if inst.enemy.is_dead() || inst.muzzle_timer <= 0.0 {
                continue;
            }
            if let Some(w) = self.weapon_world(&inst.anim, inst.enemy.pos, inst.yaw(), &inst.weapon, false) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&inst.anim, inst.enemy.pos, inst.yaw(), &inst.weapon, true) {
                    out.push((inst.weapon.name, vp * w));
                }
            }
        }
        out
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
