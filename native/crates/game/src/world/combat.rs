//! Player Combat runtime on `World` (HUNT-phase). P1: the weapon viewmodel.
//! P2: firing (edge/held), hitscan from the camera centre, muzzle flash, and hit
//! sparks. Ammo/reload (P3), recoil (P4), and player health + HUD (P5) land here
//! as the track builds out. Combat is inactive in BUILD (the fly-cam editor) —
//! every entry point below no-ops outside HUNT.

use super::*;

/// The looping background-music track (asset-relative under `native/assets/audio/`).
/// The JS default (`Game.ts`: `/music/102 Facility.mp3`). Plays in both BUILD and
/// HUNT, started once when the audio subsystem attaches and never stopped.
const BG_MUSIC: &str = "music/102 Facility.mp3";

/// Total weapon-switch dip duration (s): the outgoing gun lowers over the first
/// half, the incoming gun rises over the second. Deliberately short + fixed (not
/// tied to `reload_time`, which ranges 0.75–3 s) so switching stays snappy.
const SWITCH_TIME: f32 = 0.4;

/// Resolve one frame of free-aim from a mouse delta (pixels). Moves the crosshair
/// in aim space, clamps it to the [`AIM_MAX_RANGE`] circle, and returns the leftover
/// motion beyond the rim as a camera pan (in pixels, for `apply_look_delta`).
/// Returns `(new_aim_x, new_aim_y, pan_dx_px, pan_dy_px)`. Pure — unit-tested.
pub(crate) fn resolve_aim(aim_x: f32, aim_y: f32, dx: f32, dy: f32) -> (f32, f32, f32, f32) {
    // Screen Y is down; aim_y is up, so subtract dy.
    let raw_x = aim_x + dx * AIM_SENS;
    let raw_y = aim_y - dy * AIM_SENS;
    let mag = (raw_x * raw_x + raw_y * raw_y).sqrt();
    if mag > AIM_MAX_RANGE && mag > 1e-6 {
        let (nx, ny) = (raw_x / mag, raw_y / mag);
        // Pixels that couldn't move the (pinned) crosshair pan the camera instead.
        let over_px = (mag - AIM_MAX_RANGE) / AIM_SENS;
        (nx * AIM_MAX_RANGE, ny * AIM_MAX_RANGE, nx * over_px, -ny * over_px)
    } else {
        (raw_x, raw_y, 0.0, 0.0)
    }
}

/// Whether the straight path from `eye` to `target` is unobstructed by world
/// geometry — the line-of-sight test that gates explosion puffs so a fireball behind
/// a wall doesn't glow through it. A small end margin keeps the surface the puff is
/// stuck to (blast sits ~just off it) from counting as its own occluder.
fn los_clear(physics: &mut PhysicsWorld, eye: Vec3, target: Vec3) -> bool {
    let d = target - eye;
    let dist = d.length();
    if dist < 0.15 {
        return true;
    }
    physics
        .raycast_excluding(eye, d / dist, dist - 0.15, None)
        .is_none()
}

/// Paint blood onto a hunter's per-vertex colors at a world-space `hit` point (JS
/// `EnemyCharacter.paintDamage`): every vertex whose CURRENT (posed) world position
/// is within [`BLOOD_RADIUS`] reddens by `intensity · falloff` — `r` up toward 1,
/// `g`/`b` down toward 0 — **accumulating** on the existing color so repeated shots
/// build up persistent blood. `char_mat` is the character's world transform and
/// `joints` its skinning matrices (so `char_mat · skin(v) · v` = the vertex's world
/// position, matching the shader), which is why the blood lands where the shot
/// visually hit even mid-animation.
fn paint_blood(blood: &mut [f32], model: &SkinnedModel, char_mat: Mat4, joints: &[Mat4], hit: Vec3) {
    let radius = BLOOD_RADIUS;
    for (i, v) in model.vertices.iter().enumerate() {
        let src = Vec3::from(v.pos);
        // Linear-blend skin the vertex to its posed local position (CPU mirror of
        // the shader's LBS), then to world.
        let mut local = Vec3::ZERO;
        for k in 0..4 {
            let w = v.weights[k];
            if w != 0.0 {
                if let Some(m) = joints.get(v.joints[k] as usize) {
                    local += w * m.transform_point3(src);
                }
            }
        }
        let world = char_mat.transform_point3(local);
        let dist = world.distance(hit);
        if dist < radius {
            let blend = BLOOD_INTENSITY * (1.0 - dist / radius);
            let base = i * 3;
            blood[base] = (blood[base] + blend * 0.8).min(1.0); // r toward 1
            blood[base + 1] = (blood[base + 1] - blend).max(0.0); // g toward 0
            blood[base + 2] = (blood[base + 2] - blend).max(0.0); // b toward 0
        }
    }
}

/// Where a player shot landed on a hunter, classified by impact height above its
/// feet (a height-only proxy for the JS `BONE_ZONE_MAP`). Drives both the damage
/// multiplier and which hurt animation plays. Arms fold into `Torso` — a height
/// classifier can't separate them.
#[derive(Clone, Copy, Debug)]
enum HitZone {
    Head,
    Torso,
    Legs,
}

impl HitZone {
    /// Classify by impact height (metres) above the hunter's feet.
    fn classify(height: f32) -> Self {
        if height >= ZONE_HEAD_MIN {
            HitZone::Head
        } else if height < ZONE_LEG_MAX {
            HitZone::Legs
        } else {
            HitZone::Torso
        }
    }

    /// Damage multiplier (JS `ZONE_DAMAGE_MULTIPLIER`).
    fn damage_mult(self) -> f32 {
        match self {
            HitZone::Head => ZONE_HEAD_MULT,
            HitZone::Torso => ZONE_TORSO_MULT,
            HitZone::Legs => ZONE_LEG_MULT,
        }
    }

    /// The hurt-animation set fitting this zone.
    fn hurt_clips(self) -> &'static [&'static str] {
        match self {
            HitZone::Head => anim_set::HEAD_HIT_CLIPS,
            HitZone::Torso => anim_set::TORSO_HIT_CLIPS,
            HitZone::Legs => anim_set::LEG_HIT_CLIPS,
        }
    }
}

