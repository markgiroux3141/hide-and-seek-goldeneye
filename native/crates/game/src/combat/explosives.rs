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

use super::config::{Explosion, MineSpec, MineTrigger, ProjectileSpec};

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

/// Thrown-mine launch tuning (no oracle — authored for feel). A brisk toss with light
/// gravity gives a short, visible throw that sticks to the first surface it hits, so
/// you plant on a wall/floor/ceiling by aiming at it. A touch of loft sells the throw
/// without making level aim miss low.
const THROW_SPEED: f32 = 18.0;
const THROW_GRAVITY: f32 = 7.0;
const THROW_LOFT: f32 = 1.0;

/// A mine — first thrown along the aim (visible, tumbling), then **stuck** to the
/// first surface it contacts (wall/floor/ceiling). Once stuck it arms after
/// [`MineSpec::arm_time`] (can't be tripped while arming — your window to walk clear),
/// then waits for its [`MineTrigger`]: a proximity mine detonates when any living
/// actor enters its trip radius, a timed mine after its countdown, a remote mine only
/// when the player triggers a detonation. The flight/trip/arm math that needs no
/// world access lives here and is unit-tested; the flight collision sweep, the
/// actor-distance scan, and the actual detonation stay in `world::combat`.
#[derive(Clone, Copy, Debug)]
pub struct Mine {
    /// World position — in flight, the tumbling round; once stuck, sat just off its
    /// surface.
    pub pos: Vec3,
    /// World velocity while in flight (zeroed once stuck).
    pub vel: Vec3,
    /// Surface normal it's stuck to (drives the stuck render orientation). `Y` until
    /// it sticks.
    pub normal: Vec3,
    /// The tuning it was thrown with (trigger / arm time / explosion).
    pub spec: MineSpec,
    /// `false` while thrown/flying, `true` once attached to a surface. Arming +
    /// tripping only happen while stuck.
    pub stuck: bool,
    /// Once true the mine is live and can be tripped (arm delay elapsed post-stick).
    pub armed: bool,
    /// Counts down from `spec.arm_time` to 0 (once stuck); at 0 the mine arms.
    pub arm_timer: f32,
    /// Timed-mine countdown (s), started once armed. Meaningless for the other
    /// triggers (left at its init value and never ticked).
    pub timer: f32,
    /// Seconds spent in flight, accumulated for the tumble render + a max-flight
    /// fallback stick.
    pub flight_time: f32,
    /// Weapon-library name of the GLB to render in world (e.g. `"Proximity Mine"`).
    pub model: &'static str,
}

impl Mine {
    /// A mine already stuck to a surface at `pos`/`normal`, disarmed with its arm
    /// timer primed (used for the max-flight fallback + unit tests). A timed mine's
    /// countdown is seeded here but only ticks once armed.
    pub fn new(pos: Vec3, normal: Vec3, spec: MineSpec, model: &'static str) -> Self {
        let timer = match spec.trigger {
            MineTrigger::Timed(secs) => secs,
            _ => 0.0,
        };
        Mine {
            pos,
            vel: Vec3::ZERO,
            normal,
            spec,
            stuck: true,
            armed: false,
            arm_timer: spec.arm_time,
            timer,
            flight_time: 0.0,
            model,
        }
    }

    /// Throw a mine from `origin` along `dir` (need not be normalized), with the
    /// launch speed + a little upward loft along `up`. It flies until it sticks (see
    /// [`Self::advance`] / [`Self::stick`]).
    pub fn throw(origin: Vec3, dir: Vec3, up: Vec3, spec: MineSpec, model: &'static str) -> Self {
        let d = dir.normalize_or_zero();
        let vel = d * THROW_SPEED + up.normalize_or_zero() * THROW_LOFT;
        let timer = match spec.trigger {
            MineTrigger::Timed(secs) => secs,
            _ => 0.0,
        };
        Mine {
            pos: origin,
            vel,
            normal: Vec3::Y,
            spec,
            stuck: false,
            armed: false,
            arm_timer: spec.arm_time,
            timer,
            flight_time: 0.0,
            model,
        }
    }

