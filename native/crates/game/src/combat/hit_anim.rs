//! **Per-hit-part death and injury selection** — Perfect Dark's `animtablerow`
//! tables, transcribed from `game/chraction.c:228+` and indexed by
//! `g_AnimTablesHuman` (`chraction.c:747`).
//!
//! # What this replaces
//!
//! Our hunters classify a hit by *height above the feet* into head / torso / legs,
//! then random-pick from a small clip list per zone, and random-pick a death from all
//! 17. Perfect Dark classifies by **which body part the shot actually hit** — 15 of
//! them — and gives each part its own death table and its own injury table.
//!
//! That is a better classifier than height (it separates a forearm from a thigh at
//! the same height), but the more interesting difference is what the rows carry:
//!
//! * **`endframe`** — an injury is frequently *the first few frames of a death
//!   animation*. A torso hit plays 20 frames of `ANIM_DEATH_0022` and stops; a pelvis
//!   hit plays 10 frames of `ANIM_DEATH_0023` at quarter speed. Our mixer could not
//!   express that at all, which is why [`AnimPlayer::play_once_scaled`] exists now.
//! * **`speed`** — per row, so the same clip reads as a heavy fall in one table and a
//!   faster crumple in another.
//! * **`thudframe1`/`thudframe2`** — the frames a falling body hits the floor, for
//!   the impact sound. Recorded here; nothing drives per-frame animation events yet.
//!
//! # The `flip` flag, and why dropping it costs less than it looks
//!
//! Every row has a `flip` bool: PD ships one animation and mirrors it at runtime, so
//! `g_DeathAnimsHumanRfoot` is simply `g_DeathAnimsHumanLfoot` flipped. We have no
//! pose mirroring, so **only `flip == false` rows are kept** — those are the ones
//! already correct for their side.
//!
//! This costs less than it should because GoldenEye *did* ship the mirrors as
//! separate animations, and our clip set is drawn from them: `ANIM_0014` and
//! `ANIM_0015` are the left- and right-leg reactions as distinct clips, likewise
//! `000E`/`000F` (biceps), `0010`/`0011` (forearms), `0012`/`0013` (hands). So both
//! sides keep a correctly-sided injury.
//!
//! Where a table's rows were *all* mirrors it would be left empty, so a part with no
//! surviving rows falls back to its [mirror partner](HitPart::mirror) — which is the
//! same animation the flip would have produced, just unmirrored. Implementing real
//! mirroring is the one remaining piece of this port, and it slots in here: set the
//! dropped rows back and flip at playback.

use super::enemy_weapons::{LEFT_HAND_BONE, RIGHT_HAND_BONE};

/// Perfect Dark's animation speed is **frames per 60 Hz tick**, and `0.5` — the
/// value nearly every row carries — is 30 animation-frames per second. Our clips are
/// exported at 30 fps and played at `1.0`, so a row's speed converts by doubling.
/// (Same derivation as `pd_gltf.py`'s `DEFAULT_FPS`; see `chr_action_go_to_position`,
/// `chraction.c:2189`.)
const PD_SPEED_TO_OURS: f32 = 2.0;

/// Frames per second the clips were exported at, for converting `endframe` and
/// for reading `thudframe` cues off a playing clip's clock.
pub const PD_ANIM_FPS: f32 = 30.0;

/// Slot indices into the Perfect Dark hunter template (`world::PD_TEMPLATE_CLIPS`),
/// named by the animation they hold. `world::tests` cross-checks these against the
/// shipped filename list, so a re-ordered template cannot silently repoint a table.
mod slot {
    pub const HIT_L_BICEP: usize = 7; // ANIM_000E
    pub const HIT_R_BICEP: usize = 8; // ANIM_000F
    pub const HIT_L_FOREARM: usize = 9; // ANIM_0010
    pub const HIT_R_FOREARM: usize = 10; // ANIM_0011
    pub const HIT_L_HAND: usize = 11; // ANIM_0012
    pub const HIT_R_HAND: usize = 12; // ANIM_0013
    pub const HIT_L_LEG: usize = 13; // ANIM_0014
    pub const HIT_R_LEG: usize = 14; // ANIM_0015
    pub const DEATH_BICEP: usize = 19; // ANIM_008F
    pub const DEATH_THIGH: usize = 20; // ANIM_0092
    pub const DEATH_001A: usize = 22;
    pub const DEATH_024E: usize = 23;
    pub const DEATH_001C: usize = 24;
    pub const DEATH_0250: usize = 25;
    pub const DEATH_0253: usize = 26;
    pub const DEATH_0252: usize = 27;
    pub const DEATH_0020: usize = 28;
    pub const DEATH_0021: usize = 29;
    pub const DEATH_0022: usize = 30;
    pub const DEATH_0023: usize = 31;
    pub const DEATH_0024: usize = 32;
    pub const DEATH_0025: usize = 33;
    pub const DEATH_0038: usize = 34;
    pub const DEATH_STOMACH_LONG: usize = 35;
}

