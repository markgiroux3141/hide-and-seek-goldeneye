//! Explosive projectiles + radius-falloff detonation — the shared core behind the
//! Rocket Launcher, Grenade Launcher, and Hand Grenade.
//!
//! **There is no 3DS FPS oracle for any of this** — the JS shipped the weapon GLBs
//! but never wired a projectile or explosion system (its only "explosion" was a
//! cosmetic prop flash). So this is authored fresh, tuned for the GoldenEye feel.
//!
//! The three projectile weapons are ONE simulation ([`Projectile`]) differing only
//! in [`ProjectileSpec`] data (speed / gravity / loft / fuse / bounce). The math
//! that needs no world access — velocity + gravity integration, fuse expiry, and
//! blast falloff — lives here and is unit-tested; the surface-collision raycast and
//! the actual damage application need the physics/enemy state and stay in
//! `world::combat` (which calls into [`falloff_damage`]).

use glam::Vec3;

use super::config::{Explosion, ProjectileSpec};

/// A live explosive round in flight. Spawned along the aim (with any launch loft),
/// integrated each frame under its spec's gravity, and detonated by `world::combat`
/// on a surface contact and/or when [`Self::fuse_expired`] trips.
#[derive(Clone, Copy, Debug)]
pub struct Projectile {
    /// World-space position (metres).
    pub pos: Vec3,
    /// World-space velocity (m/s).
    pub vel: Vec3,
    /// The tuning it was fired with (gravity / fuse / bounce / explosion).
    pub spec: ProjectileSpec,
    /// Seconds alive, accumulated for the fuse check.
    pub age: f32,
    /// A bouncer that has settled onto a surface: it stops integrating (no more
    /// gravity/bounce) and just waits out its fuse in place — otherwise discrete
    /// restitution bounces never truly rest and it jitters forever.
    pub at_rest: bool,
}

impl Projectile {
    /// Spawn a projectile at `origin` firing along `dir` (need not be normalized),
    /// applying the spec's launch `speed` and upward `loft`. `up` is the world up
    /// the loft is added along (usually `Vec3::Y`).
    pub fn spawn(origin: Vec3, dir: Vec3, up: Vec3, spec: ProjectileSpec) -> Self {
        let d = dir.normalize_or_zero();
        let vel = d * spec.speed + up.normalize_or_zero() * spec.loft;
        Projectile { pos: origin, vel, spec, age: 0.0, at_rest: false }
    }

    /// Advance one frame: integrate gravity into the velocity, then the velocity
    /// into the position, and age the fuse. Returns the segment the projectile
    /// traveled this frame as `(from, to)` so the caller can sweep it against the
    /// world for a contact (a fast rocket can cross a wall within one dt, so a
    /// point test would tunnel).
    pub fn advance(&mut self, dt: f32) -> (Vec3, Vec3) {
        let from = self.pos;
        // Semi-implicit Euler: gravity → velocity → position (stable enough here).
        self.vel.y -= self.spec.gravity * dt;
        self.pos += self.vel * dt;
        self.age += dt;
        (from, self.pos)
    }

    /// Whether the fuse has burned out (only meaningful when the spec has a fuse).
    /// A fuseless projectile (rocket) never self-detonates — it waits for contact.
    pub fn fuse_expired(&self) -> bool {
        matches!(self.spec.fuse, Some(t) if self.age >= t)
    }

    /// Reflect the velocity off a surface `normal` on a bounce, scaling the whole
    /// reflected vector by the spec's restitution (energy lost, incl. tangential —
    /// so a grenade skids to a stop rather than sliding forever). The caller places
    /// `pos` just off the surface so the next sweep doesn't re-hit it.
    pub fn bounce_off(&mut self, normal: Vec3) {
        let n = normal.normalize_or_zero();
        // v' = (v - 2(v·n)n) · restitution
        let reflected = self.vel - 2.0 * self.vel.dot(n) * n;
        self.vel = reflected * self.spec.bounce;
    }

    /// Settle onto a surface: snap just off it, zero the velocity, and latch
    /// [`Self::at_rest`] so the sim stops integrating it — it now only waits out its
    /// fuse. Called when a bounce is too gentle to matter.
    pub fn come_to_rest(&mut self, surface: Vec3, normal: Vec3) {
        self.pos = surface + normal.normalize_or_zero() * 0.02;
        self.vel = Vec3::ZERO;
        self.at_rest = true;
    }
}

