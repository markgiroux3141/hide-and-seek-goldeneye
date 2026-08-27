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

/// Ground-adaptive **foot IK** post-pass: after the animation stack has posed the
/// hunter (seated with its root on the floor beneath its root cell), plant each foot on
/// the floor *beneath that foot* — so on stairs / platform edges the feet land on their
/// own tread instead of the whole model floating above the higher step or clipping
/// through it. Runs on the finished `pose`; needs the world transform + nav floor query,
/// so it lives here rather than in the engine stack.
///
/// Method (no root-motion, no sole calibration): read each foot's world position from
/// the posed skeleton, sample the floor under it, and shift the foot vertically by
/// `floor_under_foot − floor_under_root` (0 on flat ground → a no-op there). The shift
/// is eased per foot ([`FOOT_IK_EASE`]) so a stair edge glides. The pelvis is then
/// dropped by the most-negative shift (capped at [`FOOT_IK_MAX_DROP`]) so a trailing
/// foot on a lower step reaches without a leg locking straight, and each foot is solved
/// onto its target by the two-bone leg IK.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ground_feet(
    pose: &mut Pose,
    sk: &engine::skeletal::Skeleton,
    arm: &EnemyArm,
    pos: Vec3,
    yaw: f32,
    feet_off: f32,
    foot_delta: &mut [f32; 2],
    nav: &engine::sim::nav::NavWorld,
    dt: f32,
) {
    let ct = char_transform_raw(pos, yaw, feet_off);
    let inv = ct.inverse();
    let g = pose.joint_global_transforms(sk);
    let root_floor = pos.y; // the root cell's floor (already snapped by the nav step)
    let ease = 1.0 - (-dt * FOOT_IK_EASE).exp();
    let ctx = LayerCtx { skeleton: sk, dt };

    // Per-foot: sample the floor, ease the vertical shift, and build the ground target.
    let mut targets: [Option<Vec3>; 2] = [None, None];
    let mut pelvis_drop = 0.0f32; // most-negative eased shift (world m)
    for k in 0..2 {
        let (_, _, foot) = arm.legs[k];
        let foot_world = ct.transform_point3(g[foot].to_scale_rotation_translation().2);
        // Target shift vs the root's floor. No floor, or a floor far below the root
        // (a ledge / hole under the foot), → ease back to the animated height (0).
        let target_delta = match nav.floor_height_at(foot_world.x, foot_world.z, foot_world.y + 0.3) {
            Some(fy) if fy - root_floor >= -FOOT_IK_MAX_REACH => fy - root_floor,
            _ => 0.0,
        };
        foot_delta[k] += (target_delta - foot_delta[k]) * ease;
        pelvis_drop = pelvis_drop.min(foot_delta[k]);
        let target_world = foot_world + Vec3::Y * foot_delta[k];
        targets[k] = Some(inv.transform_point3(target_world));
    }

    // Drop the pelvis (root, whose local space is model space) so the lower foot reaches
    // without over-extending; convert the world drop to model units via the char scale.
    let drop = pelvis_drop.clamp(-FOOT_IK_MAX_DROP, 0.0);
    if drop < 0.0 {
        RootTranslateLayer {
            joint: arm.pelvis,
            offset: Vec3::new(0.0, drop / CHAR_SCALE, 0.0),
            enabled: true,
        }
        .apply(pose, &ctx);
    }

    // Solve each foot onto its (absolute, model-space) ground target from the lowered
    // hips. Targets were captured pre-drop, so the IK reaches the real ground point.
    for k in 0..2 {
        if let Some(target) = targets[k] {
            let (root, mid, end) = arm.legs[k];
            TwoBoneIkLayer {
                root,
                mid,
                end,
                target,
                reach_frac: 0.0,
                pole: Vec3::ZERO,
                weight: 1.0,
                enabled: true,
            }
            .apply(pose, &ctx);
        }
    }
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

/// Whether a clip index is one of the fire clips — the `enemyState === 'action'`
/// proxy the FSM's attack→cooldown transition needs, disambiguated from hit/death
/// one-shots.
///
/// That is the three per-stance defaults *and* the directional set at slots 36+, which
/// a Perfect Dark hunter's burst switches to by bearing
/// ([`crate::combat::attack_anim::slot_is_fire`]). Asking the table rather than
/// comparing three constants is what keeps this honest as the set grows.
pub(crate) fn is_fire_clip(idx: usize) -> bool {
    idx == FIRE_RIFLE_IDX
        || idx == FIRE_PISTOL_IDX
        || idx == FIRE_DUAL_IDX
        || crate::combat::attack_anim::slot_is_fire(idx)
}

impl EnemyInstance {
    /// The RENDERED horizontal facing yaw (model faces +Z at yaw 0 → `atan2(x, z)`) —
    /// the smoothed [`render_yaw`](EnemyInstance::render_yaw) if [`Self::advance_facing`]
    /// has run, else the raw AI heading. Every model / weapon / muzzle transform reads
    /// this, so the body, gun and flash all turn together and smoothly.
    pub(crate) fn yaw(&self) -> f32 {
        self.render_yaw.unwrap_or_else(|| self.raw_yaw())
    }

    /// The un-smoothed AI facing yaw straight off the [`Enemy`] heading — the target
    /// the rendered yaw eases toward.
    fn raw_yaw(&self) -> f32 {
        let h = self.enemy.heading();
        h.x.atan2(h.z)
    }

    /// Ease the rendered facing toward the AI heading at [`TURN_RATE`] (shortest arc),
    /// so a snappy heading flip becomes a believable turn instead of a per-frame spin.
    /// First call snaps (no history to ease from). Call once per rendered frame.
    pub(crate) fn advance_facing(&mut self, dt: f32) {
        // A PD simulant owns its own facing: the sim step already wrote
        // `render_yaw`, turn-rate-limited at PD's ~212 deg/s and carrying the live
        // aim error. Easing it toward the FSM heading here would smear away
        // exactly the thing the lab exists to show.
        if self.pdsim.is_some() {
            return;
        }
        let target = self.raw_yaw();
        let cur = match self.render_yaw {
            Some(y) => y,
            None => {
                self.render_yaw = Some(target);
                return;
            }
        };
        // Shortest signed angular difference in (−π, π].
        let mut delta = target - cur;
        while delta > std::f32::consts::PI {
            delta -= std::f32::consts::TAU;
        }
        while delta < -std::f32::consts::PI {
            delta += std::f32::consts::TAU;
        }
        let max = TURN_RATE * dt;
        let mut y = cur + delta.clamp(-max, max);
        // Keep it bounded to (−π, π] so it never drifts unbounded.
        if y > std::f32::consts::PI {
            y -= std::f32::consts::TAU;
        } else if y < -std::f32::consts::PI {
            y += std::f32::consts::TAU;
        }
        self.render_yaw = Some(y);
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
        if !self.char_models.is_empty() || self.enemies.is_empty() {
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

    /// Every loaded skinned-character CPU body (body-id order), for one-time GPU
    /// upload at startup — the renderer uploads one GPU mesh per body. Empty if no
    /// asset loaded.
    pub fn character_models(&self) -> &[SkinnedModel] {
        &self.char_models
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
        // they don't clash with the per-hunter `&mut` borrow. The chest-aim points the
        // gun barrel at this, so the hunters track the player's height as well as bearing.
        let aim_point = self.player_pos().map(|p| p + Vec3::Y * PLAYER_AIM_Y);
        // Head look-at + foot-IK kill-switches, read once so they don't reborrow `self`
        // in the loop.
        let head_look_on = self.head_look;
        let foot_ik_on = self.foot_ik;
        // Bodies + feet offsets + resolved rig + nav are DISJOINT fields from `enemies`,
        // so all can be held across the `&mut enemies` loop; each hunter indexes its own
        // body. `nav` is the floor-height source for ground-adaptive foot IK.
        let models = &self.char_models;
        let feet_offs = &self.char_feet_offset;
        let arms = &self.enemy_arm;
        let nav = self.nav.as_ref();
        // Physics (read-only here) — the source for the living-hit reaction blend.
        let physics = &self.physics;
        // Death-animation impact cues collected in the loop (see `thud` below) and
        // played after it, since `audio` is a sibling field of the borrowed roster.
        let mut thuds: Vec<Vec3> = Vec::new();

        for inst in &mut self.enemies {
            // A ragdolling corpse is driven entirely by the physics bodies (see
            // `character_instances`); skip the whole animation stack for it. Its fade +
            // teardown are handled by `advance_ragdolls` off the fixed step.
            if inst.ragdoll.is_some() {
                continue;
            }
            // Low-pass the AI's per-step speed → a continuous locomotion speed that
            // drives the blend layer, so the gait eases smoothly (no band thresholds).
            inst.anim_speed +=
                (inst.enemy.speed() - inst.anim_speed) * (1.0 - (-dt * LOCO_SMOOTH).exp());
            // Snap the decay tail to a hard stop (only when actually stopped) so the
            // blend settles to pure idle rather than a sliver of walk forever.
            if inst.enemy.speed() <= 0.0 && inst.anim_speed < LOCO_IDLE_EPS {
                inst.anim_speed = 0.0;
            }
            // Ease the rendered body facing toward the AI heading at a realistic turn
            // rate, so heading flips (jukes / reposition / travel↔player) read as turns
            // rather than a per-frame spin. All model/weapon transforms use this yaw.
            inst.advance_facing(dt);

            // Death fade: hold the corpse opaque THROUGH the death animation, then
            // ramp opacity 1→0 once the clip has clamped (`oneshot_finished`).
            if inst.enemy.is_dead() && inst.anim.oneshot_finished() {
                let t = inst.fade.get_or_insert(0.0);
                *t = (*t + dt).min(FADE_DURATION);
            }
            // Animation events: Perfect Dark authors the frames at which a falling
            // body strikes the floor (`thudframe1`/`thudframe2` on each death row), so
            // the impact lands with the animation instead of at the moment of the
            // shot. Collected here and played after the loop — `inst` is borrowed.
            if let Some((a, b)) = inst.thud {
                let now = inst.anim.current_time() * crate::combat::hit_anim::PD_ANIM_FPS;
                for (k, frame) in [a, b].into_iter().enumerate() {
                    if frame >= 0.0 && !inst.thud_played[k] && now >= frame {
                        inst.thud_played[k] = true;
                        thuds.push(inst.enemy.pos);
                    }
                }
            }
            // The mixer now carries ONLY the hit/death one-shots — locomotion is the
            // continuous blend layer below. Advance it so a one-shot plays out.
            inst.anim.update(dt);

            // ── Procedural post-pass: locomotion blend → aim-offset → recoil. ──
            let Some(m) = models.get(inst.body) else {
                continue; // no body for this hunter → nothing to pose
            };
            let sk = &m.skeleton;
            // Seated for the clip set THIS hunter plays — the two differ by the Perfect
            // Dark root pedestal (see `World::body_feet_offset`).
            let feet_off = feet_offs
                .get(inst.body)
                .map(|p| p[if inst.pd_anims { 0 } else { 1 }])
                .unwrap_or(0.0);
            // A hit/death one-shot takes over the whole body: feed its pose as the
            // base and bypass locomotion + aim. `is_fire_clip` guard is vestigial
            // (fire is a timer, never on the mixer) but keeps the check honest.
            let one_shot =
                inst.anim.is_playing_oneshot() && !is_fire_clip(inst.anim.current_clip());
            // Cadence: warp the gait phase rate to the hunter's ACTUAL committed ground
            // speed (which ORCA can drop below the intended gait speed), so the feet
            // cycle at the real travel rate instead of skating. Only while moving; off
            // (1.0) when foot-IK is disabled or the hunter is ~stopped.
            let stride_scale = if foot_ik_on && inst.anim_speed > 0.2 {
                let v = inst.enemy.velocity();
                let actual = (v.x * v.x + v.z * v.z).sqrt();
                (actual / inst.anim_speed).clamp(STRIDE_SCALE_MIN, STRIDE_SCALE_MAX)
            } else {
                1.0
            };
            if let Some(loco) = inst.stack.layer_as::<LocomotionBlendLayer>(ENEMY_LOCO_LAYER) {
                loco.speed = inst.anim_speed;
                loco.stride_scale = stride_scale;
                loco.enabled = !one_shot;
            }
            let engaged = matches!(
                inst.enemy.state(),
                AiState::Alert
                    | AiState::Chase
                    | AiState::Attack
                    | AiState::Cooldown
                    | AiState::TakeCover
                    | AiState::Peek
            );
            let want_aim = engaged && !inst.enemy.is_dead() && !one_shot;
            let target_w = if want_aim { 1.0 } else { 0.0 };
            // Exponential ease toward the target weight (frame-rate independent).
            inst.aim_weight += (target_w - inst.aim_weight) * (1.0 - (-dt * AIM_RAMP).exp());

            // Raise/lower the authored upper-body aim hold by weight: the overlay
            // poses BOTH arms into the correct gun hold (two-hand rifle, leveled
            // pistol, akimbo) over the running legs. The gun rides the hand bone, so
            // it follows. Bypassed during a hit/death one-shot.
            if let Some(ov) = inst.stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
                ov.weight = inst.aim_weight;
                ov.enabled = !one_shot;
            }
            // Chest-aim: swing the whole hold so the real gun barrel points at the
            // player (bearing + height), cone-clamped, eased in with the aim weight.
            // Target is the player's chest in the hunter's MODEL space.
            let aim_target = aim_point.map(|ap| {
                let ct = char_transform_raw(inst.enemy.pos, inst.yaw(), feet_off);
                ct.inverse().transform_point3(ap)
            });
            // Tracking is gated by the animation's **aim window** while a burst is
            // running: Perfect Dark lets a character swivel onto its target before it
            // can shoot and for a while after (pistol: frames 20→120 around a 58→92
            // shoot window), and resets the aim outside that (`chr_reset_aimend`,
            // chraction.c:10766). Between bursts we keep tracking, because our
            // hunters hold their weapon up whenever engaged rather than entering and
            // leaving a discrete attack action the way a PD chr does. A legacy
            // (GoldenEye) hunter's window is unbounded, so this is a no-op for it.
            let tracking = inst.fire_elapsed.is_none_or(|t| inst.fire.aiming(t));
            if let Some(ca) = inst.stack.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER) {
                if let Some(t) = aim_target {
                    ca.target = t;
                }
                ca.weight = if one_shot || !tracking { 0.0 } else { inst.aim_weight };
            }

            // ── Head look-at: turn the head toward what the hunter is thinking about. ──
            // The focus is the player while engaged, the last-known position while
            // investigating, the search point while sweeping, and nothing when idle /
            // during a hit/death one-shot (the head returns to the pose as weight fades).
            let focus = if !head_look_on || inst.enemy.is_dead() || one_shot {
                None
            } else {
                match inst.enemy.state() {
                    AiState::Alert
                    | AiState::Chase
                    | AiState::Attack
                    | AiState::Cooldown
                    | AiState::TakeCover
                    | AiState::Peek => aim_point
                        .or_else(|| inst.enemy.last_known().map(|p| p + Vec3::Y * PLAYER_AIM_Y)),
                    AiState::Investigate => {
                        inst.enemy.last_known().map(|p| p + Vec3::Y * PLAYER_AIM_Y)
                    }
                    // Going shopping: look at what it is going for. A hunter that walks
                    // past you to reach a gun reads as deliberate when its eyes are on
                    // the gun, and as broken when they aren't.
                    AiState::Fetch => inst.enemy.fetch_target().map(|p| p + Vec3::Y * PLAYER_AIM_Y),
                    // Blind & sweeping: the head visibly scans side to side (the readable
                    // cue for the invisible 360° perception sweep) while the body faces
                    // its travel direction — a point out along the scan direction.
                    AiState::Search | AiState::Idle => Some(
                        inst.enemy.pos
                            + inst.enemy.head_scan_dir() * HEAD_SCAN_DIST
                            + Vec3::Y * PLAYER_AIM_Y,
                    ),
                }
            };
            // Ease the smoothed look point toward the focus so switching focus sweeps
            // the gaze across instead of snapping (first acquire snaps — no history).
            if let Some(f) = focus {
                let track = 1.0 - (-dt * HEAD_LOOK_TRACK).exp();
                inst.head_look_point = Some(match inst.head_look_point {
                    Some(p) => p + (f - p) * track,
                    None => f,
                });
            }
            // Weight eases in while there's a focus, out otherwise (same ramp as the arm).
            let head_target_w = if focus.is_some() { 1.0 } else { 0.0 };
            inst.head_look_weight +=
                (head_target_w - inst.head_look_weight) * (1.0 - (-dt * AIM_RAMP).exp());
            // Model-space gaze target, resolved BEFORE the `&mut stack` borrow (same as
            // the chest-aim's `aim_target`).
            let head_target = inst.head_look_point.map(|wp| {
                let ct = char_transform_raw(inst.enemy.pos, inst.yaw(), feet_off);
                ct.inverse().transform_point3(wp)
            });
            if let Some(hl) = inst.stack.layer_as::<AimOffsetLayer>(ENEMY_HEAD_LOOK_LAYER) {
                if let Some(t) = head_target {
                    hl.target = t;
                }
                hl.enabled = head_look_on;
                hl.weight = if one_shot { 0.0 } else { inst.head_look_weight };
            }

            // Base pose: the hit/death one-shot when active, else the bind pose for
            // the locomotion blend layer to overwrite with the current gait.
            let base = if one_shot {
                inst.anim.pose(sk)
            } else {
                Pose::bind(sk)
            };
            let ctx = LayerCtx { skeleton: sk, dt };
            let mut pose = inst.stack.evaluate(base, &ctx);

            // ── Ground-adaptive foot IK post-pass (after the stack, not a stack layer:
            // the feet must be read from the finished locomotion pose, and grounding
            // needs the world transform + nav floor query only the `World` has). Skipped
            // during a hit/death one-shot so the canned clip plays untouched. ──
            if foot_ik_on && !one_shot {
                if let (Some(arm), Some(nav)) = (arms.get(inst.body).and_then(|a| a.as_ref()), nav) {
                    let yaw = inst.yaw();
                    ground_feet(
                        &mut pose,
                        sk,
                        arm,
                        inst.enemy.pos,
                        yaw,
                        feet_off,
                        &mut inst.foot_delta,
                        nav,
                        dt,
                    );
                }
            }

            // Living-hit stagger: blend the transient physics ragdoll's model-local
            // pose INTO the animated pose by its decaying weight — a real physical
            // reaction that eases back to the run-and-gun animation as the weight → 0.
            if let Some(reaction) = inst.reaction.as_ref() {
                let w = reaction.weight().clamp(0.0, 1.0);
                if w > 1e-3 {
                    let char_inv =
                        char_transform_raw(inst.enemy.pos, inst.yaw(), feet_off).inverse();
                    let rp = reaction.rag.model_local_pose(physics, sk, char_inv);
                    for b in 0..pose.joint_count().min(rp.joint_count()) {
                        pose.t[b] = pose.t[b].lerp(rp.t[b], w);
                        pose.r[b] = pose.r[b].slerp(rp.r[b], w);
                        pose.s[b] = pose.s[b].lerp(rp.s[b], w);
                    }
                }
            }
            inst.final_pose = Some(pose);
        }

        // A body hitting the floor. One `fallover` variant per event so repeated
        // deaths in a firefight do not machine-gun the same sample.
        if !thuds.is_empty() {
            for _ in 0..thuds.len() {
                let n = self.rand_below(FALLOVER_COUNT) + 1;
                if let Some(audio) = self.audio.as_mut() {
                    audio.play(&format!("sounds/enemies/fallover{n}.wav"), FALLOVER_VOL);
                }
            }
        }

        self.log_anim_debug();
    }

    /// Advance every active death ragdoll one fixed step: age it, and once it has
    /// settled (its fastest body slower than [`RAGDOLL_SETTLE_SPEED`]) or hit the
    /// [`RAGDOLL_MAX_SETTLE`] backstop, begin the opacity fade; when a corpse has fully
    /// faded, tear its bodies out of the physics sim. The rigid-body solver itself is
    /// stepped separately ([`PhysicsWorld::step_dynamics`]); this only drives the corpse
    /// lifecycle. No-op when no hunter is ragdolling. Index-based so the `&mut physics`
    /// teardown doesn't clash with the enemy-roster borrow.
    pub(crate) fn advance_ragdolls(&mut self, dt: f32) {
        for i in 0..self.enemies.len() {
            if self.enemies[i].ragdoll.is_none() {
                continue;
            }
            self.enemies[i].ragdoll_time += dt;
            let speed = self.enemies[i]
                .ragdoll
                .as_ref()
                .map(|r| r.max_speed(&self.physics))
                .unwrap_or(0.0);
            if speed < RAGDOLL_SETTLE_SPEED || self.enemies[i].ragdoll_time >= RAGDOLL_MAX_SETTLE {
                let t = self.enemies[i].fade.get_or_insert(0.0);
                *t = (*t + dt).min(FADE_DURATION);
            }
            if self.enemies[i].fade.is_some_and(|t| t >= FADE_DURATION) {
                if let Some(rag) = self.enemies[i].ragdoll.take() {
                    rag.remove(&mut self.physics);
                }
            }
        }
        // Living-hit reactions: age each, and once the blend has decayed to nothing,
        // tear its bodies out of the sim so the hunter is back to pure animation.
        for i in 0..self.enemies.len() {
            if self.enemies[i].reaction.is_none() {
                continue;
            }
            if let Some(r) = self.enemies[i].reaction.as_mut() {
                r.elapsed += dt;
            }
            let done = self.enemies[i]
                .reaction
                .as_ref()
                .map_or(true, |r| r.weight() < REACTION_MIN_WEIGHT);
            if done {
                if let Some(r) = self.enemies[i].reaction.take() {
                    r.rag.remove(&mut self.physics);
                }
            }
        }
    }

    /// Remove every live ragdoll's bodies from the physics sim before the enemy roster
    /// is dropped (the [`Ragdoll`] structs don't clean up on `Drop`) — both death
    /// corpses and in-flight living reactions. Called on any hunt teardown so ragdoll
    /// bodies never leak into the next HUNT.
    pub(crate) fn clear_ragdolls(&mut self) {
        for i in 0..self.enemies.len() {
            if let Some(rag) = self.enemies[i].ragdoll.take() {
                rag.remove(&mut self.physics);
            }
            if let Some(r) = self.enemies[i].reaction.take() {
                r.rag.remove(&mut self.physics);
            }
        }
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
                    AiState::Alert
                        | AiState::Chase
                        | AiState::Attack
                        | AiState::Cooldown
                        | AiState::TakeCover
                        | AiState::Peek
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
        let (reach_frac, elbow_deg, foot) = match (self.enemy_arm.get(inst.body).and_then(|a| a.as_ref()), inst.final_pose.as_ref(), self.char_models.get(inst.body)) {
            (Some(arm), Some(fp), Some(m)) => {
                let g = fp.joint_global_transforms(&m.skeleton);
                let sa = g[arm.shoulder()].to_scale_rotation_translation().2;
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

    /// The feet-seating Y offset for a given body id (0 if the body id is out of
    /// range, e.g. no assets loaded).
    /// This body's feet-seating offset for the clip set it is playing.
    ///
    /// `pd_clips` is the hunter's own [`EnemyInstance::pd_anims`]. It is not cosmetic: a
    /// Perfect Dark clip carries an absolute root pedestal that a GoldenEye clip does not,
    /// so the same body needs a different offset depending on which set drives it — 1.09 m
    /// of difference, i.e. the whole figure off the floor. See `World::new`'s sweep.
    pub(crate) fn body_feet_offset(&self, body: usize, pd_clips: bool) -> f32 {
        let pair = self.char_feet_offset.get(body).copied().unwrap_or([0.0, 0.0]);
        pair[if pd_clips { 0 } else { 1 }]
    }

    /// World transform placing a character of body `body` (feet at `feet`, facing
    /// `yaw`) with that body's feet-seating offset + `CHAR_SCALE` — the model root the
    /// skinned pose + any bone-attached weapon are expressed under.
    pub(crate) fn char_transform(&self, feet: Vec3, yaw: f32, body: usize, pd_clips: bool) -> Mat4 {
        char_transform_raw(feet, yaw, self.body_feet_offset(body, pd_clips))
    }

    /// Every skinned character to draw this frame as `(body id, model, joint matrices,
    /// opacity, blood_colors)` — one per live hunter (each its own mid-crossfade pose,
    /// positioned/faced by its AI, faded on death, with its accumulated per-vertex
    /// blood). The body id selects the renderer's GPU mesh. Empty in BUILD (no
    /// character is drawn while authoring), except the optional procedural preview
    /// (always body 0).
    pub fn character_instances(&self) -> Vec<(usize, Mat4, Vec<Mat4>, f32, &[f32])> {
        if self.char_models.is_empty() {
            return Vec::new();
        }
        let mut pd: Vec<(usize, Mat4, Vec<Mat4>, f32, &[f32])> = Vec::new();
        // Spike: in BUILD the only character is the optional procedural preview (body 0).
        if self.is_build() {
            if let Some(p) = self.procedural_preview.as_ref() {
                pd.push((
                    0,
                    self.char_transform(p.feet, p.yaw, 0, true),
                    p.joints.clone(),
                    1.0,
                    p.blood.as_slice(),
                ));
            }
            return pd;
        }
        pd.extend(self.enemies
            .iter()
            .filter_map(|inst| {
                let m = self.char_models.get(inst.body)?;
                // A ragdolling corpse: its bones are placed by the physics bodies, so
                // the skinning matrices are WORLD-space and the model transform is the
                // identity (the scale is folded into the read-back). See `Ragdoll`.
                if let Some(rag) = inst.ragdoll.as_ref() {
                    let joints = rag.skinning_matrices(&self.physics, &m.skeleton);
                    return Some((inst.body, Mat4::IDENTITY, joints, inst.opacity(), inst.blood.as_slice()));
                }
                // Prefer the procedural post-pass pose (aim + recoil); fall back to
                // the raw mixer pose on the first frame before it's computed.
                let joints = match inst.final_pose.as_ref() {
                    Some(p) => p.skinning_matrices(&m.skeleton),
                    None => inst.anim.skinning_matrices(&m.skeleton),
                };
                let model = self.char_transform(inst.enemy.pos, inst.yaw(), inst.body, inst.pd_anims);
                Some((inst.body, model, joints, inst.opacity(), inst.blood.as_slice()))
            }));
        pd
    }

    /// A hunter's joint global transforms for bone attachment (weapon), from its
    /// procedural post-pass pose when available (so the gun follows the aimed arm),
    /// else the raw mixer pose.
    fn inst_bone_globals(&self, inst: &EnemyInstance) -> Vec<Mat4> {
        let Some(m) = self.char_models.get(inst.body) else {
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
        body: usize,
        pd_clips: bool,
    ) -> Option<Mat4> {
        let m = self.char_models.get(body)?;
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
        Some(self.char_transform(feet, yaw, body, pd_clips) * bone_global * offset)
    }

    /// The enemy weapon draws this frame: `(weapon name, view_proj · world)` for
    /// each gun to render — one per live hunter (two for a dual-wielder, left + right
    /// hand); a dead hunter drops its gun. Plus any in-flight explosive round / placed
    /// mine that carries a GLB. Empty in BUILD. Keyed by name so the renderer looks up
    /// the mesh.
    pub fn enemy_weapon_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4)> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        // Spike: the BUILD bone-posing rig draws its AR33 so the foregrip can be
        // eyeballed (the enemy/projectile/mine loops below are all empty in BUILD).
        if let Some(d) = self.preview_weapon_draw(vp) {
            out.push(d);
        }
        // Guns lying on the ground ride this channel too — it is already keyed by
        // weapon name with the whole arsenal uploaded, which is exactly what a
        // pickup needs and why a weapon pickup is not a catalog prop. Unlike
        // everything below, these draw in BUILD as well: the editor has to show what
        // it is placing.
        for (name, world) in self.weapon_pickup_draws() {
            out.push((name, vp * world));
        }
        for inst in &self.enemies {
            if inst.enemy.is_dead() {
                continue; // drop the gun on death
            }
            if inst.weapon.is_unarmed() {
                continue; // nothing in its hands to draw
            }
            let globals = self.inst_bone_globals(inst);
            if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, false, inst.body, inst.pd_anims) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, true, inst.body, inst.pd_anims) {
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
            if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, false, inst.body, inst.pd_anims) {
                out.push((inst.weapon.name, vp * w));
            }
            if inst.dual {
                if let Some(w) = self.weapon_world(&globals, inst.enemy.pos, inst.yaw(), &inst.weapon, true, inst.body, inst.pd_anims) {
                    out.push((inst.weapon.name, vp * w));
                }
            }
        }
        out
    }

    /// Build the spawn pool at G→HUNT: resolve every **authored** spawn pad
    /// (`world::tools::spawn_point`) to a standable nav cell, and build the fan-out
    /// search-point pool ([`Self::search_points`], spread standable cells handed to
    /// searching hunters). No door is built — a pad is just a marked floor point.
    ///
    /// Resolving to a standable cell here is Perfect Dark's `chr_adjust_pos_for_spawn`
    /// (`player.c:403`), moved to pool-build time: PD validates fit per candidate inside
    /// the chooser, but every pad in this pool is checked once at G instead, so
    /// [`spawn::choose_spawn`] only ever sees pads a body fits on.
    ///
    /// **Empty pool → one synthetic pad at [`SPAWN_MARKER_POS`].** A level with no pads
    /// authored (an older save, the AI lab's arenas, the levelgen harness) keeps
    /// flooding its wave in at the old fixed marker rather than going quiet. Perfect
    /// Dark guards identically — `if (g_NumSpawnPoints > 0)` (`playerreset.c:398`).
    ///
    /// (Breakable-door breach/blocking stays disabled — user call 2026-07-16 — so
    /// `self.doors` stays empty; the `Door` / `breach_tick` machinery is left intact
    /// for a re-enable.)
    pub(crate) fn prepare_spawn(&mut self, nav: &NavWorld) {
        self.doors.clear();
        // Snap each pad to a standable cell (in case it sits a hair off the floor, or
        // the builder walled it into a tight spot).
        let snap = |p: spawn::SpawnPad| spawn::SpawnPad {
            pos: nav
                .nearest_standable(p.pos.x, p.pos.y + 0.1, p.pos.z, 16)
                .unwrap_or(p.pos),
            yaw: p.yaw,
        };
        let authored = self.authored_spawn_pads();
        self.spawn_pads = if authored.is_empty() {
            // A compatibility shim, not a level feature — nothing is drawn for it in
            // BUILD (see `World::spawn_marker_mesh`), so say out loud that the level is
            // un-authored rather than letting a wave appear at a point with no marker.
            log::warn!(
                "spawn: no spawn pads authored — falling back to the fixed point at \
                 {SPAWN_MARKER_POS:?}. Place pads in BUILD (O → SPAWNS) to choose where \
                 the player and the pack come in."
            );
            vec![snap(spawn::SpawnPad { pos: SPAWN_MARKER_POS, yaw: 0.0 })]
        } else {
            authored.into_iter().map(snap).collect()
        };
        // ── Drop pads that are cut off from the rest of the level ──
        // A pad on a nav island is not a spawn, it is a trap, and it is a trap for
        // *whoever* draws it. A hunter there can never path anywhere, so no behaviour can
        // move it and it stands still all round (the "some enemies just get lazy" report).
        // The **player** there is worse: everyone hunting them runs a doomed A* every
        // `REPATH_INTERVAL`, and since a failed search expands the whole region, four
        // hunters took the sim from 0.7 ms to 140 ms a frame — a 10 fps playtest whose
        // cause was one spawn pad on a roof.
        //
        // Perfect Dark has the same guard in the same place: `chr_adjust_pos_for_spawn`
        // validates each candidate and a pad that fails is never shortlisted
        // (`player.c:340`). Ours validates connectivity rather than fit, because fit is
        // already handled by the snap above.
        //
        // (`nav` is the parameter, not `self.nav` — the caller has taken the field, which
        // is why `prepare_spawn` receives it at all.)
        //
        // The grouping rule lives in `world::nav_issues` and is called from here, so the
        // BUILD NAV tab's "G will IGNORE pad 3" list is produced by the same code that
        // does the ignoring — a second copy of this rule would eventually report
        // confidently on a decision the runtime no longer makes.
        let (_, dropped) = super::nav_issues::partition_pads(&self.spawn_pads, nav);
        if !dropped.is_empty() && dropped.len() < self.spawn_pads.len() {
            for &i in &dropped {
                log::warn!(
                    "spawn: IGNORING pad {i} at {:?} — it is on a walkable island, cut off \
                     from the rest of the level. Anyone spawning there would be stranded. \
                     Give it a route (stairs / platform) or move it. (BUILD: O → NAV → \
                     Calculate shows how far off the route it is.)",
                    self.spawn_pads[i].pos
                );
            }
            let mut i = 0;
            self.spawn_pads.retain(|_| {
                let keep = !dropped.contains(&i);
                i += 1;
                keep
            });
        }
        // The wave's reference point (search-pool seed + search fallback).
        self.spawn_point = self.spawn_pads[0].pos;
        // Fan-out search pool: spread standable cells across the whole level, seeded
        // from that point (reuses the farthest-point sampler).
        self.search_points = super::pick_spread_spawns(nav, self.spawn_point, SEARCH_POINT_COUNT);
        log::info!(
            "spawn pool: {} pad(s){}; {} search points",
            self.spawn_pads.len(),
            if self.spawn_pad_count() == 0 {
                " (none authored — falling back to the fixed marker)"
            } else {
                ""
            },
            self.search_points.len()
        );
        // A pool no larger than the shortlist is the authoring shape that makes spawns
        // feel random: every pad is a candidate every time, so the "not near an enemy"
        // filter has nothing left to prefer. Perfect Dark's rule is written against
        // `pads[24]` and its levels author most of that. Worth saying out loud, because
        // the symptom (respawning in the pack) reads as a code bug and is not one.
        // A pad the rest of the level cannot be walked to from is a trap: whoever draws
        // it is out of the round, because every path request from it fails and no
        // behaviour can move a hunter that A* will not route. In play that reads as a
        // hunter loitering and ignoring you, so it is worth naming the pad at `G` rather
        // than leaving it to be discovered as an AI bug.
        let n = self.spawn_pad_count();
        if n > 0 && n <= spawn::SHORTLIST {
            log::warn!(
                "spawn: only {n} pad(s) authored — at or below the {}-slot shortlist, so \
                 every pad is always a candidate and the away-from-the-enemy filter has \
                 little to choose between. Place more pads (O → SPAWNS), spread through \
                 the level, for spawns that can actually avoid the pack.",
                spawn::SHORTLIST
            );
        }
    }

    /// Whether each spawn pad can actually **get anywhere**: for every pad, whether A\*
    /// finds a route from it to each other pad.
    ///
    /// A pad the nav grid cannot path out of is a trap, not a spawn. Whoever draws it
    /// stands there for the whole round — every path request fails, so `move_toward`
    /// reports "arrived or unreachable" forever and no behaviour can move it. It reads in
    /// play as a hunter that has gone lazy and is ignoring you, which is a long way from
    /// the actual cause, so this is worth being able to ask directly.
    /// Walkable-component sizes plus which one each spawn pad sits in — the crude first
    /// version of the nav-issues report (`DESIGN_NAV_VALIDATION.md`).
    pub fn nav_component_report(&self) -> String {
        use std::fmt::Write;
        let Some(nav) = self.nav.as_ref() else {
            return "no nav baked (call this in HUNT)".into();
        };
        let sizes = nav.component_sizes();
        let total: usize = sizes.iter().map(|(_, n)| n).sum();
        let mut s = format!(
            "{} walkable component(s), {total} standable cells:
",
            sizes.len()
        );
        for (id, n) in sizes.iter().take(12) {
            let pads: Vec<usize> = self
                .spawn_pads
                .iter()
                .enumerate()
                .filter(|(_, p)| nav.component_at(p.pos) == Some(*id))
                .map(|(i, _)| i)
                .collect();
            let _ = writeln!(
                s,
                "  comp {id}: {n} cells ({:.1}%)  pads {pads:?}",
                *n as f32 / total.max(1) as f32 * 100.0
            );
        }
        if sizes.len() > 12 {
            let _ = writeln!(s, "  … and {} more", sizes.len() - 12);
        }
        s
    }

    pub fn spawn_reachability_report(&self) -> String {
        use std::fmt::Write;
        let Some(nav) = self.nav.as_ref() else {
            return "no nav baked (call this in HUNT)".into();
        };
        let pads = &self.spawn_pads;
        let mut s = format!("{} pad(s), reachability by A*:\n", pads.len());
        for (i, a) in pads.iter().enumerate() {
            let reachable = pads
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter(|(_, b)| nav.find_path(a.pos, b.pos).is_some())
                .count();
            let _ = writeln!(
                s,
                "  pad {i} ({:6.1},{:6.1},{:6.1}) → reaches {reachable}/{} other pad(s){}",
                a.pos.x,
                a.pos.y,
                a.pos.z,
                pads.len() - 1,
                if reachable == 0 { "   ** STRANDED **" } else { "" }
            );
        }
        s
    }

    /// One line per hunter: what it is doing, what it is holding, and — when it is going
    /// shopping — where its pickup is *relative to its own feet*, split into flat and
    /// vertical.
    ///
    /// That split is the point. A hunter navigates in XZ and arrives on flat distance, but
    /// a pickup is collected inside a **vertical** window too, so "flat 0.1 m, rise 3.4 m"
    /// is a hunter standing directly under a gun on the floor above, arriving forever and
    /// collecting never. No amount of watching it would tell you that; two numbers do.
    pub fn hunter_report(&self) -> String {
        use std::fmt::Write;
        let mut s = format!("{} hunter(s):\n", self.enemies.len());
        for (i, inst) in self.enemies.iter().enumerate() {
            let p = inst.enemy.pos;
            let _ = write!(
                s,
                "  h{i}: {:?} at ({:5.1},{:5.1},{:5.1}) holding {:12} {:2}/{:<3}",
                inst.enemy.state(),
                p.x,
                p.y,
                p.z,
                inst.weapon.name,
                inst.loaded,
                inst.reserve,
            );
            if inst.enemy.is_dead() {
                let _ = write!(s, " DEAD");
            }
            if let Some(want) = Self::hunter_want(inst) {
                let _ = write!(s, " wants {want:?}");
                match self.hunter_fetch_target(inst) {
                    Some(t) => {
                        let flat = Vec3::new(t.x - p.x, 0.0, t.z - p.z).length();
                        let _ = write!(s, " → flat {flat:5.1} m, rise {:+5.1} m", t.y - p.y);
                    }
                    None => {
                        let _ = write!(s, " → nothing on the floor to fetch");
                    }
                }
            }
            s.push('\n');
        }
        s
    }

    /// Pick a pad for a body entering the level, applying Perfect Dark's rule (see
    /// [`spawn::choose_spawn`]). Returns `(pool index, pad)`; the index lets the caller
    /// notice when several bodies draw the *same* pad and ring them apart.
    ///
    /// Everyone else currently alive is passed as an occupant — the player and every
    /// live hunter but `spawning` itself — which is both PD's teamless-deathmatch case
    /// and what stops a wave stacking on one pad.
    pub(crate) fn choose_spawn_pad(&mut self, spawning: Spawning) -> Option<(usize, spawn::SpawnPad)> {
        // The pool is `Copy` data; clone it so `physics` + `char_rng` can be borrowed
        // mutably for the line-of-sight test and the roll.
        let pads = self.spawn_pads.clone();
        if pads.is_empty() {
            return None;
        }
        let occupants = self.spawn_occupants(spawning);
        let c = spawn::choose_spawn(&pads, &occupants, &mut self.physics, &mut self.spawn_rng)?;
        // One line naming the pad, how close the nearest body is, and which pass produced
        // it. `pass=3` says the filter found nothing safe — that is a level with too few
        // pads for the size of the wave, not a bug in the rule.
        log::debug!(
            "spawn: {spawning:?} → pad {} of {} ({:?}), nearest body {:.1} m, pass {}",
            c.pad,
            pads.len(),
            pads[c.pad].pos,
            c.enemy_dist,
            c.pass
        );
        Some((c.pad, pads[c.pad]))
    }

    /// Everyone already in the level that `spawning` must be kept away from: the player
    /// (unless the player is who is spawning, or is dead) and every live hunter but
    /// `spawning` itself.
    ///
    /// PD filters to *enemies* (`chr_compare_teams(..., COMPARE_ENEMIES)`), which in the
    /// teamless deathmatch this game's hunters run is everyone. Excluding the spawning
    /// body is PD's `prop != g_Vars.players[i]->prop` — a hunter must not be pushed off a
    /// good pad by where its own corpse is lying.
    fn spawn_occupants(&self, spawning: Spawning) -> Vec<Vec3> {
        let mut occupants: Vec<Vec3> = Vec::new();
        if spawning != Spawning::Player && !self.player_dead {
            if let Some(p) = self.player_pos() {
                occupants.push(p);
            }
        }
        for (j, e) in self.enemies.iter().enumerate() {
            if spawning == Spawning::Hunter(j) || e.enemy.is_dead() {
                continue;
            }
            occupants.push(e.enemy.pos);
        }
        occupants
    }

    /// Final step of every spawn: `at`, moved aside if a body is standing within
    /// [`SPAWN_BODY_CLEAR`] of it. Returns `at` unchanged in the ordinary case.
    ///
    /// **Applied once per spawn, and last.** `hunter_entry` rings a whole wave out around
    /// its pad before calling this, so running it earlier would displace twice — the wave
    /// spread wider than its own ring and the "clustered at the marker" invariant broke.
    ///
    pub(crate) fn spawn_clear_of_bodies(&mut self, spawning: Spawning, at: Vec3) -> Vec3 {
        let occupants = self.spawn_occupants(spawning);
        match self.spawn_body_clearance(at, &occupants) {
            Some(clear) => {
                log::debug!(
                    "spawn: {spawning:?} stepped {:.1} m aside — a body was standing there",
                    at.distance(clear)
                );
                clear
            }
            None => at,
        }
    }

    /// If a body is standing within [`SPAWN_BODY_CLEAR`] of `at`, the nearest spot to
    /// step aside to — else `None` (the spawn point is already clear, the usual case).
    ///
    /// This is the half of Perfect Dark's `chr_adjust_pos_for_spawn` (`chraction.c:15018`)
    /// that our port dropped. PD runs that function **per shortlist candidate**, and it
    /// does two things: check the body fits, and if the spot is occupied, try eight
    /// directions at 60 cm and take the first that is clear — rejecting the pad outright
    /// if none is. We hoisted the "does it fit" half to pool-build time (`prepare_spawn`
    /// resolves every pad to a standable cell) and lost the occupancy half with it,
    /// because at pool-build time there are no bodies in the level yet.
    ///
    /// The distance gates upstream make this near-unreachable on a level with a real pool
    /// — a pad with someone within 60 cm fails the 10 m gate and the 2 m desperate bail
    /// alike. It fires on the case that has no alternative: the single-pad fallback pool
    /// of a level that authored no pads, where the whole wave and the player share one
    /// point. Materialising *inside* a hunter there is the one outcome worth a special
    /// case.
    ///
    /// PD's ring test is `cd_test_los_oobok_getfinalroom_autoflags(..., CDTYPE_BG)` — a
    /// line-of-sight check against background geometry — so the world-only raycast here is
    /// its direct analogue and keeps the step from crossing a wall.
    fn spawn_body_clearance(&mut self, at: Vec3, occupants: &[Vec3]) -> Option<Vec3> {
        let clear_of = |p: Vec3, occ: &[Vec3]| {
            occ.iter().all(|o| {
                Vec3::new(o.x - p.x, 0.0, o.z - p.z).length() >= SPAWN_BODY_CLEAR
                    && (o.y - p.y).abs() < 2.0 * SPAWN_BODY_CLEAR
            })
        };
        // Nobody in the way (and note a body on another floor is not in the way).
        if occupants.iter().all(|o| {
            Vec3::new(o.x - at.x, 0.0, o.z - at.z).length() >= SPAWN_BODY_CLEAR
                || (o.y - at.y).abs() >= 2.0 * SPAWN_BODY_CLEAR
        }) {
            return None;
        }
        // PD's eight directions at 60 cm, then two wider rings — the fallback pool can
        // have a whole wave stacked on one point, which one ring cannot clear.
        let eye = Vec3::new(0.0, 1.0, 0.0);
        for ring in 1..=3 {
            let r = SPAWN_BODY_CLEAR * ring as f32 * 1.5;
            for k in 0..8 {
                let a = std::f32::consts::FRAC_PI_4 * k as f32;
                let to = at + Vec3::new(a.sin() * r, 0.0, a.cos() * r);
                if !clear_of(to, occupants) {
                    continue;
                }
                let d = to - at;
                let len = d.length();
                if len < 1e-3 {
                    continue;
                }
                // Don't step through a wall to get there.
                let blocked = self
                    .physics
                    .raycast_world_only(at + eye, d / len, len)
                    .is_some_and(|hit| (hit.point - (at + eye)).length() < len - 0.1);
                if !blocked {
                    return Some(to);
                }
            }
        }
        None // hemmed in on every side — spawn on the pad and let the sim sort it out
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
                // A breached panel is gone for good — permanently open to pathing.
                nav.set_door_open(di, true);
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