    /// Advance a thrown (not-yet-stuck) mine one frame: integrate gravity into the
    /// velocity, then into the position, and age the flight clock. Returns the segment
    /// traveled `(from, to)` so the caller can sweep it for a surface to stick to
    /// (a point test would tunnel a fast toss through a thin wall). No-op once stuck.
    pub fn advance(&mut self, dt: f32) -> (Vec3, Vec3) {
        let from = self.pos;
        if !self.stuck {
            self.vel.y -= THROW_GRAVITY * dt;
            self.pos += self.vel * dt;
            self.flight_time += dt;
        }
        (from, self.pos)
    }

    /// Stick the mine to a surface at `pos` (already sat just off it) with `normal`:
    /// zero the velocity and latch [`Self::stuck`] so it stops flying and its arm
    /// timer begins counting on the next [`Self::tick`].
    pub fn stick(&mut self, pos: Vec3, normal: Vec3) {
        self.pos = pos;
        self.normal = normal.normalize_or_zero();
        self.vel = Vec3::ZERO;
        self.stuck = true;
    }

    /// Advance a stuck mine one frame. While disarming, burns the arm timer and arms
    /// at 0 (returning `true` on the exact frame it goes live, so the caller can play
    /// the arm beep). Once armed, a timed mine's countdown ticks down. A mine still in
    /// flight never arms.
    pub fn tick(&mut self, dt: f32) -> bool {
        if !self.stuck {
            return false;
        }
        if !self.armed {
            self.arm_timer -= dt;
            if self.arm_timer <= 0.0 {
                self.armed = true;
                return true; // just armed this frame
            }
            return false;
        }
        if matches!(self.spec.trigger, MineTrigger::Timed(_)) {
            self.timer -= dt;
        }
        false
    }

    /// Whether an armed **proximity** mine should trip on an actor at `actor`: true
    /// when the actor is within the trigger's trip radius. Other triggers never trip
    /// on proximity.
    pub fn proximity_trips(&self, actor: Vec3) -> bool {
        if !self.armed {
            return false;
        }
        match self.spec.trigger {
            MineTrigger::Proximity(r) => self.pos.distance(actor) <= r,
            _ => false,
        }
    }

    /// Whether an armed **timed** mine's countdown has run out. Other triggers never
    /// expire on time.
    pub fn timed_expired(&self) -> bool {
        self.armed && matches!(self.spec.trigger, MineTrigger::Timed(_)) && self.timer <= 0.0
    }

    /// Whether this is a remote-triggered mine (set off by the Detonator).
    pub fn is_remote(&self) -> bool {
        matches!(self.spec.trigger, MineTrigger::Remote)
    }
}

/// Damage dealt by an [`Explosion`] to an actor `dist` metres from the blast centre.
///
/// Which model runs depends on [`use_pd_explosions`]: Perfect Dark's authored
/// falloff by default, the original authored-fresh linear sphere when switched off.
/// Both are kept because the GoldenEye explosives were tuned against the linear one
/// and the A/B is the only way to judge the change — the established pattern here.
pub fn falloff_damage(explosion: &Explosion, dist: f32) -> f32 {
    if use_pd_explosions() {
        pd_falloff_damage(explosion, dist)
    } else {
        linear_falloff_damage(explosion, dist)
    }
}

/// The original: linear falloff from `max_damage` at the centre to 0 at (and beyond)
/// `radius`. Authored fresh for the GoldenEye feel, with no oracle behind it.
pub fn linear_falloff_damage(explosion: &Explosion, dist: f32) -> f32 {
    if dist >= explosion.radius {
        return 0.0;
    }
    explosion.max_damage * (1.0 - dist / explosion.radius.max(1e-6))
}