/// One `struct animtablerow`, minus the `flip` we cannot honour yet.
#[derive(Clone, Copy, Debug)]
pub struct AnimRow {
    /// Template slot to play.
    pub slot: usize,
    /// Playback rate, already converted out of PD's 60 Hz units.
    pub speed: f32,
    /// Stop here (seconds), or `None` to play the clip out — PD's `endframe`.
    pub end: Option<f32>,
    /// Frames at which a falling body strikes the floor (`thudframe1/2`, `-1` for
    /// none). Kept for the impact sound; nothing consumes them yet.
    pub thud: (f32, f32),
}

/// Build a row the way the source writes one: `(slot, speed, endframe, thud1, thud2)`
/// with PD's own units, converted here so the tables below read like the C.
const fn row(slot: usize, speed: f32, end_frame: f32, thud1: f32, thud2: f32) -> AnimRow {
    AnimRow {
        slot,
        speed: speed * PD_SPEED_TO_OURS,
        end: if end_frame < 0.0 { None } else { Some(end_frame / PD_ANIM_FPS) },
        thud: (thud1, thud2),
    }
}


// ─── Death tables (`g_DeathAnimsHuman*`, chraction.c:228+) ───────────────────
// One const per table so the arms below can hand out `&'static` slices. Rows with
// `flip == true` are omitted (see the module docs); a table left empty by that is
// resolved through `HitPart::mirror`.

/// `g_DeathAnimsHumanLfoot` / `Lshin` / `Lthigh` — the backward spin.
const D_LEFT_LIMB: [AnimRow; 1] = [row(slot::DEATH_0020, 0.5, -1.0, 26.0, -1.0)];
/// `g_DeathAnimsHumanRthigh` — the doubling-over rows, unmirrored on this side.
const D_RTHIGH: [AnimRow; 2] = [
    row(slot::DEATH_STOMACH_LONG, 0.5, -1.0, -1.0, -1.0),
    row(slot::DEATH_THIGH, 0.4, -1.0, 42.0, 103.0),
];
/// `g_DeathAnimsHumanPelvis`.
const D_PELVIS: [AnimRow; 7] = [
    row(slot::DEATH_001A, 0.5, -1.0, 55.0, 39.0),
    row(slot::DEATH_001C, 0.5, -1.0, 29.0, -1.0),
    row(slot::DEATH_0021, 0.5, -1.0, 97.0, 64.0),
    row(slot::DEATH_0023, 0.5, -1.0, 31.0, -1.0),
    row(slot::DEATH_0024, 0.5, -1.0, 36.0, -1.0),
    row(slot::DEATH_0025, 0.5, -1.0, 28.0, -1.0),
    row(slot::DEATH_0250, 0.5, -1.0, 65.0, 105.0),
];
/// `g_DeathAnimsHumanHead` — the richest table.
const D_HEAD: [AnimRow; 10] = [
    row(slot::DEATH_001A, 0.5, -1.0, 55.0, 39.0),
    row(slot::DEATH_001C, 0.5, -1.0, 29.0, -1.0),
    row(slot::DEATH_0020, 0.5, -1.0, 26.0, -1.0),
    row(slot::DEATH_0021, 0.5, -1.0, 97.0, 64.0),
    row(slot::DEATH_0022, 0.5, -1.0, 94.0, 66.0),
    row(slot::DEATH_0023, 0.5, -1.0, 31.0, -1.0),
    row(slot::DEATH_0024, 0.5, -1.0, 36.0, -1.0),
    row(slot::DEATH_0025, 0.5, -1.0, 28.0, -1.0),
    row(slot::DEATH_0038, 0.5, -1.0, -1.0, -1.0),
    row(slot::DEATH_0252, 0.5, -1.0, 83.0, 150.0),
];
/// `g_DeathAnimsHumanRbicep` keeps `ANIM_008F` unmirrored.
const D_RBICEP: [AnimRow; 1] = [row(slot::DEATH_BICEP, 0.45, -1.0, 52.0, -1.0)];
/// `g_DeathAnimsHumanTorso`.
const D_TORSO: [AnimRow; 10] = [
    row(slot::DEATH_001A, 0.5, -1.0, 55.0, 39.0),
    row(slot::DEATH_001C, 0.5, -1.0, 29.0, -1.0),
    row(slot::DEATH_0020, 0.5, -1.0, 26.0, -1.0),
    row(slot::DEATH_0021, 0.5, -1.0, 97.0, 64.0),
    row(slot::DEATH_0022, 0.5, -1.0, 94.0, 66.0),
    row(slot::DEATH_0023, 0.5, -1.0, 31.0, -1.0),
    row(slot::DEATH_0024, 0.5, -1.0, 36.0, -1.0),
    row(slot::DEATH_0025, 0.5, -1.0, 28.0, -1.0),
    row(slot::DEATH_024E, 0.4, -1.0, 60.0, -1.0),
    row(slot::DEATH_0253, 0.5, -1.0, 22.0, -1.0),
];

