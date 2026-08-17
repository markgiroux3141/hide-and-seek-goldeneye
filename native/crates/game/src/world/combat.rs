//! Player Combat runtime on `World` (HUNT-phase). P1: the weapon viewmodel.
//! P2: firing (edge/held), hitscan from the camera centre, muzzle flash, and hit
//! sparks. Ammo/reload (P3), recoil (P4), and player health + HUD (P5) land here
//! as the track builds out. Combat is inactive in BUILD (the fly-cam editor) —
//! every entry point below no-ops outside HUNT.

use super::*;
use crate::combat::attack_anim;

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

/// Closest approach between the ray `origin + dir·t` (`dir` unit, `t ∈ [0, max_t]`) and
/// the finite segment `a`→`b`. Returns `(distance, t)`.
///
/// This is the hit test for a [PD simulant's round](World::emit_pd_shot): the segment is
/// a target's torso, so comparing the returned distance against [`PD_TORSO_RADIUS`] is a
/// ray-versus-capsule test. Segment rather than point because a body is tall — a round
/// with a little elevation error should clip a shoulder or a hip rather than register as
/// a clean centre hit or a clean miss.
///
/// The standard clamped closest-points solve: minimise |P(s) − Q(t)| over the two
/// parameters, clamp `t` into the segment, then re-solve `s` for that clamped `t` and
/// clamp it into the ray. The re-solve is what makes the answer correct when the true
/// minimum lies off the end of the segment (a shot passing above the head).
fn ray_segment_closest(origin: Vec3, dir: Vec3, max_t: f32, a: Vec3, b: Vec3) -> (f32, f32) {
    let v = b - a;
    let w0 = origin - a;
    let (bb, cc) = (dir.dot(v), v.dot(v));
    let (dd, ee) = (dir.dot(w0), v.dot(w0));
    let den = cc - bb * bb; // dir is unit, so a·a == 1
    let mut t = if den.abs() < 1e-8 {
        // Ray parallel to the segment — any point does; take the segment's near end.
        if cc > 1e-8 { (-ee / cc).clamp(0.0, 1.0) } else { 0.0 }
    } else {
        (ee - bb * dd) / den
    };
    t = t.clamp(0.0, 1.0);
    let s = (t * bb - dd).clamp(0.0, max_t);
    let miss = ((origin + dir * s) - (a + v * t)).length();
    (miss, s)
}

