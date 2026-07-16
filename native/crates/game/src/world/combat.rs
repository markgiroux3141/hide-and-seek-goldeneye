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
        Some(crate::hud::ammo_quads(
            self.weapon.magazine(),
            self.weapon.reserve(),
            aspect,
        ))
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
            Some(hit) => {
                // Nudge the marker just off the surface to avoid z-fighting.
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