// ─── Perfect Dark's explosion model ──────────────────────────────────────────
// `explosion_apply_damage` (`explosions.c:674`) — and it is not a sphere.
//
// For a character (`explosions.c:931-968`), given a blast at the origin and the
// axis distances to the victim:
//
//     frac = min(1-|dx|/R, 1-|dy|/R, 1-|dz|/R)      // per-axis box, take the min
//     frac = frac * frac                            // squared
//     damage = frac * type.damage * 8.0
//
// Three differences from ours, all of which change how a blast reads:
//
//  1. **Box, not sphere.** The min over axes means the falloff follows a cube's
//     inscribed profile, so a blast reaches further along an axis than diagonally.
//  2. **Squared.** Damage drops off much faster than linear — PD blasts are far
//     more lethal at the centre and far more survivable at the rim.
//  3. **No floor for characters.** The *object* path (`explosions.c:843`) has a
//     `frac*0.7 + 0.3` floor, so scenery always takes at least 30%; characters do
//     not get that. Easy to conflate; they are different formulas.

/// Environment kill-switch for the PD explosion model. `PD_EXPLOSIONS=0` restores
/// the original linear falloff for an A/B.
///
/// Read once and cached: this sits on the damage path, which runs per actor per
/// blast frame.
pub fn use_pd_explosions() -> bool {
    static CELL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CELL.get_or_init(|| {
        let off = matches!(
            std::env::var("PD_EXPLOSIONS").unwrap_or_default().trim(),
            "0" | "off" | "no" | "false"
        );
        if off {
            log::info!("explosions: original linear falloff (PD_EXPLOSIONS=0)");
        } else {
            log::info!("explosions: Perfect Dark model (squared per-axis box falloff)");
        }
        !off
    })
}

/// PD's character falloff, along one axis only.
///
/// A caller with a real offset vector should use [`pd_falloff_damage_axes`]; this
/// scalar form treats `dist` as the dominant axis, which is what our call sites
/// have (they pass a centre-to-actor distance). It is the same curve, just
/// evaluated with the worst case on one axis — so it never over-reports damage.
pub fn pd_falloff_damage(explosion: &Explosion, dist: f32) -> f32 {
    let r = explosion.radius.max(1e-6);
    if dist >= r {
        return 0.0;
    }
    let frac = 1.0 - dist / r;
    frac * frac * explosion.max_damage
}

/// PD's character falloff with a real per-axis offset — the faithful form.
///
/// `offset` is centre → victim in metres. Returns 0 outside the box, which is what
/// PD's own bounds test does before it gets here.
pub fn pd_falloff_damage_axes(explosion: &Explosion, offset: Vec3) -> f32 {
    let r = explosion.radius.max(1e-6);
    let (dx, dy, dz) = (offset.x.abs(), offset.y.abs(), offset.z.abs());
    if dx >= r || dy >= r || dz >= r {
        return 0.0;
    }
    let frac = (1.0 - dx / r).min(1.0 - dy / r).min(1.0 - dz / r);
    if frac <= 0.0 {
        return 0.0;
    }
    frac * frac * explosion.max_damage
}

/// PD's **object** falloff, which is a different formula from the character one and
/// worth keeping distinct: linear (not squared) and floored at 30%, so scenery
/// inside the blast always takes something (`explosions.c:843`).
pub fn pd_falloff_damage_object(explosion: &Explosion, offset: Vec3) -> f32 {
    let r = explosion.radius.max(1e-6);
    let (dx, dy, dz) = (offset.x.abs(), offset.y.abs(), offset.z.abs());
    if dx >= r || dy >= r || dz >= r {
        return 0.0;
    }
    let frac = (1.0 - dx / r).min(1.0 - dy / r).min(1.0 - dz / r);
    if frac <= 0.0 {
        return 0.0;
    }
    (frac * 0.7 + 0.3) * explosion.max_damage
}

