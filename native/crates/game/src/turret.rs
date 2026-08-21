//! The sentry gun's rig: how its six mesh pieces assemble into one articulated
//! turret, and where the bore points once it has.
//!
//! # Why there is a rig here at all
//!
//! The sentry gun ships as a GoldenEye Setup Editor export that is **not an assembled
//! object**. The editor wrote each sub-part at its own local origin and left the
//! assembly to the game, so `sentry_gun.obj` is six disjoint pieces parked on three
//! shelves along Z (−325 / 0 / +325 GoldenEye units). Each shelf is internally correct
//! — the barrel bundle really does abut the housing's +X face — but the shelves were
//! never joined, and drawn as one mesh the prop is a parts sheet floating in mid-air.
//!
//! [`engine::assets::obj_model::load_obj_components`] recovers the six pieces; this
//! module says where each one goes and which of them move. There is no authored
//! skeleton to recover, so the numbers below are authored, derived by measuring the
//! pieces and rendering candidate assemblies on the CPU (`tools/sentry/`).
//!
//! # The rig
//!
//! ```text
//! MOUNT (static, bolted to the ceiling)   cowl + dish decal
//!   └─ YAW   (about +Y)                   fin + trunnion
//!        └─ PITCH (about +Z)              housing
//!             └─ SPIN (about +X)          barrel bundle
//! ```
//!
//! Rig space has its **origin at the ceiling attach point**, +Y up, and the bore along
//! +X at rest, with the turret hanging into the room below. That is also the prop's
//! placement anchor, so a turret's authored position is the point on the ceiling it
//! bolts to.
//!
//! # The trunnion is on the bore, and that was not the first guess
//!
//! Hanging the gun from a pivot at its top-front corner — the obvious reading of "the
//! shaft holds the gun" — makes the 0.8 m housing scythe upward through the ceiling
//! plate the moment it pitches down. The trunnion belongs on the **bore line**, with
//! the housing's length balanced across it, which is both where a real gatling's
//! trunnion sits and what keeps the swept volume under the mount. See
//! [`PITCH_MIN`] for the clearance that buys.

use glam::{Mat4, Vec3};

/// GoldenEye export unit → metres, matching the OBJ loader's own import scale. Rig
/// offsets are written as `GE_UNITS * GE` so they can be read straight against the
/// measured component bounds in `tools/sentry/obj_parts.py`.
const GE: f32 = 0.001;

/// Which articulated node a mesh piece belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Node {
    /// Bolted to the ceiling; never moves.
    Mount,
    /// Turns about the vertical axis. Carries [`Node::Pitch`] and [`Node::Spin`].
    Yaw,
    /// Elevates about the trunnion. Carries [`Node::Spin`].
    Pitch,
    /// The barrel bundle, turning about the bore.
    Spin,
}

/// One mesh piece of the turret: which node moves it, what it is called on the
/// renderer, and the placement that brings it from its exported shelf into the
/// assembly (`p' = p * scale + offset`, applied before the node's rotation).
pub struct Part {
    pub node: Node,
    /// Renderer upload key — one per piece, since each draws with its own matrix.
    pub key: &'static str,
    /// Per-axis scale about the model origin.
    pub scale: Vec3,
    /// Translation into assembled rig space, in metres.
    pub offset: Vec3,
}