// ─── Injury tables (`g_InjuryAnimsHuman*`) ──────────────────────────────────

const I_LEFT_LEG: [AnimRow; 1] = [row(slot::HIT_L_LEG, 0.5, -1.0, -1.0, -1.0)];
const I_RIGHT_LEG: [AnimRow; 1] = [row(slot::HIT_R_LEG, 0.5, -1.0, -1.0, -1.0)];
/// `g_InjuryAnimsHumanRthigh` — plus the doubling-over slice.
const I_RTHIGH: [AnimRow; 2] = [
    row(slot::HIT_R_LEG, 0.5, -1.0, -1.0, -1.0),
    row(slot::DEATH_STOMACH_LONG, 0.4, 20.0, -1.0, -1.0),
];
/// `g_InjuryAnimsHumanPelvis` — every row is the opening frames of a death, the
/// quarter-speed `ANIM_DEATH_0023` included.
const I_PELVIS: [AnimRow; 3] = [
    row(slot::DEATH_0022, 0.5, 20.0, -1.0, -1.0),
    row(slot::DEATH_001A, 0.5, 15.0, -1.0, -1.0),
    row(slot::DEATH_0023, 0.25, 10.0, -1.0, -1.0),
];
/// `g_InjuryAnimsHumanHead` / `Torso` — the same two death slices.
const I_TORSO: [AnimRow; 2] = [
    row(slot::DEATH_0022, 0.5, 20.0, -1.0, -1.0),
    row(slot::DEATH_001A, 0.5, 15.0, -1.0, -1.0),
];
const I_LHAND: [AnimRow; 1] = [row(slot::HIT_L_HAND, 0.5, -1.0, -1.0, -1.0)];
const I_RHAND: [AnimRow; 1] = [row(slot::HIT_R_HAND, 0.5, -1.0, -1.0, -1.0)];
const I_LFOREARM: [AnimRow; 1] = [row(slot::HIT_L_FOREARM, 0.5, -1.0, -1.0, -1.0)];
const I_RFOREARM: [AnimRow; 1] = [row(slot::HIT_R_FOREARM, 0.5, -1.0, -1.0, -1.0)];
/// `g_InjuryAnimsHumanLbicep` — the shoulder recoil, or a death slice.
const I_LBICEP: [AnimRow; 2] = [
    row(slot::HIT_L_BICEP, 0.5, -1.0, -1.0, -1.0),
    row(slot::DEATH_0022, 0.5, 20.0, -1.0, -1.0),
];
const I_RBICEP: [AnimRow; 1] = [row(slot::HIT_R_BICEP, 0.5, -1.0, -1.0, -1.0)];
const NO_ROWS: [AnimRow; 0] = [];

/// Where a shot landed, as Perfect Dark counts it (`HITPART_*`,
/// `include/constants.h:1394`). `TAIL` (Skedar) and the non-anatomical values are
/// omitted — our hunters are all `RACE_HUMAN`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitPart {
    LFoot,
    LShin,
    LThigh,
    RFoot,
    RShin,
    RThigh,
    Pelvis,
    Head,
    LHand,
    LForearm,
    LBicep,
    RHand,
    RForearm,
    RBicep,
    Torso,
}

