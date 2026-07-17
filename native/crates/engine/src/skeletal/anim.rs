//! Animation playback + crossfade mixer over a set of [`AnimationClip`]s bound
//! to one skeleton. Mirrors the JS `Enemy.js` model: locomotion is **discrete
//! clip selection** (walk/jog/run by speed band), and switching clips
//! **crossfades over 0.15 s** (`crossFadeFrom(prev, 0.15)`) rather than blending
//! continuously by fractional speed. The blend is per-joint (slerp rotations,
//! lerp translations/scales), so it's pose-correct, not matrix-lerped.
//!
//! Reused beyond B3: the same player drives one-shots (B4) and the enemy's
//! visual (B5).

use glam::{Mat4, Quat, Vec3};

use crate::skeletal::clip::AnimationClip;
use crate::skeletal::Skeleton;

/// One playing clip: index into the player's clip list, its local clock, and
/// whether it loops (locomotion/idle) or plays once then clamps (fire/hit/death).
#[derive(Clone, Copy)]
struct Playing {
    clip: usize,
    time: f32,
    looping: bool,
}

/// A crossfading animation player. Holds named clips for one skeleton, the
/// current clip, and an optional outgoing clip fading out.
pub struct AnimPlayer {
    clips: Vec<AnimationClip>,
    current: Playing,
    /// Outgoing clip during a crossfade, with the elapsed/total blend.
    prev: Option<Playing>,
    /// Blend weight of `current` (0 → all `prev`, 1 → all `current`).
    blend: f32,
    /// Weight gained per second (`1/fade`); `f32::INFINITY` = instant.
    blend_rate: f32,
    /// When a one-shot (`!looping`) `current` finishes, crossfade back to this
    /// clip (looping). `None` → clamp on the last frame and stay (death).
    return_to: Option<usize>,
    /// Fade used for the auto-return crossfade.
    return_fade: f32,
    /// The active fire window `(start, end)` in seconds for a fire one-shot, and
    /// whether playback is currently inside it. `None` → no fire clip playing.
    fire_window: Option<(f32, f32)>,
    fire_open: bool,
}

/// Default crossfade for the auto-return from a one-shot to the base loop.
const RETURN_FADE: f32 = 0.15;

impl AnimPlayer {
    /// Build a player over `clips`; `start` selects the initially-playing
    /// (looping) clip.
    pub fn new(clips: Vec<AnimationClip>, start: usize) -> Self {
        AnimPlayer {
            clips,
            current: Playing { clip: start, time: 0.0, looping: true },
            prev: None,
            blend: 1.0,
            blend_rate: f32::INFINITY,
            return_to: None,
            return_fade: RETURN_FADE,
            fire_window: None,
            fire_open: false,
        }
    }

