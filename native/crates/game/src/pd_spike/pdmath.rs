//! Perfect Dark's angle macros and random number generator, verbatim.
//!
//! PD's `M_BADPI` is `3.141092641` — wrong in the fourth decimal — and most of the
//! bot code wraps angles at `BADDTOR(360)` (≈ 6.28218) rather than 2π, while
//! `atan2f` returns true radians. That mismatch is part of how PD behaves (it is
//! a 0.03° seam), so it is kept rather than "fixed": every constant here is PD's.

/// `M_BADPI` (`math.h:5`).
pub const M_BADPI: f32 = 3.141_092_641;
/// `M_BADTAU`.
pub const M_BADTAU: f32 = M_BADPI * 2.0;

/// `BADDTOR(deg)` = `deg * M_BADPI / 180`.
#[inline]
pub fn baddtor(deg: f32) -> f32 {
    deg * M_BADPI / 180.0
}

/// `BADDTOR2(deg)` = `deg * (M_BADPI / 180)` — same value, different rounding in C.
#[inline]
pub fn baddtor2(deg: f32) -> f32 {
    deg * (M_BADPI / 180.0)
}

/// `DTOR(deg)` — a true degree conversion.
#[inline]
pub fn dtor(deg: f32) -> f32 {
    deg * std::f32::consts::PI / 180.0
}

/// A full turn in PD's wrapping unit.
pub fn turn() -> f32 {
    baddtor(360.0)
}

/// Wrap into `[0, BADDTOR(360))` the way PD's `while` loops do.
pub fn wrap_pos(mut a: f32) -> f32 {
    let t = turn();
    while a >= t {
        a -= t;
    }
    while a < 0.0 {
        a += t;
    }
    a
}

/// PD's `random()` (`lib/rng_c.c:13`): a 64-bit shift/xor generator whose low 32
/// bits are the result.
#[derive(Clone, Debug)]
pub struct Rng {
    pub seed: u64,
}

impl Rng {
    /// `rng_set_seed`: PD adds 1 so the seed is never zero.
    pub fn new(seed: u64) -> Self {
        Rng { seed: seed.wrapping_add(1) }
    }

    /// `random()`.
    pub fn random(&mut self) -> u32 {
        let s = self.seed;
        // C precedence: shifts, then `^`, then `|` — i.e. `A | (B ^ C)`.
        let s = ((s << 63) >> 31) | (((s << 31) >> 32) ^ ((s << 44) >> 32));
        let s = ((s >> 20) & 0xfff) ^ s;
        self.seed = s;
        s as u32
    }

    /// `rng_rotate_seed` (`lib/rng_c.c:35`): `random()`'s step applied to a seed the
    /// caller owns. Same precedence trap: `A | B ^ C` is `A | (B ^ C)`.
    pub fn rotate_seed(seed: &mut u64) -> u32 {
        let s = *seed;
        let s = ((s << 63) >> 31) | (((s << 31) >> 32) ^ ((s << 44) >> 32));
        let s = ((s >> 20) & 0xfff) ^ s;
        *seed = s;
        s as u32
    }

    /// `RANDOMFRAC()` = `random() * (1.0f / U32_MAX)`.
    pub fn randomfrac(&mut self) -> f32 {
        self.random() as f32 * (1.0 / u32::MAX as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rng_is_deterministic_and_spreads() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let xs: Vec<u32> = (0..100).map(|_| a.random()).collect();
        let ys: Vec<u32> = (0..100).map(|_| b.random()).collect();
        assert_eq!(xs, ys);
        let mean = (0..10_000).map(|_| a.randomfrac()).sum::<f32>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.03, "{mean}");
    }
}
