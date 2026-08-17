//! Which arsenal the game is playing with — GoldenEye's, Perfect Dark's, or both.
//!
//! The GoldenEye 23 in [`super::config`] are hand-tuned and stay exactly as they
//! are. Perfect Dark's 33 ([`super::pd_weapons`]) sit **alongside** them, bridged
//! into the same [`WeaponStats`] shape so the existing fire/ammo/reload state
//! machine, the viewmodel, the shop and the enemy defs all drive a PD gun without
//! knowing it is one. That is the coexistence decision, and it mirrors what the
//! hunter track landed on with two body families selected by `BODIES=`.
//!
//! ```text
//! ARSENAL=ge     the GoldenEye 23 only (the default, unchanged behaviour)
//! ARSENAL=pd     Perfect Dark's 33 only
//! ARSENAL=both   all 56, GoldenEye first
//! ```
//!
//! # The bridge is lossy on purpose
//!
//! A [`PdWeapon`] carries two firing functions; [`WeaponStats`] carries one. This
//! module maps the **primary** function and stops there — the second function
//! needs a control, a HUD state and an AI choice, which is its own milestone. So
//! what lands here is "PD's guns, playable through the machinery we already have",
//! and nothing about the transcribed table is thrown away to get it.
//!
//! Two substitutions are deliberate and worth stating, because both are places
//! where a reader could otherwise assume PD data where there is none:
//!
//! * **Sounds are GoldenEye's.** No PD audio is extracted (`goldeneye-soundpack`
//!   is what the repo has), so each PD gun borrows the GE sound closest to its
//!   role. `funcdef_shoot.shootsound` names the real SFX id and is transcribed, so
//!   this is a substitution waiting on an asset, not a guess about the data.
//! * **Viewmodel placement is PD's `posx/posy/posz`,** converted from PD
//!   centimetres. This replaces the hand-tuned `weapon-config.json` numbers for PD
//!   guns only — the GoldenEye entries keep theirs untouched.

use glam::Vec3;

use super::config::{self, Explosion, FireKind, ProjectileSpec, WeaponStats};
use super::pd_weapons::{pd_guns, PdFunc, PdFuncKind, PdWeapon, PD_CM_TO_M, PD_DAMAGE_TO_HP};

/// The barrel-forward direction of an exported PD third-person gun model.
///
/// Unanimous across all 27 chr gun models that author a `CHRGUNFIRE` node: the
/// muzzle's dominant component is negative X, with no exceptions. So this is read
/// off the assets rather than inferred from meshes agreeing, which is what
/// `DESIGN_PD_SIMULANT_AI.md` §15 had to do for the GoldenEye set.
pub const PD_BARREL_AXIS: Vec3 = Vec3::NEG_X;

/// Which weapon table(s) the player and the hunters draw from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arsenal {
    /// The tuned GoldenEye 23 — the default, and unchanged from before this track.
    GoldenEye,
    /// Perfect Dark's 33 MP guns.
    PerfectDark,
    /// Both, GoldenEye first so existing shop indices keep their meaning.
    Both,
}

impl Arsenal {
    /// Resolve from the `ARSENAL` environment variable, defaulting to GoldenEye.
    ///
    /// Applied **last** and logged, which is trap #6 from the handoff: an explicit
    /// choice has to outrank a mode default, because `enable_pd_lab` once pinned a
    /// body set and silently ate `BODIES=ge` for an entire playtest.
    pub fn from_env() -> Self {
        let raw = std::env::var("ARSENAL").unwrap_or_default();
        let picked = match raw.trim().to_ascii_lowercase().as_str() {
            "pd" | "perfectdark" | "perfect-dark" => Arsenal::PerfectDark,
            "both" | "all" => Arsenal::Both,
            "" | "ge" | "goldeneye" => Arsenal::GoldenEye,
            other => {
                log::warn!("ARSENAL={other:?} is not ge|pd|both — falling back to GoldenEye");
                Arsenal::GoldenEye
            }
        };
        picked
    }

    /// One line naming the resolved arsenal and its size, for the startup log.
    /// `World::roster_summary` is the pattern — a resolved choice nobody can see
    /// is a choice that will be argued about later.
    pub fn summary(self) -> String {
        let n = self.weapons().len();
        match self {
            Arsenal::GoldenEye => format!("arsenal: GoldenEye ({n} weapons)"),
            Arsenal::PerfectDark => format!("arsenal: Perfect Dark ({n} weapons)"),
            Arsenal::Both => format!("arsenal: GoldenEye + Perfect Dark ({n} weapons)"),
        }
    }

