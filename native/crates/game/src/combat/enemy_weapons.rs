//! Enemy weapon definitions — the hunter side of the arsenal, ported from
//! `3DS FPS/src/data/EnemyWeaponConfig.ts` (the read-only oracle). An enemy holds
//! any of the player's [`WeaponStats`] guns, but attaches + animates them very
//! differently from the first-person viewmodel: the gun GLB is parented to a hand
//! **bone** (`Bone_9` right / `Bone_8` left) with a bone-local offset in GoldenEye
//! units (applied before the character's `CHAR_SCALE`), and the fire *animation* is
//! chosen by weapon **class** (pistol / rifle / dual-wield).
//!
//! The source defines exact bone offsets for only four guns (pp7, kf7, ar33,
//! rcp90). [`enemy_def_for`] reuses those verbatim and gives every other arsenal
//! weapon a per-class default offset (pp7's for pistols, kf7's for rifles), so any
//! of the 19 weapons can be equipped — and dual-wielded, since each class also
//! carries a mirrored left-hand offset.
//!
//! **Dual-wield is a runtime flag, not a weapon type** (JS `weaponOptions.dual`):
//! the same gun is attached to both hands, the left copy using
//! `left_offset`/`left_rot`, and both muzzles flash per shot. See [`crate::world`].

use glam::Vec3;

use super::config::WeaponStats;

/// How the enemy holds + fires a weapon. `Pistol` = one-handed (pistol fire anim);
/// `Rifle` = two-handed (rifle fire anim). Dual-wield is orthogonal (a `bool` flag
/// carried alongside the def) and overrides the fire anim to the dual clip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnemyWeaponClass {
    Pistol,
    Rifle,
}

/// A weapon as an enemy wields it: the shared gun/muzzle/sound assets (identical to
/// the player [`WeaponStats`] paths), the AI fire stats, and the two bone-local
/// attach transforms (right hand always; left hand only when dual-wielding).
#[derive(Clone, Copy, Debug)]
pub struct EnemyWeaponDef {
    pub name: &'static str,
    pub class: EnemyWeaponClass,
    /// Gun GLB, relative under `native/assets/weapons/` (e.g. `"kf7/gun.glb"`).
    pub gun_path: &'static str,
    /// Muzzle-flash GLB (same root); `""` when the weapon has none (e.g. sniper).
    pub muzzle_path: &'static str,
    /// Fire SFX, relative under `native/assets/audio/`.
    pub fire_sound: &'static str,
    /// Damage per hit (JS `EnemyManager` uniform override — all hunters deal this).
    pub damage: f32,
    /// Base hit chance 0–1 (scaled by distance at the shot; JS `accuracy`).
    pub accuracy: f32,
    /// Effective range in metres (the hit roll goes to 0 beyond it, and the FSM
    /// [`Self::standoff`] is derived from it).
    pub range: f32,
    /// Shots per second while inside the fire-animation window.
    pub fire_rate: f32,
    /// Distance (m) the hunter advances to and holds at while attacking — derived
    /// from [`Self::range`] via [`standoff_for`], so a sniper hangs way back and a
    /// shotgunner charges in. Threaded into the FSM ([`crate::enemy::Enemy::update`]).
    pub standoff: f32,
    /// Right-hand (`Bone_9`) bone-local offset + XYZ-euler rotation, GE units.
    pub right_offset: Vec3,
    pub right_rot: Vec3,
    /// Left-hand (`Bone_8`) offset + rotation for the dual-wield copy, GE units.
    pub left_offset: Vec3,
    pub left_rot: Vec3,
}

/// Uniform enemy damage per hit (JS `EnemyManager.ts:56` `damage: 8`). Every hunter
/// deals this regardless of weapon; the weapon varies fire-rate / accuracy / range,
/// so DPS scales with the gun.
pub const ENEMY_DAMAGE: f32 = 8.0;

/// The right-hand attach bone (JS `Bone_9`), and the left-hand bone for the dual
/// copy (JS `Bone_8`).
pub const RIGHT_HAND_BONE: &str = "Bone_9";
pub const LEFT_HAND_BONE: &str = "Bone_8";
/// The head bone (`Bone_3`, a direct child of the chest `Bone_2`) — the joint the
/// procedural head look-at rotates so a hunter turns its head toward what it's
/// focused on. There's no separate neck bone in the 15-bone rig, so this single
/// head joint carries the whole gaze (cone-clamped to hide the missing neck).
pub const HEAD_BONE: &str = "Bone_3";
/// The pelvis (root) bone and the two foot bones, for ground-adaptive foot IK. Leg
/// chains walk up from each foot: `Bone_14`←`Bone_12`←`Bone_10` (left),
/// `Bone_15`←`Bone_13`←`Bone_11` (right); all three hang off the pelvis `Bone_1`.
pub const PELVIS_BONE: &str = "Bone_1";
pub const LEFT_FOOT_BONE: &str = "Bone_14";
pub const RIGHT_FOOT_BONE: &str = "Bone_15";