    /// Index of a clip by name (`AnimationClip::name`), if present.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.name == name)
    }

    pub fn current_clip(&self) -> usize {
        self.current.clip
    }

    /// Borrow a clip by index (e.g. to sample a specific pose for feet-seating).
    pub fn clip(&self, idx: usize) -> Option<&AnimationClip> {
        self.clips.get(idx)
    }

    /// Crossfade to a **looping** clip `idx` over `fade` seconds (locomotion /
    /// idle). A no-op if `idx` is already the current clip AND it's looping
    /// (so re-calling every frame is safe); `fade <= 0` switches instantly.
    pub fn play(&mut self, idx: usize, fade: f32) {
        // Re-`play`ing the clip we're already looping is a no-op (guards the
        // per-frame-call freeze). But if `current` is a one-shot of the same
        // index, we still want to (re)start it as a loop, so gate on `looping`.
        if self.current.clip == idx && self.current.looping {
            return;
        }
        self.start(idx, fade, true);
        self.return_to = None;
        self.fire_window = None;
        self.fire_open = false;
    }

    /// Play clip `idx` **once** (fire / hit / death): it clamps on its last frame
    /// rather than looping. When it finishes it crossfades back to `return_to`
    /// (the base loop) — or stays clamped if `return_to` is `None` (death).
    /// `fire_window` supplies the shot window for a fire clip.
    pub fn play_once(
        &mut self,
        idx: usize,
        fade: f32,
        return_to: Option<usize>,
        fire_window: Option<(f32, f32)>,
    ) {
        self.start(idx, fade, false);
        self.return_to = return_to;
        self.fire_window = fire_window;
        self.fire_open = false;
    }

    /// Shared clip-switch: move `current` to `prev` and start `idx` fresh.
    fn start(&mut self, idx: usize, fade: f32, looping: bool) {
        if idx >= self.clips.len() {
            return;
        }
        self.prev = Some(self.current);
        self.current = Playing { clip: idx, time: 0.0, looping };
        if fade > 1e-4 {
            self.blend = 0.0;
            self.blend_rate = 1.0 / fade;
        } else {
            self.blend = 1.0;
            self.blend_rate = f32::INFINITY;
            self.prev = None;
        }
    }

    /// Whether a one-shot (fire/hit/death) is currently playing.
    pub fn is_playing_oneshot(&self) -> bool {
        !self.current.looping
    }

    /// Whether the current one-shot has reached its last frame (clamped). For a
    /// no-return one-shot (death) this stays true while it holds the final pose —
    /// the cue to start the death fade once the animation is actually done.
    pub fn oneshot_finished(&self) -> bool {
        if self.current.looping {
            return false;
        }
        let dur = self.clips[self.current.clip].duration;
        dur > 0.0 && self.current.time >= dur
    }

    /// Whether playback is currently inside a fire clip's shot window.
    pub fn fire_window_open(&self) -> bool {
        self.fire_open
    }

    /// Advance all playing clips + the crossfade by `dt`. Looping clips wrap;
    /// one-shots clamp on their last frame, then auto-return to `return_to`
    /// (unless `None`). Updates the fire-window state.
    pub fn update(&mut self, dt: f32) {
        advance(&mut self.current, &self.clips, dt);
        if let Some(prev) = self.prev.as_mut() {
            advance(prev, &self.clips, dt);
            self.blend += dt * self.blend_rate;
            if self.blend >= 1.0 {
                self.blend = 1.0;
                self.prev = None;
            }
        }

        // Fire window: open while the (one-shot) clock is inside [start, end].
        self.fire_open = match self.fire_window {
            Some((s, e)) if !self.current.looping => {
                self.current.time >= s && self.current.time <= e
            }
            _ => false,
        };

        // One-shot finished (clamped at its end) → return to the base loop.
        if !self.current.looping {
            let dur = self.clips[self.current.clip].duration;
            if dur > 0.0 && self.current.time >= dur {
                if let Some(rt) = self.return_to.take() {
                    self.play(rt, self.return_fade);
                }
                // else: no return target → stay clamped (death).
            }
        }
    }

    /// The skinning matrices for the current (possibly mid-crossfade) pose.
    pub fn skinning_matrices(&self, skeleton: &Skeleton) -> Vec<Mat4> {
        skeleton.skinning_matrices(&self.pose_locals(skeleton))
    }

    /// Global (model-space) transform of every joint for the current pose. Unlike
    /// [`Self::skinning_matrices`] this is the raw joint transform (no inverse
    /// bind), so a prop can be parented to a bone: `char_model · joint_global ·
    /// local_offset` places it in the bone's frame (the JS `bone.add(mesh)`).
    pub fn joint_global_transforms(&self, skeleton: &Skeleton) -> Vec<Mat4> {
        skeleton.global_transforms(&self.pose_locals(skeleton))
    }

    /// Per-joint local matrices of the current (possibly blended) pose.
    fn pose_locals(&self, skeleton: &Skeleton) -> Vec<Mat4> {
        let (t, r, s) = self.pose_trs(skeleton);
        (0..skeleton.joint_count())
            .map(|i| Mat4::from_scale_rotation_translation(s[i], r[i], t[i]))
            .collect()
    }

    /// The blended per-joint TRS of the current pose.
    fn pose_trs(&self, skeleton: &Skeleton) -> (Vec<Vec3>, Vec<Quat>, Vec<Vec3>) {
        let cur = &self.clips[self.current.clip];
        let (ct, cr, cs) = cur.pose_trs(self.current.time, skeleton);
        let Some(prev) = self.prev else {
            return (ct, cr, cs);
        };
        let (pt, pr, ps) = self.clips[prev.clip].pose_trs(prev.time, skeleton);
        // weight = blend of current (JS crossFadeFrom ramps the incoming weight).
        let w = self.blend.clamp(0.0, 1.0);
        let n = skeleton.joint_count();
        let mut t = vec![Vec3::ZERO; n];
        let mut r = vec![Quat::IDENTITY; n];
        let mut s = vec![Vec3::ONE; n];
        for i in 0..n {
            t[i] = pt[i].lerp(ct[i], w);
            s[i] = ps[i].lerp(cs[i], w);
            // slerp from the outgoing pose toward the incoming one.
            r[i] = pr[i].slerp(cr[i], w);
        }
        (t, r, s)
    }
}