impl HitPart {
    /// The part a rig bone belongs to. Bone roles are the same on both character
    /// families — the Perfect Dark exporter renames PD's parts onto the GoldenEye
    /// names (`pd_gltf.py`'s `PART_TO_BONE`), so `Bone_9` is the right hand on
    /// either. `None` for a bone with no anatomical meaning here.
    /// A Perfect Dark body also carries `Blend_n` joints — the midpoint frames at
    /// each seam, which are `Bone_n`'s sibling and carry half its rotation. Skin at a
    /// seam is often weighted most heavily to one of those, so they resolve to the
    /// same body part as the bone they belong to; otherwise a shot to the elbow
    /// would land on no part at all.
    pub fn for_bone(bone: &str) -> Option<HitPart> {
        let bone = match bone.strip_prefix("Blend_") {
            Some(n) => return HitPart::for_bone(&format!("Bone_{n}")),
            None => bone,
        };
        Some(match bone {
            "Bone_1" => HitPart::Pelvis,
            "Bone_2" => HitPart::Torso,
            "Bone_3" => HitPart::Head,
            "Bone_4" => HitPart::LBicep,
            "Bone_5" => HitPart::RBicep,
            "Bone_6" => HitPart::LForearm,
            "Bone_7" => HitPart::RForearm,
            b if b == LEFT_HAND_BONE => HitPart::LHand, // Bone_8
            b if b == RIGHT_HAND_BONE => HitPart::RHand, // Bone_9
            "Bone_10" => HitPart::LThigh,
            "Bone_11" => HitPart::RThigh,
            "Bone_12" => HitPart::LShin,
            "Bone_13" => HitPart::RShin,
            "Bone_14" => HitPart::LFoot,
            "Bone_15" => HitPart::RFoot,
            _ => return None,
        })
    }

    /// The same part on the other side — the row a `flip` would have produced.
    /// Self-symmetric parts return themselves.
    pub fn mirror(self) -> HitPart {
        use HitPart::*;
        match self {
            LFoot => RFoot,
            RFoot => LFoot,
            LShin => RShin,
            RShin => LShin,
            LThigh => RThigh,
            RThigh => LThigh,
            LHand => RHand,
            RHand => LHand,
            LForearm => RForearm,
            RForearm => LForearm,
            LBicep => RBicep,
            RBicep => LBicep,
            other => other,
        }
    }

    /// **A hand holding a gun is remapped to the forearm**, because the hand injury
    /// animations assume an empty hand (`chraction.c:3508`). Our hunters always hold
    /// a weapon in the right hand, and in both when dual-wielding.
    pub fn with_gun_in_hand(self, dual: bool) -> HitPart {
        match self {
            HitPart::RHand => HitPart::RForearm,
            HitPart::LHand if dual => HitPart::LForearm,
            other => other,
        }
    }

    /// This part's death rows, falling back to the mirror partner's when every row
    /// for this side was a `flip` we cannot produce (see the module docs).
    pub fn deaths(self) -> &'static [AnimRow] {
        let own = self.deaths_raw();
        if own.is_empty() {
            self.mirror().deaths_raw()
        } else {
            own
        }
    }

    /// This part's injury rows, with the same mirror fallback.
    pub fn injuries(self) -> &'static [AnimRow] {
        let own = self.injuries_raw();
        if own.is_empty() {
            self.mirror().injuries_raw()
        } else {
            own
        }
    }

    fn deaths_raw(self) -> &'static [AnimRow] {
        use HitPart::*;
        match self {
            LFoot | LShin | LThigh | LHand | LForearm | LBicep => &D_LEFT_LIMB,
            RThigh => &D_RTHIGH,
            // Pure mirrors of the left side — resolved by the fallback in `deaths`.
            RFoot | RShin | RHand | RForearm => &NO_ROWS,
            Pelvis => &D_PELVIS,
            Head => &D_HEAD,
            RBicep => &D_RBICEP,
            Torso => &D_TORSO,
        }
    }

    fn injuries_raw(self) -> &'static [AnimRow] {
        use HitPart::*;
        match self {
            LFoot | LShin | LThigh => &I_LEFT_LEG,
            RFoot | RShin => &I_RIGHT_LEG,
            RThigh => &I_RTHIGH,
            Pelvis => &I_PELVIS,
            Head | Torso => &I_TORSO,
            LHand => &I_LHAND,
            RHand => &I_RHAND,
            LForearm => &I_LFOREARM,
            RForearm => &I_RFOREARM,
            LBicep => &I_LBICEP,
            RBicep => &I_RBICEP,
        }
    }
}