/// A Perfect Dark blast as it **propagates** (`explosion_apply_damage`,
/// `explosions.c:674`).
///
/// This is the structural idea our single instantaneous sphere had no equivalent
/// for, and it is the reason chain reactions and "the blast caught up with me" read
/// the way they do in PD:
///
/// * The blast has TWO radii. `blast_radius` is the visible fireball;
///   `damage_radius` is the lethal volume, and it is up to 2x larger.
/// * The **first frame** applies the full `damage_radius` at once. Later frames
///   apply a radius growing from `blast_radius` toward `damage_radius` across the
///   blast's `duration`, at 5% strength per tick.
///
/// So a blast is one hard hit followed by a widening, weakening wake — not a single
/// sphere and not a slow expansion from nothing.
#[derive(Clone, Copy, Debug)]
pub struct Blast {
    /// The damage + lethal radius this blast came from.
    pub explosion: Explosion,
    /// The visible fireball radius (metres) the growth starts from.
    pub blast_radius: f32,
    /// Seconds the blast lives and grows over.
    pub duration: f32,
    /// Seconds elapsed.
    pub age: f32,
    /// Whether the first (full-radius, full-strength) frame has been applied.
    pub first_frame_done: bool,
}

/// Sustained-frame damage scale (`explosions.c:983`: `minfrac *= 0.05f * lvupdate60`).
/// A tick is 1/60 s, so this is the per-tick fraction.
const PD_SUSTAIN_SCALE: f32 = 0.05;

impl Blast {
    /// Start a blast from an [`Explosion`] plus its visible radius and duration —
    /// both of which come off a [`crate::combat::pd_weapons::PdExplosion`] row.
    pub fn new(explosion: Explosion, blast_radius: f32, duration: f32) -> Self {
        Blast {
            explosion,
            blast_radius,
            duration: duration.max(1e-3),
            age: 0.0,
            first_frame_done: false,
        }
    }

    /// The lethal radius right now.
    ///
    /// Full `damage_radius` on the first frame; afterwards it grows from
    /// `blast_radius` toward `damage_radius` in proportion to `age/duration`, capped.
    pub fn current_radius(&self) -> f32 {
        let full = self.explosion.radius;
        if !self.first_frame_done {
            return full;
        }
        let t = (self.age / self.duration).clamp(0.0, 1.0);
        (self.blast_radius + (full - self.blast_radius) * t).min(full)
    }

    /// Whether the blast has finished applying damage.
    pub fn finished(&self) -> bool {
        self.age >= self.duration
    }

    /// Damage to an actor at `offset` from the centre this frame, and advance the
    /// blast by `dt`.
    ///
    /// The first call is the hard hit (full radius, full strength); later calls are
    /// the weakening wake. `dt` scales the sustained contribution so the total does
    /// not depend on frame rate.
    pub fn damage_at(&self, offset: Vec3, dt: f32) -> f32 {
        let r = self.current_radius();
        let scaled = Explosion { radius: r, max_damage: self.explosion.max_damage };
        let base = pd_falloff_damage_axes(&scaled, offset);
        if !self.first_frame_done {
            base
        } else {
            base * PD_SUSTAIN_SCALE * (dt * 60.0)
        }
    }

    /// Advance the blast one frame, latching the first-frame flag.
    pub fn advance(&mut self, dt: f32) {
        self.first_frame_done = true;
        self.age += dt;
    }
}

#[cfg(test)]
mod pd_explosion_tests {
    use super::*;

    fn blast() -> Explosion {
        Explosion { radius: 4.0, max_damage: 100.0 }
    }