impl World {
    /// Attach the audio subsystem (called once at startup by the app, after the
    /// device is initialized). Preloads the weapon's fire/reload/empty sounds so
    /// the first shot doesn't hitch, starts the looping background music, and
    /// stores the manager so `combat_step` can play the weapon's queued cues.
    pub fn attach_audio(&mut self, mut audio: AudioManager) {
        // Preload the fire sound of EVERY weapon in the inventory (JS loads all
        // weapon sounds upfront) so the first shot after a swap never hitches on a
        // first-play decode. Reload/empty are shared, so load them once.
        for w in &self.weapons {
            audio.load(w.config().fire_sound);
        }
        audio.load(self.weapon().config().reload_sound);
        audio.load(self.weapon().config().empty_sound);
        // Track A enemy-hit SFX: the flesh bullet-hit + every pain vocal, so a hit
        // never hitches on a first-play decode.
        audio.load("sounds/enemies/bullet-hit.wav");
        for n in 1..=PAIN_COUNT {
            audio.load(&format!("sounds/enemies/pain-{n}.wav"));
        }
        // A3/P5: the player's own hit vocal. (Enemy gun reports reuse the player
        // weapon fire sounds, already preloaded in the loop above.)
        audio.load(PLAYER_HIT_SOUND);
        // Explosives: preload the blast so the first detonation doesn't hitch (the
        // launcher/throw/detonator fire sounds ride the per-weapon loop above).
        audio.load(EXPLOSION_SOUND);
        // Mines: the attach beep (on stick), the timed-mine arm beep, and the remote
        // detonation click — none is a weapon fire_sound, so preload them here.
        audio.load(MINE_PLACE_SOUND);
        audio.load(MINE_TIMER_SOUND);
        audio.load(DETONATOR_SOUND);
        audio.play_music(BG_MUSIC, true);
        self.audio = Some(audio);
    }

    /// The crosshair's screen-space offset this frame, in aspect-corrected NDC
    /// (so the circular aim boundary reads round on screen). `aspect` = w/h.
    /// `(0, 0)` = centered (BUILD, or HUNT not aiming). Fed to the renderer.
    pub fn aim_offset(&self, aspect: f32) -> (f32, f32) {
        (self.aim_x / aspect.max(1e-6), self.aim_y)
    }

    /// Whether the HUNT free-aim reticle should be drawn this frame — true only
    /// while **aiming** (RMB held), matching GoldenEye's aim-mode reticle. (BUILD
    /// draws its own small white editor cross via a separate renderer path, so it
    /// isn't gated on this.)
    pub fn crosshair_visible(&self) -> bool {
        self.aiming
    }

    /// The ammo-counter HUD quads for this frame, or `None` outside HUNT (BUILD is
    /// the fly-cam editor — no HUD). Right-aligned bottom-right; shows `MAG / RESERVE`,
    /// or `RELOADING` mid-reload. `aspect` = framebuffer w/h. Fed to the renderer's
    /// HUD pipeline each frame.
    pub fn hud_mesh(&self, aspect: f32) -> Option<Vec<engine::render::mesh::HudVertex>> {
        if self.mode != Mode::Hunt {
            return None;
        }
        // Dead → the "YOU DIED / PRESS R" text (over the dark death overlay); else
        // the ammo counter.
        if self.player_dead {
            Some(crate::hud::death_quads(aspect))
        } else {
            Some(crate::hud::ammo_quads(
                self.weapon().magazine(),
                self.weapon().reserve(),
                aspect,
            ))
        }
    }

    /// Manual weapon reload (the `R` key in HUNT). No-op outside HUNT; the weapon
    /// itself gates the request (not already reloading, not mid post-fire delay,
    /// magazine not full, reserve remaining).
    pub fn reload_weapon(&mut self) {
        if self.mode == Mode::Hunt {
            self.weapon_mut().request_reload();
        }
    }

    /// The active weapon (JS `WeaponSystem.slot`) — the inventory entry
    /// [`weapon_index`] points at.
    pub(crate) fn weapon(&self) -> &Weapon {
        &self.weapons[self.weapon_index]
    }
    pub(crate) fn weapon_mut(&mut self) -> &mut Weapon {
        &mut self.weapons[self.weapon_index]
    }

    /// Begin cycling to the next weapon (JS `WeaponSystem.cycleWeapon`, bound to
    /// `Q` / N64 `A`). No-op outside HUNT, with a single weapon, or while a switch
    /// is already running. Kicks off the lower→raise dip animation; the actual mesh
    /// swap + "rack" sound happen at the bottom of the dip, driven per-frame by
    /// [`Self::combat_step`]. Cancels any in-progress reload on the outgoing weapon
    /// (its ammo is preserved). The app polls [`Self::take_models_dirty`] to know
    /// when to re-upload the swapped gun/muzzle meshes.
    pub fn begin_weapon_switch(&mut self) {
        if self.mode != Mode::Hunt || self.weapons.len() < 2 || self.switching {
            return;
        }
        self.weapon_mut().cancel_reload();
        self.switching = true;
        self.switch_target = (self.weapon_index + 1) % self.weapons.len();
        self.switch_timer = 0.0;
        self.switch_swapped = false;
    }

    /// Drain the "weapon meshes changed" flag (a switch swapped the active gun's
    /// mesh mid-animation). The app re-uploads the viewmodel + muzzle when true.
    pub fn take_models_dirty(&mut self) -> bool {
        std::mem::take(&mut self.models_dirty)
    }

    /// Advance the weapon-switch dip one frame (HUNT). Runs the outgoing gun down to
    /// the bottom of the dip, swaps to `switch_target` there (loading its meshes +
    /// playing the "rack" reload sound, JS `loadCurrentWeapon`), then raises the new
    /// gun back up. Feeds the dip progress to the active viewmodel each frame. Called
    /// from [`Self::combat_step`]; no-op when not switching.
    fn switch_step(&mut self, dt: f32) {
        if !self.switching {
            return;
        }
        self.switch_timer += dt;
        let t = (self.switch_timer / SWITCH_TIME).min(1.0);

        // Halfway (gun at the bottom): swap the mesh + play the raise "rack".
        if !self.switch_swapped && t >= 0.5 {
            self.weapon_mut().view.cancel_switch(); // stop the outgoing gun's dip
            self.weapon_index = self.switch_target;
            let cfg = *self.weapon().config();
            let (gun, muzzle) = load_weapon_models(&cfg);
            self.gun_model = gun;
            self.muzzle_model = muzzle;
            self.models_dirty = true;
            // 0.7 = the shared reload volume (matches `combat::mod`'s `RELOAD_VOL`).
            if let Some(audio) = self.audio.as_mut() {
                audio.play(cfg.reload_sound, 0.7);
            }
            self.switch_swapped = true;
            log::info!(
                "weapon → {} ({}/{})",
                cfg.name,
                self.weapon().magazine(),
                self.weapon().reserve()
            );
        }

        // Drive the active viewmodel's dip (outgoing before the swap, incoming after).
        self.weapon_mut().view.set_switch_t(t);
        if t >= 1.0 {
            self.switching = false;
            self.weapon_mut().view.cancel_switch();
        }
    }
    /// The weapon's static gun mesh, for one-time GPU upload at startup. `None` if
    /// the asset failed to load.
    pub fn gun_model(&self) -> Option<&TexturedModel> {
        self.gun_model.as_ref()
    }