// ─── Bespoke source offsets (EnemyWeaponConfig.ts) ──────────────────────────────
// Pistol class defaults come from pp7; rifle class defaults from kf7; the dual
// (left-hand) rifle offset comes from rcp90 — the source's canonical dual weapon.
const PISTOL_R_OFF: Vec3 = Vec3::new(-150.0, 30.0, 115.0);
const PISTOL_R_ROT: Vec3 = Vec3::new(-0.39, -1.49, -1.84);
const PISTOL_L_OFF: Vec3 = Vec3::new(175.0, -30.0, 115.0);
const PISTOL_L_ROT: Vec3 = Vec3::new(3.11, 1.66, -1.49);

const RIFLE_R_OFF: Vec3 = Vec3::new(-90.0, 0.0, 145.0);
const RIFLE_R_ROT: Vec3 = Vec3::new(0.0, -1.49, -1.69);
// rcp90's left-hand offset — a plausible mirrored grip for any two-handed gun.
const RIFLE_L_OFF: Vec3 = Vec3::new(-145.0, 0.0, 0.0);
const RIFLE_L_ROT: Vec3 = Vec3::new(0.26, 1.56, 1.26);

/// Player-weapon names that an enemy holds one-handed (the pistol class). Every
/// other arsenal weapon is held two-handed (rifle class). Matches the JS split
/// (pistols one-handed, SMGs/rifles/shotguns/special two-handed).
const PISTOL_NAMES: &[&str] = &[
    "PP7",
    "DD44 Dostovei",
    "Cougar Magnum",
    "Golden Gun",
    "Gold PP7",
    "Silver PP7",
    "PP7 (Silenced)",
];

// ─── Engagement range (drives standoff + accuracy falloff) ───────────────────────
// The class default `range` (pistol 8 / rifle 12) doesn't distinguish a shotgun from
// a sniper — both are class Rifle. These name bands give the CQC and long-reach guns
// an engagement range that matches how they actually fight, so the derived standoff
// (and the accuracy falloff) reads right: a shotgunner charges in, a sniper hangs back.

/// Close-quarters weapons — the hunter closes right in (short range → short standoff).
/// Shotguns and SMGs are murderous up close and fall off fast at distance.
const CQC_NAMES: &[&str] = &[
    "Shotgun",
    "Auto Shotgun",
    "Klobb",
    "D5K Deutsche",
    "D5K (Silenced)",
    "Phantom",
    "ZMG 9mm",
];
/// Long-reach weapons — the hunter deliberately hangs way back (long range → the
/// standoff clamps near the perception edge). Snipers + the laser reach across a room.
const LONG_NAMES: &[&str] = &["Sniper Rifle", "Moonraker Laser"];

/// The hunter's effective engagement range in metres: the CQC / long bands above,
/// else the weapon's class default (`class_default`).
fn engagement_range(w: &WeaponStats, class_default: f32) -> f32 {
    if CQC_NAMES.contains(&w.name) {
        5.0
    } else if LONG_NAMES.contains(&w.name) {
        18.0
    } else {
        class_default
    }
}

/// Fraction of a weapon's effective `range` a hunter holds at while attacking — it
/// advances to here, plants, and fires. `< 1` so it stays comfortably inside its own
/// range (where its accuracy is still up).
const STANDOFF_FRAC: f32 = 0.6;
/// Standoff clamps (m): never closer than knife-fight range, never past what the
/// hunter can still perceive — kept below the FSM `DETECTION_RANGE` (12 m) so it
/// re-acquires the player at its hold distance.
const MIN_STANDOFF: f32 = 2.5;
const MAX_STANDOFF: f32 = 11.0;

/// The FSM standoff for a weapon of effective `range`: a fixed fraction of range,
/// clamped. A shotgun (range 5) holds at 3 m; a sniper (range 18) holds at ~11 m.
pub fn standoff_for(range: f32) -> f32 {
    (range * STANDOFF_FRAC).clamp(MIN_STANDOFF, MAX_STANDOFF)
}