    /// Both models agree at the two ends — full damage dead centre, nothing at the
    /// rim — and disagree in between, which is the whole point of the change.
    #[test]
    fn the_two_models_agree_at_the_ends_and_differ_in_the_middle() {
        let e = blast();
        assert!((linear_falloff_damage(&e, 0.0) - 100.0).abs() < 1e-3);
        assert!((pd_falloff_damage(&e, 0.0) - 100.0).abs() < 1e-3);
        assert_eq!(linear_falloff_damage(&e, 4.0), 0.0);
        assert_eq!(pd_falloff_damage(&e, 4.0), 0.0);
        // Half-way: linear gives 50, PD's squared curve gives 25.
        let (lin, pd) = (linear_falloff_damage(&e, 2.0), pd_falloff_damage(&e, 2.0));
        assert!((lin - 50.0).abs() < 1e-3, "linear at half radius: {lin}");
        assert!((pd - 25.0).abs() < 1e-3, "PD squared at half radius: {pd}");
        assert!(pd < lin, "PD's falloff is sharper everywhere inside the rim");
    }

    /// The per-axis box: PD's blast reaches further along an axis than diagonally,
    /// because it takes the MIN over axes rather than a Euclidean distance. A sphere
    /// model cannot produce this, so it is the signature of the port being real.
    #[test]
    fn the_falloff_is_a_per_axis_box_not_a_sphere() {
        let e = blast();
        let along = pd_falloff_damage_axes(&e, Vec3::new(2.0, 0.0, 0.0));
        let diagonal = pd_falloff_damage_axes(&e, Vec3::new(2.0, 2.0, 0.0));
        assert!(
            (along - 25.0).abs() < 1e-3,
            "2 m along one axis is the same as the scalar form: {along}"
        );
        assert!(
            (diagonal - along).abs() < 1e-3,
            "the min-over-axes makes these equal, unlike a sphere: {diagonal} vs {along}"
        );
        // Held against the SAME curve evaluated on Euclidean distance, the box is
        // more generous on the diagonal — which is the shape difference, isolated
        // from the curve. (Comparing against the linear model instead would prove
        // nothing: two different curves crossing says nothing about geometry.)
        let sphere_diag = pd_falloff_damage(&e, Vec3::new(2.0, 2.0, 0.0).length());
        assert!(
            diagonal > sphere_diag,
            "the box reaches further on the diagonal than a sphere: {diagonal} vs {sphere_diag}"
        );
        // Outside the box on any single axis is nothing at all.
        assert_eq!(pd_falloff_damage_axes(&e, Vec3::new(4.5, 0.0, 0.0)), 0.0);
    }

    /// Scenery and characters use DIFFERENT formulas, and the object one is floored
    /// at 30% — easy to conflate, so pinned.
    #[test]
    fn objects_keep_pds_thirty_percent_floor_and_characters_do_not() {
        let e = blast();
        let near_rim = Vec3::new(3.9, 0.0, 0.0);
        let chr = pd_falloff_damage_axes(&e, near_rim);
        let obj = pd_falloff_damage_object(&e, near_rim);
        assert!(chr < 1.0, "a character at the rim takes almost nothing: {chr}");
        assert!(obj > 30.0, "scenery at the rim still takes its 30% floor: {obj}");
    }

    /// Propagation: the first frame is the hard hit at the FULL damage radius, and
    /// later frames are a widening, much weaker wake starting from the fireball.
    #[test]
    fn a_blast_hits_hard_once_then_widens_weakly() {
        // PD's rocket: 2 m fireball, 4 m lethal, 1.5 s.
        let mut b = Blast::new(blast(), 2.0, 1.5);
        let at = Vec3::new(3.0, 0.0, 0.0); // outside the fireball, inside the lethal radius

        let first = b.damage_at(at, 1.0 / 60.0);
        assert!(first > 0.0, "the first frame reaches the full damage radius");
        b.advance(1.0 / 60.0);

        let second = b.damage_at(at, 1.0 / 60.0);
        assert!(
            second < first * 0.2,
            "the wake is far weaker than the initial hit: {second} vs {first}"
        );

        // The lethal radius grows from the fireball toward the damage radius.
        let early = b.current_radius();
        // 1.5 s at 60 Hz is 90 frames; two of them are already spent above.
        for _ in 0..90 {
            b.advance(1.0 / 60.0);
        }
        let late = b.current_radius();
        assert!(late > early, "the blast expands: {early} -> {late}");
        assert!(late <= blast().radius + 1e-3, "and never past its damage radius");
        assert!(b.finished(), "and it ends");
    }