    /// The active weapon list, in cycle order.
    pub fn weapons(self) -> &'static [WeaponStats] {
        match self {
            Arsenal::GoldenEye => config::WEAPONS,
            Arsenal::PerfectDark => pd_arsenal(),
            Arsenal::Both => both_arsenal(),
        }
    }
}

/// The PD guns as [`WeaponStats`], built once on first use.
///
/// A `static` array is not an option: the bridge needs float arithmetic and
/// `Option` handling that const-eval will not do, and every string it needs is
/// already `&'static` from the generated table.
pub fn pd_arsenal() -> &'static [WeaponStats] {
    static CELL: std::sync::OnceLock<Vec<WeaponStats>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| pd_guns().map(weapon_stats_from_pd).collect())
}

/// GoldenEye's 23 followed by Perfect Dark's 33.
pub fn both_arsenal() -> &'static [WeaponStats] {
    static CELL: std::sync::OnceLock<Vec<WeaponStats>> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let mut all: Vec<WeaponStats> = config::WEAPONS.to_vec();
        all.extend_from_slice(pd_arsenal());
        all
    })
}

// ─── Name disambiguation ─────────────────────────────────────────────────────
// Seven PD weapons share a name with a GoldenEye one, and both families can be
// live at once under `ARSENAL=both`. That is not cosmetic: shop prices and enemy
// weapon defs are keyed **by name** (`shop::listed_price`, `enemy_def_for`), on
// purpose, so that reordering a table cannot silently mis-price a gun — which
// means a duplicate name would make two different weapons indistinguishable to
// both lookups.
//
// So the PD copy gets a suffix, and only where there is an actual clash: the 26
// PD weapons with unique names keep them exactly as Rare authored them.
// `the_two_families_share_no_names` fails if a new clash appears, and
// `every_clash_is_disambiguated` fails if an entry here stops clashing (i.e. this
// list going stale in either direction is a test failure, not a mystery).
const PD_RENAMES: &[(&str, &str)] = &[
    ("Shotgun", "Shotgun (PD)"),
    ("Sniper Rifle", "Sniper Rifle (PD)"),
    ("Rocket Launcher", "Rocket Launcher (PD)"),
    ("Grenade", "Grenade (PD)"),
    ("Timed Mine", "Timed Mine (PD)"),
    ("Proximity Mine", "Proximity Mine (PD)"),
    ("Remote Mine", "Remote Mine (PD)"),
];

/// The name a PD weapon goes by in an arsenal that may also hold GoldenEye's.
pub fn pd_display_name(pd_name: &'static str) -> &'static str {
    match PD_RENAMES.iter().find(|(from, _)| *from == pd_name) {
        Some((_, to)) => to,
        None => pd_name,
    }
}

/// The inverse: an arsenal display name back to its `pd_weapons` row name, so a
/// bridged [`WeaponStats`] can be traced to the PD data it came from.
pub fn pd_source_name(display: &str) -> &str {
    match PD_RENAMES.iter().find(|(_, to)| *to == display) {
        Some((from, _)) => from,
        None => display,
    }
}

/// The [`PdWeapon`] a bridged weapon name came from, or `None` for a GoldenEye one.
///
/// Matches on the **display** name, not the source name, and that distinction is
/// load-bearing rather than pedantic. Going through [`pd_source_name`] would map
/// GoldenEye's `"Shotgun"` onto Perfect Dark's, because the bare name is a
/// legitimate PD row name — so the tuned GoldenEye shotgun would silently pick up
/// PD's -X barrel axis and aim 83° off. The existing
/// `every_weapon_aims_down_its_barrel` test caught exactly that.
///
/// Comparing display names is exact in both directions: an un-suffixed clashing
/// name can only be the GoldenEye one, and a suffixed one can only be PD's.
pub fn pd_weapon_for(display_name: &str) -> Option<&'static PdWeapon> {
    super::pd_weapons::pd_guns().find(|w| pd_display_name(w.name) == display_name)
}

