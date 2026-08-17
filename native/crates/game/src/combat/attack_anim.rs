//! **`attackanimconfig`** — Perfect Dark's authored per-animation attack timing,
//! transcribed from `game/chraction.c:912+` (`struct attackanimconfig`,
//! `include/types.h:333`).
//!
//! # Why this is a transcription and not a guess
//!
//! Our hunters' fire timing came from `anim_set::FIRE_TIMING` — hand-set windows
//! ported from the 3DS FPS JavaScript, keyed by the GoldenEye clip's hex id. Perfect
//! Dark authors the same thing properly: a row per animation giving when the
//! character may *aim*, when it may *shoot*, when it *recoils*, where the clip is
//! trimmed, and how far the aim may swing before the body has to lean.
//!
//! Those rows apply to us directly, because **the two games share one animation
//! bank** (see `tools/pd-assets/pd_animmap.py`): the three fire clips a hunter can
//! play are `ANIM_0032`, `ANIM_0041` and `ANIM_007A`, and Perfect Dark has an
//! authored row for each. So this is not a port of *similar* data — it is the real
//! table for the exact animations already on disk.
//!
//! Comparing them is what showed the guess was wrong, and where:
//!
//! | Clip | `FIRE_TIMING` guess | Authored | |
//! |---|---|---|---|
//! | rifle `ANIM_0032` | frames 27–80 | shoot 30–81 | close |
//! | dual `ANIM_007A` | frames 28–65 | shoot 28–68 | close |
//! | pistol `ANIM_0041` | frames 63–66 | shoot 58–92 | **a 3-frame sliver inside a 34-frame window** |
//!
//! The pistol guess was so narrow that a pistol hunter often got one shot, or none,
//! out of a burst. And every row carries two things the guess had no way to express
//! at all: an **aim window wider than the shoot window** (the character swivels onto
//! you while still raising the gun — 20 → 58 on the pistol is 1.3 s of tracking
//! before the first round), and **anisotropic aim limits**.
//!
//! # What is deliberately not ported
//!
//! `chr_calculate_aimend` (`chraction.c:9071`) consumes these rows to drive shoulder
//! / back / lean offsets against PD's own skeleton and gun props. Our
//! [`AimOffsetLayer`](engine::skeletal::layers::AimOffsetLayer) already does that
//! job — it measures the real barrel and swings the chest — so what is taken here is
//! the *data* (windows and limits), not PD's solver.
//!
//! `turnangleperframe` and `angleoffset` are 0 on all three of our rows, so there is
//! nothing to carry across; `freearmfrac*` (the gunless arm following the gun arm)
//! is recorded but unused — we have no free-arm layer yet.

use engine::skeletal::layers::AimCone;

use super::enemy_weapons::EnemyWeaponClass;

/// Frames per second the PD clips were exported at (`pd_gltf.py`'s `DEFAULT_FPS`,
/// derived from `chr_action_go_to_position`). Frame numbers in the table below are
/// animation frames, so this converts them to the seconds our mixer runs on.
pub const PD_ANIM_FPS: f32 = 30.0;

/// Rare's pi (`M_BADPI`, `include/math.h:5`) — the constant `BADDTOR` uses. Kept
/// rather than `std::f32::consts::PI` so the transcribed angles are the values the
/// game actually computed.
const BAD_PI: f32 = 3.141_092_6;

/// `BADDTOR` — degrees to radians through Rare's pi.
const fn baddtor(deg: f32) -> f32 {
    deg * BAD_PI / 180.0
}

/// One row of `struct attackanimconfig`, in the source's own field order.
///
/// The invariant the original author documented (`types.h:339`):
///
/// ```text
/// start <= aimstart <= shootstart <= recoilstart <= recoilend <= shootend <= aimend <= end
/// ```
#[derive(Clone, Copy, Debug)]
pub struct AttackAnimConfig {
    /// The Perfect Dark animation id this row configures — provenance, and what
    /// makes the row checkable against `pd_roster.json`.
    pub anim: &'static str,
    /// Clip trim: start later / finish earlier than the raw animation.
    /// `end_frame < 0` means "to the end of the clip".
    pub start_frame: f32,
    pub end_frame: f32,
    /// The character fires during these frames, given line of sight.
    pub shoot: (f32, f32),
    /// Recoil frames, for single-shot pistols; `None` when the clip has none.
    pub recoil: Option<(f32, f32)>,
    /// When the character may swivel its aim — **wider than [`Self::shoot`]**: it
    /// can track while still raising its arm, and keeps tracking after the last
    /// round.
    pub aim: (f32, f32),
    /// Aim limits (radians). Past these the character leans back / forward rather
    /// than swivelling further. `left` positive, `right` negative in the source;
    /// stored here as positive magnitudes, which is what [`AimCone`] wants.
    pub max_up: f32,
    pub max_down: f32,
    pub max_left: f32,
    pub max_right: f32,
    /// When aiming up / down, the gunless arm moves by this fraction of the gun
    /// arm. Recorded for completeness — nothing drives a free arm yet.
    pub free_arm_up: f32,
    pub free_arm_down: f32,
}

