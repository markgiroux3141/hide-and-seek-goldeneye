//! HUNT-phase runtime on `World`: the hunter roster's per-frame animation +
//! render data (skinned poses, hand-attached weapons, muzzle flashes),
//! breakable-door build/breach, and the BUILD-phase animation-preview viewer.

use super::*;

/// Character model matrix (feet-seated, faced, scaled) — the `&self`-free core of
/// [`World::char_transform`], so the aim post-pass can build it inside the
/// `&mut enemies` loop without reborrowing `self`.
pub(crate) fn char_transform_raw(feet: Vec3, yaw: f32, feet_off: f32) -> Mat4 {
    let pos = Vec3::new(feet.x, feet.y + feet_off, feet.z);
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y(yaw)
        * Mat4::from_scale(Vec3::splat(CHAR_SCALE))
}

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
    idx == FIRE_RIFLE_IDX || idx == FIRE_PISTOL_IDX || idx == FIRE_DUAL_IDX
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

    /// xorshift64 → an index in `[0, n)`. Drives the Track A hit/death/pain rolls.
    pub(crate) fn rand_below(&mut self, n: usize) -> usize {
        let mut x = self.char_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.char_rng = x;
        (x % n.max(1) as u64) as usize
    }

    /// Advance every hunter's animation mixer once per render frame (HUNT only; JS
    /// `mixer.update(delta)` cadence). Position/facing come from each hunter's
    /// nav/AI-driven [`Enemy`] (the model is purely visual); a fire/hit/death one-shot
    /// isn't stomped. No-op in BUILD (nothing animated there).
    pub fn advance_animation(&mut self, dt: f32) {
        if self.is_build() {
            // Spike: the BUILD-phase procedural-anim preview (if toggled on).
            self.advance_procedural_preview(dt);
            return;
        }
        // Player aim point (chest) + feet-seat offset, pulled out before the loop so
        // it doesn't clash with the per-hunter `&mut` borrow.
        let aim_point = self.player_pos().map(|p| p + Vec3::Y * PLAYER_AIM_Y);
        let feet_off = self.char_feet_offset;
        let arm = self.enemy_arm; // Copy — right-hand joint index for the foregrip
        // Skeleton borrow is a DISJOINT field from `enemies`, so both can be held.
        let skeleton = self.char_model.as_ref().map(|m| &m.skeleton);

        for inst in &mut self.enemies {
            // Low-pass the AI's per-step speed → a continuous locomotion speed that
            // drives the blend layer, so the gait eases smoothly (no band thresholds).
            inst.anim_speed +=
                (inst.enemy.speed() - inst.anim_speed) * (1.0 - (-dt * LOCO_SMOOTH).exp());
            // Snap the decay tail to a hard stop (only when actually stopped) so the
            // blend settles to pure idle rather than a sliver of walk forever.
            if inst.enemy.speed() <= 0.0 && inst.anim_speed < LOCO_IDLE_EPS {
                inst.anim_speed = 0.0;
            }

            // Death fade: hold the corpse opaque THROUGH the death animation, then
            // ramp opacity 1→0 once the clip has clamped (`oneshot_finished`).
            if inst.enemy.is_dead() && inst.anim.oneshot_finished() {
                let t = inst.fade.get_or_insert(0.0);
                *t = (*t + dt).min(FADE_DURATION);
            }
            // The mixer now carries ONLY the hit/death one-shots — locomotion is the
            // continuous blend layer below. Advance it so a one-shot plays out.
            inst.anim.update(dt);

            // ── Procedural post-pass: locomotion blend → aim → recoil. ──
            let Some(sk) = skeleton else {
                continue; // no skinned model → nothing to pose
            };
            // A hit/death one-shot takes over the whole body: feed its pose as the
            // base and bypass locomotion + aim. `is_fire_clip` guard is vestigial
            // (fire is a timer, never on the mixer) but keeps the check honest.
            let one_shot =
                inst.anim.is_playing_oneshot() && !is_fire_clip(inst.anim.current_clip());
            if let Some(loco) = inst.stack.layer_as::<LocomotionBlendLayer>(ENEMY_LOCO_LAYER) {
                loco.speed = inst.anim_speed;
                loco.enabled = !one_shot;
            }
            let engaged = matches!(
                inst.enemy.state(),
                AiState::Alert | AiState::Chase | AiState::Attack | AiState::Cooldown
            );
            let want_aim = engaged && !inst.enemy.is_dead() && !one_shot;
            let target_w = if want_aim { 1.0 } else { 0.0 };
            // Exponential ease toward the target weight (frame-rate independent).
            inst.aim_weight += (target_w - inst.aim_weight) * (1.0 - (-dt * AIM_RAMP).exp());

            // Aim at the player in model space. The IK layer (reach-fraction mode)
            // takes this as a far aim point and reaches `AIM_REACH_FRAC` of its own
            // current arm length toward it, so the elbow extends the same amount
            // wherever the shoulder currently sits (anchoring to a stored shoulder
            // under-reached and left the arm bent).
            // Aim: the IK places the hand on the line to the player; the look-at
            // (aim_axis fixed at spawn from the gun's barrel) rotates the hand so the
            // muzzle points exactly at the player.
            if let Some(ap) = aim_point {
                let ct = char_transform_raw(inst.enemy.pos, inst.yaw(), feet_off);
                let model_p = ct.inverse().transform_point3(ap);
                if let Some(ik) = inst.stack.layer_as::<TwoBoneIkLayer>(ENEMY_IK_LAYER) {
                    ik.target = model_p;
                    ik.weight = inst.aim_weight;
                }
                if let Some(look) = inst.stack.layer_as::<LookAtLayer>(ENEMY_LOOK_LAYER) {
                    look.target = model_p;
                    look.weight = inst.aim_weight;
                }
            } else {
                if let Some(ik) = inst.stack.layer_as::<TwoBoneIkLayer>(ENEMY_IK_LAYER) {
                    ik.weight = inst.aim_weight;
                }
                if let Some(look) = inst.stack.layer_as::<LookAtLayer>(ENEMY_LOOK_LAYER) {
                    look.weight = inst.aim_weight;
                }
            }

            // Two-handed foregrip: reach the left hand to a point on the gun (rifles
            // only — `grip_local` is None otherwise). Target uses the right hand's
            // global from LAST frame's pose (a 1-frame lag, invisible), so the off
            // hand tracks wherever the aim/recoil put the gun.
            let grip_target = match (inst.grip_local, inst.final_pose.as_ref(), arm) {
                (Some(gl), Some(fp), Some(a)) => fp
                    .joint_global_transforms(sk)
                    .get(a.end)
                    .map(|g9| g9.transform_point3(gl)),
                _ => None,
            };
            if let Some(lik) = inst.stack.layer_as::<TwoBoneIkLayer>(ENEMY_LGRIP_LAYER) {
                match grip_target {
                    Some(t) => {
                        lik.target = t;
                        lik.enabled = true;
                        lik.weight = inst.aim_weight;
                    }
                    None => lik.weight = 0.0, // one-handed / no pose yet → left arm free
                }
            }

            // Base pose: the hit/death one-shot when active, else the bind pose for
            // the locomotion blend layer to overwrite with the current gait.
            let base = if one_shot {
                inst.anim.pose(sk)
            } else {
                Pose::bind(sk)
            };
            let ctx = LayerCtx { skeleton: sk, dt };
            inst.final_pose = Some(inst.stack.evaluate(base, &ctx));
        }

        self.log_anim_debug();
    }

    /// `ANIM_DEBUG=1`: dump the nearest engaged hunter's per-frame state so
    /// run-and-gun jank can be diagnosed from a pasted log. One line/frame; watch
    /// for `band` flip-flopping, `yaw` jumping (spin), `pos` moving in chunks
    /// (fixed-step stutter), or `aimw`/recoil churn.
    fn log_anim_debug(&mut self) {
        if !self.anim_debug {
            return;
        }
        let Some(p) = self.player_pos() else { return };
        let mut best: Option<(usize, f32)> = None;
        for (i, inst) in self.enemies.iter().enumerate() {
            if inst.enemy.is_dead()
                || !matches!(
                    inst.enemy.state(),
                    AiState::Alert | AiState::Chase | AiState::Attack | AiState::Cooldown
                )
            {
                continue;
            }
            let d = (inst.enemy.pos - p).length();
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        let Some((i, d)) = best else { return };
        let inst = &self.enemies[i];
        let spd = inst.enemy.speed();
        // Band as ACTUALLY selected (from the smoothed speed), not the raw speed —
        // the raw-speed band masked the walk-in-place decay tail last time.
        let band = band_for_speed(inst.anim_speed);
        let h = inst.enemy.heading();
        let yaw = h.x.atan2(h.z);
        let fire = inst
            .fire_elapsed
            .map(|t| format!("{t:.2}"))
            .unwrap_or_else(|| "-".into());
        // Measure the actual arm the IK produced: how far the hand reaches from
        // the shoulder (as a fraction of full arm length) and the elbow interior
        // angle. reach≈0.97 / elbow≈150°+ = straight; reach≈0.7 / elbow≈90° = bent.
        // Also measure the rendered right foot so we can see if the LEGS actually
        // move while "stopped" (foot pos changing frame-to-frame = walking legs),
        // and log the clip the mixer is really playing vs the band we requested.
        let (reach_frac, elbow_deg, foot) = match (self.enemy_arm, inst.final_pose.as_ref(), self.char_model.as_ref()) {
            (Some(arm), Some(fp), Some(m)) => {
                let g = fp.joint_global_transforms(&m.skeleton);
                let sa = g[arm.root].to_scale_rotation_translation().2;
                let el = g[arm.mid].to_scale_rotation_translation().2;
                let ha = g[arm.end].to_scale_rotation_translation().2;
                let arm_len = (el - sa).length() + (ha - el).length();
                let frac = if arm_len > 1e-5 { (ha - sa).length() / arm_len } else { 0.0 };
                let elbow = (sa - el)
                    .normalize_or_zero()
                    .dot((ha - el).normalize_or_zero())
                    .clamp(-1.0, 1.0)
                    .acos()
                    .to_degrees();
                let foot = m
                    .skeleton
                    .index_of("Bone_14")
                    .map(|f| g[f].to_scale_rotation_translation().2)
                    .unwrap_or(Vec3::ZERO);
                (frac, elbow, foot)
            }
            _ => (0.0, 0.0, Vec3::ZERO),
        };
        self.anim_dbg_frame = self.anim_dbg_frame.wrapping_add(1);
        log::info!(
            "[anim {:>4}] e{i} {:?} d={d:.2} spd={spd:.1} band={band} clip={} yaw={yaw:+.3} aimw={:.2} fire={fire} reach={reach_frac:.2} elbow={elbow_deg:.0} foot=({:.0},{:.0},{:.0}) pos=({:.2},{:.2})",
            self.anim_dbg_frame,
            inst.enemy.state(),
            inst.anim.current_clip(),
            inst.aim_weight,
            foot.x,
            foot.y,
            foot.z,
            inst.enemy.pos.x,
            inst.enemy.pos.z,
        );
    }

    /// World transform placing a character (feet at `feet`, facing `yaw`) with the
    /// feet-seating offset + `CHAR_SCALE` — the model root the skinned pose + any
    /// bone-attached weapon are expressed under.
    fn char_transform(&self, feet: Vec3, yaw: f32) -> Mat4 {
        char_transform_raw(feet, yaw, self.char_feet_offset)
    }

    /// Every skinned character to draw this frame as `(model, joint matrices,
    /// opacity, blood_colors)` — one per live hunter (each its own mid-crossfade pose,
    /// positioned/faced by its AI, faded on death, with its accumulated per-vertex
    /// blood). Empty in BUILD (no character is drawn while authoring).
    pub fn character_instances(&self) -> Vec<(Mat4, Vec<Mat4>, f32, &[f32])> {
        let Some(m) = self.char_model.as_ref() else {
            return Vec::new();
        };
        // Spike: in BUILD the only character is the optional procedural preview.
        if self.is_build() {
            return match self.procedural_preview.as_ref() {
                Some(p) => vec![(
                    self.char_transform(p.feet, p.yaw),
                    p.joints.clone(),
                    1.0,
                    p.blood.as_slice(),
                )],
                None => Vec::new(),
            };
        }
        self.enemies
            .iter()
            .map(|inst| {
                // Prefer the procedural post-pass pose (aim + recoil); fall back to
                // the raw mixer pose on the first frame before it's computed.
                let joints = match inst.final_pose.as_ref() {
                    Some(p) => p.skinning_matrices(&m.skeleton),
                    None => inst.anim.skinning_matrices(&m.skeleton),
                };
                let model = self.char_transform(inst.enemy.pos, inst.yaw());
                (model, joints, inst.opacity(), inst.blood.as_slice())
            })
            .collect()
    }

    /// A hunter's joint global transforms for bone attachment (weapon), from its
    /// procedural post-pass pose when available (so the gun follows the aimed arm),
    /// else the raw mixer pose.
    fn inst_bone_globals(&self, inst: &EnemyInstance) -> Vec<Mat4> {
        let Some(m) = self.char_model.as_ref() else {
            return Vec::new();
        };
        match inst.final_pose.as_ref() {
            Some(p) => p.joint_global_transforms(&m.skeleton),
            None => inst.anim.joint_global_transforms(&m.skeleton),
        }
    }

    /// World transform of a weapon attached to a character's hand bone
    /// (`char_model · bone_global · local_offset`, the JS `bone.add(gun)`). `left`
    /// selects the left-hand (dual) bone + offset. Offsets are GE bone-local units,
    /// converted to metres by the model's scale.
    fn weapon_world(
        &self,
        bone_globals: &[Mat4],
        feet: Vec3,
        yaw: f32,
        def: &EnemyWeaponDef,
        left: bool,
    ) -> Option<Mat4> {
        let m = self.char_model.as_ref()?;
        let bone_name = if left { LEFT_HAND_BONE } else { RIGHT_HAND_BONE };
        let bone = m.skeleton.index_of(bone_name)?;
        let bone_global = *bone_globals.get(bone)?;
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
    /// each gun to render — one per live hunter (two for a dual-wielder, left + right
    /// hand); a dead hunter drops its gun. Plus any in-flight explosive round / placed
    /// mine that carries a GLB. Empty in BUILD. Keyed by name so the renderer looks up
    /// the mesh.
    pub fn enemy_weapon_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4)> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        for inst in &self.enemies {
            if inst.enemy.is_dead() {
                continue; // drop the gun on death
            }
            let globals = self.inst_bone_globals(inst);
            if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, false) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, true) {
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
        // Mines ride the same world-space draw path, keyed by their weapon name. In
        // flight they tumble (like the grenade round); once stuck they orient flat to
        // the surface (the model's +Y up rotated onto the surface normal).
        for m in &self.mines {
            let orient = if m.stuck {
                Mat4::from_quat(glam::Quat::from_rotation_arc(Vec3::Y, m.normal.normalize_or_zero()))
            } else {
                Mat4::from_euler(
                    EulerRot::XYZ,
                    m.flight_time * PROJECTILE_SPIN_X,
                    m.flight_time * PROJECTILE_SPIN_Y,
                    0.0,
                )
            };
            let world = Mat4::from_translation(m.pos)
                * orient
                * Mat4::from_scale(Vec3::splat(MINE_MODEL_SCALE));
            out.push((m.model, vp * world));
        }
        out
    }

    /// The enemy muzzle-flash draws this frame (same bone frames as the guns),
    /// shown only while a shot's flash is active — one per live firing hunter (both
    /// hands when dual). Empty in BUILD.
    pub fn enemy_muzzle_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4)> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        for inst in &self.enemies {
            if inst.enemy.is_dead() || inst.muzzle_timer <= 0.0 {
                continue;
            }
            let globals = self.inst_bone_globals(inst);
            if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, false) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, true) {
                    out.push((inst.weapon.name, vp * w));
                }
            }
        }
        out
    }

    /// Prepare the enemy spawn at G→HUNT from the **fixed** [`SPAWN_MARKER_POS`] (a
    /// consistent world point the level is built around — **not** derived from where
    /// the player is standing): snap it to a standable cell for [`Self::spawn_point`],
    /// and build the fan-out search-point pool ([`Self::search_points`], spread
    /// standable cells handed to searching hunters). No door is built — the ingress is
    /// just the marked floor point (see [`World::spawn_marker_mesh`]).
    ///
    /// (Breakable-door breach/blocking stays disabled — user call 2026-07-16 — so
    /// `self.doors` stays empty; the `Door` / `breach_tick` machinery is left intact
    /// for a re-enable.)
    pub(crate) fn prepare_spawn(&mut self, nav: &NavWorld) {
        self.doors.clear();
        // Snap the fixed marker to a standable cell (in case it sits a hair off the
        // floor, or the builder walled it into a tight spot).
        let m = SPAWN_MARKER_POS;
        self.spawn_point = nav.nearest_standable(m.x, m.y + 0.1, m.z, 16).unwrap_or(m);
        // Fan-out search pool: spread standable cells across the whole level, seeded
        // from the spawn point (reuses the farthest-point sampler).
        self.search_points = super::pick_spread_spawns(nav, self.spawn_point, SEARCH_POINT_COUNT);
        log::info!(
            "wave spawns at {:?} (marker {:?}); {} search points",
            self.spawn_point,
            m,
            self.search_points.len()
        );
    }

    /// Drain a breaching door's hp; on break, remove its panel collider and flip
    /// the live nav flag. Currently unused (breakable doors stay disabled; the spawn
    /// is a marked floor point, not a door) but retained for the re-enable. **The
    /// thesis in code:**
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