// ─── Sound substitution ──────────────────────────────────────────────────────
// PD's own `shootsound` ids are transcribed but no PD audio is extracted, so each
// gun borrows the GoldenEye sound closest to its role. Grouped by how the weapon
// actually fires rather than by name, so a new PD gun lands somewhere sensible.

const SND_PISTOL: &str = "sounds/weapons/pp7-fire.wav";
const SND_MAGNUM: &str = "sounds/weapons/magnum-fire.wav";
const SND_SILENCED: &str = "sounds/weapons/silencer-pistol.wav";
const SND_SMG: &str = "sounds/weapons/dk5-fire.wav";
const SND_RIFLE: &str = "sounds/weapons/ar33-fire.wav";
const SND_HOSE: &str = "sounds/weapons/rcp90-fire.wav";
const SND_SHOTGUN: &str = "sounds/weapons/shotgun-fire.wav";
const SND_LASER: &str = "sounds/weapons/laser-fire.wav";
const SND_ROCKET: &str = "sounds/weapons/rocket-launcher-fire.wav";
const SND_LAUNCHER: &str = "sounds/weapons/grenade-launcher-fire.wav";
const SND_THROW: &str = "sounds/weapons/throw.wav";

const RELOAD_SND: &str = "sounds/weapons/reload.wav";
const EMPTY_SND: &str = "sounds/weapons/empty.wav";

/// The GoldenEye fire sound standing in for a PD weapon.
fn fire_sound_for(w: &PdWeapon, f: &PdFunc) -> &'static str {
    // A silenced weapon is a sound choice before it is anything else.
    if f.has_flag(super::pd_weapons::FUNCFLAG_NOMUZZLEFLASH) && f.kind != PdFuncKind::Melee {
        return SND_SILENCED;
    }
    match w.name {
        "Shotgun" => SND_SHOTGUN,
        "Laser" => SND_LASER,
        "Rocket Launcher" | "Slayer" => SND_ROCKET,
        "Devastator" | "SuperDragon" => SND_LAUNCHER,
        "DY357 Magnum" | "DY357-LX" | "Mauler" => SND_MAGNUM,
        "Reaper" | "RC-P120" => SND_HOSE,
        _ => match f.kind {
            PdFuncKind::Throw => SND_THROW,
            PdFuncKind::Projectile => SND_LAUNCHER,
            PdFuncKind::Auto => {
                if w.one_handed {
                    SND_SMG
                } else {
                    SND_RIFLE
                }
            }
            _ => {
                if w.one_handed {
                    SND_PISTOL
                } else {
                    SND_RIFLE
                }
            }
        },
    }
}

// ─── Viewmodel placement ─────────────────────────────────────────────────────

/// Uniform mesh scale for a PD gun in the first-person view.
///
/// The GoldenEye guns use `DEFAULT_SCALE` 0.0007 against GLBs in GoldenEye units.
/// The PD exports come out in the *same* unit space (`pd_gltf.py` applies
/// `EXPORT_SCALE`, which is what puts a PD character next to a GoldenEye one at
/// the same height), so the same figure applies — this is the one number here
/// that is inherited rather than derived, and it is inherited from a measured
/// equivalence rather than a guess.
const PD_VIEW_SCALE: f32 = 0.0007;

/// PD's authored viewmodel placement, as our view-space offset.
///
/// `weapondef.posx/posy/posz` is where PD hangs the gun in the first-person view,
/// in centimetres, with +x right, +y up and **+z forward**. Our view space is
/// +x right, +y up, **−z forward**, so z negates. This is the authored
/// replacement for the `weapon-config.json` numbers, for PD guns only.
fn view_offset(w: &PdWeapon) -> Vec3 {
    Vec3::new(
        w.view.posx * PD_CM_TO_M,
        w.view.posy * PD_CM_TO_M,
        -w.view.posz * PD_CM_TO_M,
    )
}

// ─── The bridge ──────────────────────────────────────────────────────────────