/// The turret's pieces, **indexed by connected-component order** as returned by
/// [`engine::assets::obj_model::load_obj_components`] (most triangles first, ties by
/// earliest face). `engine`'s `sentry_gun_splits_into_its_six_parts` pins that order
/// against the measured sizes, so this indexing cannot drift silently.
pub const PARTS: [Part; 6] = [
    // 0 — the 6-barrel bundle (0.60 m). Rides the housing's placement so the two stay
    //     joined exactly as exported; only the spin node separates them.
    Part {
        node: Node::Spin,
        key: "sentry_gun_barrel",
        scale: Vec3::ONE,
        offset: Vec3::new(300.0 * GE, -650.0 * GE, 325.0 * GE),
    },
    // 1 — the housing (0.80 × 0.50 × 0.25 m). Shifted +300 in X so its length is
    //     balanced across the trunnion instead of hanging off behind it.
    Part {
        node: Node::Pitch,
        key: "sentry_gun_housing",
        scale: Vec3::ONE,
        offset: Vec3::new(300.0 * GE, -650.0 * GE, 325.0 * GE),
    },
    // 2 — the vented cowl: the ceiling plate. Its top face (Y = +50) is raised to
    //     Y = 0 and centred on the hang axis, so it reads as bolted flat overhead.
    Part {
        node: Node::Mount,
        key: "sentry_gun_cowl",
        scale: Vec3::ONE,
        offset: Vec3::new(300.0 * GE, -50.0 * GE, -325.0 * GE),
    },
    // 3 — the yaw fin. Stretched 1.2425× in Y so it spans plate-to-trunnion exactly
    //     (497 units) rather than leaving an 87-unit gap at its natural 400.
    Part {
        node: Node::Yaw,
        key: "sentry_gun_fin",
        scale: Vec3::new(1.0, 1.2425, 1.0),
        offset: Vec3::new(300.0 * GE, -647.0 * GE, 0.0),
    },
    // 4 — the trunnion prism, centred on the bore: the axle the housing pitches on.
    Part {
        node: Node::Yaw,
        key: "sentry_gun_trunnion",
        scale: Vec3::ONE,
        offset: Vec3::new(300.0 * GE, -647.0 * GE, 0.0),
    },
    // 5 — the dish decal plane inside the cowl; rides the plate.
    Part {
        node: Node::Mount,
        key: "sentry_gun_panel",
        scale: Vec3::ONE,
        offset: Vec3::new(300.0 * GE, -50.0 * GE, -325.0 * GE),
    },
];

/// Bore height in rig space: the barrel bundle's own centreline once placed. The pitch
/// axis and the muzzle are both pinned to it, so the gun rotates about the line it
/// shoots along.
pub const BORE_Y: f32 = -697.0 * GE;

/// The pitch axis passes through the trunnion, on the bore.
pub const PITCH_PIVOT: Vec3 = Vec3::new(0.0, BORE_Y, 0.0);

/// The spin axis is the bore. `Z = −2` units is the bundle's own centreline, which the
/// export left fractionally off the housing's.
pub const SPIN_PIVOT: Vec3 = Vec3::new(0.0, BORE_Y, -2.0 * GE);

/// The muzzle in rig space — the +X end of the bundle, on the bore. Shots, the flash
/// and the report all originate here, carried by yaw and pitch.
pub const MUZZLE: Vec3 = Vec3::new(1000.0 * GE, BORE_Y, -2.0 * GE);

/// Authoring scale, applied as the placed prop's transform scale.
///
/// Assembled, the turret is 1.40 m long and hangs 1.05 m at raw export scale. A room
/// in this world is 8 world-tiles = **2.0 m** tall, so raw scale would put a gatling
/// gun at head height and make it longer than half the room — the same N64-scale trap
/// the door props hit. 0.45 gives a 0.63 m gun hanging 0.47 m: one you duck under.
pub const RIG_SCALE: f32 = 0.45;

/// How far the gun can depress. Past this its housing's back corner rises into the
/// ceiling plate it hangs from; at the limit there is 96 GoldenEye units (0.043 m at
/// [`RIG_SCALE`]) of clearance, which `pitch_clamp_clears_the_mount` holds to.
///
/// It is also enough gun: the bore sits ~1.69 m above the floor, so −50° reaches a
/// target's chest half a metre away and everything further is a shallower shot.
pub const PITCH_MIN: f32 = -50.0 * std::f32::consts::PI / 180.0;

/// How far the gun can elevate before it would be firing into its own mount.
pub const PITCH_MAX: f32 = 15.0 * std::f32::consts::PI / 180.0;

/// Barrel speed at full song, radians/sec (three turns a second).
pub const SPIN_RATE: f32 = 1080.0 * std::f32::consts::PI / 180.0;

/// A rotation of `angle` about `axis_dir` passing through `pivot`.
fn pivot_rot(rot: Mat4, pivot: Vec3) -> Mat4 {
    Mat4::from_translation(pivot) * rot * Mat4::from_translation(-pivot)
}

/// The **aim basis**: yaw then pitch, the frame the gun points and shoots in. The
/// muzzle and the bore direction both come from here, so barrel spin (which turns
/// about the bore and therefore cannot move either) is deliberately not in it.
pub fn aim_basis(yaw: f32, pitch: f32) -> Mat4 {
    Mat4::from_rotation_y(yaw) * pivot_rot(Mat4::from_rotation_z(pitch), PITCH_PIVOT)
}