/// Damage dealt by an [`Explosion`] to an actor `dist` metres from the blast centre:
/// linear falloff from `max_damage` at the centre to 0 at (and beyond) `radius`.
/// A GoldenEye blast is lethal point-blank and survivable at the rim.
pub fn falloff_damage(explosion: &Explosion, dist: f32) -> f32 {
    if dist >= explosion.radius {
        return 0.0;
    }
    explosion.max_damage * (1.0 - dist / explosion.radius.max(1e-6))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::config;

    fn rocket_spec() -> ProjectileSpec {
        match config::ROCKET_LAUNCHER.fire_kind {
            crate::combat::config::FireKind::Projectile(p) => p,
            _ => unreachable!("rocket launcher is a projectile"),
        }
    }

    /// A rocket (no gravity) flies dead straight: after 1 s at 40 m/s it's 40 m out
    /// along the aim with no vertical drop.
    #[test]
    fn rocket_flies_straight() {
        let mut p = Projectile::spawn(Vec3::ZERO, -Vec3::Z, Vec3::Y, rocket_spec());
        for _ in 0..100 {
            p.advance(0.01);
        }
        assert!((p.pos.z - -40.0).abs() < 0.5, "≈40 m down -Z: {}", p.pos.z);
        assert!(p.pos.y.abs() < 1e-3, "no vertical drop without gravity: {}", p.pos.y);
    }

    /// A lofted, gravity-bound projectile arcs: it rises then falls back below its
    /// launch height.
    #[test]
    fn grenade_arcs_under_gravity() {
        let spec = match config::GRENADE.fire_kind {
            crate::combat::config::FireKind::Projectile(p) => p,
            _ => unreachable!(),
        };
        let mut p = Projectile::spawn(Vec3::new(0.0, 1.0, 0.0), -Vec3::Z, Vec3::Y, spec);
        let mut max_y: f32 = p.pos.y;
        for _ in 0..300 {
            p.advance(0.01);
            max_y = max_y.max(p.pos.y);
        }
        assert!(max_y > 1.0, "loft carried it above the launch height: {max_y}");
        assert!(p.pos.y < max_y, "then gravity pulled it back down");
    }

    /// The fuse trips only after the spec's fuse time; a fuseless rocket never does.
    #[test]
    fn fuse_expires_on_time() {
        let mut g = Projectile::spawn(Vec3::ZERO, -Vec3::Z, Vec3::Y, {
            match config::GRENADE.fire_kind {
                crate::combat::config::FireKind::Projectile(p) => p,
                _ => unreachable!(),
            }
        });
        // Grenade fuse is 3.5 s.
        for _ in 0..340 {
            g.advance(0.01);
        }
        assert!(!g.fuse_expired(), "not yet at 3.4 s");
        for _ in 0..20 {
            g.advance(0.01);
        }
        assert!(g.fuse_expired(), "expired past 3.5 s");

        let mut r = Projectile::spawn(Vec3::ZERO, -Vec3::Z, Vec3::Y, rocket_spec());
        for _ in 0..10000 {
            r.advance(0.01);
        }
        assert!(!r.fuse_expired(), "a fuseless rocket never self-detonates");
    }

    /// Falloff: full damage at the centre, zero at/after the radius, linear between.
    #[test]
    fn blast_falloff_is_linear() {
        let e = Explosion { radius: 5.0, max_damage: 200.0 };
        assert_eq!(falloff_damage(&e, 0.0), 200.0, "max at centre");
        assert!((falloff_damage(&e, 2.5) - 100.0).abs() < 1e-3, "half at half-radius");
        assert_eq!(falloff_damage(&e, 5.0), 0.0, "zero at the rim");
        assert_eq!(falloff_damage(&e, 9.0), 0.0, "zero beyond the rim");
    }

    /// A bounce reflects the velocity off the surface normal and sheds energy per
    /// the restitution (grenade bounce = 0.4).
    #[test]
    fn bounce_reflects_and_damps() {
        let spec = match config::GRENADE.fire_kind {
            crate::combat::config::FireKind::Projectile(p) => p,
            _ => unreachable!(),
        };
        let mut p = Projectile::spawn(Vec3::ZERO, Vec3::new(0.0, -1.0, 0.0), Vec3::Y, spec);
        p.vel = Vec3::new(0.0, -10.0, 0.0); // straight down at 10 m/s
        let speed_before = p.vel.length();
        p.bounce_off(Vec3::Y); // bounce off a floor
        assert!(p.vel.y > 0.0, "now moving upward after the floor bounce");
        assert!(
            (p.vel.length() - speed_before * spec.bounce).abs() < 1e-3,
            "speed scaled by restitution {}",
            spec.bounce
        );
    }
}