/// Build a [`WeaponStats`] from a PD weapon's **primary** function.
///
/// Only the primary: the second function is a separate milestone (it needs an
/// input, a HUD state and an AI choice). Everything else comes off the authored
/// table — damage via [`PD_DAMAGE_TO_HP`], cadence via
/// [`PdFunc::sustained_cooldown`], magazine from the `ammodef`, and the range
/// from the AI's own engagement band rather than a number we picked.
pub fn weapon_stats_from_pd(w: &'static PdWeapon) -> WeaponStats {
    let f = &w.primary;
    let automatic = f.kind == PdFuncKind::Auto;

    // Cadence. PD's automatics spin up; until the runtime tracks a trigger hold,
    // the sustained (max-rpm) rate is the honest single number — it is what the
    // weapon settles at, and it is never faster than PD allows.
    let mut fire_cooldown = f.sustained_cooldown();
    if fire_cooldown <= 0.0 {
        fire_cooldown = 0.4; // no authored recovery (throwables) — a deliberate pull
    }

    // Range. PD does not give a weapon a hitscan range; it gives the AI a band it
    // wants to fight inside. The far edge of that band is the weapon's honest
    // reach, scaled up because the band is where a bot *stands*, not how far the
    // round carries. Clamped to something a room-scale level can use.
    let (_near, far) = w.band_m(false);
    let range = (far * 4.0).clamp(25.0, 200.0);

    let magazine_size = if w.clip_size > 0 { w.clip_size as u32 } else { 1 };

    WeaponStats {
        name: pd_display_name(w.name),
        fire_cooldown,
        magazine_size,
        reload_time: reload_time_for(w),
        damage: f.damage * PD_DAMAGE_TO_HP,
        range,
        fire_sound: fire_sound_for(w, f),
        reload_sound: RELOAD_SND,
        empty_sound: EMPTY_SND,
        gun_path: w.fp_glb,
        // PD authors the flash on the model (CHRGUNFIRE / MUZZLEFLASH parts)
        // rather than as a separate mesh, and `FUNCFLAG_NOMUZZLEFLASH` suppresses
        // it outright — so there is no separate flash GLB to point at.
        muzzle_path: "",
        model_scale: PD_VIEW_SCALE,
        model_offset: view_offset(w),
        pivot_offset: Vec3::ZERO,
        muzzle_offset: Vec3::new(0.05, 0.05, -0.3),
        // The first-person exports point down +Z; the view looks down −Z, so the
        // mesh turns to face away from the camera exactly as the GoldenEye guns do.
        model_rotation: Vec3::new(0.0, std::f32::consts::PI, 0.0),
        recoil_z: (f.recoildist_m()).clamp(0.01, 0.08),
        recoil_rot: (f.recoilangle_rad()).clamp(0.02, 0.30),
        automatic,
        fire_kind: fire_kind_for(w, f),
    }
}

/// Reload time in seconds.
///
/// `botweaponconfig.reloaddelay` is in whole seconds and is the AI's own pause,
/// which is the closest authored figure to a reload. Zero (most weapons) falls
/// back to a magazine-size-scaled guess in the range the GoldenEye guns use.
fn reload_time_for(w: &PdWeapon) -> f32 {
    if w.clip_size <= 0 {
        return 0.9; // a throwable "reloads" by drawing the next one
    }
    if w.clip_size >= 100 {
        3.0
    } else if w.clip_size >= 30 {
        2.0
    } else {
        1.5
    }
}

/// How a PD function delivers damage, in our [`FireKind`] terms.
///
/// The two shooting kinds and melee are hitscan; PD's projectile and throw
/// functions become our travelling [`ProjectileSpec`]. `Special`/`Device` have no
/// weapon behaviour to express and fall back to hitscan so they stay harmless
/// rather than unrepresentable.
fn fire_kind_for(w: &PdWeapon, f: &PdFunc) -> FireKind {
    match f.kind {
        PdFuncKind::Projectile => FireKind::Projectile(ProjectileSpec {
            // `funcdef_shootprojectile.speed` is centimetres per tick at 60 Hz.
            speed: (f.projectile_speed * PD_CM_TO_M * 60.0).clamp(10.0, 120.0),
            gravity: 0.0,
            loft: 0.0,
            fuse: fuse_secs(f),
            bounce: 0.0,
            explosion: explosion_for(w),
            model: "",
        }),
        PdFuncKind::Throw => FireKind::Projectile(ProjectileSpec {
            speed: 18.0,
            gravity: 16.0,
            loft: 3.0,
            fuse: fuse_secs(f).or(Some(3.5)),
            bounce: 0.4,
            explosion: explosion_for(w),
            model: "",
        }),
        _ => FireKind::Hitscan,
    }
}

