//! Animation-clip loading + runtime sampling for the shared GoldenEye skeleton.
//!
//! **The clip GLBs are malformed glTF:** their animation-sampler `interpolation`
//! is the integer `9729` (`GL_LINEAR`) where the spec requires the string
//! `"LINEAR"`, so `gltf::import` rejects them outright (verified). Rather than
//! hand-roll an accessor reader, [`load`] patches the GLB's JSON chunk
//! (`9729 → "LINEAR"`, `9728 → "STEP"`) and feeds the corrected bytes to
//! `gltf::import_slice`, reusing the crate's battle-tested accessor decoding.
//!
//! Channels are bound to skeleton joints **by bone name** (`Bone_1`..), not by
//! raw node index — the shared-skeleton assumption is validated at load, with an
//! index fallback + warning if a name doesn't resolve. Sampling supports all
//! three glTF interpolation modes (LINEAR/STEP/CUBICSPLINE); the locomotion
//! clips are rotation-only LINEAR, but hit/death clips (B4) may use the others.

use glam::{Mat4, Quat, Vec3};

use crate::skeletal::Skeleton;

const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Interp {
    Linear,
    Step,
    CubicSpline,
}

/// The keyframe outputs of one channel — one variant per animatable property.
/// For `CubicSpline` each keyframe stores 3 values (in-tangent, value,
/// out-tangent), so the vec length is `3 × times.len()`; otherwise it's
/// `times.len()`.
enum Track {
    Rotation(Vec<Quat>),
    Translation(Vec<Vec3>),
    Scale(Vec<Vec3>),
}

struct Channel {
    /// Skeleton joint index (name-matched at load).
    joint: usize,
    interp: Interp,
    times: Vec<f32>,
    track: Track,
}

/// One animation clip bound to a specific [`Skeleton`].
pub struct AnimationClip {
    pub name: String,
    /// Clip length in seconds (max keyframe time across all channels).
    pub duration: f32,
    channels: Vec<Channel>,
}

impl AnimationClip {
    /// Number of channels successfully bound to skeleton joints (for validation).
    pub fn bound_channels(&self) -> usize {
        self.channels.len()
    }

    /// Per-joint local TRS at `time` seconds: start from the bind TRS and apply
    /// each channel's sampled value. Returned as separate translation/rotation/
    /// scale arrays so a mixer can blend two poses correctly (slerp rotations,
    /// lerp translations/scales) before composing. `time` is clamped per channel;
    /// the caller wraps it into `[0, duration)` for looping.
    pub fn pose_trs(&self, time: f32, skeleton: &Skeleton) -> (Vec<Vec3>, Vec<Quat>, Vec<Vec3>) {
        let n = skeleton.joint_count();
        let mut t = skeleton.bind_t.clone();
        let mut r = skeleton.bind_r.clone();
        let mut s = skeleton.bind_s.clone();
        for ch in &self.channels {
            if ch.joint >= n {
                continue;
            }
            match &ch.track {
                Track::Rotation(v) => r[ch.joint] = sample_quat(&ch.times, v, ch.interp, time),
                Track::Translation(v) => t[ch.joint] = sample_vec3(&ch.times, v, ch.interp, time),
                Track::Scale(v) => s[ch.joint] = sample_vec3(&ch.times, v, ch.interp, time),
            }
        }
        (t, r, s)
    }

    /// Local pose transforms per joint at `time` (composed `T·R·S`).
    pub fn pose_locals(&self, time: f32, skeleton: &Skeleton) -> Vec<Mat4> {
        let (t, r, s) = self.pose_trs(time, skeleton);
        (0..skeleton.joint_count())
            .map(|i| Mat4::from_scale_rotation_translation(s[i], r[i], t[i]))
            .collect()
    }

    /// Skinning matrices at `time` — `pose_locals` fed through the skeleton
    /// hierarchy. This is what the renderer uploads.
    pub fn skinning_matrices(&self, time: f32, skeleton: &Skeleton) -> Vec<Mat4> {
        skeleton.skinning_matrices(&self.pose_locals(time, skeleton))
    }
}