    /// The muzzle-flash mesh, for one-time GPU upload at startup. `None` if the
    /// weapon has no flash or the asset failed to load.
    pub fn muzzle_model(&self) -> Option<&TexturedModel> {
        self.muzzle_model.as_ref()
    }

    /// The gun's overlay clip transform this frame (`projection · viewmodel`), or
    /// `None` when the weapon shouldn't render — outside HUNT, or if the gun asset
    /// failed to load. `aspect` = framebuffer width / height. The renderer hides
    /// the gun on `None`.
    pub fn viewmodel_transform(&self, aspect: f32) -> Option<Mat4> {
        if self.mode != Mode::Hunt || self.gun_model.is_none() {
            return None;
        }
        Some(self.weapon().view.clip_transform(aspect, self.aim_x, self.aim_y))
    }

    /// The muzzle-flash overlay transform this frame, or `None` when it shouldn't
    /// render (outside HUNT, no flash asset, or no shot's flash currently active).
    /// The flash shares the gun's pivot/scale/rotation, so it uses the SAME clip
    /// transform as the gun (JS adds the flash to the same `model` group).
    pub fn muzzle_transform(&self, aspect: f32) -> Option<Mat4> {
        if self.mode != Mode::Hunt || self.muzzle_model.is_none() || !self.weapon().flash_active() {
            return None;
        }
        Some(self.weapon().view.clip_transform(aspect, self.aim_x, self.aim_y))
    }

    /// Advance the weapon one frame and fire if the trigger + cooldown allow it
    /// (called once per render frame in HUNT — JS `WeaponSystem.update(dt)`
    /// cadence, real dt). A shot casts a ray from the camera centre; a hit spawns
    /// a spark at the impact point. Also decays live sparks. No-op outside HUNT.
    pub fn combat_step(&mut self, dt: f32, input: &InputState) {
        if self.mode != Mode::Hunt {
            return;
        }

        // Decay hit sparks (drop the expired).
        for s in &mut self.sparks {
            s.ttl -= dt;
        }
        self.sparks.retain(|s| s.ttl > 0.0);

        // Advance any in-progress weapon switch (lower→swap→raise dip).
        self.switch_step(dt);

        // Fire: left mouse held (only while the cursor is grabbed), blocked mid
        // weapon-switch (JS gates fire on `!switching`). The weapon gates on
        // cooldown + the semi/auto edge rule.
        let trigger = input.pointer_locked && input.mouse_left_down() && !self.switching;
        let fired = self.weapon_mut().update(dt, trigger);

        // Play any sound cues the weapon queued this frame — fire, reload (manual
        // `R`, empty-click auto-reload, or the post-empty auto-reload), and the
        // empty click. Drained every frame regardless of whether a shot fired, so
        // a reload-only frame (e.g. `R` with a partial mag) still gets its sound.
        let cues = self.weapon_mut().take_cues();
        if let Some(audio) = self.audio.as_mut() {
            for cue in cues {
                audio.play(cue.name, cue.volume);
            }
        }

        // Advance explosives every frame (projectiles fly + detonate, VFX decays),
        // regardless of whether a shot fired this frame.
        self.explosives_step(dt);

        if !fired {
            return;
        }

        // A shot is a loud noise: nearby searching/investigating hunters converge on
        // it (firing while hidden gives you away). Engaged hunters keep their better
        // info; the seeking ones swing toward the sound.
        self.alert_enemies_to_noise();

        // A shot left the barrel — resolve the aim direction through the crosshair
        // (which may be offset by free-aim). Copy eye + look out so the character
        // borrow ends before the mutable physics borrow, then bend the ray toward
        // the crosshair's aim-space offset (same offset the gun tilts to).
        let Some((eye, fwd)) = self.character.as_ref().map(|c| (c.eye(), c.forward())) else {
            return;
        };
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        let dir = (fwd + AIM_FOV_TAN * (self.aim_x * right + self.aim_y * up)).normalize_or_zero();
        let dir = if dir == Vec3::ZERO { fwd } else { dir };

        // Delivery branches on the weapon's fire kind. Recoil is gun-only (the
        // viewmodel kick, armed in `Weapon::update`) — no camera kick, matching
        // GoldenEye — for every kind.
        match self.weapon().config().fire_kind {
            crate::combat::FireKind::Hitscan => self.fire_hitscan(eye, dir),
            crate::combat::FireKind::Projectile(spec) => {
                // Spawn a bit ahead of the eye so it clears the player, along the
                // aim; loft is added along world-up so grenades arc even when aimed
                // level. Grenades are "thrown" from the same origin — the arc + low
                // launch speed sell the throw.
                let proj = crate::combat::Projectile::spawn(eye + dir * 0.5, dir, Vec3::Y, spec);
                log::info!(
                    "launched {} projectile ({} m/s, blast r={} m)",
                    self.weapon().config().name,
                    spec.speed,
                    spec.explosion.radius
                );
                self.projectiles.push(proj);
            }
            crate::combat::FireKind::Mine(spec) => self.throw_mine(eye, dir, spec),
        }
    }

    /// Emit a gunfire noise ping at the player's position: every living hunter within
    /// [`GUNSHOT_HEARING_RANGE`] that's still hunting blind (searching / investigating)
    /// is pulled toward the sound to investigate. A hunter already engaged keeps its
    /// own (better) information — [`crate::enemy::Enemy::hear_noise`] gates that.
    fn alert_enemies_to_noise(&mut self) {
        let Some(ppos) = self.player_pos() else { return };
        for inst in &mut self.enemies {
            if inst.enemy.is_dead() {
                continue;
            }
            if inst.enemy.pos.distance(ppos) <= GUNSHOT_HEARING_RANGE {
                inst.enemy.hear_noise(ppos);
            }
        }
    }

    /// Throw a mine along the aim (a `FireKind::Mine` shot): spawn it just ahead of
    /// the eye, flying, and let [`Self::mines_step`] carry it until it sticks to the
    /// first surface it hits (wall/floor/ceiling) — where it then arms + trips. The
    /// throw sound rode the weapon's fire cue; the attach beep plays on the stick.
    /// Named by the weapon so the renderer finds its GLB.
    fn throw_mine(&mut self, eye: Vec3, dir: Vec3, spec: crate::combat::MineSpec) {
        let name = self.weapon().config().name;
        // Spawn a little ahead of the eye so it clears the player, along the aim.
        let mine = crate::combat::Mine::throw(eye + dir * 0.4, dir, Vec3::Y, spec, name);
        self.mines.push(mine);
        log::info!(
            "threw {} — arms {:.1}s after it sticks, blast r={:.1} m",
            name,
            spec.arm_time,
            spec.explosion.radius
        );
    }

