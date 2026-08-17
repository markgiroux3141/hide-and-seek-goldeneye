//! **Per-shot bullet spread** — Perfect Dark's `bgun_calculate_bot_shot_spread`
//! (`bondgun.c:5142`), the second half of "how often does a bullet actually land".
//!
//! The [zeroing model](crate::pdsim::zeroing) explains where a simulant's *body* is
//! pointing. It is a slow, damped random walk — the increment is held for a third to
//! two thirds of a second — so it is essentially **constant across a burst**. On its
//! own it therefore produces an all-or-nothing outcome: while the walk sits on you
//! every round in the magazine connects, and while it sits off you none do.
//!
//! Perfect Dark does not have that problem because every individual bullet also gets
//! an independent two-axis offset from this function, re-rolled per shot. A burst is a
//! *pattern*, not one ray fired repeatedly, and the weapon's `spread` field decides how
//! wide that pattern is. Note which weapons carry the big numbers: it is the
//! **automatics**. A Falcon 2 is `1` and a DY357 Magnum, the Sniper Rifle and the Laser
//! are `0`; the CMP150 and Callisto are `9`, the AR34 `8`, the Shotgun `30`, the Reaper
//! `56`. Precision weapons ride on the zeroing model alone; hosers do not.
//!
//! # Deriving the angle
//!
//! PD works in screen space and we work in radians, so the conversion is worth writing
//! down. `bgun_calculate_bot_shot_spread` computes
//!
//! ```text
//! radius = 120 * spread / fov_y                       // fov_y is 60° (lib/vi.c:35)
//! x      = (RANDOMFRAC() - 0.5) * RANDOMFRAC() * radius
//! ```
//!
//! and displaces the shot vector by `x` in the same units the player's crosshair uses —
//! screen pixels on a 240-line frame. 120 is the screen *half*-height, so
//! `radius = spread * (halfheight / fov_y) = 2 * spread` pixels, and the frame covers
//! `fov_y` over its full 240 lines, i.e. 4 pixels per degree. The angular offset per
//! axis is therefore
//!
//! ```text
//! offset_deg = randfactor * 2 * spread / 4 = randfactor * spread / 2
//! ```
//!
//! with `randfactor = (U − 0.5) · U ∈ [−0.5, 0.5]`. So a weapon's `spread` field reads
//! as **±spread/4 degrees of worst-case error per axis**, RMS ≈ `spread/12` degrees.
//!
//! # The distribution matters
//!
//! `randfactor` is a *product of two uniforms*, not one. That makes it sharply
//! centre-weighted — its RMS is 0.167 against a 0.5 maximum — so most rounds land near
//! the middle of the cone and the occasional one flies wide. A uniform disc would feel
//! quite different: it would spray evenly and never produce the "mostly on target, then
//! one round misses" texture that makes an automatic survivable but still threatening.

/// PD's vertical field of view in degrees (`lib/vi.c:35`) — the denominator that turns
/// a weapon's `spread` field into an on-screen radius.
const FOV_Y_DEG: f32 = 60.0;
/// The screen half-height PD scales spread by (`120.0f` in the source, a 240-line frame).
const HALF_HEIGHT_PX: f32 = 120.0;
/// Pixels per degree on that frame: the full 240 lines cover `FOV_Y_DEG`.
const PX_PER_DEG: f32 = 2.0 * HALF_HEIGHT_PX / FOV_Y_DEG;

/// Dual-wielding widens the cone (`spread *= 1.5f`) — one of PD's three modifiers.
const DUAL_MULT: f32 = 1.5;

/// PD's `spread` field for each kind of weapon a hunter can carry, taken from the
/// `funcdef_shoot` rows in `invitems.c`. Our arsenal is GoldenEye's, so these are
/// matched by *role* rather than by name — the point is preserving the shape of the
/// table (hosers scatter, precision weapons do not), not a one-to-one gun mapping.
pub mod table {
    /// Falcon 2 (`invitems.c:496`) — a service pistol barely scatters at all.
    pub const PISTOL: f32 = 1.0;
    /// AR34 (`invitems.c:2107`) — the assault-rifle baseline.
    pub const RIFLE: f32 = 8.0;
    /// CMP150 / Callisto NTG (`invitems.c:1369`, `1717`) — the widest of the
    /// bullet-hose class, and the reason an SMG is survivable at range.
    pub const SMG: f32 = 9.0;
    /// PD's Shotgun (`invitems.c:2517`) — a genuine cone.
    pub const SHOTGUN: f32 = 30.0;
    /// Sniper Rifle / Laser (`invitems.c:4039`, `4122`) — **zero**. These ride on the
    /// zeroing model alone, exactly as PD intends: a marksman's weapon adds no error
    /// of its own, so its accuracy is purely a statement about the shooter's tier.
    pub const PRECISION: f32 = 0.0;
}