/// Load a clip-only GLB and bind its channels to `skeleton`.
pub fn load(path: &str, skeleton: &Skeleton) -> Result<AnimationClip, String> {
    let raw = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let patched = patch_interpolation(&raw).map_err(|e| format!("{path}: {e}"))?;
    let (doc, buffers, _images) =
        gltf::import_slice(&patched).map_err(|e| format!("gltf import {path}: {e}"))?;

    let anim = doc
        .animations()
        .next()
        .ok_or_else(|| format!("{path}: no animation"))?;
    let name = anim.name().unwrap_or("clip").to_string();

    let mut channels: Vec<Channel> = Vec::new();
    let mut duration = 0.0_f32;

    for chan in anim.channels() {
        let target = chan.target().node();
        // Bind by bone name (shared-skeleton contract); fall back to node index.
        let joint = match target.name().and_then(|nm| skeleton.index_of(nm)) {
            Some(j) => j,
            None => {
                // Clips list `Armature` as node 0 then `Bone_1..`, so node i maps
                // to joint i-1. Use that as a fallback and warn — a silent
                // mismatch would misanimate.
                let fallback = target.index().checked_sub(1);
                match fallback.filter(|&j| j < skeleton.joint_count()) {
                    Some(j) => {
                        log::warn!(
                            "{path}: channel node {:?} (idx {}) not name-matched; using joint {j}",
                            target.name(),
                            target.index()
                        );
                        j
                    }
                    None => {
                        log::warn!(
                            "{path}: channel node {:?} (idx {}) unbindable; skipped",
                            target.name(),
                            target.index()
                        );
                        continue;
                    }
                }
            }
        };

        let interp = match chan.sampler().interpolation() {
            gltf::animation::Interpolation::Linear => Interp::Linear,
            gltf::animation::Interpolation::Step => Interp::Step,
            gltf::animation::Interpolation::CubicSpline => Interp::CubicSpline,
        };

        let reader = chan.reader(|b| Some(&buffers[b.index()]));
        let times: Vec<f32> = reader
            .read_inputs()
            .ok_or_else(|| format!("{path}: channel has no input times"))?
            .collect();
        if let Some(&last) = times.last() {
            duration = duration.max(last);
        }

        use gltf::animation::util::ReadOutputs;
        let track = match reader
            .read_outputs()
            .ok_or_else(|| format!("{path}: channel has no outputs"))?
        {
            ReadOutputs::Rotations(r) => {
                Track::Rotation(r.into_f32().map(Quat::from_array).collect())
            }
            ReadOutputs::Translations(t) => Track::Translation(t.map(Vec3::from).collect()),
            ReadOutputs::Scales(s) => Track::Scale(s.map(Vec3::from).collect()),
            ReadOutputs::MorphTargetWeights(_) => continue, // no morph targets on these rigs
        };

        channels.push(Channel {
            joint,
            interp,
            times,
            track,
        });
    }

    if channels.is_empty() {
        return Err(format!("{path}: no bindable channels"));
    }
    Ok(AnimationClip {
        name,
        duration,
        channels,
    })
}

/// Rewrite a GLB's JSON chunk so animation-sampler `interpolation` integers
/// (`9729`=`GL_LINEAR`, `9728`=`GL_NEAREST`) become the spec strings the `gltf`
/// crate accepts. The BIN chunk is copied verbatim.
fn patch_interpolation(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() < 12 {
        return Err("GLB too short".into());
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != GLB_MAGIC {
        return Err("not a GLB (bad magic)".into());
    }

    // Split into (chunk_type, chunk_data) pairs.
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut off = 12;
    while off + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        let ctype = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
        let start = off + 8;
        let end = start + len;
        if end > bytes.len() {
            return Err("GLB chunk overruns file".into());
        }
        chunks.push((ctype, bytes[start..end].to_vec()));
        off = end;
    }

    let json_idx = chunks
        .iter()
        .position(|(t, _)| *t == CHUNK_JSON)
        .ok_or("GLB has no JSON chunk")?;

    let mut json: serde_json::Value = serde_json::from_slice(&chunks[json_idx].1)
        .map_err(|e| format!("GLB JSON parse: {e}"))?;
    if let Some(anims) = json.get_mut("animations").and_then(|a| a.as_array_mut()) {
        for anim in anims.iter_mut() {
            if let Some(samplers) = anim.get_mut("samplers").and_then(|s| s.as_array_mut()) {
                for s in samplers.iter_mut() {
                    if let Some(interp) = s.get_mut("interpolation") {
                        if let Some(code) = interp.as_u64() {
                            let name = match code {
                                9728 => "STEP",   // GL_NEAREST
                                9729 => "LINEAR", // GL_LINEAR
                                other => {
                                    log::warn!(
                                        "unknown interpolation code {other}; treating as LINEAR"
                                    );
                                    "LINEAR"
                                }
                            };
                            *interp = serde_json::Value::String(name.into());
                        }
                    }
                }
            }
        }
    }

    let mut new_json = serde_json::to_vec(&json).map_err(|e| format!("GLB JSON reserialize: {e}"))?;
    // glTF chunks are 4-byte aligned; the JSON chunk pads with spaces (0x20).
    while new_json.len() % 4 != 0 {
        new_json.push(b' ');
    }
    chunks[json_idx].1 = new_json;

    // Reassemble: 12-byte header + each [len][type][data].
    let body_len: usize = chunks.iter().map(|(_, d)| 8 + d.len()).sum();
    let total = 12 + body_len;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes()); // glTF version 2
    out.extend_from_slice(&(total as u32).to_le_bytes());
    for (ctype, data) in &chunks {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctype.to_le_bytes());
        out.extend_from_slice(data);
    }
    Ok(out)
}