/// The rig-space matrix for a node at a given articulation.
pub fn node_matrix(node: Node, yaw: f32, pitch: f32, spin: f32) -> Mat4 {
    match node {
        Node::Mount => Mat4::IDENTITY,
        Node::Yaw => Mat4::from_rotation_y(yaw),
        Node::Pitch => aim_basis(yaw, pitch),
        Node::Spin => aim_basis(yaw, pitch) * pivot_rot(Mat4::from_rotation_x(spin), SPIN_PIVOT),
    }
}

/// The rig-space matrix for one mesh piece: its placement onto the assembly, then its
/// node's rotation. Multiply by the prop's world transform to draw it.
pub fn part_matrix(part: &Part, yaw: f32, pitch: f32, spin: f32) -> Mat4 {
    node_matrix(part.node, yaw, pitch, spin)
        * Mat4::from_translation(part.offset)
        * Mat4::from_scale(part.scale)
}

/// Rig-space muzzle point at this articulation.
pub fn muzzle(yaw: f32, pitch: f32) -> Vec3 {
    aim_basis(yaw, pitch).transform_point3(MUZZLE)
}

/// Rig-space bore direction (unit) at this articulation.
pub fn bore_dir(yaw: f32, pitch: f32) -> Vec3 {
    aim_basis(yaw, pitch).transform_vector3(Vec3::X).normalize_or_zero()
}

/// The `(yaw, pitch)` that points the bore along `dir` (rig space, need not be
/// normalised), with pitch clamped to what the mount allows. Inverse of
/// [`bore_dir`] up to that clamp — `aim_solves_the_bore_direction` holds the pair
/// together, so neither can pick up a sign error on its own.
pub fn aim_at(dir: Vec3) -> (f32, f32) {
    let d = dir.normalize_or_zero();
    if d == Vec3::ZERO {
        return (0.0, 0.0);
    }
    // At yaw = pitch = 0 the bore is +X; yaw turns it toward −Z, pitch lifts it +Y.
    let yaw = (-d.z).atan2(d.x);
    let pitch = d.y.clamp(-1.0, 1.0).asin().clamp(PITCH_MIN, PITCH_MAX);
    (yaw, pitch)
}

/// The rig's model-space AABB **at rest**, given the loaded pieces in [`PARTS`] order.
///
/// This is what the placement ghost and the prop anchor measure. Taking the raw
/// export's bounds instead would measure the parts sheet — a box nearly a metre deep
/// in Z, most of it empty space between shelves — so the ghost would not match the
/// turret you are actually placing.
pub fn assembled_bounds(
    parts: &[engine::assets::textured_model::TexturedModel],
) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for (part, model) in PARTS.iter().zip(parts) {
        let m = part_matrix(part, 0.0, 0.0, 0.0);
        for v in &model.vertices {
            let p = m.transform_point3(Vec3::from_array(v.pos));
            min = min.min(p);
            max = max.max(p);
        }
    }
    if min.x > max.x {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    (min, max)
}

