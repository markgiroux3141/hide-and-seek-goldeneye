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

impl World {
    /// Attach the audio subsystem (called once at startup by the app, after the
    /// device is initialized). Preloads the weapon's fire/reload/empty sounds so
    /// the first shot doesn't hitch, starts the looping background music, and
    /// stores the manager so `combat_step` can play the weapon's queued cues.
    pub fn attach_audio(&mut self, mut audio: AudioManager) {
        let cfg = self.weapon.config();
        audio.load(cfg.fire_sound);
        audio.load(cfg.reload_sound);
        audio.load(cfg.empty_sound);
        // Track A enemy-hit SFX: the flesh bullet-hit + every pain vocal, so a hit
        // never hitches on a first-play decode.
        audio.load("sounds/enemies/bullet-hit.wav");
        for n in 1..=PAIN_COUNT {
            audio.load(&format!("sounds/enemies/pain-{n}.wav"));
        }
        // A3/P5: the hunter's rifle report + the player's own hit vocal.
        audio.load(ENEMY_FIRE_SOUND);
        audio.load(PLAYER_HIT_SOUND);
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
                self.weapon.magazine(),
                self.weapon.reserve(),
                aspect,
            ))
        }
    }

    /// Manual weapon reload (the `R` key in HUNT). No-op outside HUNT; the weapon
    /// itself gates the request (not already reloading, not mid post-fire delay,
    /// magazine not full, reserve remaining).
    pub fn reload_weapon(&mut self) {
        if self.mode == Mode::Hunt {
            self.weapon.request_reload();
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
        Some(self.weapon.view.clip_transform(aspect, self.aim_x, self.aim_y))
    }

    /// The muzzle-flash overlay transform this frame, or `None` when it shouldn't
    /// render (outside HUNT, no flash asset, or no shot's flash currently active).
    /// The flash shares the gun's pivot/scale/rotation, so it uses the SAME clip
    /// transform as the gun (JS adds the flash to the same `model` group).
    pub fn muzzle_transform(&self, aspect: f32) -> Option<Mat4> {
        if self.mode != Mode::Hunt || self.muzzle_model.is_none() || !self.weapon.flash_active() {
            return None;
        }
        Some(self.weapon.view.clip_transform(aspect, self.aim_x, self.aim_y))
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

        // Fire: left mouse held (only while the cursor is grabbed). The weapon
        // gates on cooldown + the semi/auto edge rule.
        let trigger = input.pointer_locked && input.mouse_left_down();
        let fired = self.weapon.update(dt, trigger);

        // Play any sound cues the weapon queued this frame — fire, reload (manual
        // `R`, empty-click auto-reload, or the post-empty auto-reload), and the
        // empty click. Drained every frame regardless of whether a shot fired, so
        // a reload-only frame (e.g. `R` with a partial mag) still gets its sound.
        let cues = self.weapon.take_cues();
        if let Some(audio) = self.audio.as_mut() {
            for cue in cues {
                audio.play(cue.name, cue.volume);
            }
        }

        if !fired {
            return;
        }

        // A shot left the barrel — cast through the crosshair (which may be
        // offset by free-aim). Copy eye + look out so the character borrow ends
        // before the mutable physics borrow, then bend the ray toward the
        // crosshair's aim-space offset (same offset the gun tilts to).
        let Some((eye, fwd)) = self.character.as_ref().map(|c| (c.eye(), c.forward())) else {
            return;
        };
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        let dir = (fwd + AIM_FOV_TAN * (self.aim_x * right + self.aim_y * up)).normalize_or_zero();
        let dir = if dir == Vec3::ZERO { fwd } else { dir };
        let range = self.weapon.config().range;
        // Player collider excluded — `None` today (the native player is a transient
        // shape-cast, not a registered collider), threaded for Track A. Recoil is
        // gun-only (the viewmodel kick, armed in `Weapon::update`) — no camera kick,
        // matching GoldenEye.
        match crate::combat::shooting::cast(&mut self.physics, eye, dir, range, None) {
            Some(hit) if Some(hit.collider) == self.physics.enemy_collider_handle() => {
                // Track A: the shot landed on the hunter's capsule — damage it (no
                // wall spark; the hit reaction + pain SFX are the feedback).
                self.hit_enemy();
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

    /// Apply a PP7 hit to the hunter (Track A). Damages it; on the lethal shot
    /// plays a random death one-shot (clamps), starts the 2 s opacity fade, and
    /// removes the capsule collider (a corpse can't be shot). Otherwise plays a
    /// random hit reaction that auto-returns to locomotion and stuns the hunter
    /// for the clip's length. Always plays the pain + bullet-hit SFX (JS `onHit`).
    pub(crate) fn hit_enemy(&mut self) {
        let dmg = self.weapon.config().damage;
        // Apply damage — borrow the enemy alone; bail if already dead / gone.
        let died = match self.enemy.as_mut() {
            Some(e) if !e.is_dead() => e.take_damage(dmg),
            _ => return,
        };

        // On-hit SFX: a random pain vocal + the flesh bullet-hit.
        let pain = self.rand_below(PAIN_COUNT) + 1;
        if let Some(audio) = self.audio.as_mut() {
            audio.play(&format!("sounds/enemies/pain-{pain}.wav"), PAIN_VOL);
            audio.play("sounds/enemies/bullet-hit.wav", BULLET_HIT_VOL);
        }

        if died {
            self.char_dead = true;
            // Fade is NOT started here — it begins only once the death animation
            // finishes (see `advance_animation`), so the body stays visible while
            // it plays out. `enemy_fade` stays `None` (opacity 1) until then.
            self.physics.remove_enemy_collider();
            let death_start = CHAR_HIT_START + anim_set::HIT_CLIPS.len();
            let pick = self.rand_below(anim_set::DEATH_CLIPS.len());
            if let Some(anim) = self.char_anim.as_mut() {
                // No return target → the death pose clamps and holds while it fades.
                anim.play_once(death_start + pick, 0.2, None, None);
            }
            log::info!("HUNTER DOWN ({})", anim_set::DEATH_CLIPS[pick]);
        } else {
            let idx = CHAR_HIT_START + self.rand_below(anim_set::HIT_CLIPS.len());
            // Return to the current locomotion band so the one-shot flips
            // `is_playing_oneshot` back off, letting the HUNT driver resume.
            let band = band_for_speed(self.enemy.as_ref().map(|e| e.speed()).unwrap_or(0.0));
            let dur = self
                .char_anim
                .as_ref()
                .and_then(|a| a.clip(idx))
                .map(|c| c.duration)
                .unwrap_or(0.4);
            if let Some(anim) = self.char_anim.as_mut() {
                anim.play_once(idx, 0.1, Some(band), None);
            }
            if let Some(e) = self.enemy.as_mut() {
                e.stun(dur);
            }
            let hp = self.enemy.as_ref().map(|e| e.health()).unwrap_or(0.0);
            log::info!(
                "hunter hit — {dmg:.0} dmg, {hp:.0} hp left ({})",
                anim_set::HIT_CLIPS[idx - CHAR_HIT_START]
            );
        }
    }

    /// Start a fire burst on the shared animation mixer — the hunter entered
    /// `attack` (A3). Plays the rifle fire one-shot with its FIRE_TIMING window;
    /// the per-shot cadence + damage roll run in [`Self::enemy_combat_step`]. Resets
    /// the cadence so the first shot waits for the window's `fireStart`.
    pub(crate) fn start_enemy_fire(&mut self) {
        if let Some(anim) = self.char_anim.as_mut() {
            // Return to idle when done; the HUNT driver re-selects a band after.
            anim.play_once(CHAR_FIRE_IDX, 0.1, Some(0), Some(anim_set::FIRE_WINDOW));
        }
        self.enemy_shot_timer = 0.0;
        log::info!("hunter firing");
    }

    /// Per-frame enemy combat + player damage-feedback (HUNT only). Pumps the
    /// hunter's rifle shots while its fire animation is inside the FIRE_TIMING
    /// window — one shot per `1/ENEMY_FIRE_RATE` seconds, the JS
    /// `EnemyCharacter.tick` pump — and decays the muzzle flash + the red damage
    /// flash + the health-HUD pop timer. Called once per render frame after
    /// [`Self::advance_animation`] (which advances the fire window).
    pub fn enemy_combat_step(&mut self, dt: f32) {
        if self.mode != Mode::Hunt {
            return;
        }
        // Decay feedback timers (these run even while dead so a final flash fades).
        if self.damage_flash > 0.0 {
            self.damage_flash = (self.damage_flash - dt * DAMAGE_FLASH_DECAY).max(0.0);
        }
        if self.hud_show_timer > 0.0 {
            self.hud_show_timer = (self.hud_show_timer - dt).max(0.0);
        }
        if self.enemy_muzzle_timer > 0.0 {
            self.enemy_muzzle_timer = (self.enemy_muzzle_timer - dt).max(0.0);
        }
        if self.player_dead {
            return;
        }

        // The hunter fires only while its FIRE one-shot is inside its window
        // (the hard-won FIRE_TIMING mapping), spaced by 1/fireRate.
        let (firing, window_open) = self
            .char_anim
            .as_ref()
            .map(|a| {
                let f = a.is_playing_oneshot() && a.current_clip() == CHAR_FIRE_IDX;
                (f, f && a.fire_window_open())
            })
            .unwrap_or((false, false));
        if !firing {
            self.enemy_shot_timer = 0.0;
            return;
        }
        if window_open {
            self.enemy_shot_timer -= dt;
            if self.enemy_shot_timer <= 0.0 {
                self.enemy_shot_timer = 1.0 / ENEMY_FIRE_RATE;
                self.emit_enemy_shot();
            }
        }
    }

    /// One rifle shot from the hunter (JS `EnemyCharacter.onShotFired` + the AI
    /// damage callback): muzzle flash + gun report always; then, when LOS is clear,
    /// roll `accuracy·(1−dist/maxRange)` and apply damage to the player on a hit.
    fn emit_enemy_shot(&mut self) {
        let epos = match self.enemy.as_ref() {
            Some(e) if !e.is_dead() => e.pos,
            _ => return,
        };
        let Some(ppos) = self.player_pos() else { return };
        // Flash + report fire on every shot, hit or miss.
        self.enemy_muzzle_timer = ENEMY_MUZZLE_TIME;
        if let Some(audio) = self.audio.as_mut() {
            audio.play(ENEMY_FIRE_SOUND, ENEMY_FIRE_VOL);
        }
        // Walls block the shot (re-checked per shot, JS-faithful).
        if !crate::enemy::line_of_sight(&mut self.physics, epos, ppos) {
            return;
        }
        let dist = Vec3::new(ppos.x - epos.x, 0.0, ppos.z - epos.z).length();
        let dist_factor = (1.0 - dist / ENEMY_MAX_RANGE).max(0.0);
        let hit_chance = ENEMY_ACCURACY * dist_factor;
        if self.rand_float() < hit_chance {
            self.take_player_damage(ENEMY_DAMAGE);
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

    /// A combined colored mesh of the live hit sparks (bright markers at impact
    /// points), for the renderer's spark pass. `None` when no sparks are active.
    pub fn spark_mesh(&self) -> Option<ColoredMesh> {
        if self.sparks.is_empty() {
            return None;
        }
        let mut verts: Vec<ColorVertex> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        for s in &self.sparks {
            let min = s.pos - Vec3::splat(SPARK_HALF);
            let max = s.pos + Vec3::splat(SPARK_HALF);
            push_colored_box(&mut verts, &mut idx, min, max, [1.0, 0.92, 0.35]);
        }
        Some(ColoredMesh {
            vertices: verts,
            indices: idx,
        })
    }
}