// ── Sampling ────────────────────────────────────────────────────────────────

/// Locate the keyframe span bracketing `t`: returns `(i0, i1, alpha)` where
/// `alpha ∈ [0,1]` interpolates between keyframes `i0` and `i1`. Clamps at both
/// ends (`i0 == i1`, `alpha == 0`).
fn find_span(times: &[f32], t: f32) -> (usize, usize, f32) {
    let n = times.len();
    if n == 0 {
        return (0, 0, 0.0);
    }
    if t <= times[0] {
        return (0, 0, 0.0);
    }
    let last = n - 1;
    if t >= times[last] {
        return (last, last, 0.0);
    }
    // `partition_point` → count of keyframes with time <= t; minus 1 = i0.
    let i0 = times.partition_point(|&x| x <= t) - 1;
    let i1 = i0 + 1;
    let span = times[i1] - times[i0];
    let alpha = if span > 1e-9 { (t - times[i0]) / span } else { 0.0 };
    (i0, i1, alpha)
}

/// Keyframe value index for `k`, accounting for the CubicSpline 3-per-key layout
/// (in-tangent, value, out-tangent) — the *value* is the middle element.
fn value_index(interp: Interp, k: usize) -> usize {
    match interp {
        Interp::CubicSpline => 3 * k + 1,
        _ => k,
    }
}

fn sample_quat(times: &[f32], v: &[Quat], interp: Interp, t: f32) -> Quat {
    let (i0, i1, a) = find_span(times, t);
    match interp {
        Interp::Step => v[value_index(interp, i0)],
        Interp::Linear => {
            if i0 == i1 {
                v[i0]
            } else {
                v[i0].slerp(v[i1], a).normalize()
            }
        }
        Interp::CubicSpline => {
            if i0 == i1 {
                v[3 * i0 + 1]
            } else {
                let dt = times[i1] - times[i0];
                let p0 = v[3 * i0 + 1];
                let m0 = v[3 * i0 + 2]; // out-tangent of i0
                let p1 = v[3 * i1 + 1];
                let m1 = v[3 * i1]; // in-tangent of i1
                let q = hermite_vec4(
                    glam::Vec4::from(p0.to_array()),
                    glam::Vec4::from(m0.to_array()),
                    glam::Vec4::from(p1.to_array()),
                    glam::Vec4::from(m1.to_array()),
                    a,
                    dt,
                );
                Quat::from_array(q.to_array()).normalize()
            }
        }
    }
}

fn sample_vec3(times: &[f32], v: &[Vec3], interp: Interp, t: f32) -> Vec3 {
    let (i0, i1, a) = find_span(times, t);
    match interp {
        Interp::Step => v[value_index(interp, i0)],
        Interp::Linear => {
            if i0 == i1 {
                v[i0]
            } else {
                v[i0].lerp(v[i1], a)
            }
        }
        Interp::CubicSpline => {
            if i0 == i1 {
                v[3 * i0 + 1]
            } else {
                let dt = times[i1] - times[i0];
                let p0 = v[3 * i0 + 1];
                let m0 = v[3 * i0 + 2];
                let p1 = v[3 * i1 + 1];
                let m1 = v[3 * i1];
                let r = hermite_vec4(
                    glam::Vec4::new(p0.x, p0.y, p0.z, 0.0),
                    glam::Vec4::new(m0.x, m0.y, m0.z, 0.0),
                    glam::Vec4::new(p1.x, p1.y, p1.z, 0.0),
                    glam::Vec4::new(m1.x, m1.y, m1.z, 0.0),
                    a,
                    dt,
                );
                Vec3::new(r.x, r.y, r.z)
            }
        }
    }
}