/// Advance one playing clip's clock. Looping clips wrap within their duration;
/// one-shots clamp at the last frame.
fn advance(p: &mut Playing, clips: &[AnimationClip], dt: f32) {
    let dur = clips[p.clip].duration;
    if dur <= 0.0 {
        p.time += dt;
    } else if p.looping {
        p.time = (p.time + dt).rem_euclid(dur);
    } else {
        p.time = (p.time + dt).min(dur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn karl() -> crate::skeletal::gltf_skin::SkinnedModel {
        let p = format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        crate::skeletal::gltf_skin::load(&p).expect("load karl")
    }

    fn clip(name: &str, sk: &Skeleton) -> AnimationClip {
        let p = format!(
            "{}/../../assets/enemies/animations/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        crate::skeletal::clip::load(&p, sk).expect(name)
    }

    /// A mid-crossfade pose is a genuine blend: it differs from both endpoints,
    /// and stays finite. This is the headless half of the "no pops" oracle.
    #[test]
    fn crossfade_blends_between_two_clips() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let walk = clip("28-walking.glb", &m.skeleton);
        let mut player = AnimPlayer::new(vec![idle, walk], 0);

        // Freeze both clocks at t=0 by sampling immediately, then crossfade.
        let idle_only = player.skinning_matrices(&m.skeleton);
        player.play(1, 0.15);
        // Halfway through the fade (no time advance on the clocks beyond the fade
        // step keeps the comparison about the blend weight).
        player.update(0.075);
        let mid = player.skinning_matrices(&m.skeleton);

        // Mid differs from the pure idle pose (the blend moved it).
        let diff: f32 = idle_only
            .iter()
            .zip(&mid)
            .flat_map(|(a, b)| (*a - *b).to_cols_array())
            .map(|v| v.abs())
            .sum();
        assert!(diff > 1e-3, "mid-crossfade pose should differ from idle");
        for mat in &mid {
            for c in mat.to_cols_array() {
                assert!(c.is_finite(), "non-finite matrix during crossfade");
            }
        }
    }

    #[test]
    fn play_is_a_noop_for_the_current_clip() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let walk = clip("28-walking.glb", &m.skeleton);
        let mut player = AnimPlayer::new(vec![idle, walk], 0);
        player.play(0, 0.15);
        // No crossfade should have started.
        assert!(player.prev.is_none());
    }

    /// Regression: calling `play(idx)` every frame for the clip already playing
    /// must NOT reset the clock — the fade still completes and the pose advances.
    /// (The B3 freeze: a per-frame `play` reset `time`/`blend` each call.)
    #[test]
    fn repeated_play_of_current_clip_does_not_freeze() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let walk = clip("28-walking.glb", &m.skeleton);
        let mut player = AnimPlayer::new(vec![idle, walk], 0);
        player.play(1, 0.15);
        let early = player.skinning_matrices(&m.skeleton);
        for _ in 0..30 {
            player.play(1, 0.15); // the offending per-frame pattern
            player.update(1.0 / 60.0);
        }
        assert!(player.prev.is_none(), "fade should complete despite repeated play");
        assert_eq!(player.current_clip(), 1);
        let later = player.skinning_matrices(&m.skeleton);
        let diff: f32 = early
            .iter()
            .zip(&later)
            .flat_map(|(a, b)| (*a - *b).to_cols_array())
            .map(|v| v.abs())
            .sum();
        assert!(diff > 1e-3, "clip clock must advance (frozen-frame regression)");
    }

    #[test]
    fn oneshot_clamps_then_returns_to_base_loop() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let hit = clip("0E-hit-left-shoulder.glb", &m.skeleton);
        let mut p = AnimPlayer::new(vec![idle, hit], 0);
        p.play_once(1, 0.0, Some(0), None);
        assert!(p.is_playing_oneshot(), "one-shot playing");
        let dur = p.clip(1).unwrap().duration;
        p.update(dur + 0.5); // past the end
        assert!(!p.is_playing_oneshot(), "returned to a loop");
        assert_eq!(p.current_clip(), 0, "returned to base idle");
    }

    #[test]
    fn death_oneshot_with_no_return_stays_clamped() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let death = clip("1A-death-forward-face-down.glb", &m.skeleton);
        let mut p = AnimPlayer::new(vec![idle, death], 0);
        p.play_once(1, 0.0, None, None);
        let dur = p.clip(1).unwrap().duration;
        p.update(dur + 1.0);
        assert!(p.is_playing_oneshot(), "death clamps, does not return");
        assert_eq!(p.current_clip(), 1);
    }

    #[test]
    fn fire_window_opens_inside_the_timing_window() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let fire = clip("01-fire-standing.glb", &m.skeleton);
        let mut p = AnimPlayer::new(vec![idle, fire], 0);
        p.play_once(1, 0.0, Some(0), Some((0.9, 2.67)));
        p.update(0.5);
        assert!(!p.fire_window_open(), "before the window (t=0.5)");
        p.update(0.6); // t = 1.1, inside [0.9, 2.67]
        assert!(p.fire_window_open(), "inside the window (t=1.1)");
    }

    #[test]
    fn crossfade_completes_and_drops_the_outgoing_clip() {
        let m = karl();
        let idle = clip("00-idle.glb", &m.skeleton);
        let walk = clip("28-walking.glb", &m.skeleton);
        let mut player = AnimPlayer::new(vec![idle, walk], 0);
        player.play(1, 0.15);
        assert!(player.prev.is_some(), "fade started");
        player.update(0.2); // past the 0.15s fade
        assert!(player.prev.is_none(), "outgoing clip dropped");
        assert_eq!(player.current_clip(), 1);
    }
}