    /// The sustained wake is frame-rate independent — the same elapsed time deals
    /// the same total damage whether stepped at 30 or 240 Hz. A per-frame scale
    /// without the `dt` term would make explosives lethality depend on frame rate.
    #[test]
    fn the_sustained_damage_is_frame_rate_independent() {
        let total_at = |hz: f32| {
            let mut b = Blast::new(blast(), 2.0, 1.0);
            let at = Vec3::new(1.0, 0.0, 0.0);
            let dt = 1.0 / hz;
            let mut total = 0.0;
            // Skip the first frame so only the sustained wake is measured.
            b.advance(dt);
            while !b.finished() {
                total += b.damage_at(at, dt);
                b.advance(dt);
            }
            total
        };
        let (slow, fast) = (total_at(30.0), total_at(240.0));
        let rel = (slow - fast).abs() / slow.max(1e-6);
        assert!(rel < 0.05, "30 Hz gave {slow}, 240 Hz gave {fast}");
    }

    /// Every lethal PD explosion row builds a coherent blast: a fireball no larger
    /// than its lethal radius, a real duration, and a first hit that hurts.
    #[test]
    fn every_pd_explosion_row_makes_a_usable_blast() {
        use crate::combat::pd_weapons::PD_EXPLOSIONS;
        let mut checked = 0;
        for e in PD_EXPLOSIONS.iter().filter(|e| e.damage > 0.0) {
            let ex = Explosion {
                radius: e.damage_radius_m,
                max_damage: e.peak_damage_hp(),
            };
            let b = Blast::new(ex, e.blast_radius_m, e.duration_s);
            assert!(b.blast_radius <= ex.radius + 1e-3, "{}", e.name);
            assert!(b.duration > 0.0, "{}", e.name);
            assert!(b.damage_at(Vec3::ZERO, 1.0 / 60.0) > 0.0, "{} is inert", e.name);
            checked += 1;
        }
        assert!(checked >= 20, "most rows are lethal, checked {checked}");
    }
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
        // Explicitly the LINEAR model. This test predates the Perfect Dark
        // explosion port and encoded the old default by calling the dispatcher —
        // which now selects PD's squared falloff, under which "half at
        // half-radius" is simply false (it is a quarter). The property it was
        // written to check still holds for the model it names.
        let e = Explosion { radius: 5.0, max_damage: 200.0 };
        assert_eq!(linear_falloff_damage(&e, 0.0), 200.0, "max at centre");
        assert!(
            (linear_falloff_damage(&e, 2.5) - 100.0).abs() < 1e-3,
            "half at half-radius"
        );
        assert_eq!(linear_falloff_damage(&e, 5.0), 0.0, "zero at the rim");
        assert_eq!(linear_falloff_damage(&e, 9.0), 0.0, "zero beyond the rim");
    }

    // ── Mines ──────────────────────────────────────────────────────────────────

    fn mine_spec(name: &str) -> super::MineSpec {
        let cfg = config::WEAPONS.iter().find(|w| w.name == name).unwrap();
        match cfg.fire_kind {
            config::FireKind::Mine(m) => m,
            _ => unreachable!("{name} is a mine"),
        }
    }

    /// A mine is inert while arming and can't be tripped; it goes live exactly once
    /// the arm delay elapses (and `tick` reports that transition once).
    #[test]
    fn mine_arms_after_delay() {
        let spec = mine_spec("Proximity Mine"); // arm 1.5 s, trip 2.5 m
        let mut m = Mine::new(Vec3::ZERO, Vec3::Y, spec, "Proximity Mine");
        // An actor sitting right on top of it can't trip it while it arms.
        assert!(!m.armed);
        assert!(!m.proximity_trips(Vec3::ZERO), "disarmed mine never trips");
        let mut armed_reports = 0;
        for _ in 0..149 {
            if m.tick(0.01) {
                armed_reports += 1;
            }
        }
        assert!(!m.armed, "not armed just before 1.5 s");
        for _ in 0..2 {
            if m.tick(0.01) {
                armed_reports += 1;
            }
        }
        assert!(m.armed, "armed past 1.5 s");
        assert_eq!(armed_reports, 1, "the arm transition is reported exactly once");
    }

    /// Once armed, a proximity mine trips on an actor inside its trip radius and
    /// ignores one outside it.
    #[test]
    fn proximity_mine_trips_inside_radius() {
        let spec = mine_spec("Proximity Mine"); // trip 2.5 m
        let mut m = Mine::new(Vec3::ZERO, Vec3::Y, spec, "Proximity Mine");
        while !m.armed {
            m.tick(0.01);
        }
        assert!(m.proximity_trips(Vec3::new(2.0, 0.0, 0.0)), "actor inside 2.5 m trips it");
        assert!(!m.proximity_trips(Vec3::new(3.0, 0.0, 0.0)), "actor outside 2.5 m does not");
    }

    /// A timed mine only starts its countdown once armed, then expires after the
    /// countdown — and never trips on proximity.
    #[test]
    fn timed_mine_expires_after_countdown() {
        let spec = mine_spec("Timed Mine"); // arm 1.5 s, then 4 s countdown
        let mut m = Mine::new(Vec3::ZERO, Vec3::Y, spec, "Timed Mine");
        // Arm it (1.5 s).
        for _ in 0..151 {
            m.tick(0.01);
        }
        assert!(m.armed);
        assert!(!m.timed_expired(), "countdown only just started");
        assert!(!m.proximity_trips(Vec3::ZERO), "a timed mine ignores proximity");
        // Burn ~4 s of countdown.
        for _ in 0..401 {
            m.tick(0.01);
        }
        assert!(m.timed_expired(), "expired ~4 s after arming");
    }

    /// A thrown mine flies (no arming mid-air) until it sticks; only then does its
    /// arm timer start counting.
    #[test]
    fn thrown_mine_flies_then_arms_after_sticking() {
        let spec = mine_spec("Proximity Mine");
        let mut m = Mine::throw(Vec3::ZERO, -Vec3::Z, Vec3::Y, spec, "Proximity Mine");
        assert!(!m.stuck, "starts in flight");
        // Fly for a while — it moves, but never arms while airborne.
        let mut moved = false;
        for _ in 0..30 {
            let (from, to) = m.advance(1.0 / 60.0);
            moved |= (to - from).length() > 0.0;
            assert!(!m.tick(1.0 / 60.0), "an airborne mine never arms");
            assert!(!m.armed);
        }
        assert!(moved, "the thrown mine travels");
        // Stick it to a wall; now the arm timer counts.
        m.stick(Vec3::new(0.0, 1.0, -5.0), Vec3::Z);
        assert!(m.stuck && !m.armed);
        while !m.armed {
            m.tick(0.01);
        }
        assert!(m.armed, "arms once stuck + arm delay elapsed");
        assert!(m.proximity_trips(Vec3::new(0.0, 1.0, -4.0)), "then trips normally");
    }

    /// A remote mine never self-trips (no proximity, no timer) — only a player-
    /// triggered detonation sets it off, which is the world layer's job.
    #[test]
    fn remote_mine_never_self_trips() {
        let spec = mine_spec("Remote Mine");
        let mut m = Mine::new(Vec3::ZERO, Vec3::Y, spec, "Remote Mine");
        assert!(m.is_remote());
        for _ in 0..1000 {
            m.tick(0.01);
        }
        assert!(m.armed, "it still arms");
        assert!(!m.proximity_trips(Vec3::ZERO), "but never trips on proximity");
        assert!(!m.timed_expired(), "nor on a timer");
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