/// One shot's angular offset, in **radians**, as `(yaw, pitch)`.
///
/// `spread` is the weapon's PD `spread` field (see [`table`]); `dual` applies PD's
/// dual-wield widening. `u` is four uniform `[0, 1)` samples — passed in rather than
/// drawn here so the caller keeps a single deterministic RNG and the function stays
/// pure (and therefore testable).
///
/// PD also halves the cone while squatting (`CROUCHPOS_SQUAT`) and quarters it for the
/// first round of a burst from a weapon flagged `INVAIMFLAG_ACCURATESINGLESHOT`.
/// Neither is ported: our hunters have no crouch posture and no per-weapon aim flags.
pub fn shot_offset(spread: f32, dual: bool, u: [f32; 4]) -> (f32, f32) {
    let spread = if dual { spread * DUAL_MULT } else { spread };
    if spread <= 0.0 {
        return (0.0, 0.0);
    }
    let radius_px = HALF_HEIGHT_PX * spread / FOV_Y_DEG;
    // `(U − 0.5) · U` — the centre-weighted product of two uniforms, per axis.
    let axis = |a: f32, b: f32| ((a - 0.5) * b * radius_px / PX_PER_DEG).to_radians();
    (axis(u[0], u[1]), axis(u[2], u[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented conversion: a weapon's `spread` field is `±spread/4` degrees of
    /// worst-case error per axis. Drive the two uniforms to their extremes and check
    /// the bound is what the derivation in the module docs says it is.
    #[test]
    fn spread_field_reads_as_a_quarter_degree_per_point() {
        for spread in [1.0, 8.0, 9.0, 30.0] {
            let (yaw, _) = shot_offset(spread, false, [1.0, 1.0, 0.5, 0.0]);
            let worst = spread / 4.0;
            assert!(
                (yaw.to_degrees() - worst).abs() < 1e-4,
                "spread {spread} should peak at {worst}°, got {}°",
                yaw.to_degrees()
            );
        }
    }

    /// A precision weapon (sniper / laser, `spread == 0`) adds no error whatever the
    /// rolls — its accuracy is entirely the shooter's tier, which is PD's intent.
    #[test]
    fn a_precision_weapon_adds_no_error() {
        let (y, p) = shot_offset(table::PRECISION, false, [0.99, 0.99, 0.01, 0.99]);
        assert_eq!((y, p), (0.0, 0.0));
    }

    /// Dual-wielding widens the cone by half again (PD's `spread *= 1.5f`).
    #[test]
    fn dual_wielding_widens_the_cone() {
        let u = [1.0, 1.0, 1.0, 1.0];
        let single = shot_offset(table::RIFLE, false, u).0;
        let dual = shot_offset(table::RIFLE, true, u).0;
        assert!((dual / single - DUAL_MULT).abs() < 1e-5, "dual should be 1.5x, got {}", dual / single);
    }

    /// The load-bearing statistical property: `(U − 0.5)·U` is centre-weighted. Its RMS
    /// is 0.167 against a 0.5 maximum — **a third of the peak**, where a uniform over
    /// the same range would give 0.289, well over half. That gap is what makes most
    /// rounds land near the middle of the cone with the occasional one flying wide,
    /// instead of an even spray across it.
    #[test]
    fn the_distribution_is_centre_weighted_not_uniform() {
        // Deterministic LCG so the test is reproducible.
        let mut s: u64 = 0xC0FFEE_1234_5678;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f32) / ((1u64 << 31) as f32)
        };
        let (mut sum_sq, n) = (0.0f64, 20_000);
        let mut worst = 0.0f32;
        for _ in 0..n {
            let (yaw, _) = shot_offset(table::RIFLE, false, [next(), next(), next(), next()]);
            let d = yaw.to_degrees();
            sum_sq += (d * d) as f64;
            worst = worst.max(d.abs());
        }
        let rms = (sum_sq / n as f64).sqrt() as f32;
        let peak = table::RIFLE / 4.0; // 2.0° for the rifle
        let ratio = rms / peak;
        // ~1/3 for the product of two uniforms; a uniform would sit near 0.577.
        assert!(
            (0.30..0.37).contains(&ratio),
            "RMS/peak should be ~0.33 (centre-weighted, vs 0.58 uniform), got {ratio:.3}"
        );
        assert!(worst <= peak + 1e-3, "no sample may exceed the peak {peak}°, got {worst}°");
    }
}