    /// Set off every live Remote mine at once (player-triggered — pad A+B together or
    /// the keyboard detonate key; the mines carry the blast). Plays the detonation
    /// "click" and applies the blasts with chain reaction. No-op outside HUNT or while
    /// dead. Collect first, then apply — the borrow pattern used across the explosive
    /// step.
    pub fn detonate_remote_mines(&mut self) {
        if self.mode != Mode::Hunt || self.player_dead {
            return;
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.play(DETONATOR_SOUND, DETONATOR_VOL);
        }
        let mut dets: Vec<(Vec3, crate::combat::Explosion)> = Vec::new();
        let mut i = 0;
        while i < self.mines.len() {
            if self.mines[i].is_remote() {
                let m = self.mines.remove(i);
                dets.push((m.pos, m.spec.explosion));
            } else {
                i += 1;
            }
        }
        if dets.is_empty() {
            log::info!("detonator fired — no remote mines placed");
        } else {
            log::info!("detonator fired — {} remote mine(s) detonated", dets.len());
        }
        self.apply_detonations(dets);
    }

    /// The original instant-ray shot (the 19 base guns): cast from the eye along the
    /// aimed `dir`, damage a hit hunter or drop a wall spark. Split out of
    /// [`Self::combat_step`] so the fire path can branch cleanly on [`FireKind`].
    fn fire_hitscan(&mut self, eye: Vec3, dir: Vec3) {
        let range = self.weapon().config().range;
        // Player collider excluded — `None` today (the native player is a transient
        // shape-cast, not a registered collider), threaded for Track A.
        match crate::combat::shooting::cast(&mut self.physics, eye, dir, range, None) {
            Some(hit) if self.physics.is_enemy_collider(hit.collider) => {
                if let Some(i) = self
                    .enemies
                    .iter()
                    .position(|e| e.collider == hit.collider && !e.enemy.is_dead())
                {
                    self.hit_enemy(i, hit.point);
                }
            }
            Some(hit) => {
                // World geometry: nudge the marker just off the surface (z-fighting).
                self.sparks.push(Spark {
                    pos: hit.point + hit.normal * 0.01,
                    ttl: SPARK_TTL,
                });
                log::info!(
                    "shot hit at ({:.2}, {:.2}, {:.2}) dist {:.1} m",
                    hit.point.x,
                    hit.point.y,
                    hit.point.z,
                    hit.distance
                );
            }
            None => log::info!("shot — no hit within {range:.0} m"),
        }
    }

    /// Advance every live projectile one frame and decay the explosion VFX (HUNT
    /// only). Each projectile is swept from its old to its new position and raycast
    /// against the world: a contact either **bounces** it (grenades, while their
    /// fuse still burns) or **detonates** it (rockets, launched grenades on impact,
    /// or any grenade whose fuse is already spent); a spent fuse detonates it in the
    /// air; and a projectile that contacts nothing for [`PROJECTILE_MAX_LIFE`] is
    /// dropped silently so it can't leak. Detonations are collected first (they need
    /// `&mut self` for the blast) then applied.
    fn explosives_step(&mut self, dt: f32) {
        // Age the explosion puffs + refresh their line-of-sight visibility (so a
        // fireball behind a wall doesn't glow through it), then drop the finished.
        // Split-borrow blasts + physics (disjoint fields) so the raycast can run
        // while mutating each puff.
        let eye = self.character.as_ref().map(|c| c.eye());
        {
            let (blasts, physics) = (&mut self.blasts, &mut self.physics);
            for b in blasts.iter_mut() {
                b.age += dt;
                b.vis = match eye {
                    Some(e) if !los_clear(physics, e, b.pos) => 0.0,
                    _ => 1.0,
                };
            }
        }
        self.blasts.retain(|b| b.age < b.delay + b.life);

        // Advance + resolve each projectile; collect the detonation points.
        let mut detonations: Vec<(Vec3, crate::combat::Explosion)> = Vec::new();
        let mut i = 0;
        while i < self.projectiles.len() {
            // A settled bouncer just waits out its fuse in place — no integration.
            if self.projectiles[i].at_rest {
                self.projectiles[i].age += dt;
                if self.projectiles[i].fuse_expired() {
                    let p = &self.projectiles[i];
                    detonations.push((p.pos, p.spec.explosion));
                    self.projectiles.remove(i);
                } else {
                    i += 1;
                }
                continue;
            }

            let (from, to) = self.projectiles[i].advance(dt);
            let seg = to - from;
            let dist = seg.length();
            let mut resolved = false; // detonated OR dropped → remove this projectile

            if dist > 1e-6 {
                // Sweep the segment for a contact. The direction MUST be normalized:
                // rapier's time-of-impact is measured in multiples of the ray-dir
                // length, so a raw (length-`dist`) direction with `max_toi = dist`
                // would only test the first `dist²` metres — a fast/small per-frame
                // move then tunnels straight through walls and floors. Normalized,
                // `max_toi = dist` tests the whole segment in real metres.
                let dir = seg / dist;
                if let Some(hit) = crate::combat::shooting::cast(&mut self.physics, from, dir, dist, None) {
                    let p = &mut self.projectiles[i];
                    if p.spec.bounce > 0.0 && !p.fuse_expired() {
                        // Bounce off the surface, then decide whether it should keep
                        // going or settle: a gentle post-bounce speed means it's done
                        // moving, so rest it in place (stops the resting jitter);
                        // otherwise reseat just off the surface and keep riding the fuse.
                        p.bounce_off(hit.normal);
                        if p.vel.length() < PROJECTILE_REST_SPEED {
                            p.come_to_rest(hit.point, hit.normal);
                        } else {
                            p.pos = hit.point + hit.normal * 0.02;
                        }
                    } else {
                        detonations.push((hit.point, p.spec.explosion));
                        resolved = true;
                    }
                }
            }

            // Fuse burnout detonates in the air (only if it didn't already contact).
            if !resolved && self.projectiles[i].fuse_expired() {
                let p = &self.projectiles[i];
                detonations.push((p.pos, p.spec.explosion));
                resolved = true;
            }

            // A projectile that never hits anything (fuseless rocket into the void)
            // is dropped without a boom once it's lived too long.
            if !resolved && self.projectiles[i].age > PROJECTILE_MAX_LIFE {
                log::info!("projectile expired without contact — dropped");
                resolved = true;
            }

            if resolved {
                self.projectiles.remove(i);
            } else {
                i += 1;
            }
        }

        // Advance placed mines (arm timers + trip checks) and fold their detonations
        // in with the projectiles', then apply the whole batch — cascading through
        // any mines caught in a blast (sympathetic detonation).
        detonations.extend(self.mines_step(dt));
        self.apply_detonations(detonations);
    }