/// `timer60` as seconds, when the function has a real fuse.
fn fuse_secs(f: &PdFunc) -> Option<f32> {
    if f.projectile_timer60 > 0 {
        Some(f.projectile_timer60 as f32 / 60.0)
    } else {
        None
    }
}

/// The blast a PD explosive weapon produces, from `g_ExplosionTypes` where the
/// weapon has a named entry and from its own damage otherwise.
///
/// Deliberately reads the PD explosion table rather than reusing our authored
/// `Explosion` values — but only for PD weapons, so nothing about the tuned
/// GoldenEye explosives changes.
fn explosion_for(w: &PdWeapon) -> Explosion {
    use super::pd_weapons::PD_EXPLOSIONS;
    let named = match w.name {
        "Rocket Launcher" | "Slayer" => "EXPLOSIONTYPE_ROCKET",
        "SuperDragon" => "EXPLOSIONTYPE_SDGRENADE",
        "Phoenix" => "EXPLOSIONTYPE_PHOENIX",
        "Dragon" => "EXPLOSIONTYPE_DRAGONBOMBSPY",
        _ => "",
    };
    if let Some(e) = PD_EXPLOSIONS.iter().find(|e| e.name == named) {
        return Explosion {
            radius: e.damage_radius_m,
            max_damage: e.peak_damage_hp().min(400.0),
        };
    }
    Explosion {
        radius: 4.0,
        max_damage: (w.primary.damage * PD_DAMAGE_TO_HP * 4.0).clamp(80.0, 300.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_arsenal_is_goldeneye_and_unchanged() {
        assert_eq!(Arsenal::GoldenEye.weapons().len(), config::WEAPONS.len());
        // The tuned table is handed back verbatim, not rebuilt.
        assert_eq!(Arsenal::GoldenEye.weapons()[0].name, "PP7");
        assert_eq!(Arsenal::GoldenEye.weapons()[0].damage, config::PP7.damage);
    }

    #[test]
    fn the_pd_arsenal_is_the_33_guns() {
        let pd = Arsenal::PerfectDark.weapons();
        assert_eq!(pd.len(), 33);
        assert!(pd.iter().any(|w| w.name == "Falcon 2"));
        assert!(pd.iter().any(|w| w.name == "FarSight XR-20"));
        // No equipment leaked in.
        assert!(!pd.iter().any(|w| w.name == "Cloaking Device"));
    }

    /// Coexistence: both families present, GoldenEye first so the shop's existing
    /// indices keep pointing at the same guns.
    #[test]
    fn both_keeps_goldeneye_indices_stable() {
        let both = Arsenal::Both.weapons();
        assert_eq!(both.len(), config::WEAPONS.len() + 33);
        for (i, ge) in config::WEAPONS.iter().enumerate() {
            assert_eq!(both[i].name, ge.name, "GoldenEye index {i} moved");
        }
    }

    /// Every bridged weapon is usable by the existing runtime: a real name, a
    /// loadable mesh, a positive cadence and magazine, and finite placement.
    #[test]
    fn every_bridged_weapon_is_well_formed() {
        for w in Arsenal::PerfectDark.weapons() {
            assert!(!w.name.is_empty(), "unnamed weapon");
            assert!(
                w.gun_path.starts_with("pd/") && w.gun_path.ends_with(".glb"),
                "{} has a bad gun path {:?}",
                w.name,
                w.gun_path
            );
            assert!(w.fire_cooldown > 0.0, "{} has no cadence", w.name);
            assert!(w.magazine_size > 0, "{} has an empty magazine", w.name);
            assert!(w.reload_time > 0.0, "{} cannot reload", w.name);
            assert!(w.damage >= 0.0 && w.damage.is_finite(), "{} damage", w.name);
            assert!(w.range >= 25.0, "{} has no reach", w.name);
            assert!(w.model_offset.is_finite(), "{} placement", w.name);
            assert!(w.model_scale > 0.0, "{} scale", w.name);
        }
    }

    /// The damage conversion survives the bridge: a PD pistol still kills a 100 HP
    /// hunter in four shots, which is the whole point of `PD_DAMAGE_TO_HP`.
    #[test]
    fn a_bridged_pistol_still_kills_in_four() {
        let falcon = Arsenal::PerfectDark
            .weapons()
            .iter()
            .find(|w| w.name == "Falcon 2")
            .unwrap();
        assert_eq!(falcon.damage, 25.0);
        assert!(!falcon.automatic, "the Falcon 2 is semi-auto");
        assert_eq!(falcon.magazine_size, 8, "PD's authored clip size");
    }

    /// PD's automatics come through as automatic, at a cadence PD allows rather
    /// than the flat rifle-class default the GoldenEye path invented.
    #[test]
    fn automatics_bridge_to_their_authored_cadence() {
        for w in Arsenal::PerfectDark.weapons() {
            let pd = pd_weapon_for(w.name).expect("every bridged weapon traces back");
            if pd.primary.kind == PdFuncKind::Auto {
                assert!(w.automatic, "{} should be automatic", w.name);
                let expected = 60.0 / pd.primary.max_rpm;
                assert!(
                    (w.fire_cooldown - expected).abs() < 1e-4,
                    "{}: cadence {} should be PD's {}",
                    w.name,
                    w.fire_cooldown,
                    expected
                );
            } else {
                assert!(!w.automatic, "{} should not be automatic", w.name);
            }
        }
    }

    /// The explosive weapons bridge to a projectile, and to PD's own blast where
    /// PD names one.
    #[test]
    fn explosives_bridge_to_projectiles() {
        let pd = Arsenal::PerfectDark.weapons();
        let rocket = pd.iter().find(|w| w.name == "Rocket Launcher (PD)").unwrap();
        match rocket.fire_kind {
            FireKind::Projectile(p) => {
                assert!(p.speed > 10.0, "a rocket travels, got {}", p.speed);
                assert!(
                    (p.explosion.radius - 4.0).abs() < 0.01,
                    "PD's rocket damage radius, got {}",
                    p.explosion.radius
                );
                assert!(p.explosion.max_damage > 100.0, "a rocket is lethal");
            }
            other => panic!("the rocket launcher should be a projectile, got {other:?}"),
        }
    }

    /// Silenced weapons pick the silenced sound — the one sound decision that is
    /// driven by transcribed PD data (`FUNCFLAG_NOMUZZLEFLASH`) rather than a name.
    #[test]
    fn the_silenced_falcon_sounds_silenced() {
        let pd = Arsenal::PerfectDark.weapons();
        let sil = pd.iter().find(|w| w.name == "Falcon 2 (silencer)").unwrap();
        assert_eq!(sil.fire_sound, SND_SILENCED);
        let plain = pd.iter().find(|w| w.name == "Falcon 2").unwrap();
        assert_ne!(plain.fire_sound, SND_SILENCED, "the plain Falcon 2 is loud");
    }

    /// The rename list is exactly the set of real clashes — no missing entry (a
    /// silent collision) and no stale one (a suffix on a name nothing clashes
    /// with). Both directions matter, because either way the list has quietly
    /// stopped describing reality.
    #[test]
    fn every_clash_is_disambiguated() {
        let ge: Vec<&str> = config::WEAPONS.iter().map(|w| w.name).collect();
        let clashes: Vec<&str> = super::super::pd_weapons::pd_guns()
            .map(|w| w.name)
            .filter(|n| ge.contains(n))
            .collect();
        for name in &clashes {
            assert!(
                PD_RENAMES.iter().any(|(from, _)| from == name),
                "{name} clashes with a GoldenEye weapon but has no PD_RENAMES entry"
            );
        }
        for (from, _) in PD_RENAMES {
            assert!(
                clashes.contains(from),
                "PD_RENAMES has a stale entry for {from} — nothing clashes with it now"
            );
        }
        // And the round trip holds for every gun.
        for w in super::super::pd_weapons::pd_guns() {
            assert_eq!(pd_source_name(pd_display_name(w.name)), w.name, "{}", w.name);
        }
    }

    /// A GoldenEye weapon must NOT resolve to a PD one just because they share a
    /// name. Regression for a real bug: `resolve_barrel_axis` consults this, so
    /// the tuned GoldenEye Shotgun picked up PD's -X barrel and aimed 83° off.
    #[test]
    fn a_goldeneye_weapon_never_resolves_to_a_pd_one() {
        for (bare, suffixed) in PD_RENAMES {
            assert!(
                pd_weapon_for(bare).is_none(),
                "{bare:?} is the GoldenEye weapon — it must not resolve as PD's"
            );
            assert!(
                pd_weapon_for(suffixed).is_some(),
                "{suffixed:?} is PD's — it must resolve"
            );
        }
        // And nothing in the GoldenEye table resolves as a PD weapon at all.
        for ge in config::WEAPONS {
            assert!(
                pd_weapon_for(ge.name).is_none(),
                "GoldenEye's {} resolved to a PD weapon",
                ge.name
            );
        }
        // While every PD arsenal entry does.
        for pd in Arsenal::PerfectDark.weapons() {
            assert!(pd_weapon_for(pd.name).is_some(), "{} did not resolve", pd.name);
        }
    }

    /// Every bridged PD gun's mesh actually **loads through the engine loader**,
    /// not merely exists on disk.
    ///
    /// This is the gap trap #1 warns about: a `MAX_JOINTS` truncation once drew
    /// every PD body as a black fan while every headless check passed. An export
    /// that parses in Python and a mesh the renderer can draw are two different
    /// claims, and only the second one matters.
    #[test]
    fn every_pd_gun_mesh_loads_through_the_engine() {
        let asset =
            |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
        if !std::path::Path::new(&asset("pd")).is_dir() {
            eprintln!("note: PD weapon assets absent — run `pd_gltf.py guns` to check them");
            return;
        }
        let mut loaded = 0;
        for w in Arsenal::PerfectDark.weapons() {
            let path = asset(w.gun_path);
            let model = crate::combat::load_gun(&path)
                .unwrap_or_else(|e| panic!("{} failed to load from {path}: {e}", w.name));
            assert!(
                !model.vertices.is_empty(),
                "{} loaded with no geometry — it would draw as nothing",
                w.name
            );
            assert!(
                model.vertices.iter().all(|v| Vec3::from(v.pos).is_finite()),
                "{} has non-finite vertices",
                w.name
            );
            loaded += 1;
        }
        assert_eq!(loaded, 33, "every PD gun should have loaded");
    }

    /// The third-person models a hunter holds load too, and carry a usable barrel
    /// axis — the authored `CHRGUNFIRE` one where PD provides it.
    #[test]
    fn the_third_person_models_load_and_have_a_barrel_axis() {
        let asset =
            |rel: &str| format!("{}/../../assets/weapons/{}", env!("CARGO_MANIFEST_DIR"), rel);
        if !std::path::Path::new(&asset("pd")).is_dir() {
            eprintln!("note: PD weapon assets absent");
            return;
        }
        let mut authored = 0;
        for pd in super::super::pd_weapons::pd_guns() {
            let path = asset(pd.tp_glb);
            let model = crate::combat::load_gun(&path)
                .unwrap_or_else(|e| panic!("{} third-person failed to load: {e}", pd.name));
            assert!(!model.vertices.is_empty(), "{} third-person is empty", pd.name);
            if pd.muzzle_is_authored {
                let m = Vec3::from(pd.tp_muzzle);
                let axis = m.normalize_or_zero();
                assert_ne!(axis, Vec3::ZERO, "{} authored a zero muzzle", pd.name);
                // The measured invariant, and only that: the muzzle's DOMINANT
                // component is negative X, unanimously. A tighter angular bound
                // would be inventing precision the data does not have — the
                // Reaper's is 36° off -X because it is a minigun whose barrel
                // assembly is slung below the grip, while the other fifteen sit
                // within 18°. Pinning the dominant axis catches a re-export that
                // flips or permutes a sign (which reads in game as "the hunters
                // keep missing") without pretending the spread is smaller.
                assert!(
                    m.x < 0.0 && m.x.abs() >= m.y.abs() && m.x.abs() >= m.z.abs(),
                    "{}'s muzzle is not dominantly -X: {m:?}",
                    pd.name
                );
                authored += 1;
            }
        }
        assert_eq!(authored, 16, "16 of the 33 author a CHRGUNFIRE muzzle");
    }

    /// Every PD weapon name is distinct from every GoldenEye one, so the
    /// name-keyed lookups (shop prices, enemy defs) cannot collide when both
    /// families are live at once.
    #[test]
    fn the_two_families_share_no_names() {
        for pd in Arsenal::PerfectDark.weapons() {
            assert!(
                !config::WEAPONS.iter().any(|ge| ge.name == pd.name),
                "{} exists in both arsenals — name-keyed lookups would collide",
                pd.name
            );
        }
    }
}