/// Which body part a world-space `hit` landed on — Perfect Dark's `HITPART_*`,
/// resolved by finding the **posed vertex** nearest the impact and reading the bone
/// it is weighted to.
///
/// Nearest *vertex* rather than nearest *bone origin*: bone origins are joints, so a
/// shot to the middle of a thigh is roughly equidistant from the hip and the knee and
/// the answer flips on sub-centimetre noise. The skin has no such ambiguity, and it is
/// the same geometry the blood is painted onto, so the stain and the reaction agree.
/// `None` if the model has no vertices or the nearest one is weighted only to joints
/// with no anatomical meaning (a blend joint).
fn nearest_hit_part(
    model: &SkinnedModel,
    char_mat: Mat4,
    joints: &[Mat4],
    hit: Vec3,
) -> Option<crate::combat::hit_anim::HitPart> {
    let mut best: Option<(f32, usize)> = None;
    for v in &model.vertices {
        let src = Vec3::from(v.pos);
        let mut p = Vec3::ZERO;
        let mut heaviest = (0.0f32, usize::MAX);
        for k in 0..4 {
            let w = v.weights[k];
            if w == 0.0 {
                continue;
            }
            let j = v.joints[k] as usize;
            if let Some(m) = joints.get(j) {
                p += w * m.transform_point3(src);
            }
            if w > heaviest.0 {
                heaviest = (w, j);
            }
        }
        if heaviest.1 == usize::MAX {
            continue;
        }
        let d = char_mat.transform_point3(p).distance_squared(hit);
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, heaviest.1));
        }
    }
    let joint = best?.1;
    let name = model.skeleton.names.get(joint)?;
    crate::combat::hit_anim::HitPart::for_bone(name)
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
    /// Classify by impact height (metres) above the hunter's feet, against that
    /// body's own `(head_min, leg_max)` boundaries — the families differ in height,
    /// so a fixed 1.1 m head line would be mid-chest on a Perfect Dark body.
    fn classify(height: f32, (head_min, leg_max): (f32, f32)) -> Self {
        if height >= head_min {
            HitZone::Head
        } else if height < leg_max {
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
        // Enemy grenade flush (#5): the toss SFX isn't a player weapon sound, so
        // preload it here or the first lob hitches.
        audio.load(GRENADE_THROW_SOUND);
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
            let mut q = crate::hud::ammo_quads(
                self.weapon().magazine(),
                self.weapon().reserve(),
                aspect,
            );
            // Live difficulty-dial readout along the top edge (`=` / `-` to change).
            q.extend(crate::hud::danger_quads(self.difficulty, DIFFICULTY_MAX, aspect));
            // Credit balance (top-left) — money earned from kills, spent in the shop.
            q.extend(crate::hud::credits_quads(self.economy.credits(), aspect));
            Some(q)
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

    /// The player's current credit balance (HUD readout + shop affordability).
    pub fn credits(&self) -> u32 {
        self.economy.credits()
    }

    /// Whether the player owns the weapon at `idx` in `config::WEAPONS`.
    pub fn owns_weapon(&self, idx: usize) -> bool {
        self.owned.get(idx).copied().unwrap_or(false)
    }

    /// Number of weapons in the arsenal (the shop list length; = `config::WEAPONS`).
    pub fn weapon_count(&self) -> usize {
        self.weapons.len()
    }

    /// Index of the currently-equipped weapon (so the shop can mark it).
    pub fn active_weapon_index(&self) -> usize {
        self.weapon_index
    }

    /// `(magazine, reserve)` for weapon `idx`, or `None` if out of range — the shop's
    /// ammo readout.
    pub fn weapon_ammo(&self, idx: usize) -> Option<(u32, u32)> {
        self.weapons.get(idx).map(|w| (w.magazine(), w.reserve()))
    }

    /// Buy weapon `idx` from the shop: spend its [`crate::shop`] price and mark it
    /// owned (so it enters the `Q` cycle). Returns `false` — a no-op — if the index
    /// is invalid, the weapon is already owned, or the player can't afford it.
    pub fn buy_weapon(&mut self, idx: usize) -> bool {
        if idx >= self.owned.len() || self.owned[idx] {
            return false;
        }
        let name = crate::combat::config::WEAPONS[idx].name;
        let price = crate::shop::weapon_price(name);
        if self.economy.try_spend(price) {
            self.owned[idx] = true;
            log::info!(
                "bought {name} for ${price} — balance {}",
                self.economy.credits()
            );
            true
        } else {
            false
        }
    }

    /// Buy an ammo refill ([`crate::shop::AMMO_MAGS_PER_BUY`] magazines) for weapon
    /// `idx`. Must own the weapon. Returns `false` if invalid, unowned, or
    /// unaffordable.
    pub fn buy_ammo(&mut self, idx: usize) -> bool {
        if idx >= self.weapons.len() || !self.owned.get(idx).copied().unwrap_or(false) {
            return false;
        }
        let cfg = crate::combat::config::WEAPONS[idx];
        let price = crate::shop::ammo_price(cfg.name);
        if self.economy.try_spend(price) {
            let rounds = cfg.magazine_size * crate::shop::AMMO_MAGS_PER_BUY;
            self.weapons[idx].add_reserve(rounds);
            log::info!(
                "bought {rounds} rounds for {} (${price}) — balance {}",
                cfg.name,
                self.economy.credits()
            );
            true
        } else {
            false
        }
    }

    /// Grant the kill bounty for a defeated hunter. The single funnel for combat
    /// income, so future bounty scaling (by archetype / difficulty) stays in one
    /// place. Called from [`Self::start_death`] — the one death choke-point.
    fn award_kill(&mut self) {
        self.economy.earn(crate::economy::KILL_BOUNTY);
        log::info!(
            "+{} credits (hunter down) — balance {}",
            crate::economy::KILL_BOUNTY,
            self.economy.credits()
        );
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
        if self.mode != Mode::Hunt || self.switching {
            return;
        }
        // Only cycle to weapons the player actually owns. With just the PP7 (or any
        // single owned weapon) there's nothing to switch to.
        let Some(target) = self.next_owned(self.weapon_index) else {
            return;
        };
        self.weapon_mut().cancel_reload();
        self.switching = true;
        self.switch_target = target;
        self.switch_timer = 0.0;
        self.switch_swapped = false;
    }

    /// The next **owned** weapon index after `from`, scanning forward through the
    /// inventory with wraparound; `None` when the player owns fewer than two weapons
    /// (nothing else to switch to). Drives [`Self::begin_weapon_switch`].
    fn next_owned(&self, from: usize) -> Option<usize> {
        let n = self.weapons.len();
        (1..n)
            .map(|step| (from + step) % n)
            .find(|&i| self.owned[i])
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

    /// Footstep noise (#6, difficulty-scaled): a *moving* player emits a quiet
    /// movement ping that pulls nearby **blind** hunters (searching / investigating /
    /// idle) toward the sound — much shorter-ranged than gunfire, and 0-range at
    /// difficulty 0 (a baseline hunter is deaf to footsteps). Engaged hunters keep
    /// their own better info ([`crate::enemy::Enemy::hear_noise`] gates that). Called
    /// each fixed step in HUNT; reads the player's actual travelled speed, so hugging
    /// a wall (little real motion) or standing still is silent.
    pub(crate) fn alert_enemies_to_movement(&mut self) {
        let Some(c) = self.character.as_ref() else { return };
        let speed = c.speed();
        if speed < MOVE_NOISE_MIN_SPEED {
            return; // sneaking / stopped — no footstep noise
        }
        let ppos = c.pos;
        let t = self.difficulty as f32 / DIFFICULTY_MAX as f32;
        let speed_frac = (speed / PLAYER_RUN_SPEED).clamp(0.0, 1.0);
        let range = MOVE_NOISE_RANGE_MAX * t * speed_frac;
        if range <= 0.0 {
            return; // difficulty 0 → footsteps carry nowhere
        }
        for inst in &mut self.enemies {
            if inst.enemy.is_dead() {
                continue;
            }
            if inst.enemy.pos.distance(ppos) <= range {
                inst.enemy.hear_noise(ppos);
            }
        }
    }

    /// Grenade flush (#5, difficulty-scaled): track how long the player has held one
    /// spot and, once they've camped past a difficulty-scaled dwell, have an engaged
    /// hunter within range lob a grenade at that spot to flush them out. **Off at
    /// difficulty 0.** The lob is a **blind** throw at the camp spot (no LOS needed —
    /// the point is to shift a camper who's behind cover); the existing projectile sim
    /// (`explosives_step` → `detonate`) arcs it, detonates it, and damages the player.
    /// Squad-wide cooldown so the pack doesn't drop a simultaneous volley. Called each
    /// fixed step in HUNT.
    pub(crate) fn grenade_flush_step(&mut self, dt: f32) {
        if self.grenade_cooldown > 0.0 {
            self.grenade_cooldown = (self.grenade_cooldown - dt).max(0.0);
        }
        // Kill-switch (default OFF — hunters were killing themselves with it; see
        // `World::grenades`). Same quiescent camp tracker as difficulty 0.
        let t = if self.grenades {
            self.difficulty as f32 / DIFFICULTY_MAX as f32
        } else {
            0.0
        };
        if t <= 0.0 {
            // Difficulty 0: no grenades. Keep the camp tracker quiescent.
            self.camp_anchor = None;
            self.camp_timer = 0.0;
            return;
        }
        let Some(ppos) = self.player_pos() else { return };
        // Update the camp tracker: stay within CAMP_RADIUS of the anchor → accrue time;
        // wander off → re-anchor here and reset the clock.
        match self.camp_anchor {
            Some(a) if a.distance(ppos) <= CAMP_RADIUS => self.camp_timer += dt,
            _ => {
                self.camp_anchor = Some(ppos);
                self.camp_timer = 0.0;
            }
        }
        let dwell = CAMP_DWELL_MAX + (CAMP_DWELL_MIN - CAMP_DWELL_MAX) * t; // 5 s → 2 s
        if self.camp_timer < dwell || self.grenade_cooldown > 0.0 {
            return;
        }
        let Some(anchor) = self.camp_anchor else { return };
        // Don't lob if ANY living hunter is close to the camp spot: it would be caught
        // in its own blast (the self/friendly-fire bug), and a hunter that close should
        // just shoot. Grenades are only for a camper the pack is held away from.
        let anyone_close = self
            .enemies
            .iter()
            .any(|e| !e.enemy.is_dead() && e.enemy.pos.distance(anchor) < GRENADE_SAFE_DIST);
        if anyone_close {
            return;
        }
        // Pick the nearest engaged, living hunter within throw range of the camp spot
        // (guaranteed ≥ GRENADE_SAFE_DIST away by the guard above).
        let thrower = self
            .enemies
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.enemy.is_dead() && e.enemy.is_engaged())
            .map(|(i, e)| (i, e.enemy.pos.distance(anchor)))
            .filter(|(_, d)| *d <= GRENADE_THROW_RANGE)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let Some((idx, _)) = thrower else { return };
        let origin = self.enemies[idx].enemy.pos + Vec3::Y * GRENADE_THROW_Y;
        self.throw_enemy_grenade(origin, anchor);
        self.grenade_cooldown = GRENADE_COOLDOWN;
        self.camp_timer = 0.0; // re-arm the camp clock after a lob
        log::info!("hunter {idx} lobs a grenade to flush the camping player");
    }

    /// Lob a grenade from `origin` to land near `target`, blind (a `#5` flush throw).
    /// Reuses the [`crate::combat::GRENADE`] blast/model but computes a per-throw
    /// ballistic 45° lob (launch speed from the horizontal distance) so it arcs onto
    /// the camp spot instead of flying flat. Detonates on impact (no bounce) so it
    /// lands where aimed. The projectile then rides the normal `explosives_step`.
    fn throw_enemy_grenade(&mut self, origin: Vec3, target: Vec3) {
        let base = match crate::combat::config::GRENADE.fire_kind {
            crate::combat::FireKind::Projectile(p) => p,
            _ => return,
        };
        let to = Vec3::new(target.x - origin.x, 0.0, target.z - origin.z);
        let d = to.length();
        let dir_h = if d > 1e-3 { to / d } else { Vec3::X };
        // 45° lob: for a launch at 45°, range = 2·u²/g (u = each component's speed), so
        // u = sqrt(d·g/2). Split equally between horizontal aim + vertical loft.
        let g = base.gravity.max(1.0);
        let u = (d * g / 2.0).sqrt().clamp(GRENADE_LOB_MIN, GRENADE_LOB_MAX);
        let spec = crate::combat::ProjectileSpec {
            speed: u,
            gravity: base.gravity,
            loft: u,
            fuse: Some(3.0), // air backstop if it somehow never lands
            bounce: 0.0,     // detonate on impact → lands on the camp spot
            explosion: base.explosion,
            model: base.model,
        };
        // Release a little ahead of the chest along the throw so the round clears the
        // thrower's own capsule and doesn't detonate on it at launch.
        let launch = origin + dir_h * 0.6;
        let proj = crate::combat::Projectile::spawn(launch, dir_h, Vec3::Y, spec);
        self.projectiles.push(proj);
        if let Some(audio) = self.audio.as_mut() {
            audio.play(GRENADE_THROW_SOUND, GRENADE_THROW_VOL);
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
            Some(hit) if self.physics.is_prop_collider(hit.collider) => {
                self.hit_prop(hit.collider, hit.point);
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

    /// Route a player shot into a destructible prop (Milestone 3): deduct the weapon's
    /// per-shot damage from the prop's [`crate::ecs::Health`] (which darkens it via
    /// [`Self::prop_draws`]), and once its health hits zero mark it
    /// [`crate::ecs::Destroyed`] and [`Self::detonate`] its catalog blast at its centre
    /// — reusing the whole explosive VFX/SFX/falloff path, so the blast damages nearby
    /// hunters + the player. A spark marks the impact either way.
    ///
    /// GoldenEye-faithful: a destroyed prop is **not** removed — the charred husk stays
    /// in place, still solid (its collider is kept), just inert. Re-shooting the husk
    /// only sparks; it can't be re-damaged or blow a second time.
    fn hit_prop(&mut self, collider: ColliderHandle, point: Vec3) {
        // Impact spark for hit feedback (same as the world-geometry arm).
        self.sparks.push(Spark { pos: point, ttl: SPARK_TTL });
        let Some(&entity) = self.prop_colliders.get(&collider) else {
            return; // stale handle — the spark is enough
        };
        // A spent husk (already blown): shots just spark off it — no re-damage/re-blast.
        let spent = self
            .ecs
            .world()
            .entity(entity)
            .ok()
            .is_some_and(|e| e.get::<&crate::ecs::Destroyed>().is_some());
        if spent {
            return;
        }
        let dmg = self.weapon().config().damage;
        let died = match self
            .ecs
            .world_mut()
            .query_one_mut::<&mut crate::ecs::Health>(entity)
        {
            Ok(hp) => {
                hp.hp -= dmg;
                hp.hp <= 0.0
            }
            Err(_) => false, // no Health (shouldn't happen for a mapped prop)
        };
        if !died {
            return; // still standing — the darker tint is the feedback
        }
        // Destroyed: resolve its catalog blast + world centre. The collider + draw stay
        // (the husk remains); we only mark it spent so it can't blow again, then blow it.
        let mesh = self
            .ecs
            .world()
            .entity(entity)
            .ok()
            .and_then(|e| e.get::<&crate::ecs::Renderable>().map(|r| r.mesh));
        let blast = mesh
            .and_then(crate::props::def)
            .and_then(|d| d.destructible)
            .map(|d| d.blast);
        let center = self
            .prop_world_aabb(entity)
            .map(|(min, max)| (min + max) * 0.5)
            .unwrap_or(point);
        let _ = self.ecs.world_mut().insert_one(entity, crate::ecs::Destroyed);
        if let Some(blast) = blast {
            self.detonate(center, blast);
            log::info!("prop {mesh:?} destroyed → charred husk + blast r={:.1} m", blast.radius);
        } else {
            log::info!("prop {mesh:?} destroyed → charred husk");
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
                self.blast_hit_enemy(idx, dmg, center_mass, center);
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
    fn blast_hit_enemy(&mut self, idx: usize, dmg: f32, at: Vec3, blast_center: Vec3) {
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
            // Radial knockback: shove the corpse away from the blast centre (lifted a
            // little so it's flung, not scraped). `start_death` handles ragdoll vs clip.
            let dir = (at - blast_center).normalize_or_zero();
            let knock = (dir + Vec3::Y * 0.3).normalize_or_zero() * RAGDOLL_BLAST_IMPULSE;
            self.start_death(idx, collider, at, knock);
            log::info!("HUNTER DOWN (blast, {dmg:.0} dmg)");
        } else if self.ragdoll {
            // Phase 3 default: a brief physics-ragdoll stagger (radial from the blast) +
            // a short stun, blended into animation, then the hunter fights on.
            let dir = (at - blast_center).normalize_or_zero();
            let knock = (dir + Vec3::Y * 0.25).normalize_or_zero() * REACTION_IMPULSE;
            self.spawn_reaction(idx, at, knock);
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.enemy.stun(REACTION_STUN);
            }
            let hp = self.enemies.get(idx).map(|i| i.enemy.health()).unwrap_or(0.0);
            log::info!("hunter staggered by blast — {dmg:.0} dmg, {hp:.0} hp left");
        } else if self.hit_reactions {
            // GoldenEye-style flinch (opt-in — OFF by default, matching `hit_enemy`):
            // a non-lethal blast staggers with a torso hurt clip + brief stun. The
            // sim-style default plays NO flinch, so a blasted hunter keeps fighting.
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
        } else {
            // Sim style (default): damage + pain SFX, no flinch/stun.
            let hp = self.enemies.get(idx).map(|i| i.enemy.health()).unwrap_or(0.0);
            log::info!("hunter caught in blast — {dmg:.0} dmg, {hp:.0} hp left (no flinch)");
        }
    }

    /// Begin a hunter's death: drop its hitscan capsule, then either spawn a physics
    /// ragdoll (the [`World::ragdoll`] flag, default on) seeded from its current pose +
    /// the killing `impulse` at `impact`, or fall back to the canned death clip. Shared
    /// by the bullet ([`Self::hit_enemy`]) and blast ([`Self::blast_hit_enemy`]) paths.
    /// A Perfect Dark reaction row for hunter `idx`: the death or injury table for
    /// the body part its last shot landed on, random-picked exactly as
    /// `chraction.c:3271` / `:3516` do. `None` for a GoldenEye hunter (whose clips
    /// are not PD's, so the tables would index the wrong animations) or before any
    /// hit has been recorded — the caller keeps its zone-based pick in that case.
    fn pd_reaction(&mut self, idx: usize, death: bool) -> Option<crate::combat::hit_anim::AnimRow> {
        if !self.authored_reactions {
            return None;
        }
        let inst = self.enemies.get(idx)?;
        if !inst.pd_anims {
            return None;
        }
        // A kill with no recorded part — a blast, or damage that never went through
        // `hit_enemy` — is PD's `HITPART_GENERAL`; the torso table is its stand-in, so
        // a Perfect Dark hunter always dies on an authored animation rather than
        // silently falling through to the physics path.
        let part = inst.hit_part.unwrap_or(crate::combat::hit_anim::HitPart::Torso);
        let rows = if death { part.deaths() } else { part.injuries() };
        if rows.is_empty() {
            return None;
        }
        Some(rows[self.rand_below(rows.len())])
    }

    fn start_death(&mut self, idx: usize, collider: ColliderHandle, impact: Vec3, impulse: Vec3) {
        // A hunter just went down — pay the kill bounty. Every enemy death funnels
        // through here (bullet + blast paths both call it), so this is the one place
        // combat income is granted.
        self.award_kill();
        // The corpse can't be shot: the capsule goes now, either way.
        self.physics.remove_enemy_collider(collider);
        // **Authored first for a Perfect Dark hunter.** It has a real death table for
        // the part that was hit (`chraction.c:3271`), and playing that is the whole
        // point of porting them — a physics ragdoll would discard it. The ragdoll
        // stays the default for GoldenEye hunters, which have no such tables, and
        // `set_authored_reactions(false)` puts PD hunters back on it for an A/B.
        if let Some(r) = self.pd_reaction(idx, true) {
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.anim.play_once_scaled(r.slot, 0.2, None, None, r.speed, r.end);
                inst.thud = Some(r.thud);
            }
        } else if self.ragdoll {
            self.spawn_ragdoll(idx, impact, impulse);
        } else {
            // Pre-ragdoll baseline: a random canned death one-shot that clamps + holds
            // (no return target) while the body fades out (see `advance_animation`).
            let death_start = CHAR_HIT_START + anim_set::HIT_CLIPS.len();
            let pick = self.rand_below(anim_set::DEATH_CLIPS.len());
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.anim.play_once(death_start + pick, 0.2, None, None);
            }
        }
    }

    /// Seed a ragdoll for hunter `idx` from its CURRENT animated pose (so it starts
    /// exactly where the live model was) with the `impulse` at world `impact`. Returns
    /// the built ragdoll without storing it — the caller decides whether it's a death
    /// takeover ([`Self::spawn_ragdoll`]) or a living-hit reaction ([`Self::spawn_reaction`]).
    /// `None` if the hunter or its body model is gone.
    fn build_ragdoll_for(&mut self, idx: usize, impact: Vec3, impulse: Vec3) -> Option<Ragdoll> {
        // Seed inputs, gathered with only immutable, disjoint field borrows.
        let (body, feet, yaw) = match self.enemies.get(idx) {
            Some(i) => (i.body, i.enemy.pos, i.yaw()),
            None => return None,
        };
        let pd_clips = self.enemies.get(idx).is_some_and(|i| i.pd_anims);
        let feet_off = self.body_feet_offset(body, pd_clips);
        let m = self.char_models.get(body)?;
        let sk = &m.skeleton;
        // Model-space bone globals of the current pose (post-stack if it exists).
        let model_globals = match self.enemies[idx].final_pose.as_ref() {
            Some(p) => p.joint_global_transforms(sk),
            None => self.enemies[idx].anim.joint_global_transforms(sk),
        };
        // World bone transforms = char_transform · model_global (metres, scale folded in).
        let char_mat = crate::world::hunt::char_transform_raw(feet, yaw, feet_off);
        let world_bone: Vec<Mat4> = model_globals.iter().map(|g| char_mat * *g).collect();
        // Build in the sim (disjoint fields: &mut physics + &char_models via `sk`).
        Some(Ragdoll::build(&mut self.physics, sk, &world_bone, CHAR_SCALE, impulse, impact))
    }

    /// Death takeover: build the corpse ragdoll and store it on the instance. From here
    /// [`Self::advance_ragdolls`] + [`Self::character_instances`] drive and draw it. Any
    /// in-flight living reaction on this hunter is torn down first (death supersedes it).
    fn spawn_ragdoll(&mut self, idx: usize, impact: Vec3, impulse: Vec3) {
        if let Some(r) = self.enemies.get_mut(idx).and_then(|i| i.reaction.take()) {
            r.rag.remove(&mut self.physics);
        }
        if let Some(rag) = self.build_ragdoll_for(idx, impact, impulse) {
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.ragdoll = Some(rag);
            }
        }
    }

    /// Living-hit stagger (Phase 3): a non-lethal hit spawns a brief physics ragdoll
    /// that's BLENDED into the running animation ([`Self::advance_animation`]) by a
    /// decaying weight, then torn down. A re-hit while still staggering just re-kicks the
    /// existing ragdoll and restarts its decay (no rebuild — avoids body churn under fire).
    fn spawn_reaction(&mut self, idx: usize, impact: Vec3, impulse: Vec3) {
        let already = self.enemies.get(idx).is_some_and(|i| i.reaction.is_some());
        if already {
            // Re-kick the live reaction (scoped shared borrow), then re-peak its decay.
            if let Some(inst) = self.enemies.get(idx) {
                if let Some(r) = inst.reaction.as_ref() {
                    r.rag.kick(&mut self.physics, impulse, impact);
                }
            }
            if let Some(r) = self.enemies.get_mut(idx).and_then(|i| i.reaction.as_mut()) {
                r.elapsed = 0.0;
            }
            return;
        }
        if let Some(rag) = self.build_ragdoll_for(idx, impact, impulse) {
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.reaction = Some(Reaction { rag, elapsed: 0.0 });
            }
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
        self.hit_enemy_with(idx, hit_point, base);
    }

    /// [`Self::hit_enemy`] with the damage supplied rather than read off the player's
    /// weapon — the entry point for a shot that did not come from the player, i.e. one
    /// hunter hitting another (see `emit_pd_shot`). Everything downstream is shared,
    /// so a hunter shot by a packmate bleeds, flinches and dies identically.
    pub(crate) fn hit_enemy_with(&mut self, idx: usize, hit_point: Vec3, base: f32) {
        // Paint blood at the impact (before damage, so it shows even on the kill
        // shot). Needs this hunter's body model (immut) + its pose/blood (mut) —
        // disjoint fields, split-borrowed. Body id + its feet offset read out first.
        let body = self.enemies.get(idx).map(|i| i.body).unwrap_or(0);
        let pd_clips = self.enemies.get(idx).is_some_and(|i| i.pd_anims);
        let feet_offset = self.body_feet_offset(body, pd_clips);
        // The bone the shot actually landed on, for Perfect Dark's per-hit-part
        // reaction tables. Resolved from the SAME posed skeleton the blood painting
        // uses, so the part and the stain agree by construction.
        let mut hit_part = None;
        if let Some(model) = self.char_models.get(body) {
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
                    hit_part = nearest_hit_part(model, char_mat, &joints, hit_point)
                        .map(|p| p.with_gun_in_hand(inst.dual));
                    paint_blood(&mut inst.blood, model, char_mat, &joints, hit_point);
                }
            }
        }
        if let Some(inst) = self.enemies.get_mut(idx) {
            inst.hit_part = hit_part;
        }
        // Classify the zone, scale the damage, apply — bail if already dead / gone.
        // `body_hit_zones` borrows `self` immutably, so read it before taking the
        // hunter mutably below.
        let zones = self.enemies.get(idx).map(|i| self.body_hit_zones(i.body)).unwrap_or_default();
        let (died, collider, dmg, zone) = match self.enemies.get_mut(idx) {
            Some(inst) if !inst.enemy.is_dead() => {
                let zone = HitZone::classify(hit_point.y - inst.enemy.pos.y, zones);
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
            // The killing shot's knockback: from the player's eye toward the impact
            // (with a slight lift so the corpse arcs rather than slides). `start_death`
            // drops the capsule and spawns a physics ragdoll (flag on) or the canned
            // death clip (off).
            let eye = self.character.as_ref().map(|c| c.eye());
            let dir = eye
                .map(|e| (hit_point - e).normalize_or_zero())
                .unwrap_or(Vec3::Y);
            let knock = (dir + Vec3::Y * 0.25).normalize_or_zero() * RAGDOLL_BULLET_IMPULSE;
            self.start_death(idx, collider, hit_point, knock);
            log::info!("HUNTER DOWN ({zone:?}, {dmg:.0} dmg)");
        } else if let Some(r) = self.pd_reaction(idx, false) {
            // Perfect Dark's injury table for the part that was hit — usually the
            // opening frames of a death animation rather than a purpose-made flinch
            // (`chr_begin_argh`, chraction.c:3409). This runs ahead of the ragdoll
            // stagger for a PD hunter, and does not consult `hit_reactions`: PD chrs
            // *do* enter `ACT_ARGH` when they survive a hit — there is no aibot
            // exemption in `chraction.c:3600` — so "no flinch" was never the Perfect
            // Dark behaviour that flag's name claimed.
            let Some(inst) = self.enemies.get_mut(idx) else { return };
            let band = band_for_speed(inst.enemy.speed());
            let dur = r
                .end
                .or_else(|| inst.anim.clip(r.slot).map(|c| c.duration))
                .unwrap_or(0.4)
                / r.speed.max(0.01);
            inst.anim.play_once_scaled(r.slot, 0.1, Some(band), None, r.speed, r.end);
            inst.enemy.stun(dur);
            let hp = inst.enemy.health();
            log::info!("hunter hit — {zone:?} {dmg:.0} dmg, {hp:.0} hp left (PD injury table)");
            self.stop_enemy_fire(idx); // `chr_stop_firing` before `ACT_ARGH`
        } else if self.ragdoll {
            // Phase 3 default: a brief physics-ragdoll stagger blended into the run-and-
            // gun animation + a short stun, then the hunter resumes fighting. Flat across
            // difficulties (the `ragdoll` flag is the kill-switch). Knock from the shot line.
            let eye = self.character.as_ref().map(|c| c.eye());
            let dir = eye
                .map(|e| (hit_point - e).normalize_or_zero())
                .unwrap_or(Vec3::Y);
            let knock = (dir + Vec3::Y * 0.2).normalize_or_zero() * REACTION_IMPULSE;
            self.spawn_reaction(idx, hit_point, knock);
            if let Some(inst) = self.enemies.get_mut(idx) {
                inst.enemy.stun(REACTION_STUN);
            }
            self.stop_enemy_fire(idx); // a stagger is a flinch: drop the trigger
            let hp = self.enemies.get(idx).map(|i| i.enemy.health()).unwrap_or(0.0);
            log::info!("hunter staggered — {zone:?} {dmg:.0} dmg, {hp:.0} hp left");
        } else if self.hit_reactions {
            // GoldenEye-style flinch (opt-in — off by default): play a zone-appropriate
            // hurt clip + a brief stun. A Perfect Dark hunter never reaches here; its
            // per-hit-part injury table is handled above.
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
            self.stop_enemy_fire(idx); // `chr_stop_firing` before `ACT_ARGH`
        } else {
            // Perfect-Dark "sim" style (default): no flinch animation or stun — the
            // hunter keeps chasing + firing through the hit. The pain vocal + the
            // persistent blood color already signal the damage.
            let hp = self.enemies.get(idx).map(|i| i.enemy.health()).unwrap_or(0.0);
            log::info!("hunter hit — {zone:?} {dmg:.0} dmg, {hp:.0} hp left (no flinch)");
        }
    }

    /// Where hunter `idx` is currently trying to engage: whoever its simulant has
    /// picked, or the player. `None` in BUILD, or when neither exists.
    ///
    /// A simulant's target is already resolved every step (`pd_lab::step_simulant` →
    /// [`EnemyInstance::pd_target`]), so this is a read, not a decision.
    pub(crate) fn engage_target_pos(&self, idx: usize) -> Option<Vec3> {
        let player = self.character.as_ref().map(|c| c.pos);
        match self.enemies.get(idx).and_then(|i| i.pd_target) {
            Some(pd_lab::PdTarget::Player) | None => player,
            Some(pd_lab::PdTarget::Hunter(j)) => {
                self.enemies.get(j).filter(|e| !e.enemy.is_dead()).map(|e| e.enemy.pos).or(player)
            }
        }
    }

    /// The attack animation Perfect Dark would start for hunter `idx` right now.
    ///
    /// `chr_attack` (`chraction.c:2825`) does not pick by weapon — it picks by the
    /// **bearing to the target at the instant the burst begins**, out of a 32-slot
    /// table per stance, and then holds that row for the whole animation. So this is
    /// evaluated once here rather than per frame, exactly as PD stores it in
    /// `chr->act_attack.animcfg`.
    ///
    /// `None` for a GoldenEye hunter (one hand-timed clip, fixed at spawn) or when
    /// there is nothing to bear on.
    fn pd_fire_row(&mut self, idx: usize) -> Option<&'static attack_anim::AttackAnimConfig> {
        let (pd, pos, yaw, class, dual) = {
            let inst = self.enemies.get(idx)?;
            (inst.pd_anims, inst.enemy.pos, inst.yaw(), inst.weapon.class, inst.dual)
        };
        if !pd {
            return None;
        }
        let target = self.engage_target_pos(idx)?;
        let to = Vec3::new(target.x - pos.x, 0.0, target.z - pos.z);
        if to.length_squared() < 1e-8 {
            return None;
        }
        let rel = attack_anim::relative_angle(to.x.atan2(to.z), yaw);
        // PD's `index = random() % len`; the group applies the modulo, so any draw does.
        let roll = self.rand_below(1 << 16);
        Some(attack_anim::select(attack_anim::table_for(class, dual), rel, roll))
    }

    /// **Cancel hunter `idx`'s fire burst** — `chr_stop_firing` (`chraction.c:9414`).
    ///
    /// Perfect Dark calls this immediately before entering `ACT_ARGH`, at both injury sites
    /// (`chraction.c:3476` and `:3520`): a character that flinches drops both triggers, its
    /// aim-end is reset and its fire slots are freed. Leaving `actiontype` also stops
    /// `chr_tick_attack` pumping shots at all.
    ///
    /// We needed it because **firing here is a timer**, not an animation: `fire_elapsed`
    /// runs in `enemy_combat_step` and knows nothing about the mixer or the stun, so a
    /// hunter mid-flinch kept shooting out of an in-flight burst. Visible the moment the
    /// authored reactions went on by default.
    ///
    /// (PD's own simulants never reach the injury code — `chr->aibot` returns early at
    /// `chraction.c:3427`, which is the "sim style" no-flinch behaviour `hit_enemy`'s last
    /// branch still offers. Ours flinch *and* stop firing, which is the guard's half of PD
    /// paired with the simulant's aim; the alternative is not flinching at all.)
    fn stop_enemy_fire(&mut self, idx: usize) {
        let Some(inst) = self.enemies.get_mut(idx) else { return };
        inst.fire_elapsed = None;
        inst.shot_timer = 0.0;
        inst.burst_shot = 0;
    }

    /// Start a fire burst on hunter `idx` — it entered `attack` (A3), or its simulant
    /// pulled the trigger. The per-shot cadence + damage roll run in
    /// [`Self::enemy_combat_step`]; this resolves *which* attack animation the burst
    /// plays and resets the cadence so the first shot waits for its `shootstartframe`.
    ///
    /// A Perfect Dark hunter re-resolves the animation here from the bearing to its
    /// target ([`Self::pd_fire_row`]), which is what makes a guard taken from behind
    /// visibly slower to shoot than one you walk up to. A GoldenEye hunter keeps the
    /// single clip its stack was built with.
    pub(crate) fn start_enemy_fire(&mut self, idx: usize) {
        let row = self.pd_fire_row(idx);
        if let Some(row) = row {
            self.install_fire_row(idx, row);
        }
        let Some(inst) = self.enemies.get_mut(idx) else { return };
        // Firing is a timer, not a full-body clip: the hunter keeps its locomotion
        // (legs running) while the procedural stack aims the arm + kicks recoil. The
        // shot window / cadence run off `fire_elapsed` in `enemy_combat_step`.
        // The burst clock starts at the animation's authored `startframe`, not at 0 —
        // a PD row may trim the lead-in off the clip (the pistol's first 12 frames).
        inst.fire_elapsed = Some(inst.fire.start);
        inst.shot_timer = 0.0;
        log::info!("hunter firing ({})", inst.weapon.name);
    }

    /// Point hunter `idx`'s whole fire pipeline at one [`AttackAnimConfig`] row: the
    /// timing windows, the authored aim cone, the aim-overlay clip and hold frame, and
    /// the barrel axis the chest-aim swings against (looked up from the spawn-time
    /// [`EnemyInstance::fire_axes`] measurement — see `lifecycle::spawn_wave`).
    ///
    /// The axis is the part that cannot be skipped: each animation holds the gun its
    /// own way, and re-using the previous clip's axis would leave the chest-aim
    /// correcting for a pose that is no longer on the body. If the row's clip or its
    /// measurement is missing, the row is refused outright rather than half-applied.
    ///
    /// [`AttackAnimConfig`]: attack_anim::AttackAnimConfig
    fn install_fire_row(&mut self, idx: usize, row: &'static attack_anim::AttackAnimConfig) {
        let Some(inst) = self.enemies.get_mut(idx) else { return };
        let Some(clip) = inst.anim.clip(row.slot).cloned() else { return };
        let Some(&(_, axis)) = inst.fire_axes.iter().find(|(s, _)| *s == row.slot) else {
            return;
        };
        inst.fire = attack_anim::FireTiming::from_pd(row, clip.duration);
        let hold = inst.fire.shoot.0;
        let cone = inst.fire.cone;
        if let Some(ov) = inst.stack.layer_as::<ClipOverlayLayer>(ENEMY_AIM_OVERLAY_LAYER) {
            ov.set_clip(clip);
            ov.time = hold;
        }
        if let Some(ca) = inst.stack.layer_as::<AimOffsetLayer>(ENEMY_CHEST_AIM_LAYER) {
            ca.forward = axis;
            ca.cone = cone;
        }
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
        // Per-hunter muzzle-flash decay (blood is persistent — no decay).
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
        // The visual cadence is the weapon's own fire rate (difficulty no longer scales
        // it — the damage that lands is capped by MAX_HIT_RATE in `emit_enemy_shot`).
        let mut shots: Vec<usize> = Vec::new();
        // Hunters whose burst just ended on a sideways attack animation, to be handed
        // back to their stance's forward one (see the loop after the shots).
        let mut ended_sideways: Vec<usize> = Vec::new();
        for (i, inst) in self.enemies.iter_mut().enumerate() {
            let Some(t) = inst.fire_elapsed else {
                inst.shot_timer = 0.0;
                // Trigger released → the burst counter resets, exactly as PD's
                // `aibot->burstsdone` does when `firing` goes false (`bot.c:3661`).
                inst.burst_shot = 0;
                continue;
            };
            let t = t + dt;
            // Pump shots while inside the authored shoot window. A PD simulant with an
            // automatic runs PD's BURST cadence — three rounds close together, then a
            // pause — instead of a flat 1/fireRate stream. Everything else keeps the
            // flat cadence.
            let burst = inst.pdsim.is_some() && inst.weapon.automatic;
            if inst.fire.shooting(t) {
                inst.shot_timer -= dt;
                if inst.shot_timer <= 0.0 {
                    inst.shot_timer = if burst {
                        inst.burst_shot += 1;
                        if inst.burst_shot >= PD_BURST_ROUNDS {
                            inst.burst_shot = 0;
                            PD_BURST_GAP
                        } else {
                            PD_BURST_SPACING
                        }
                    } else {
                        1.0 / inst.weapon.fire_rate.max(0.001)
                    };
                    shots.push(i);
                }
            }
            // End the burst once past the window (+ a short tail), or at the row's
            // own `endframe` if it keeps the animation running longer than that.
            let over = t >= inst.fire.shoot.1 + ENEMY_FIRE_TAIL && t >= inst.fire.end;
            inst.fire_elapsed = if over { None } else { Some(t) };
            if over && inst.fire.angle_offset != 0.0 {
                ended_sideways.push(i);
            }
        }
        for i in shots {
            self.emit_enemy_shot(i);
        }
        // A burst that played a **sideways** attack animation has to hand the hold back
        // to the forward one. Between bursts a hunter keeps its weapon up and tracking
        // (`advance_animation`), and a clip drawn firing 90° off the body would leave the
        // chest-aim pinned at its cone limit — the gun visibly held past the target,
        // for as long as the hunter went without firing again. The next burst re-picks
        // from the bearing anyway, so this is purely about the gap between them.
        for i in ended_sideways {
            let default = self.enemies.get(i).map(|inst| {
                attack_anim::config_for(inst.weapon.class, inst.dual)
            });
            if let Some(row) = default {
                self.install_fire_row(i, row);
            }
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
        // Flash + report fire on every shot, hit or miss; kick the procedural recoil
        // for PISTOLS only (autos fire too fast for the kick to read — user call).
        if let Some(inst) = self.enemies.get_mut(idx) {
            inst.muzzle_timer = ENEMY_MUZZLE_TIME;
            // Recoil only inside the animation's authored recoil frames, when it has
            // them (`recoilstart`/`recoilend` — "for single shot pistols", types.h:347).
            // A row with none kicks on every shot, which is the legacy behaviour.
            let in_window = inst.fire_elapsed.is_none_or(|t| inst.fire.recoiling(t));
            if inst.weapon.class == EnemyWeaponClass::Pistol && in_window {
                if let Some(r) = inst.stack.layer_as::<AdditiveDecayLayer>(ENEMY_RECOIL_LAYER) {
                    r.kick(ENEMY_RECOIL_KICK);
                }
            }
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.play(weapon.fire_sound, ENEMY_FIRE_VOL);
        }
        // ── A real shot down a real barrel ──
        //
        // **There is no hit roll any more.** The bullet leaves along the yaw the zeroing
        // model produced — which carries the hunter's live aim error — and connects only
        // if that yaw was actually pointing at the player. Every "accuracy" behaviour is
        // emergent from where the barrel is.
        //
        // What this retired, deliberately and together (see `DESIGN_PD_SIMULANT_AI.md`
        // §9 and §17):
        //
        // * `rand() < accuracy · (1 − dist/range)` — the probability model,
        // * `MAX_HIT_RATE` — the global landed-hit ceiling that existed *because* of it,
        //   since a rolled full-auto would otherwise delete the player in one burst,
        // * `DiffParams::accuracy_mult` / `falloff_ease` — the difficulty levers that
        //   scaled the roll.
        //
        // Keeping the ceiling on top of the zeroing model would have clipped the top of
        // exactly the range the difficulty table expresses: Hard and Dark both saturate
        // it and become indistinguishable. The honest ceiling is the burst gap, which
        // `enemy_combat_step` applies to the cadence rather than to the damage.
        self.emit_pd_shot(idx, epos, ppos, collider, weapon);
    }

    /// One shot from a **PD simulant** — a genuine hitscan, no probability roll.
    ///
    /// This is the payoff of the whole zeroing port. The bullet leaves along the
    /// body yaw the model produced (which carries its live aim error), and lands
    /// only if that yaw was actually pointing at the player. A simulant that is
    /// mid-convergence, or that was just forced to swing across the room, misses
    /// because its gun is genuinely pointing somewhere else — not because a
    /// `rand()` said so.
    ///
    /// The test is analytic rather than a second raycast: measure the closest approach
    /// between the shot line and each body's torso segment, and compare it against a
    /// torso radius. That is exactly a ray-versus-vertical-capsule test, and it avoids
    /// adding a player collider the physics world does not currently carry.
    ///
    /// # Two independent sources of error, which is the whole point
    ///
    /// **Body yaw** carries the zeroing model's error — a slow damped random walk whose
    /// increment is held for a third to two thirds of a second, so it barely changes
    /// across a burst. **Per-shot spread** ([`crate::pdsim::spread`], PD's
    /// `bgun_calculate_bot_shot_spread`) then offsets each individual bullet in yaw AND
    /// pitch, re-rolled every round.
    ///
    /// Both are needed and they do different jobs. Zeroing alone is all-or-nothing:
    /// while the walk sits on you every round in the magazine connects and you die in a
    /// blink, and while it sits off you take nothing. Spread turns a burst from one ray
    /// fired repeatedly into a *pattern*, so an automatic weapon is threatening without
    /// being a guaranteed kill the instant the aim crosses you. PD puts its widest
    /// spread values on exactly the automatics for this reason, and zero on the sniper
    /// and laser — which therefore still hit every time they are properly zeroed.
    ///
    /// The vertical axis matters for the same reason. PD aims at the target's chest
    /// (`chr_calculate_aimend` drops the aim point by 0.4 × eye height) and the spread
    /// cone is round, so a marginal shot can miss high or low as readily as wide. A
    /// purely horizontal model throws away half of the miss space.
    pub(crate) fn emit_pd_shot(
        &mut self,
        idx: usize,
        epos: Vec3,
        ppos: Vec3,
        collider: ColliderHandle,
        weapon: EnemyWeaponDef,
    ) {
        // The BARREL yaw, not the body's: a hunter mid-sideways attack animation has
        // its torso deliberately turned off the target (see `Simulant::yaw`), and the
        // round leaves along the gun.
        let Some(yaw) =
            self.enemies.get(idx).and_then(|i| i.pdsim.as_ref()).map(|s| s.barrel_yaw())
        else {
            return;
        };
        let dual = self.enemies.get(idx).is_some_and(|i| i.dual);

        // The vertical half of the aim. PD points the barrel at the target's chest
        // rather than its origin; whoever the simulant is *currently* engaging sets the
        // elevation, and the round then goes wherever that points — the same rule as
        // yaw. Pick the intended target's chest, defaulting to the player's.
        let intended = self
            .enemies
            .get(idx)
            .and_then(|i| i.pd_target)
            .and_then(|t| match t {
                pd_lab::PdTarget::Player => Some(ppos),
                pd_lab::PdTarget::Hunter(j) => self.enemies.get(j).map(|e| e.enemy.pos),
            })
            .unwrap_or(ppos);
        let muzzle = epos + Vec3::Y * PD_MUZZLE_HEIGHT;
        let aim_at = intended + Vec3::Y * PD_TORSO_AIM;
        let flat = Vec3::new(aim_at.x - muzzle.x, 0.0, aim_at.z - muzzle.z).length();
        let pitch = (aim_at.y - muzzle.y).atan2(flat.max(1e-4));

        // Per-shot spread: an independent two-axis offset for THIS bullet (see the doc
        // comment above for why this is not redundant with the zeroing error).
        let u = [self.rand_float(), self.rand_float(), self.rand_float(), self.rand_float()];
        let (dyaw, dpitch) = crate::pdsim::spread::shot_offset(weapon.spread, dual, u);
        let (yaw, pitch) = (yaw + dyaw, pitch + dpitch);
        // Same yaw convention as the rendered model: yaw 0 faces +Z.
        let (sin_p, cos_p) = pitch.sin_cos();
        let shot_dir = Vec3::new(cos_p * yaw.sin(), sin_p, cos_p * yaw.cos()).normalize_or_zero();
        if shot_dir == Vec3::ZERO {
            return;
        }

        // **Whoever is on the line takes it.** The round leaves along the barrel and
        // the nearest body it passes through is hit, which is why a simulant that
        // fires across a packmate hits the packmate — friendly fire is emergent here,
        // not a special case. The intended target above set the elevation and nothing
        // else; the round itself does not consult it.
        let mut hits: Vec<(f32, Option<usize>)> = Vec::new();
        let consider = |feet: Vec3, who: Option<usize>, hits: &mut Vec<(f32, Option<usize>)>| {
            let lo = feet + Vec3::Y * PD_TORSO_LO;
            let hi = feet + Vec3::Y * PD_TORSO_HI;
            let (miss, along) = ray_segment_closest(muzzle, shot_dir, weapon.range, lo, hi);
            if along <= 0.0 || miss > PD_TORSO_RADIUS {
                return; // behind the barrel, out of range, or the line misses this body
            }
            hits.push((along, who));
        };
        consider(ppos, None, &mut hits);
        // Packmates are on the line too, always. Hunters no longer *target* each other
        // (the team check, §16.1), but a round that crosses one still hits it — which is
        // what Perfect Dark's teammates do to each other, and it is the half of the
        // emergent-friendly-fire behaviour worth keeping.
        for (j, other) in self.enemies.iter().enumerate() {
            if j == idx || other.enemy.is_dead() {
                continue;
            }
            consider(other.enemy.pos, Some(j), &mut hits);
        }
        // Nearest body along the line takes it, if a **wall** does not stop the round
        // first. The wall test is `perception_los` (world geometry only) rather than
        // a capsule-blocking cast: a body in the way is not an
        // obstruction here, it is the thing that gets shot, and the nearest-hit sort
        // above has already decided which. `collider` is unused for the same reason.
        let _ = collider;
        hits.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(&(_, victim)) = hits.first() else { return };
        let victim_pos = match victim {
            None => ppos,
            Some(j) => match self.enemies.get(j) {
                Some(e) => e.enemy.pos,
                None => return,
            },
        };
        if !crate::enemy::perception_los(&mut self.physics, epos, victim_pos) {
            return;
        }
        // NOTE: [`MAX_HIT_RATE`] deliberately does **not** apply here.
        //
        // That cap exists because our normal hunters roll for hits, so a fast
        // weapon would otherwise delete the player in a burst — it is an
        // artificial lethality limiter bolted on top of an artificial accuracy
        // model. The zeroing model replaces both: how often a simulant lands a
        // shot is already governed by where its barrel is, and re-imposing the cap
        // would clip the top of exactly the range the difficulty table is supposed
        // to express (a DarkSim and a HardSim would both saturate it and become
        // indistinguishable).
        //
        // The consequence is real and intended: a well-zeroed high-tier simulant
        // with an automatic weapon kills very fast. That is what a DarkSim does in
        // Perfect Dark. If the lab turns out to need a ceiling for playability,
        // it belongs on the *weapon*, not on a global damage throttle.
        match victim {
            None => self.take_player_damage(weapon.damage),
            Some(j) => {
                // Hunter-on-hunter. It runs through the same `hit_enemy` the player's
                // shots do, so the victim gets a hit part, blood, a pain vocal and an
                // authored reaction exactly as it would from the player — one damage
                // path, not two.
                log::info!("hunter {idx} shot hunter {j}");
                let chest = self
                    .enemies
                    .get(j)
                    .map(|e| self.body_height(e.body) * 0.55)
                    .unwrap_or(0.8);
                self.hit_enemy_with(j, victim_pos + Vec3::Y * chest, weapon.damage);
            }
        }
    }

    /// Apply `dmg` to the player (JS `Actor.takeDamage`: armor-first, then health)
    /// with the damage feedback — red flash (peak α = min(0.5, dmg/40)), the
    /// breathe SFX, and the health-HUD pop. Death (→ YOU DIED) at 0 health.
    pub(crate) fn take_player_damage(&mut self, dmg: f32) {
        if self.player_dead || self.player_invulnerable {
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

    /// Toggle player invincibility (dev/observe — bound to `I`). Returns the new
    /// state so the caller can surface it. Enemies keep aiming + firing; the player
    /// just stops taking damage — so you can stand and watch them work.
    pub fn toggle_invulnerable(&mut self) -> bool {
        self.player_invulnerable = !self.player_invulnerable;
        log::info!(
            "player invincibility: {}",
            if self.player_invulnerable { "ON" } else { "off" }
        );
        self.player_invulnerable
    }

    /// Toggle player invisibility (dev/observe — bound to `N`). Returns the new state.
    /// When ON, no hunter can perceive the player, so the pack drops to searching and
    /// you can watch the head-scan behaviour without being engaged. See
    /// [`crate::enemy::Enemy::set_detectable`]; applied per hunter in `fixed_step`.
    pub fn toggle_invisible(&mut self) -> bool {
        self.player_invisible = !self.player_invisible;
        log::info!(
            "player invisibility: {}",
            if self.player_invisible { "ON" } else { "off" }
        );
        self.player_invisible
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