impl AttackAnimConfig {
    /// The authored aim limits as a layer cone.
    pub fn cone(&self) -> AimCone {
        AimCone {
            up: self.max_up,
            down: self.max_down,
            left: self.max_left,
            right: self.max_right,
        }
    }
}

/// `var80065758[0]` (`chraction.c:920`) — the two-handed standing attack, reached
/// through `g_StandHeavyAttackAnims[RACE_HUMAN]`. Our rifle fire clip.
pub const RIFLE: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0032",
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (30.0, 81.0),
    recoil: None,
    aim: (25.0, 81.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 1.6,
    free_arm_down: 1.75,
};

/// `chraction.c:981` — the one-handed standing attack, through
/// `g_StandLightAttackAnims[RACE_HUMAN]`. Our pistol fire clip, and the row whose
/// shoot window is eleven times wider than the one we were guessing.
pub const PISTOL: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0041",
    start_frame: 12.0,
    end_frame: 140.0,
    shoot: (58.0, 92.0),
    recoil: Some((60.0, 79.0)),
    aim: (20.0, 120.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `chraction.c:1064` — the dual-wield standing attack, through
/// `g_StandDualAttackAnims[RACE_HUMAN]`.
pub const DUAL: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007A",
    start_frame: 7.0,
    end_frame: 92.0,
    shoot: (28.0, 68.0),
    recoil: None,
    aim: (11.0, 73.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// The authored row for a weapon class, matching how PD selects one: dual-wield
/// first, then one-handed (`g_StandLightAttackAnims`) versus two-handed
/// (`g_StandHeavyAttackAnims`) — the same split
/// [`fire_clip_index`](crate::world::hunt::fire_clip_index) makes.
pub fn config_for(class: EnemyWeaponClass, dual: bool) -> &'static AttackAnimConfig {
    if dual {
        &DUAL
    } else {
        match class {
            EnemyWeaponClass::Pistol => &PISTOL,
            EnemyWeaponClass::Rifle => &RIFLE,
        }
    }
}

/// A hunter's resolved fire timing, in **seconds** into its fire animation — what
/// the combat pump and the aim layers actually read each frame.
///
/// Built either from a Perfect Dark [`AttackAnimConfig`] or from the legacy
/// `anim_set::FIRE_TIMING` guess, so the runtime code has one shape and the choice
/// is made once, at spawn.
#[derive(Clone, Copy, Debug)]
pub struct FireTiming {
    /// Shots are pumped while the burst clock is inside this window.
    pub shoot: (f32, f32),
    /// The chest tracks the target while inside this window. Wider than
    /// [`Self::shoot`] under the PD rows; the whole burst under the legacy one.
    pub aim: (f32, f32),
    /// Recoil kicks on shots inside this window; `None` kicks on every shot (the
    /// legacy behaviour).
    pub recoil: Option<(f32, f32)>,
    /// Where the burst clock starts — a PD row may trim the first frames off.
    pub start: f32,
    /// Where the burst ends and the hunter drops back to locomotion.
    pub end: f32,
    /// How far the chest may swing to point the barrel.
    pub cone: AimCone,
    /// Whether this came from Perfect Dark's authored table (for the debug overlay
    /// and for tests that need to tell the two apart).
    pub authored: bool,
}

impl FireTiming {
    /// Resolve a PD row against the clip it configures. `clip_duration` closes an
    /// open (`-1`) `end_frame`, and clamps a trim that runs past a clip we exported
    /// at a different length than the ROM's.
    pub fn from_pd(cfg: &AttackAnimConfig, clip_duration: f32) -> Self {
        let f = |frame: f32| (frame / PD_ANIM_FPS).clamp(0.0, clip_duration.max(0.0));
        let end = if cfg.end_frame < 0.0 { clip_duration } else { f(cfg.end_frame) };
        FireTiming {
            shoot: (f(cfg.shoot.0), f(cfg.shoot.1)),
            aim: (f(cfg.aim.0), f(cfg.aim.1)),
            recoil: cfg.recoil.map(|(a, b)| (f(a), f(b))),
            start: f(cfg.start_frame),
            end,
            cone: cfg.cone(),
            authored: true,
        }
    }

    /// The pre-existing behaviour: the `FIRE_TIMING` window, aim tracking for the
    /// whole burst, recoil on every shot, one isotropic cone. Kept so GoldenEye
    /// hunters are byte-for-byte unchanged while the PD rows are evaluated in the
    /// lab.
    pub fn legacy(window: (f32, f32), cone: f32) -> Self {
        FireTiming {
            shoot: window,
            aim: (0.0, f32::INFINITY),
            recoil: None,
            start: 0.0,
            end: window.1,
            cone: AimCone::uniform(cone),
            authored: false,
        }
    }

    /// Whether the burst clock is inside the shoot window.
    pub fn shooting(&self, t: f32) -> bool {
        t >= self.shoot.0 && t <= self.shoot.1
    }

    /// Whether the burst clock is inside the aim window (so the chest tracks).
    pub fn aiming(&self, t: f32) -> bool {
        t >= self.aim.0 && t <= self.aim.1
    }

    /// Whether a shot at burst clock `t` should kick the visual recoil.
    pub fn recoiling(&self, t: f32) -> bool {
        match self.recoil {
            Some((a, b)) => t >= a && t <= b,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcribed rows obey the ordering invariant the original author wrote
    /// down (`types.h:339`). A typo in any frame number would break it, and nothing
    /// else in the game would notice — a shoot window that started before the aim
    /// window would just quietly fire without tracking.
    #[test]
    fn rows_obey_the_authored_frame_ordering() {
        for cfg in [&RIFLE, &PISTOL, &DUAL] {
            let (aim_s, aim_e) = cfg.aim;
            let (shoot_s, shoot_e) = cfg.shoot;
            let end = if cfg.end_frame < 0.0 { f32::INFINITY } else { cfg.end_frame };
            assert!(cfg.start_frame <= aim_s, "{}: start <= aimstart", cfg.anim);
            assert!(aim_s <= shoot_s, "{}: aimstart <= shootstart", cfg.anim);
            assert!(shoot_s <= shoot_e, "{}: shootstart <= shootend", cfg.anim);
            assert!(shoot_e <= aim_e, "{}: shootend <= aimend", cfg.anim);
            assert!(aim_e <= end, "{}: aimend <= end", cfg.anim);
            if let Some((r_s, r_e)) = cfg.recoil {
                assert!(shoot_s <= r_s, "{}: shootstart <= recoilstart", cfg.anim);
                assert!(r_s <= r_e, "{}: recoilstart <= recoilend", cfg.anim);
                assert!(r_e <= shoot_e, "{}: recoilend <= shootend", cfg.anim);
            }
        }
    }

    /// **Every row aims wider than it shoots.** This is the structural difference
    /// from the `FIRE_TIMING` guess, which had a single window doing both jobs — a
    /// hunter that tracks only while its trigger is down snaps onto the player at
    /// the moment it fires instead of swivelling onto them first.
    #[test]
    fn the_aim_window_is_wider_than_the_shoot_window() {
        for cfg in [&RIFLE, &PISTOL, &DUAL] {
            assert!(cfg.aim.0 < cfg.shoot.0, "{}: tracks before it fires", cfg.anim);
            assert!(cfg.aim.1 >= cfg.shoot.1, "{}: tracks past the last round", cfg.anim);
        }
    }

    /// Resolution to seconds is clamped by the clip, so an authored `endframe` past
    /// our exported clip length cannot produce a window the burst never leaves.
    #[test]
    fn resolving_clamps_to_the_clip() {
        let t = FireTiming::from_pd(&PISTOL, 6.13); // ANIM_0041 is 185 frames
        assert!((t.shoot.0 - 58.0 / 30.0).abs() < 1e-4);
        assert!((t.shoot.1 - 92.0 / 30.0).abs() < 1e-4);
        assert!(t.aim.0 < t.shoot.0 && t.aim.1 > t.shoot.1, "aim brackets shoot");
        assert!(t.recoiling(70.0 / 30.0), "mid-recoil-window shot kicks");
        assert!(!t.recoiling(85.0 / 30.0), "a later shot in the same burst does not");

        // A short clip clamps rather than producing an unreachable window.
        let short = FireTiming::from_pd(&PISTOL, 1.0);
        assert!(short.shoot.1 <= 1.0 && short.end <= 1.0);

        // An open end frame resolves to the clip's own length.
        let open = FireTiming::from_pd(&RIFLE, 3.5);
        assert!((open.end - 3.5).abs() < 1e-4, "endframe -1 means the whole clip");
    }

    /// The legacy path keeps the old behaviour exactly: track for the whole burst,
    /// recoil on every shot, one cone.
    #[test]
    fn legacy_timing_tracks_and_recoils_throughout() {
        let t = FireTiming::legacy((0.9, 2.67), 1.4);
        assert!(!t.authored);
        assert!(t.aiming(0.0) && t.aiming(10.0), "legacy aims for the whole burst");
        assert!(t.recoiling(0.0) && t.recoiling(10.0), "legacy recoils on every shot");
        assert!(t.shooting(1.0) && !t.shooting(3.0));
    }
}