/// Cubic Hermite spline (glTF CUBICSPLINE): tangents are scaled by the span
/// `dt` per the spec. Operates component-wise on a Vec4 (covers Vec3 with w=0
/// and quaternions before renormalization).
fn hermite_vec4(
    p0: glam::Vec4,
    m0: glam::Vec4,
    p1: glam::Vec4,
    m1: glam::Vec4,
    a: f32,
    dt: f32,
) -> glam::Vec4 {
    let a2 = a * a;
    let a3 = a2 * a;
    let h00 = 2.0 * a3 - 3.0 * a2 + 1.0;
    let h10 = a3 - 2.0 * a2 + a;
    let h01 = -2.0 * a3 + 3.0 * a2;
    let h11 = a3 - a2;
    p0 * h00 + m0 * (h10 * dt) + p1 * h01 + m1 * (h11 * dt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_path(name: &str) -> String {
        format!(
            "{}/../../assets/enemies/animations/{name}",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    fn karl() -> crate::skeletal::gltf_skin::SkinnedModel {
        let p = format!(
            "{}/../../assets/enemies/characters/russian-guard_karl.glb",
            env!("CARGO_MANIFEST_DIR")
        );
        crate::skeletal::gltf_skin::load(&p).expect("load karl")
    }

    /// The malformed idle clip loads via the JSON-chunk patch, binds all 15
    /// rotation channels to the shared skeleton by name, and has a sane duration.
    #[test]
    fn loads_idle_clip_and_binds_all_bones() {
        let m = karl();
        let clip = load(&clip_path("00-idle.glb"), &m.skeleton).expect("load idle");
        assert_eq!(clip.name, "Idle");
        assert_eq!(clip.bound_channels(), 15, "all 15 bones bound by name");
        assert!(clip.duration > 0.0, "duration {}", clip.duration);
    }

    /// Sampling at t=0 gives a pose whose joint matrices differ from the bind
    /// pose (the idle is not the rest pose) yet stay finite and well-formed —
    /// the headless half of the "idle animates" oracle.
    #[test]
    fn idle_pose_is_finite_and_moves_off_bind() {
        let m = karl();
        let clip = load(&clip_path("00-idle.glb"), &m.skeleton).expect("load idle");
        let mats = clip.skinning_matrices(0.0, &m.skeleton);
        assert_eq!(mats.len(), 15);
        let mut moved = false;
        for mat in &mats {
            for c in mat.to_cols_array() {
                assert!(c.is_finite(), "non-finite matrix element");
            }
            if (*mat - Mat4::IDENTITY).to_cols_array().iter().any(|v| v.abs() > 1e-3) {
                moved = true;
            }
        }
        assert!(moved, "idle pose should differ from the bind (identity) pose");
    }

    /// The three locomotion clips all load and bind.
    #[test]
    fn all_locomotion_clips_load() {
        let m = karl();
        for f in ["28-walking.glb", "2A-jogging.glb", "29-running.glb"] {
            let clip = load(&clip_path(f), &m.skeleton).expect(f);
            assert_eq!(clip.bound_channels(), 15, "{f} bound all bones");
            assert!(clip.duration > 0.0, "{f} duration");
        }
    }

    /// Seating oracle: the idle pose's lowest skinned point sits *below* the
    /// bind-pose AABB minimum — i.e. the standing legs drop below the splayed
    /// bind star, which is exactly why seating must use the posed min, not the
    /// bind AABB (the "waist-deep" bug). Also confirms it's finite.
    #[test]
    fn idle_pose_sinks_below_the_bind_aabb() {
        let m = karl();
        let clip = load(&clip_path("00-idle.glb"), &m.skeleton).expect("load idle");
        let mut posed_min = f32::INFINITY;
        for i in 0..24 {
            let t = clip.duration * i as f32 / 24.0;
            let mats = clip.skinning_matrices(t, &m.skeleton);
            posed_min = posed_min.min(m.skinned_min_y(&mats));
        }
        assert!(posed_min.is_finite(), "posed min y not finite");
        assert!(
            posed_min < m.bounds_min.y + 1e-3,
            "idle feet ({posed_min}) should reach at/below the bind AABB min ({}) — \
             seating by the bind AABB would leave the character sunk",
            m.bounds_min.y
        );
    }

    #[test]
    fn find_span_clamps_and_interpolates() {
        let times = [0.0, 1.0, 2.0];
        assert_eq!(find_span(&times, -5.0), (0, 0, 0.0));
        assert_eq!(find_span(&times, 5.0), (2, 2, 0.0));
        let (i0, i1, a) = find_span(&times, 1.5);
        assert_eq!((i0, i1), (1, 2));
        assert!((a - 0.5).abs() < 1e-6);
    }

    #[test]
    fn linear_quat_sampling_is_the_midpoint_slerp() {
        let times = [0.0, 1.0];
        let q0 = Quat::IDENTITY;
        let q1 = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let v = [q0, q1];
        let mid = sample_quat(&times, &v, Interp::Linear, 0.5);
        let expect = q0.slerp(q1, 0.5).normalize();
        assert!(mid.abs_diff_eq(expect, 1e-6), "got {mid:?} expected {expect:?}");
        // STEP holds the left keyframe.
        assert!(sample_quat(&times, &v, Interp::Step, 0.5).abs_diff_eq(q0, 1e-6));
    }

    #[test]
    fn linear_vec3_sampling_is_the_lerp() {
        let times = [0.0, 2.0];
        let v = [Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0)];
        let mid = sample_vec3(&times, &v, Interp::Linear, 1.0);
        assert!((mid - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-6, "got {mid:?}");
    }
}
