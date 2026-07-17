//! Portable animation DATA ported from `3DS FPS/src/data/AnimationSet.ts`
//! (itself ported from the GoldenEye enemy system). Speeds are already metres;
//! fire-timing windows are seconds into the clip.
//!
//! B4 uses `FIRE_WINDOW`/`FIRE_TIMING` for the fire one-shot's shot window, and
//! `HIT_CLIPS`/`DEATH_CLIPS` as the random-pick sets. The full `FIRE_TIMING`
//! table is ported for the later combat track even though the B4 demo fires only
//! the standing rifle clip (`01`).

/// Speed thresholds (m/s) — `SPEED_THRESHOLDS` in the JS. Band selection:
/// `≥run → run, ≥jog → jog, else walk` (see `Enemy.js::_playLocomotion`).
pub const SPEED_WALK: f32 = 1.5;
pub const SPEED_JOG: f32 = 3.5;
pub const SPEED_RUN: f32 = 5.0;

/// Per-clip bullet-fire window `(fireStart, fireEnd)` in seconds — `FIRE_TIMING`
/// keyed by the clip's hex id. The enemy emits shots while the animation is
/// inside this window (not on trigger press).
pub const FIRE_TIMING: &[(&str, f32, f32)] = &[
    // Two-handed assault rifle
    ("01", 0.9, 2.67),
    ("02", 0.76, 1.77),
    ("03", 0.7, 2.07),
    ("04", 0.5, 2.1),
    ("05", 1.27, 3.13),
    ("06", 0.97, 2.5),
    ("07", 1.17, 2.5),
    ("08", 1.6, 3.33),
    ("09", 1.13, 3.03),
    ("0A", 1.17, 2.87),
    ("0B", 3.53, 5.2),
    ("0C", 2.43, 3.63),
    // Pistol — single shot
    ("41", 2.1, 2.2),
    ("42", 1.5, 1.6),
    ("43", 1.67, 1.77),
    ("44", 1.07, 1.17),
    ("45", 0.8, 0.9),
    ("46", 0.73, 0.83),
    // Dual wield
    ("6C", 0.0, 1.1),
    ("6D", 0.0, 1.1),
    ("74", 1.0, 2.93),
    ("7A", 0.93, 2.17),
    ("7B", 1.4, 2.9),
    ("7C", 1.87, 3.33),
];

/// Fire window for a clip hex id, if it has one.
pub fn fire_window(hex: &str) -> Option<(f32, f32)> {
    FIRE_TIMING
        .iter()
        .find(|(h, _, _)| *h == hex)
        .map(|(_, s, e)| (*s, *e))
}

/// The B4 demo fire clip (standing rifle — `russian-guard` is a rifle unit) and
/// its window (`FIRE_TIMING["01"]`).
pub const FIRE_CLIP: &str = "01-fire-standing.glb";
pub const FIRE_WINDOW: (f32, f32) = (0.9, 2.67);

/// Hit-reaction clips grouped by body zone (a subset of [`HIT_CLIPS`], chosen by
/// their descriptive filenames) so a shot to the head / torso / legs plays a
/// fitting reaction. Head → the neck snap; torso → shoulder/arm recoils; legs →
/// the leg buckles. Each name must also appear in [`HIT_CLIPS`] (the loaded set).
pub const HEAD_HIT_CLIPS: &[&str] = &["17-hit-neck.glb"];
pub const TORSO_HIT_CLIPS: &[&str] = &[
    "0E-hit-left-shoulder.glb",
    "0F-hit-right-shoulder.glb",
    "10-hit-left-arm.glb",
    "11-hit-right-arm.glb",
];
pub const LEG_HIT_CLIPS: &[&str] = &["14-hit-left-leg.glb", "15-hit-right-leg.glb"];

/// Position of a hit clip within [`HIT_CLIPS`] (its offset from the first hit clip
/// in the loaded `AnimPlayer` layout), or `None` if the name isn't a hit clip.
pub fn hit_clip_pos(name: &str) -> Option<usize> {
    HIT_CLIPS.iter().position(|&c| c == name)
}

/// Hit-reaction clips (`HIT_ANIMS`, 12) — the full loaded set; the zone groups
/// above pick from it, and unzoned callers (the BUILD demo) random-pick any.
pub const HIT_CLIPS: &[&str] = &[
    "0E-hit-left-shoulder.glb",
    "0F-hit-right-shoulder.glb",
    "10-hit-left-arm.glb",
    "11-hit-right-arm.glb",
    "12-hit-left-hand.glb",
    "13-hit-right-hand.glb",
    "14-hit-left-leg.glb",
    "15-hit-right-leg.glb",
    "17-hit-neck.glb",
    "36-hit-butt-long.glb",
    "37-hit-butt-short.glb",
    "81-hit-taser.glb",
];

/// Death clips (`DEATH_ANIMS`, 17) — random-picked on death; clamp on the last
/// frame (the body stays down).
pub const DEATH_CLIPS: &[&str] = &[
    "16-death-genitalia.glb",
    "18-death-neck.glb",
    "19-death-stagger-back-to-wall.glb",
    "1A-death-forward-face-down.glb",
    "1B-death-forward-spin-face-up.glb",
    "1C-death-backward-fall-face-up-1.glb",
    "1D-death-backward-spin-face-down-right.glb",
    "1E-death-backward-spin-face-up-right.glb",
    "1F-death-backward-spin-face-down-left.glb",
    "20-death-backward-spin-face-up-left.glb",
    "21-death-forward-face-down-hard.glb",
    "22-death-forward-face-down-soft.glb",
    "23-death-fetal-position-right.glb",
    "24-death-fetal-position-left.glb",
    "25-death-backward-fall-face-up-2.glb",
    "38-death-head.glb",
    "39-death-left-leg.glb",
];