    /// Advance every placed mine one frame (HUNT, called from [`Self::explosives_step`]):
    /// tick each mine's arm timer (beeping once when a timed mine goes live), then
    /// collect the ones that trip this frame — an armed proximity mine when any living
    /// hunter OR the player is within its trip radius, an armed timed mine at 0.
    /// Returns the detonation points (removing the tripped mines); the caller applies
    /// them. Remote mines never self-trip (only a player detonation sets them off).
    ///
    /// A mine still in flight is first swept from its old to its new position and
    /// raycast against the world (same normalized-dir sweep the projectiles use, so a
    /// fast toss can't tunnel a thin wall); the first surface contact **sticks** it
    /// there (playing the attach beep), oriented to the surface normal. A toss that
    /// hits nothing for [`MINE_MAX_FLIGHT`] seconds sticks in place as a fallback.
    fn mines_step(&mut self, dt: f32) -> Vec<(Vec3, crate::combat::Explosion)> {
        // Trip targets: living hunters + the player, measured at centre-mass so a
        // mine on the floor still notices a nearby actor. Read out first (the tick
        // + removal below borrows `self.mines`/`self.audio` mutably).
        let mut targets: Vec<Vec3> = self
            .enemies
            .iter()
            .filter(|e| !e.enemy.is_dead())
            .map(|e| e.enemy.pos + Vec3::Y * ENEMY_CENTER_Y)
            .collect();
        if let Some(ppos) = self.player_pos() {
            targets.push(ppos + Vec3::Y * PLAYER_CENTER_Y);
        }

        let mut detonations: Vec<(Vec3, crate::combat::Explosion)> = Vec::new();
        let mut i = 0;
        while i < self.mines.len() {
            // In flight: fly + sweep for a surface to stick to.
            if !self.mines[i].stuck {
                let (from, to) = self.mines[i].advance(dt);
                let seg = to - from;
                let dist = seg.length();
                let mut stuck_now = false;
                if dist > 1e-6 {
                    let dir = seg / dist; // normalized — see the projectile sweep note
                    if let Some(hit) =
                        crate::combat::shooting::cast(&mut self.physics, from, dir, dist, None)
                    {
                        let pos = hit.point + hit.normal * MINE_SURFACE_OFFSET;
                        self.mines[i].stick(pos, hit.normal);
                        stuck_now = true;
                    }
                }
                // Fallback: a toss that never contacts anything sticks where it is.
                if !stuck_now && self.mines[i].flight_time > MINE_MAX_FLIGHT {
                    let pos = self.mines[i].pos;
                    self.mines[i].stick(pos, Vec3::Y);
                    stuck_now = true;
                }
                if stuck_now {
                    if let Some(audio) = self.audio.as_mut() {
                        audio.play(MINE_PLACE_SOUND, MINE_PLACE_VOL);
                    }
                }
                i += 1;
                continue;
            }

            // Stuck: arm + trip.
            let just_armed = self.mines[i].tick(dt);
            // A timed mine chirps once when it goes live.
            if just_armed && matches!(self.mines[i].spec.trigger, crate::combat::MineTrigger::Timed(_)) {
                if let Some(audio) = self.audio.as_mut() {
                    audio.play(MINE_TIMER_SOUND, MINE_TIMER_VOL);
                }
            }
            let trips = self.mines[i].timed_expired()
                || targets.iter().any(|&t| self.mines[i].proximity_trips(t));
            if trips {
                let m = self.mines.remove(i);
                detonations.push((m.pos, m.spec.explosion));
            } else {
                i += 1;
            }
        }
        detonations
    }

    /// Apply a batch of detonations, cascading through any placed mines caught in a
    /// blast (chain reaction / sympathetic detonation): each blast trips every mine
    /// within its radius, whose own blast is queued in turn, so a cluster goes up
    /// together. Collect-then-apply keeps the `&mut self` borrow simple. Shared by
    /// [`Self::explosives_step`] and [`Self::detonate_remote_mines`].
    fn apply_detonations(&mut self, initial: Vec<(Vec3, crate::combat::Explosion)>) {
        let mut queue = initial;
        while let Some((center, ex)) = queue.pop() {
            // Sympathetic detonation: any mine within this blast goes up too.
            let mut i = 0;
            while i < self.mines.len() {
                if self.mines[i].pos.distance(center) <= ex.radius {
                    let m = self.mines.remove(i);
                    queue.push((m.pos, m.spec.explosion));
                } else {
                    i += 1;
                }
            }
            self.detonate(center, ex);
        }
    }

    /// Detonate a blast of `explosion` at `center`: spawn the VFX burst + play the
    /// explosion SFX, then apply radius-falloff damage to every actor whose
    /// centre-mass lies inside the blast sphere — each living hunter AND the player.
    /// Distance is measured to centre-mass (not feet), so an overhead or point-blank
    /// burst still bites.
    fn detonate(&mut self, center: Vec3, explosion: crate::combat::Explosion) {
        // Layered fireball VFX: a central core puff plus satellites at small random
        // offsets with staggered starts + varied sizes — GoldenEye builds its big
        // fireball from several overlapping sprites, which reads as one dense,
        // roiling, lingering explosion. (Damage below is applied once, here.)
        let r = explosion.radius;
        let puffs = (BLAST_PUFFS_MIN + (r * 0.5) as usize).clamp(BLAST_PUFFS_MIN, BLAST_PUFFS_MAX);
        for k in 0..puffs {
            let (offset, delay, size) = if k == 0 {
                (Vec3::ZERO, 0.0, 1.0) // anchored core: full size, immediate
            } else {
                let s = r * BLAST_SPREAD_FRAC;
                let off = Vec3::new(
                    (self.rand_float() * 2.0 - 1.0) * s,
                    (self.rand_float() * 2.0 - 1.0) * s * 0.6, // less vertical scatter
                    (self.rand_float() * 2.0 - 1.0) * s,
                );
                (off, self.rand_float() * BLAST_STAGGER, 0.55 + self.rand_float() * 0.4)
            };
            let life = BLAST_TTL * (0.85 + self.rand_float() * 0.3);
            self.blasts.push(Blast {
                pos: center + offset,
                age: 0.0,
                delay,
                life,
                half: r * BLAST_QUAD_HALF_FRAC * size,
                vis: 1.0,
            });
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.play(EXPLOSION_SOUND, EXPLOSION_VOL);
        }
        log::info!(
            "BOOM at ({:.1}, {:.1}, {:.1}) — r={:.1} m, max {:.0} dmg",
            center.x,
            center.y,
            center.z,
            explosion.radius,
            explosion.max_damage
        );

        // Hunters in range (centre-mass distance → falloff damage).
        for idx in 0..self.enemies.len() {
            let alive_pos = match self.enemies.get(idx) {
                Some(inst) if !inst.enemy.is_dead() => inst.enemy.pos,
                _ => continue,
            };
            let center_mass = alive_pos + Vec3::Y * ENEMY_CENTER_Y;
            let dmg = crate::combat::falloff_damage(&explosion, center_mass.distance(center));
            if dmg > 0.0 {
                self.blast_hit_enemy(idx, dmg, center_mass);
            }
        }

        // The player, if inside the blast (splash hurts you too — mind your feet).
        if let Some(ppos) = self.player_pos() {
            let center_mass = ppos + Vec3::Y * PLAYER_CENTER_Y;
            let dmg = crate::combat::falloff_damage(&explosion, center_mass.distance(center));
            if dmg > 0.0 {
                self.take_player_damage(dmg);
            }
        }
    }