/// Build the enemy-side definition for any player [`WeaponStats`]. The four
/// source-defined guns (pp7/kf7/ar33/rcp90) get their exact bone offsets + AI
/// stats; every other weapon gets its class defaults so the full arsenal is
/// equippable. Asset paths + fire sound come straight off the player weapon (the
/// enemy and player share the same GLBs).
pub fn enemy_def_for(w: &WeaponStats) -> EnemyWeaponDef {
    let class = if PISTOL_NAMES.contains(&w.name) {
        EnemyWeaponClass::Pistol
    } else {
        EnemyWeaponClass::Rifle
    };

    // Class-default AI stats + offsets (pp7 for pistols, kf7 for rifles). The class
    // default `range` / `fire_rate` are refined below by the weapon's identity.
    let (accuracy, class_range, class_fire_rate, r_off, r_rot, l_off, l_rot) = match class {
        EnemyWeaponClass::Pistol => (
            0.85, 8.0, 2.0, PISTOL_R_OFF, PISTOL_R_ROT, PISTOL_L_OFF, PISTOL_L_ROT,
        ),
        EnemyWeaponClass::Rifle => (
            0.75, 12.0, 8.0, RIFLE_R_OFF, RIFLE_R_ROT, RIFLE_L_OFF, RIFLE_L_ROT,
        ),
    };

    // Effective engagement range — CQC guns close in, snipers hang back (drives both
    // the standoff and the accuracy falloff).
    let range = engagement_range(w, class_range);
    // Enemy fire cadence. A pump shotgun / sniper / single-shot is class `Rifle`, so
    // the 8/s class default turns it full-auto. Clamp NON-automatic weapons to their
    // real cadence (`1/fire_cooldown`), only ever slowing below the class default —
    // so a shotgun fires ~1.25/s and a sniper ~0.8/s, while true autos keep 8/s.
    let fire_rate = if w.automatic {
        class_fire_rate
    } else {
        (1.0 / w.fire_cooldown).min(class_fire_rate)
    };

    // Bespoke overrides for the four source-defined guns (exact EnemyWeaponConfig.ts
    // values). `None` → keep the class defaults above.
    let bespoke: Option<(f32, f32, f32, Vec3, Vec3, Vec3, Vec3)> = match w.name {
        "PP7" => Some((
            0.85, 8.0, 2.0,
            Vec3::new(-150.0, 30.0, 115.0), Vec3::new(-0.39, -1.49, -1.84),
            Vec3::new(175.0, -30.0, 115.0), Vec3::new(3.11, 1.66, -1.49),
        )),
        "KF7 Soviet" => Some((
            0.75, 12.0, 8.0,
            Vec3::new(-90.0, 0.0, 145.0), Vec3::new(0.0, -1.49, -1.69),
            RIFLE_L_OFF, RIFLE_L_ROT, // no source dual data → mirrored default
        )),
        "AR33" => Some((
            0.8, 10.0, 6.0,
            Vec3::new(-90.0, 0.0, 145.0), Vec3::new(0.0, -1.49, -1.69),
            RIFLE_L_OFF, RIFLE_L_ROT,
        )),
        "RC-P90" => Some((
            0.7, 8.0, 12.0,
            Vec3::new(145.0, 0.0, 0.0), Vec3::new(0.0, -1.59, -1.59),
            Vec3::new(-145.0, 0.0, 0.0), Vec3::new(0.26, 1.56, 1.26),
        )),
        _ => None,
    };
    let (accuracy, range, fire_rate, r_off, r_rot, l_off, l_rot) =
        bespoke.unwrap_or((accuracy, range, fire_rate, r_off, r_rot, l_off, l_rot));

    EnemyWeaponDef {
        name: w.name,
        class,
        gun_path: w.gun_path,
        muzzle_path: w.muzzle_path,
        fire_sound: w.fire_sound,
        damage: ENEMY_DAMAGE,
        accuracy,
        range,
        fire_rate,
        standoff: standoff_for(range),
        right_offset: r_off,
        right_rot: r_rot,
        left_offset: l_off,
        left_rot: l_rot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::config;

    #[test]
    fn pistols_are_one_handed_rest_two_handed() {
        assert_eq!(enemy_def_for(&config::PP7).class, EnemyWeaponClass::Pistol);
        assert_eq!(enemy_def_for(&config::DD44).class, EnemyWeaponClass::Pistol);
        assert_eq!(enemy_def_for(&config::KLOBB).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::KF7).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::SHOTGUN).class, EnemyWeaponClass::Rifle);
        assert_eq!(enemy_def_for(&config::LASER).class, EnemyWeaponClass::Rifle);
    }

    #[test]
    fn bespoke_offsets_match_source() {
        let kf7 = enemy_def_for(&config::KF7);
        assert_eq!(kf7.right_offset, Vec3::new(-90.0, 0.0, 145.0));
        assert_eq!(kf7.right_rot, Vec3::new(0.0, -1.49, -1.69));
        let rcp = enemy_def_for(&config::RCP90);
        assert_eq!(rcp.right_offset, Vec3::new(145.0, 0.0, 0.0));
        assert_eq!(rcp.left_offset, Vec3::new(-145.0, 0.0, 0.0));
    }

    #[test]
    fn every_weapon_has_a_nonzero_dual_left_offset() {
        // Any weapon must be dual-wieldable → a real (mirrored) left-hand grip.
        for w in config::WEAPONS {
            let d = enemy_def_for(w);
            assert!(
                d.left_offset.length_squared() > 1.0,
                "{} has a degenerate left-hand offset",
                d.name
            );
        }
    }

    #[test]
    fn assets_come_from_the_player_weapon() {
        let d = enemy_def_for(&config::KF7);
        assert_eq!(d.gun_path, config::KF7.gun_path);
        assert_eq!(d.fire_sound, config::KF7.fire_sound);
    }

    /// A semi-auto weapon that inherits the rifle class (the pump shotgun, the
    /// sniper, single-shot launchers) must NOT fire at the 8/s rifle-class default —
    /// it fires at its real cadence (`1/fire_cooldown`). Regression for the
    /// "shotgun fires full-auto" bug.
    #[test]
    fn semi_auto_weapons_are_not_full_auto() {
        // Shotgun: class Rifle, semi (0.8 s cooldown) → ~1.25/s, well below 8/s.
        let shotgun = enemy_def_for(&config::SHOTGUN);
        assert_eq!(shotgun.class, EnemyWeaponClass::Rifle);
        assert!(
            (shotgun.fire_rate - 1.0 / config::SHOTGUN.fire_cooldown).abs() < 1e-3,
            "shotgun fires at its real cadence, got {}",
            shotgun.fire_rate
        );
        assert!(shotgun.fire_rate < 2.0, "shotgun is pistol-slow, not full-auto");
        // Sniper: even slower (1.2 s cooldown → ~0.83/s).
        let sniper = enemy_def_for(&config::SNIPER);
        assert!(sniper.fire_rate < 1.0, "sniper is single-shot-slow, got {}", sniper.fire_rate);
    }

    /// True automatic weapons keep the sustained class fire rate (they're meant to
    /// hose), and pistols keep their 2/s default (the semi cap only slows below it).
    #[test]
    fn automatic_weapons_keep_the_class_rate() {
        assert_eq!(enemy_def_for(&config::KLOBB).fire_rate, 8.0, "SMG hoses");
        assert_eq!(enemy_def_for(&config::LASER).fire_rate, 8.0, "laser hoses");
        assert_eq!(enemy_def_for(&config::PP7).fire_rate, 2.0, "pistol default unchanged");
    }

    /// Standoff scales with the weapon's engagement range: a shotgunner charges in,
    /// a rifleman holds mid, a sniper hangs way back.
    #[test]
    fn standoff_scales_with_range() {
        let shotgun = enemy_def_for(&config::SHOTGUN);
        let rifle = enemy_def_for(&config::KF7);
        let sniper = enemy_def_for(&config::SNIPER);
        assert!(
            shotgun.standoff < rifle.standoff && rifle.standoff < sniper.standoff,
            "shotgun {} < rifle {} < sniper {}",
            shotgun.standoff,
            rifle.standoff,
            sniper.standoff
        );
        // The shotgunner closes right in; the sniper holds near the perception edge.
        assert!(shotgun.standoff <= 3.5, "shotgun charges in, got {}", shotgun.standoff);
        assert!(sniper.standoff >= 9.0, "sniper hangs way back, got {}", sniper.standoff);
        // Every standoff stays inside the perception range so the hunter re-acquires
        // at its hold distance.
        for w in config::WEAPONS {
            let d = enemy_def_for(w);
            assert!(d.standoff <= MAX_STANDOFF, "{} standoff exceeds the cap", d.name);
            assert!(d.standoff >= MIN_STANDOFF, "{} standoff below the floor", d.name);
        }
    }
}