/// Every part, for tests and for iterating the tables.
pub const ALL_PARTS: &[HitPart] = &[
    HitPart::LFoot,
    HitPart::LShin,
    HitPart::LThigh,
    HitPart::RFoot,
    HitPart::RShin,
    HitPart::RThigh,
    HitPart::Pelvis,
    HitPart::Head,
    HitPart::LHand,
    HitPart::LForearm,
    HitPart::LBicep,
    HitPart::RHand,
    HitPart::RForearm,
    HitPart::RBicep,
    HitPart::Torso,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every part resolves to a non-empty death AND injury table — the mirror
    /// fallback exists precisely so dropping `flip` rows cannot leave a hit with no
    /// reaction to play.
    #[test]
    fn every_part_has_a_death_and_an_injury() {
        for &p in ALL_PARTS {
            assert!(!p.deaths().is_empty(), "{p:?} has a death");
            assert!(!p.injuries().is_empty(), "{p:?} has an injury");
        }
    }

    /// The parts whose rows were all mirrors fall back to the other side, and the
    /// fallback really is the same animation the flip would have produced.
    #[test]
    fn mirrored_parts_fall_back_to_their_partner() {
        assert!(HitPart::RFoot.deaths_raw().is_empty(), "the right foot's rows are all flips");
        assert_eq!(
            HitPart::RFoot.deaths()[0].slot,
            HitPart::LFoot.deaths()[0].slot,
            "and it falls back to the same clip, unmirrored",
        );
        // Mirroring is an involution, so no part can fall back into a loop.
        for &p in ALL_PARTS {
            assert_eq!(p.mirror().mirror(), p, "{p:?} mirrors back to itself");
        }
    }

    /// `endframe` is what makes an injury out of a death: the torso flinch is the
    /// first 20 frames of `ANIM_DEATH_0022`, not the whole fall.
    #[test]
    fn injuries_are_slices_of_deaths_where_pd_says_so() {
        let torso = HitPart::Torso.injuries();
        assert!(torso.iter().all(|r| r.end.is_some()), "every torso flinch is cut short");
        let first = torso[0];
        assert_eq!(first.slot, slot::DEATH_0022, "and it is a death animation");
        assert!((first.end.unwrap() - 20.0 / 30.0).abs() < 1e-4, "cut at frame 20");
        assert!(
            HitPart::Torso.deaths().iter().any(|r| r.slot == slot::DEATH_0022),
            "the same clip plays in full as a death",
        );
        // A limb injury is a purpose-made reaction and plays out.
        assert!(HitPart::LForearm.injuries()[0].end.is_none());
    }

    /// PD's speeds are frames-per-60Hz-tick, so the near-universal `0.5` has to come
    /// out as our `1.0` or every reaction plays at half speed.
    #[test]
    fn pd_speeds_convert_to_our_frame_rate() {
        let normal = HitPart::Torso.deaths()[0];
        assert!((normal.speed - 1.0).abs() < 1e-6, "0.5 in PD is normal speed here");
        let quarter = HitPart::Pelvis.injuries()[2];
        assert!((quarter.speed - 0.5).abs() < 1e-6, "0.25 in PD is half speed here");
        for &p in ALL_PARTS {
            for r in p.deaths().iter().chain(p.injuries()) {
                assert!(r.speed > 0.0 && r.speed <= 2.0, "{p:?} row speed {} is sane", r.speed);
            }
        }
    }

    /// A hand holding a gun is remapped to the forearm; an empty left hand is not.
    #[test]
    fn a_gun_hand_is_remapped_to_the_forearm() {
        assert_eq!(HitPart::RHand.with_gun_in_hand(false), HitPart::RForearm);
        assert_eq!(HitPart::LHand.with_gun_in_hand(false), HitPart::LHand);
        assert_eq!(HitPart::LHand.with_gun_in_hand(true), HitPart::LForearm);
        assert_eq!(HitPart::Head.with_gun_in_hand(true), HitPart::Head);
    }

    /// Bone names map to the parts the rig actually has, both sides distinct.
    #[test]
    fn bones_map_to_parts() {
        assert_eq!(HitPart::for_bone("Bone_3"), Some(HitPart::Head));
        assert_eq!(HitPart::for_bone(RIGHT_HAND_BONE), Some(HitPart::RHand));
        assert_eq!(HitPart::for_bone(LEFT_HAND_BONE), Some(HitPart::LHand));
        assert_eq!(HitPart::for_bone("Bone_14"), Some(HitPart::LFoot));
        assert_eq!(HitPart::for_bone("Bone_15"), Some(HitPart::RFoot));
        // A seam's blend joint belongs to the same part as its bone.
        assert_eq!(HitPart::for_bone("Blend_4"), Some(HitPart::LBicep));
        assert_eq!(HitPart::for_bone("Blend_15"), Some(HitPart::RFoot));
        assert_eq!(HitPart::for_bone("Bone_99"), None, "an unknown bone has no part");
        // All 15 rig bones are covered, and no two map to the same part.
        let parts: Vec<_> =
            (1..=15).filter_map(|i| HitPart::for_bone(&format!("Bone_{i}"))).collect();
        assert_eq!(parts.len(), 15, "every rig bone is a hit part");
        for &p in ALL_PARTS {
            assert!(parts.contains(&p), "{p:?} is reachable from a bone");
        }
    }
}