/// Shortest signed angle from `from` to `to`, in (−π, π] — so a turret crossing the
/// wrap point slews the short way instead of unwinding all the way round.
pub fn angle_delta(from: f32, to: f32) -> f32 {
    let mut d = (to - from) % std::f32::consts::TAU;
    if d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    } else if d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rig node is claimed by at least one piece, no renderer key repeats, and
    /// the moving mass is where the rig says it is: exactly one piece spins.
    #[test]
    fn the_rig_covers_every_node_exactly_once_per_key() {
        let mut keys: Vec<&str> = PARTS.iter().map(|p| p.key).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "duplicate renderer key in the rig");
        for node in [Node::Mount, Node::Yaw, Node::Pitch, Node::Spin] {
            assert!(PARTS.iter().any(|p| p.node == node), "no piece on {node:?}");
        }
        assert_eq!(PARTS.iter().filter(|p| p.node == Node::Spin).count(), 1);
    }

    /// [`aim_at`] really is the inverse of [`bore_dir`]: solving for a direction and
    /// pointing there gets the bore back. Written as a round trip because the two
    /// carry opposite sign conventions and a matched pair of sign errors — the classic
    /// way a turret ends up tracking its target's mirror image — cancels in any test
    /// that only checks one of them.
    #[test]
    fn aim_solves_the_bore_direction() {
        for yaw_deg in (-180..180).step_by(15) {
            for pitch_deg in -45..15 {
                let (y, p) = (
                    (yaw_deg as f32).to_radians(),
                    (pitch_deg as f32).to_radians(),
                );
                let dir = bore_dir(y, p);
                let (sy, sp) = aim_at(dir);
                let back = bore_dir(sy, sp);
                assert!(
                    back.distance(dir) < 1e-4,
                    "yaw {yaw_deg} pitch {pitch_deg}: {dir:?} -> {back:?}"
                );
            }
        }
    }

    /// The muzzle sits on the bore, so spinning the barrels cannot move it and the
    /// distance from the pitch pivot is invariant under aiming.
    #[test]
    fn the_muzzle_rides_the_bore() {
        let rest = muzzle(0.0, 0.0);
        assert!((rest - MUZZLE).length() < 1e-5, "at rest the muzzle is the rig point");
        let reach = (MUZZLE - PITCH_PIVOT).length();
        for (y, p) in [(0.4, -0.3), (-2.1, 0.2), (3.0, PITCH_MIN)] {
            let m = muzzle(y, p);
            assert!(((m - PITCH_PIVOT).length() - reach).abs() < 1e-4);
            // The bore direction points from the pivot at the muzzle.
            let along = (m - PITCH_PIVOT).normalize();
            assert!(along.dot(bore_dir(y, p)) > 0.999);
        }
        // Spin turns the bundle about the bore, so a point on the axis cannot move.
        for spin in [0.0_f32, 1.0, 3.3] {
            let m = node_matrix(Node::Spin, 0.3, -0.2, spin).transform_point3(SPIN_PIVOT);
            let m0 = node_matrix(Node::Spin, 0.3, -0.2, 0.0).transform_point3(SPIN_PIVOT);
            assert!(m.distance(m0) < 1e-5, "spin moved a point on its own axis");
        }
    }

    /// The pitch clamp is what stops the housing swinging up into the ceiling plate it
    /// hangs from. Swept across the whole allowed range, the highest point of the
    /// pitching housing stays below the plate's underside.
    ///
    /// This is the check the first rig failed: with the trunnion at the housing's
    /// top-front corner rather than on the bore, the housing cleared the plate at rest
    /// and scythed straight through it under depression.
    #[test]
    fn pitch_clamp_clears_the_mount() {
        // Housing extents after placement, and the plate's underside — both measured
        // from the export (see tools/sentry/obj_parts.py).
        let housing = &PARTS[1];
        let corners = [
            Vec3::new(-700.0 * GE, -400.0 * GE, 0.0),
            Vec3::new(-700.0 * GE, 100.0 * GE, 0.0),
            Vec3::new(100.0 * GE, -400.0 * GE, 0.0),
            Vec3::new(100.0 * GE, 100.0 * GE, 0.0),
        ];
        let plate_underside = -200.0 * GE;
        let mut peak = f32::MIN;
        for step in 0..=1000 {
            let pitch = PITCH_MIN + (PITCH_MAX - PITCH_MIN) * step as f32 / 1000.0;
            let m = part_matrix(housing, 0.0, pitch, 0.0);
            for c in corners {
                peak = peak.max(m.transform_point3(c).y);
            }
        }
        assert!(
            peak < plate_underside,
            "housing reaches y={peak:.4} against a plate underside of {plate_underside:.4}"
        );
        // And the clearance is the documented 96 GoldenEye units, not a hair's breadth.
        assert!(
            (plate_underside - peak) > 90.0 * GE,
            "clearance is only {:.1} GE units",
            (plate_underside - peak) / GE
        );
    }

    /// Slewing takes the short way round the wrap point.
    #[test]
    fn angle_delta_takes_the_short_way() {
        use std::f32::consts::PI;
        assert!((angle_delta(3.0, -3.0) - (2.0 * PI - 6.0)).abs() < 1e-5);
        assert!((angle_delta(-3.0, 3.0) + (2.0 * PI - 6.0)).abs() < 1e-5);
        assert!((angle_delta(0.1, 0.4) - 0.3).abs() < 1e-5);
    }
}