    /// Apply `dmg` blast damage to hunter `idx` (already verified in range). Plays
    /// the pain + flesh-hit SFX, and on the lethal blast removes the capsule collider
    /// and plays a death animation; otherwise a torso stagger. A whole-body blast has
    /// no hit zone, so it uses the torso hurt set. Mirrors the death/hurt tail of
    /// [`Self::hit_enemy`] without the per-vertex blood paint (the fireball is the
    /// feedback).
    fn blast_hit_enemy(&mut self, idx: usize, dmg: f32, at: Vec3) {
        let _ = at; // reserved for a future directional knockback/blood
        let (died, collider) = match self.enemies.get_mut(idx) {
            Some(inst) if !inst.enemy.is_dead() => (inst.enemy.take_damage(dmg), inst.collider),
            _ => return,
        };

        let pain = self.rand_below(PAIN_COUNT) + 1;
        if let Some(audio) = self.audio.as_mut() {
            audio.play(&format!("sounds/enemies/pain-{pain}.wav"), PAIN_VOL);
            audio.play("sounds/enemies/bullet-hit.wav", BULLET_HIT_VOL);
        }

        if died {
            self.physics.remove_enemy_collider(collider);
            let death_start = CHAR_HIT_START + anim_set::HIT_CLIPS.len();
            let pick = self.rand_below(anim_set::DEATH_CLIPS.len());
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.anim.play_once(death_start + pick, 0.2, None, None);
            }
            log::info!("HUNTER DOWN (blast, {dmg:.0} dmg)");
        } else {
            let clips = anim_set::TORSO_HIT_CLIPS;
            let name = clips[self.rand_below(clips.len())];
            let clip = CHAR_HIT_START + anim_set::hit_clip_pos(name).unwrap_or(0);
            let Some(inst) = self.enemies.get_mut(idx) else { return };
            let band = band_for_speed(inst.enemy.speed());
            let dur = inst.anim.clip(clip).map(|c| c.duration).unwrap_or(0.4);
            inst.anim.play_once(clip, 0.1, Some(band), None);
            inst.enemy.stun(dur);
            let hp = inst.enemy.health();
            log::info!("hunter caught in blast — {dmg:.0} dmg, {hp:.0} hp left");
        }
    }

    /// Apply a player-weapon hit to hunter `idx` at world impact point `hit_point`
    /// (Track A). The [`HitZone`] (head/torso/legs, from the impact height above the
    /// hunter's feet) scales the damage (headshots hit ×4) and picks a fitting hurt
    /// animation; the impact also **paints blood** onto the nearby vertices
    /// (accumulating, persistent). On the lethal shot plays a random death one-shot
    /// (clamps) and removes the capsule collider (a corpse can't be shot). Otherwise
    /// plays the zone's hurt reaction, which auto-returns to locomotion, and stuns
    /// the hunter for the clip's length. Always plays the pain + bullet-hit SFX (JS
    /// `onHit`). The death fade begins later, once the death animation finishes.
    pub(crate) fn hit_enemy(&mut self, idx: usize, hit_point: Vec3) {
        let base = self.weapon().config().damage;
        // Paint blood at the impact (before damage, so it shows even on the kill
        // shot). Needs the shared model (immut) + this hunter's pose/blood (mut) —
        // disjoint fields, split-borrowed. `char_feet_offset` read out first.
        let feet_offset = self.char_feet_offset;
        if let Some(model) = self.char_model.as_ref() {
            if let Some(inst) = self.enemies.get_mut(idx) {
                if !inst.enemy.is_dead() {
                    let joints = inst.anim.skinning_matrices(&model.skeleton);
                    let feet = inst.enemy.pos;
                    let char_mat = Mat4::from_translation(Vec3::new(
                        feet.x,
                        feet.y + feet_offset,
                        feet.z,
                    )) * Mat4::from_rotation_y(inst.yaw())
                        * Mat4::from_scale(Vec3::splat(CHAR_SCALE));
                    paint_blood(&mut inst.blood, model, char_mat, &joints, hit_point);
                }
            }
        }
        // Classify the zone, scale the damage, apply — bail if already dead / gone.
        let (died, collider, dmg, zone) = match self.enemies.get_mut(idx) {
            Some(inst) if !inst.enemy.is_dead() => {
                let zone = HitZone::classify(hit_point.y - inst.enemy.pos.y);
                let dmg = base * zone.damage_mult();
                (inst.enemy.take_damage(dmg), inst.collider, dmg, zone)
            }
            _ => return,
        };

        // On-hit SFX: a random pain vocal + the flesh bullet-hit.
        let pain = self.rand_below(PAIN_COUNT) + 1;
        if let Some(audio) = self.audio.as_mut() {
            audio.play(&format!("sounds/enemies/pain-{pain}.wav"), PAIN_VOL);
            audio.play("sounds/enemies/bullet-hit.wav", BULLET_HIT_VOL);
        }

        if died {
            // Remove the capsule now; the body stays visible (opacity 1) until the
            // death animation finishes, then fades (see `advance_animation`).
            self.physics.remove_enemy_collider(collider);
            let death_start = CHAR_HIT_START + anim_set::HIT_CLIPS.len();
            let pick = self.rand_below(anim_set::DEATH_CLIPS.len());
            if let Some(inst) = self.enemies.get_mut(idx) {
                // No return target → the death pose clamps and holds while it fades.
                inst.anim.play_once(death_start + pick, 0.2, None, None);
            }
            log::info!("HUNTER DOWN ({zone:?}, {dmg:.0} dmg — {})", anim_set::DEATH_CLIPS[pick]);
        } else {
            // Pick a hurt clip fitting the zone, resolve it to an AnimPlayer index.
            let clips = zone.hurt_clips();
            let name = clips[self.rand_below(clips.len())];
            let clip = CHAR_HIT_START + anim_set::hit_clip_pos(name).unwrap_or(0);
            let Some(inst) = self.enemies.get_mut(idx) else { return };
            // Return to the current locomotion band so the one-shot flips
            // `is_playing_oneshot` back off, letting the HUNT driver resume.
            let band = band_for_speed(inst.enemy.speed());
            let dur = inst.anim.clip(clip).map(|c| c.duration).unwrap_or(0.4);
            inst.anim.play_once(clip, 0.1, Some(band), None);
            inst.enemy.stun(dur);
            let hp = inst.enemy.health();
            log::info!("hunter hit — {zone:?} {dmg:.0} dmg, {hp:.0} hp left ({name})");
        }
    }

    /// Start a fire burst on hunter `idx`'s mixer — it entered `attack` (A3). Plays
    /// its weapon-class fire one-shot (rifle / pistol / dual) with that clip's
    /// FIRE_TIMING window; the per-shot cadence + damage roll run in
    /// [`Self::enemy_combat_step`]. Resets the cadence so the first shot waits for
    /// the window's `fireStart`.
    pub(crate) fn start_enemy_fire(&mut self, idx: usize) {
        let Some(inst) = self.enemies.get_mut(idx) else { return };
        let clip = fire_clip_index(inst.weapon.class, inst.dual);
        let win = fire_window_for(inst.weapon.class, inst.dual);
        // Return to idle when done; the HUNT driver re-selects a band after.
        inst.anim.play_once(clip, 0.1, Some(0), Some(win));
        inst.shot_timer = 0.0;
        log::info!("hunter firing ({})", inst.weapon.name);
    }

    /// Per-frame enemy combat + player damage-feedback (HUNT only). Pumps EACH
    /// hunter's shots while its fire animation is inside the FIRE_TIMING window —
    /// one shot per `1/fireRate` seconds, the JS `EnemyCharacter.tick` pump — and
    /// decays the per-hunter muzzle flashes + the red damage flash + the health-HUD
    /// pop timer. Called once per render frame after [`Self::advance_animation`]
    /// (which advances the fire windows).
    pub fn enemy_combat_step(&mut self, dt: f32) {
        if self.mode != Mode::Hunt {
            return;
        }
        // Player feedback timers (once per frame, run even while dead so a final
        // flash fades).
        if self.damage_flash > 0.0 {
            self.damage_flash = (self.damage_flash - dt * DAMAGE_FLASH_DECAY).max(0.0);
        }
        if self.hud_show_timer > 0.0 {
            self.hud_show_timer = (self.hud_show_timer - dt).max(0.0);
        }
        // Per-hunter muzzle decay (blood is persistent — no decay).
        for inst in &mut self.enemies {
            if inst.muzzle_timer > 0.0 {
                inst.muzzle_timer = (inst.muzzle_timer - dt).max(0.0);
            }
        }
        if self.player_dead {
            return;
        }

        // Each hunter fires only while its FIRE one-shot is inside its window
        // (the FIRE_TIMING mapping), spaced by 1/fireRate. Collect the shot events
        // first (emitting needs `&mut self`, which would clash with the iterator).
        let mut shots: Vec<usize> = Vec::new();
        for (i, inst) in self.enemies.iter_mut().enumerate() {
            let firing =
                inst.anim.is_playing_oneshot() && is_fire_clip(inst.anim.current_clip());
            if !firing {
                inst.shot_timer = 0.0;
                continue;
            }
            if inst.anim.fire_window_open() {
                inst.shot_timer -= dt;
                if inst.shot_timer <= 0.0 {
                    inst.shot_timer = 1.0 / inst.weapon.fire_rate.max(0.001);
                    shots.push(i);
                }
            }
        }
        for i in shots {
            self.emit_enemy_shot(i);
        }
    }

    /// One shot from hunter `idx` (JS `EnemyCharacter.onShotFired` + the AI damage
    /// callback): muzzle flash + the weapon's gun report always; then, when LOS is
    /// clear, roll `accuracy·(1−dist/range)` and apply the weapon's damage to the
    /// player on a hit. Uses the equipped weapon's stats.
    fn emit_enemy_shot(&mut self, idx: usize) {
        let (epos, collider, weapon) = match self.enemies.get(idx) {
            Some(inst) if !inst.enemy.is_dead() => (inst.enemy.pos, inst.collider, inst.weapon),
            _ => return,
        };
        let Some(ppos) = self.player_pos() else { return };
        // Flash + report fire on every shot, hit or miss.
        if let Some(inst) = self.enemies.get_mut(idx) {
            inst.muzzle_timer = ENEMY_MUZZLE_TIME;
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.play(weapon.fire_sound, ENEMY_FIRE_VOL);
        }
        // Walls (and other hunters) block the shot (re-checked per shot).
        if !crate::enemy::line_of_sight(&mut self.physics, epos, ppos, collider) {
            return;
        }
        let dist = Vec3::new(ppos.x - epos.x, 0.0, ppos.z - epos.z).length();
        let dist_factor = (1.0 - dist / weapon.range).max(0.0);
        let hit_chance = weapon.accuracy * dist_factor;
        if self.rand_float() < hit_chance {
            self.take_player_damage(weapon.damage);
        }
    }

    /// Apply `dmg` to the player (JS `Actor.takeDamage`: armor-first, then health)
    /// with the damage feedback — red flash (peak α = min(0.5, dmg/40)), the
    /// breathe SFX, and the health-HUD pop. Death (→ YOU DIED) at 0 health.
    pub(crate) fn take_player_damage(&mut self, dmg: f32) {
        if self.player_dead {
            return;
        }
        let absorbed = self.player_armor.min(dmg);
        self.player_armor -= absorbed;
        let to_health = dmg - absorbed;
        self.player_health = (self.player_health - to_health).max(0.0);
        self.damage_flash = (dmg / 40.0).min(0.5);
        self.hud_show_timer = HUD_SHOW_TIME;
        if let Some(audio) = self.audio.as_mut() {
            audio.play(PLAYER_HIT_SOUND, PLAYER_HIT_VOL);
        }
        if self.player_health <= 0.0 {
            self.player_dead = true;
            log::info!("YOU DIED — press R to restart");
        }
    }

    /// xorshift64 → a float in `[0, 1)` (reuses the character RNG state) for the
    /// probabilistic hit roll.
    fn rand_float(&mut self) -> f32 {
        (self.rand_below(1 << 24) as f32) / ((1u32 << 24) as f32)
    }

    /// Player health / armor + death, for the HUD and the app's restart routing.
    pub fn player_health(&self) -> f32 {
        self.player_health
    }
    pub fn player_armor(&self) -> f32 {
        self.player_armor
    }
    pub fn is_player_dead(&self) -> bool {
        self.player_dead
    }
    /// Red damage-flash alpha this frame (0 = none).
    pub fn damage_flash(&self) -> f32 {
        self.damage_flash
    }
    /// The radial health graphic's pixel dimensions (for the renderer texture),
    /// or `None` if it failed to load.
    pub fn health_hud_dims(&self) -> Option<(u32, u32)> {
        self.health_hud.as_ref().map(|h| (h.w, h.h))
    }

    /// Bake the radial-health RGBA for the current health/armor (top-down segment
    /// depletion). `None` if the graphic failed to load. Re-baked only when health
    /// changes (the app tracks that).
    pub fn health_hud_rgba(&self) -> Option<Vec<u8>> {
        let h = self.health_hud.as_ref()?;
        let hp = (self.player_health / PLAYER_MAX_HEALTH).clamp(0.0, 1.0);
        let ap = (self.player_armor / PLAYER_MAX_ARMOR).clamp(0.0, 1.0);
        Some(h.render(hp, ap))
    }

    /// Radial-HUD opacity this frame (pops to 1 on damage, fades over the last
    /// [`HUD_FADE_TAIL`] seconds). 0 = hidden.
    pub fn hud_alpha(&self) -> f32 {
        if self.hud_show_timer <= 0.0 {
            0.0
        } else if self.hud_show_timer > HUD_FADE_TAIL {
            1.0
        } else {
            self.hud_show_timer / HUD_FADE_TAIL
        }
    }

    /// Restart after death (the `R` key on the YOU DIED screen): reset player
    /// health/armor and return to BUILD (which also clears the hunter + colliders).
    pub fn restart_after_death(&mut self) {
        if !self.player_dead {
            return;
        }
        self.player_health = PLAYER_MAX_HEALTH;
        self.player_armor = 0.0;
        self.player_dead = false;
        self.damage_flash = 0.0;
        self.hud_show_timer = 0.0;
        if self.mode == Mode::Hunt {
            self.toggle_mode();
        }
    }

    /// A combined colored mesh of the live hit sparks (bright impact markers) and the
    /// in-flight model-less projectiles (the rocket's bright box + trail), for the
    /// renderer's spark pass. `None` when nothing is active. Explosion fireballs are
    /// textured billboards (see [`Self::blast_mesh`]); grenade rounds draw their GLB
    /// (see `enemy_weapon_draws`), so neither appears here.
    pub fn spark_mesh(&self) -> Option<ColoredMesh> {
        if self.sparks.is_empty() && self.projectiles.iter().all(|p| !p.spec.model.is_empty()) {
            return None;
        }
        let mut verts: Vec<ColorVertex> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for s in &self.sparks {
            let min = s.pos - Vec3::splat(SPARK_HALF);
            let max = s.pos + Vec3::splat(SPARK_HALF);
            push_colored_box(&mut verts, &mut idx, min, max, [1.0, 0.92, 0.35]);
        }
        // In-flight projectiles WITHOUT a GLB (the rocket): a bright box at the
        // round's current position plus a short motion trail stepping back along its
        // travel, so it reads as a streak crossing the room. Model-carrying rounds
        // (the grenades) draw their GLB via `enemy_weapon_draws` instead.
        for p in self.projectiles.iter().filter(|p| p.spec.model.is_empty()) {
            push_colored_box(
                &mut verts,
                &mut idx,
                p.pos - Vec3::splat(PROJECTILE_HALF),
                p.pos + Vec3::splat(PROJECTILE_HALF),
                [1.0, 0.9, 0.55], // hot near-white core
            );
            let step = -p.vel.normalize_or_zero() * (PROJECTILE_HALF * 1.6);
            for t in 1..=PROJECTILE_TRAIL {
                let tf = t as f32 / (PROJECTILE_TRAIL as f32 + 1.0);
                let c = p.pos + step * t as f32;
                let h = PROJECTILE_HALF * (1.0 - tf * 0.6); // taper toward the tail
                push_colored_box(
                    &mut verts,
                    &mut idx,
                    c - Vec3::splat(h),
                    c + Vec3::splat(h),
                    [1.0, 0.5 + 0.35 * (1.0 - tf), (0.5 - 0.45 * tf).max(0.05)], // → orange/red
                );
            }
        }

        // (Blasts now render as textured billboards — see `blast_mesh` — not here.)
        Some(ColoredMesh {
            vertices: verts,
            indices: idx,
        })
    }

    /// The explosion-fireball billboards this frame: one camera-facing quad per live
    /// blast, playing the baked GoldenEye fireball atlas. Each quad steps through the
    /// [`BLAST_FRAMES`] atlas frames by the blast's age, scales up, and fades out —
    /// the signed-off preview pipeline, now drawn additively in world space by the
    /// renderer's billboard pass. `None` outside HUNT or when no blasts are live.
    /// Quads face the player using the camera's right/up basis (spherical billboard).
    pub fn blast_mesh(&self) -> Option<TexturedMesh> {
        if self.blasts.is_empty() {
            return None;
        }
        // Camera basis from the player's eye/look (same as the fire-ray derivation).
        let (_eye, fwd) = self.character.as_ref().map(|c| (c.eye(), c.forward()))?;
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        if right == Vec3::ZERO || up == Vec3::ZERO {
            return None; // looking straight up/down — skip this frame
        }

        let ease_out = |x: f32| 1.0 - (1.0 - x) * (1.0 - x);
        let mut m = TexturedMesh::default();
        for b in &self.blasts {
            if b.vis <= 0.0 {
                continue; // occluded by a wall this frame
            }
            // Per-puff local time (0→1) over its own life, after its start delay.
            let local = (b.age - b.delay) / b.life;
            if !(0.0..1.0).contains(&local) {
                continue; // not started yet, or finished
            }
            let fi = ((local * BLAST_FRAMES as f32) as usize).min(BLAST_FRAMES - 1);
            let scale_anim = 0.55 + 0.9 * ease_out(local);
            let half = b.half * scale_anim;
            let alpha = if local < 0.7 { 1.0 } else { (1.0 - (local - 0.7) / 0.3).max(0.0) };

            // Atlas frame sub-rect (half-texel inset to avoid neighbour-frame bleed).
            let u0 = fi as f32 / BLAST_FRAMES as f32 + BLAST_UV_INSET_U;
            let u1 = (fi + 1) as f32 / BLAST_FRAMES as f32 - BLAST_UV_INSET_U;
            let v0 = BLAST_UV_INSET_V;
            let v1 = 1.0 - BLAST_UV_INSET_V;

            let c = b.pos;
            let color = [1.0, 1.0, 1.0, alpha]; // atlas is pre-coloured; alpha = fade
            let n = [0.0, 0.0, 1.0]; // unused by the billboard shader
            let base = m.vertices.len() as u32;
            // TL, TR, BR, BL
            let corners = [
                c - right * half + up * half,
                c + right * half + up * half,
                c + right * half - up * half,
                c - right * half - up * half,
            ];
            let uvs = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
            for k in 0..4 {
                m.vertices.push(TexVertex {
                    pos: corners[k].to_array(),
                    normal: n,
                    uv: uvs[k],
                    color,
                });
            }
            m.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Some(m)
    }
}
