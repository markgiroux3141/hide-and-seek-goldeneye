//! **`attackanimconfig`** — Perfect Dark's authored per-animation attack timing,
//! transcribed from `game/chraction.c:912+` (`struct attackanimconfig`,
//! `include/types.h:333`), plus the **32-slot direction tables** that choose between
//! the rows (`g_StandHeavyAttackAnims` / `Light` / `Dual`, `chraction.c:956+`).
//!
//! # Why this is a transcription and not a guess
//!
//! Our hunters' fire timing came from `anim_set::FIRE_TIMING` — hand-set windows
//! ported from the 3DS FPS JavaScript, keyed by the GoldenEye clip's hex id. Perfect
//! Dark authors the same thing properly: a row per animation giving when the
//! character may *aim*, when it may *shoot*, when it *recoils*, where the clip is
//! trimmed, and how far the aim may swing before the body has to lean.
//!
//! Those rows apply to us directly, because **the two games share one animation
//! bank** (see `tools/pd-assets/pd_animmap.py`): every animation named below is on
//! disk as a numbered slot of the Perfect Dark hunter template, decoded from PD's
//! own ROM. So this is not a port of *similar* data — it is the real table for the
//! exact animations the mixer loads.
//!
//! Comparing them is what showed the guess was wrong, and where:
//!
//! | Clip | `FIRE_TIMING` guess | Authored | |
//! |---|---|---|---|
//! | rifle `ANIM_0032` | frames 27–80 | shoot 30–81 | close |
//! | dual `ANIM_007A` | frames 28–65 | shoot 28–68 | close |
//! | pistol `ANIM_0041` | frames 63–66 | shoot 58–92 | **a 3-frame sliver inside a 34-frame window** |
//!
//! The pistol guess was so narrow that a pistol hunter often got one shot, or none,
//! out of a burst. And every row carries two things the guess had no way to express
//! at all: an **aim window wider than the shoot window** (the character swivels onto
//! you while still raising the gun — 20 → 58 on the pistol is 1.3 s of tracking
//! before the first round), and **anisotropic aim limits**.
//!
//! # The direction tables
//!
//! PD does not have one fire animation per weapon. It has 32 animation *groups* per
//! stance, indexed by the bearing from the character to its target — `chr_attack`
//! (`chraction.c:2825`):
//!
//! ```c
//! angle = chr_get_attack_entity_relative_angle(chr, attackflags, entityid);
//! groupindex = angle * 5.0937690734863f + 0.5f;   // 5.09377 == 32 / BADDTOR(360)
//! if (groupindex < 0 || groupindex > 31) groupindex = 0;
//! index = random() % animgroups[groupindex]->len;
//! animcfg = &animgroups[groupindex]->animcfg[index];
//! ```
//!
//! Adjacent slots share a group in runs, so the three human standing tables resolve
//! to only 4–6 distinct groups each ([`STAND_HEAVY`], [`STAND_LIGHT`],
//! [`STAND_DUAL`]). What the bearing actually buys is **time**: `shootstartframe`
//! grows with the turn the guard has to make. Facing you it fires on frame 23
//! (`ANIM_0002`); at your flank, frame 30 (`ANIM_0032`); with its back to you, frame
//! 39 of a 121-frame animation (`ANIM_0006`). Coming from behind buys real time, as
//! authored.
//!
//! ## Which way is positive
//!
//! `chr_get_angle_to_pos` (`chraction.c:13787`) returns `atan2f(dx, dz) - theta`
//! wrapped into `[0, BADDTOR(360))` — the same `atan2(x, z)` convention this game
//! uses for yaw, so the two are directly comparable. **Positive is the character's
//! left**, which the source says outright at `chraction.c:9313`:
//!
//! ```text
//! // aimendsideback positive is aiming left
//! // aimendsideback negative is aiming right
//! ```
//!
//! That is what makes the tables readable, and the tables agree with it: the
//! `DTOR(90)` rows sit at slots 10–15 (bearings 112°–169°, hard left) and their
//! `DTOR(270)` mirror partners at 16–21 (180°–236°, behind and right). Each table is
//! exactly symmetric under `i → 31 - i`.
//!
//! ## `angleoffset` is a turn tolerance, not an aim correction
//!
//! `angleoffset` states how far an animation's aim-zero sits off the body's facing.
//! PD does not correct for it — it *targets* it: the row's `angleoffset` is passed to
//! `chr_turn` as the turn **tolerance** (`chraction.c:10758`), which turns the body
//! until `bearing - angleoffset == 0`. So a `DTOR(90)` animation is played with the
//! body deliberately left facing 90° away from the target, and the animation's own
//! authored aim is what lands on it. `unk04` ([`AttackAnimConfig::turn_end_frame`])
//! is the frame that turn stops on.
//!
//! # What is deliberately not ported
//!
//! `chr_calculate_aimend` (`chraction.c:9071`) consumes these rows to drive shoulder
//! / back / lean offsets against PD's own skeleton and gun props. Our
//! [`AimOffsetLayer`](engine::skeletal::layers::AimOffsetLayer) already does that
//! job — it measures the real barrel and swings the chest — so what is taken here is
//! the *data* (windows and limits), not PD's solver.
//!
//! The `flip` path (`chr_attack` mirrors the index for the other-handed case) needs
//! pose mirroring, which we do not have. `freearmfrac*` (the gunless arm following
//! the gun arm) is recorded but unused — we have no free-arm layer yet.
//!
//! The `RACE_SKEDAR` half of every table is one group of `ANIM_034A` at all 32
//! slots — no Skedar in this game, and nothing directional to port from it.

use engine::skeletal::layers::AimCone;

use super::enemy_weapons::EnemyWeaponClass;

/// Frames per second the PD clips were exported at (`pd_gltf.py`'s `DEFAULT_FPS`,
/// derived from `chr_action_go_to_position`). Frame numbers in the table below are
/// animation frames, so this converts them to the seconds our mixer runs on.
pub const PD_ANIM_FPS: f32 = 30.0;

/// Rare's pi (`M_BADPI`, `include/math.h:5`) — the constant `BADDTOR` uses. Kept
/// rather than `std::f32::consts::PI` so the transcribed angles are the values the
/// game actually computed.
const BAD_PI: f32 = 3.141_092_6;

/// `BADDTOR(360)` — a full turn in Rare's radians. The direction-table arithmetic
/// uses this rather than `TAU`, because [`GROUP_SCALE`] was derived from it: PD's
/// literal `5.0937690734863` is `32 / BADDTOR(360)`, not `32 / 2π`. Wrapping with
/// the wrong one puts slot 31's boundary a tenth of a degree out.
pub const BAD_TAU: f32 = 2.0 * BAD_PI;

/// `BADDTOR` — degrees to radians through Rare's pi.
const fn baddtor(deg: f32) -> f32 {
    deg * BAD_PI / 180.0
}

/// `DTOR` — degrees to radians through the real pi, which is what the `angleoffset`
/// column uses (`DTOR(90)`, `DTOR(270)`), unlike the aim limits beside it.
const fn dtor(deg: f32) -> f32 {
    deg * std::f32::consts::PI / 180.0
}

/// One row of `struct attackanimconfig`, in the source's own field order.
///
/// The invariant the original author documented (`types.h:339`):
///
/// ```text
/// start <= aimstart <= shootstart <= recoilstart <= recoilend <= shootend <= aimend <= end
/// ```
///
/// Two rows in the real data break the recoil half of it — see
/// [`tests::rows_obey_the_authored_frame_ordering`].
#[derive(Clone, Copy, Debug)]
pub struct AttackAnimConfig {
    /// The Perfect Dark animation id this row configures — provenance, and what
    /// makes the row checkable against `pd_roster.json`.
    pub anim: &'static str,
    /// Which slot of the Perfect Dark hunter template (`world::PD_TEMPLATE_CLIPS`)
    /// holds [`Self::anim`]. The mixer addresses clips by index, so this is the join
    /// between the transcribed table and the exported assets — the same coupling
    /// `combat::hit_anim` already has, and pinned by the same kind of test.
    pub slot: usize,
    /// `unk04` — the frame the body's turn-toward-target stops on. PD passes it to
    /// `chr_turn` as `endanimframe` (`chraction.c:10758`) alongside
    /// [`Self::angle_offset`] as the tolerance.
    pub turn_end_frame: f32,
    /// How far this animation's aim-zero sits off the body's facing. `+` is the
    /// character's **left**. PD turns the body to `bearing - angleoffset` rather
    /// than correcting the animation (see the module docs).
    pub angle_offset: f32,
    /// Clip trim: start later / finish earlier than the raw animation.
    /// `end_frame < 0` means "to the end of the clip".
    pub start_frame: f32,
    pub end_frame: f32,
    /// The character fires during these frames, given line of sight.
    pub shoot: (f32, f32),
    /// Recoil frames, for single-shot pistols; `None` when the clip has none.
    ///
    /// This drives the visual kick for us. In PD it is *also* the fire window, but
    /// only down the `everytick` branch (`chraction.c:10805`), which `chr_attack`
    /// reaches for weapons firing under one tick per shot — "the only weapon that
    /// can enter this branch is the laser". The ordinary path fires on
    /// [`Self::shoot`], which is the one we take.
    pub recoil: Option<(f32, f32)>,
    /// When the character may swivel its aim — **wider than [`Self::shoot`]**: it
    /// can track while still raising its arm, and keeps tracking after the last
    /// round.
    pub aim: (f32, f32),
    /// Aim limits (radians). Past these the character leans back / forward rather
    /// than swivelling further. `left` positive, `right` negative in the source;
    /// stored here as positive magnitudes, which is what [`AimCone`] wants.
    pub max_up: f32,
    pub max_down: f32,
    pub max_left: f32,
    pub max_right: f32,
    /// When aiming up / down, the gunless arm moves by this fraction of the gun
    /// arm. Recorded for completeness — nothing drives a free arm yet.
    pub free_arm_up: f32,
    pub free_arm_down: f32,
}

impl AttackAnimConfig {
    /// The authored aim limits as a layer cone.
    pub fn cone(&self) -> AimCone {
        AimCone {
            up: self.max_up,
            down: self.max_down,
            left: self.max_left,
            right: self.max_right,
        }
    }
}

// ─── The rows ────────────────────────────────────────────────────────────────
// Field order below follows the source literal exactly, so a row can be diffed
// against `chraction.c` by eye. `turnangleperframe` is 0 on every human row, so it
// has no field here.

/// `var800656c0` (`chraction.c:912`) — two-handed, target dead ahead. The quickest
/// heavy attack there is: it shoots on frame 23, seven frames before the flank
/// animation and sixteen before the turn-around one.
pub const HEAVY_FRONT: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0002",
    slot: 36,
    turn_end_frame: 28.0,
    angle_offset: dtor(0.0),
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (23.0, 54.0),
    recoil: None,
    aim: (18.0, 54.0),
    max_up: baddtor(50.0),
    max_down: baddtor(30.0),
    max_left: baddtor(60.0),
    max_right: baddtor(20.0),
    free_arm_up: 1.6,
    free_arm_down: 1.8,
};

/// `var80065758[0]` (`chraction.c:920`) — the two-handed standing attack, reached
/// through `g_StandHeavyAttackAnims[RACE_HUMAN]` at the flanking slots. Our default
/// rifle fire clip.
pub const RIFLE: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0032",
    slot: 4,
    turn_end_frame: 37.0,
    angle_offset: dtor(0.0),
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (30.0, 81.0),
    recoil: None,
    aim: (25.0, 81.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 1.6,
    free_arm_down: 1.75,
};

/// `var80065758[1]` — the rifle group's second row, a shorter swing (shoot 22–61)
/// with a much tighter downward limit.
pub const HEAVY_FLANK: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0003",
    slot: 37,
    turn_end_frame: 27.0,
    angle_offset: dtor(0.0),
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (22.0, 61.0),
    recoil: None,
    aim: (17.0, 61.0),
    max_up: baddtor(50.0),
    max_down: baddtor(15.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 2.0,
    free_arm_down: 1.0,
};

/// `var80065918` (`chraction.c:934`) — two-handed, drawn firing **90° to the
/// character's left**, and the row whose `angleoffset` proves the convention: its
/// aim limits are lopsided the matching way (25° further left, 60° back right).
pub const HEAVY_LEFT: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0004",
    slot: 38,
    turn_end_frame: 19.0,
    angle_offset: dtor(90.0),
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (19.0, 61.0),
    recoil: None,
    aim: (14.0, 61.0),
    max_up: baddtor(50.0),
    max_down: baddtor(20.0),
    max_left: baddtor(25.0),
    max_right: baddtor(60.0),
    free_arm_up: 2.5,
    free_arm_down: 2.5,
};

/// `var800659b0` (`chraction.c:940`) — two-handed with the target *behind*. Aim-zero
/// is straight ahead, so the body spins the whole way; the cost is in the frames,
/// which is why nothing leaves the barrel until frame 39.
pub const HEAVY_BEHIND: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0006",
    slot: 39,
    turn_end_frame: 27.0,
    angle_offset: dtor(0.0),
    start_frame: 0.0,
    end_frame: -1.0,
    shoot: (39.0, 74.0),
    recoil: None,
    aim: (34.0, 74.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(45.0),
    max_right: baddtor(40.0),
    free_arm_up: 1.5,
    free_arm_down: 1.5,
};

/// `chraction.c:981` — the one-handed standing attack, through
/// `g_StandLightAttackAnims[RACE_HUMAN]`. Our default pistol fire clip, and the row
/// whose shoot window is eleven times wider than the one we were guessing.
pub const PISTOL: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0041",
    slot: 5,
    turn_end_frame: 26.0,
    angle_offset: dtor(0.0),
    start_frame: 12.0,
    end_frame: 140.0,
    shoot: (58.0, 92.0),
    recoil: Some((60.0, 79.0)),
    aim: (20.0, 120.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80065be0[1]` — one-handed forward variant, only in the dead-ahead group.
pub const LIGHT_FRONT_B: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0044",
    slot: 40,
    turn_end_frame: 0.0,
    angle_offset: dtor(0.0),
    start_frame: 17.0,
    end_frame: 100.0,
    shoot: (25.0, 87.0),
    recoil: Some((30.0, 55.0)),
    aim: (20.0, 93.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(60.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80065be0[2]` — the quickest one-handed row (shoot 19), dead-ahead only.
pub const LIGHT_FRONT_C: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0045",
    slot: 41,
    turn_end_frame: 0.0,
    angle_offset: dtor(0.0),
    start_frame: 12.0,
    end_frame: 64.0,
    shoot: (19.0, 51.0),
    recoil: Some((24.0, 46.0)),
    aim: (14.0, 58.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(30.0),
    max_right: baddtor(45.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80065be0[3]` — one-handed, in every forward-ish light group.
pub const LIGHT_FRONT_D: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0046",
    slot: 42,
    turn_end_frame: 22.0,
    angle_offset: dtor(0.0),
    start_frame: 4.0,
    end_frame: 69.0,
    shoot: (22.0, 49.0),
    recoil: Some((22.0, 33.0)),
    aim: (8.0, 58.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(25.0),
    max_right: baddtor(45.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80065e30[2]` — one-handed, drawn firing 90° left; the long variant (130
/// frames), used in the left-mix group only.
pub const LIGHT_LEFT_A: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0049",
    slot: 43,
    turn_end_frame: 0.0,
    angle_offset: dtor(90.0),
    start_frame: 7.0,
    end_frame: 130.0,
    shoot: (45.0, 93.0),
    recoil: Some((56.0, 73.0)),
    aim: (26.0, 107.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(20.0),
    max_right: baddtor(30.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80065e30[3]` — one-handed, 90° left, the short variant. Appears twice in the
/// source with different `unk04` (15 here, 19 in `var80066110`); [`LIGHT_LEFT_B_ONLY`]
/// is the other copy.
pub const LIGHT_LEFT_B: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_004A",
    slot: 44,
    turn_end_frame: 15.0,
    angle_offset: dtor(90.0),
    start_frame: 5.0,
    end_frame: 76.0,
    shoot: (20.0, 31.0),
    recoil: Some((31.0, 38.0)),
    aim: (15.0, 49.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(30.0),
    max_right: baddtor(60.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80066110` — the hard-left light group's only row: [`LIGHT_LEFT_B`]'s
/// animation with `unk04` 19 rather than 15. Transcribed separately rather than
/// deduplicated, because the two copies really do differ in the source.
pub const LIGHT_LEFT_B_ONLY: AttackAnimConfig =
    AttackAnimConfig { turn_end_frame: 19.0, ..LIGHT_LEFT_B };

/// `var80065fa0[2]` — [`LIGHT_LEFT_A`]'s mirror: one-handed, 90° **right**.
pub const LIGHT_RIGHT_A: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0047",
    slot: 45,
    turn_end_frame: 0.0,
    angle_offset: dtor(270.0),
    start_frame: 7.0,
    end_frame: 139.0,
    shoot: (54.0, 105.0),
    recoil: Some((61.0, 88.0)),
    aim: (26.0, 120.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(35.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var800661a8` — [`LIGHT_LEFT_B`]'s mirror: one-handed, 90° **right**, short.
pub const LIGHT_RIGHT_B: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_0048",
    slot: 46,
    turn_end_frame: 19.0,
    angle_offset: dtor(270.0),
    start_frame: 4.0,
    end_frame: 79.0,
    shoot: (21.0, 50.0),
    recoil: Some((26.0, 42.0)),
    aim: (10.0, 64.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(35.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `chraction.c:1064` — the dual-wield standing attack, through
/// `g_StandDualAttackAnims[RACE_HUMAN]`. Our default dual fire clip.
pub const DUAL: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007A",
    slot: 6,
    turn_end_frame: 26.0,
    angle_offset: dtor(0.0),
    start_frame: 7.0,
    end_frame: 92.0,
    shoot: (28.0, 68.0),
    recoil: None,
    aim: (11.0, 73.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(40.0),
    max_right: baddtor(40.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80066470[0]` — dual-wield, drawn firing 90° left.
pub const DUAL_LEFT_A: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007B",
    slot: 47,
    turn_end_frame: 26.0,
    angle_offset: dtor(90.0),
    start_frame: 9.0,
    end_frame: 112.0,
    shoot: (38.0, 87.0),
    recoil: None,
    aim: (19.0, 98.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(25.0),
    max_right: baddtor(25.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80066470[1]` — dual-wield, 90° left, second variant.
pub const DUAL_LEFT_B: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007D",
    slot: 48,
    turn_end_frame: 25.0,
    angle_offset: dtor(90.0),
    start_frame: 10.0,
    end_frame: 112.0,
    shoot: (32.0, 86.0),
    recoil: None,
    aim: (19.0, 97.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(25.0),
    max_right: baddtor(25.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80066550[0]` — [`DUAL_LEFT_A`]'s mirror: dual-wield, 90° **right**.
pub const DUAL_RIGHT_A: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007C",
    slot: 49,
    turn_end_frame: 39.0,
    angle_offset: dtor(270.0),
    start_frame: 22.0,
    end_frame: 127.0,
    shoot: (44.0, 102.0),
    recoil: None,
    aim: (28.0, 112.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(25.0),
    max_right: baddtor(25.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// `var80066550[1]` — [`DUAL_LEFT_B`]'s mirror: dual-wield, 90° **right**.
pub const DUAL_RIGHT_B: AttackAnimConfig = AttackAnimConfig {
    anim: "ANIM_007E",
    slot: 50,
    turn_end_frame: 39.0,
    angle_offset: dtor(270.0),
    start_frame: 23.0,
    end_frame: 130.0,
    shoot: (46.0, 100.0),
    recoil: None,
    aim: (30.0, 110.0),
    max_up: baddtor(50.0),
    max_down: baddtor(40.0),
    max_left: baddtor(25.0),
    max_right: baddtor(25.0),
    free_arm_up: 0.0,
    free_arm_down: 0.0,
};

/// Every row in the three human standing tables — the set the asset-pinning tests
/// walk, so a re-exported or re-ordered template cannot silently repoint one.
pub const ALL_ROWS: &[&AttackAnimConfig] = &[
    &HEAVY_FRONT,
    &RIFLE,
    &HEAVY_FLANK,
    &HEAVY_LEFT,
    &HEAVY_BEHIND,
    &PISTOL,
    &LIGHT_FRONT_B,
    &LIGHT_FRONT_C,
    &LIGHT_FRONT_D,
    &LIGHT_LEFT_A,
    &LIGHT_LEFT_B,
    &LIGHT_LEFT_B_ONLY,
    &LIGHT_RIGHT_A,
    &LIGHT_RIGHT_B,
    &DUAL,
    &DUAL_LEFT_A,
    &DUAL_LEFT_B,
    &DUAL_RIGHT_A,
    &DUAL_RIGHT_B,
];

// ─── The groups ──────────────────────────────────────────────────────────────
// `struct attackanimgroup { animcfg, len }`. `len` is filled in at boot by
// `race_init_anim_groups` (`race.c:119`) from the `{0, ...}` sentinel row, so a slice
// length is the faithful representation.

/// A run of interchangeable rows for one bearing band. PD picks between them with
/// `random() % len`, so every row in a group has to be a plausible answer for the
/// whole band it covers.
#[derive(Debug)]
pub struct AttackAnimGroup(pub &'static [&'static AttackAnimConfig]);

impl AttackAnimGroup {
    /// The row PD's `index = random() % len` lands on, for a caller-supplied roll.
    pub fn pick(&self, roll: usize) -> &'static AttackAnimConfig {
        self.0[roll % self.0.len()]
    }
}

static G_HEAVY_FRONT: AttackAnimGroup = AttackAnimGroup(&[&HEAVY_FRONT]);
/// `var80065830`. `var80065910` (slots 22–30) is a second, byte-identical copy in the
/// source; one group covers both.
static G_HEAVY_FLANK: AttackAnimGroup = AttackAnimGroup(&[&RIFLE, &HEAVY_FLANK]);
static G_HEAVY_LEFT: AttackAnimGroup = AttackAnimGroup(&[&HEAVY_LEFT]);
static G_HEAVY_BEHIND: AttackAnimGroup = AttackAnimGroup(&[&HEAVY_BEHIND]);

static G_LIGHT_FRONT_WIDE: AttackAnimGroup =
    AttackAnimGroup(&[&PISTOL, &LIGHT_FRONT_B, &LIGHT_FRONT_C, &LIGHT_FRONT_D]);
static G_LIGHT_FRONT: AttackAnimGroup = AttackAnimGroup(&[&PISTOL, &LIGHT_FRONT_D]);
static G_LIGHT_LEFT_MIX: AttackAnimGroup =
    AttackAnimGroup(&[&PISTOL, &LIGHT_FRONT_D, &LIGHT_LEFT_A, &LIGHT_LEFT_B]);
static G_LIGHT_LEFT: AttackAnimGroup = AttackAnimGroup(&[&LIGHT_LEFT_B_ONLY]);
static G_LIGHT_RIGHT: AttackAnimGroup = AttackAnimGroup(&[&LIGHT_RIGHT_B]);
static G_LIGHT_RIGHT_MIX: AttackAnimGroup =
    AttackAnimGroup(&[&PISTOL, &LIGHT_FRONT_D, &LIGHT_RIGHT_A, &LIGHT_RIGHT_B]);

static G_DUAL_FRONT: AttackAnimGroup = AttackAnimGroup(&[&DUAL]);
static G_DUAL_LEFT: AttackAnimGroup = AttackAnimGroup(&[&DUAL_LEFT_A, &DUAL_LEFT_B]);
static G_DUAL_RIGHT: AttackAnimGroup = AttackAnimGroup(&[&DUAL_RIGHT_A, &DUAL_RIGHT_B]);

/// How many bearing slots a stance table has (`animgroups[32]`).
pub const GROUPS: usize = 32;

/// PD's own literal for `32 / BADDTOR(360)` (`chraction.c:2827`).
pub const GROUP_SCALE: f32 = 5.093_769_073_486_3;

/// One stance's bearing → animation-group table (`g_Stand*AttackAnims[RACE_HUMAN]`).
pub struct DirectionTable(pub [&'static AttackAnimGroup; GROUPS]);

impl DirectionTable {
    /// Every distinct row this table can select, deduplicated by template slot — the
    /// clips one hunter in this stance might ever play, and so the set whose barrel
    /// axis has to be measured for it at spawn.
    ///
    /// Deduplicating by slot rather than by row is deliberate: `ANIM_004A` appears
    /// twice with different `unk04`, and the two copies are the same *animation*.
    pub fn rows(&self) -> Vec<&'static AttackAnimConfig> {
        let mut out: Vec<&'static AttackAnimConfig> = Vec::new();
        for g in self.0 {
            for r in g.0 {
                if !out.iter().any(|x| x.slot == r.slot) {
                    out.push(r);
                }
            }
        }
        out
    }
}

/// `g_StandHeavyAttackAnims[RACE_HUMAN]` (`chraction.c:956`) — two-handed.
pub static STAND_HEAVY: DirectionTable = DirectionTable([
    &G_HEAVY_FRONT, // 0 — dead ahead
    &G_HEAVY_FLANK, // 1
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK, // 9
    &G_HEAVY_LEFT,  // 10 — hard left (angleoffset 90°)
    &G_HEAVY_LEFT,
    &G_HEAVY_LEFT,
    &G_HEAVY_LEFT,
    &G_HEAVY_LEFT,
    &G_HEAVY_LEFT,   // 15
    &G_HEAVY_BEHIND, // 16 — behind, spinning through
    &G_HEAVY_BEHIND,
    &G_HEAVY_BEHIND,
    &G_HEAVY_BEHIND,
    &G_HEAVY_BEHIND,
    &G_HEAVY_BEHIND, // 21
    &G_HEAVY_FLANK,  // 22 — `var80065910`, identical to `var80065830`
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK,
    &G_HEAVY_FLANK, // 30
    &G_HEAVY_FRONT, // 31
]);

/// `g_StandLightAttackAnims[RACE_HUMAN]` (`chraction.c:1039`) — one-handed.
pub static STAND_LIGHT: DirectionTable = DirectionTable([
    &G_LIGHT_FRONT_WIDE, // 0
    &G_LIGHT_FRONT_WIDE, // 1
    &G_LIGHT_FRONT,      // 2
    &G_LIGHT_FRONT,
    &G_LIGHT_FRONT,    // 4
    &G_LIGHT_LEFT_MIX, // 5
    &G_LIGHT_LEFT_MIX,
    &G_LIGHT_LEFT_MIX,
    &G_LIGHT_LEFT_MIX,
    &G_LIGHT_LEFT_MIX, // 9
    &G_LIGHT_LEFT,     // 10
    &G_LIGHT_LEFT,
    &G_LIGHT_LEFT,
    &G_LIGHT_LEFT,
    &G_LIGHT_LEFT,
    &G_LIGHT_LEFT,  // 15
    &G_LIGHT_RIGHT, // 16
    &G_LIGHT_RIGHT,
    &G_LIGHT_RIGHT,
    &G_LIGHT_RIGHT,
    &G_LIGHT_RIGHT,
    &G_LIGHT_RIGHT,     // 21
    &G_LIGHT_RIGHT_MIX, // 22
    &G_LIGHT_RIGHT_MIX,
    &G_LIGHT_RIGHT_MIX,
    &G_LIGHT_RIGHT_MIX,
    &G_LIGHT_RIGHT_MIX, // 26
    &G_LIGHT_FRONT,     // 27
    &G_LIGHT_FRONT,
    &G_LIGHT_FRONT,      // 29
    &G_LIGHT_FRONT_WIDE, // 30
    &G_LIGHT_FRONT_WIDE, // 31
]);

/// `g_StandDualAttackAnims[RACE_HUMAN]` (`chraction.c:1092`) — akimbo.
pub static STAND_DUAL: DirectionTable = DirectionTable([
    &G_DUAL_FRONT, // 0
    &G_DUAL_FRONT,
    &G_DUAL_FRONT,
    &G_DUAL_FRONT,
    &G_DUAL_FRONT, // 4
    &G_DUAL_LEFT,  // 5
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,
    &G_DUAL_LEFT,  // 15
    &G_DUAL_RIGHT, // 16
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT,
    &G_DUAL_RIGHT, // 26
    &G_DUAL_FRONT, // 27
    &G_DUAL_FRONT,
    &G_DUAL_FRONT,
    &G_DUAL_FRONT,
    &G_DUAL_FRONT, // 31
]);

/// The stance table for a weapon class + dual flag, matching how PD selects one:
/// dual-wield first (`g_StandDualAttackAnims`), then one-handed
/// (`g_StandLightAttackAnims`) versus two-handed (`g_StandHeavyAttackAnims`) —
/// `chr_choose_attack_animation`, `chraction.c:2239`.
pub fn table_for(class: EnemyWeaponClass, dual: bool) -> &'static DirectionTable {
    if dual {
        &STAND_DUAL
    } else {
        match class {
            EnemyWeaponClass::Pistol => &STAND_LIGHT,
            EnemyWeaponClass::Rifle => &STAND_HEAVY,
        }
    }
}

/// The **spawn default** row for a weapon class + dual flag: the forward-facing
/// member of that stance, which is the clip at [`FIRE_RIFLE_IDX`]
/// / `_PISTOL_` / `_DUAL_` and the one the hunter's layer stack is built and measured
/// against. [`select`] takes over per burst once the hunter has a bearing.
///
/// [`FIRE_RIFLE_IDX`]: crate::world::FIRE_RIFLE_IDX
pub fn config_for(class: EnemyWeaponClass, dual: bool) -> &'static AttackAnimConfig {
    if dual {
        &DUAL
    } else {
        match class {
            EnemyWeaponClass::Pistol => &PISTOL,
            EnemyWeaponClass::Rifle => &RIFLE,
        }
    }
}

/// The bearing (rad) from a body facing `facing_yaw` to a target at `bearing_yaw`,
/// wrapped into `[0, BAD_TAU)` — `chr_get_angle_to_pos` (`chraction.c:13787`).
/// Positive is the body's **left**. Both arguments are this game's `atan2(x, z)`
/// yaws, which is the convention PD uses too.
pub fn relative_angle(bearing_yaw: f32, facing_yaw: f32) -> f32 {
    let mut a = bearing_yaw - facing_yaw;
    // PD adds one full turn to a negative angle and does not loop, because its
    // inputs are already in range; ours can be up to 2 turns out, so this does.
    a %= BAD_TAU;
    if a < 0.0 {
        a += BAD_TAU;
    }
    a
}

/// `chr_attack`'s group index for a relative bearing — the `+ 0.5` truncation and the
/// out-of-range fallback to slot 0, both exactly as written (`chraction.c:2827`).
/// Slot 0 is the dead-ahead group, so a bearing that lands past 31 (the last sliver
/// before a full turn) falling back to it is correct rather than merely safe.
pub fn group_index(relative_angle: f32) -> usize {
    let i = (relative_angle * GROUP_SCALE + 0.5) as i32; // C float→int truncates
    if i < 0 || i > 31 {
        0
    } else {
        i as usize
    }
}

/// The animation Perfect Dark would start for a target at `relative_angle` off this
/// stance's facing. `roll` stands in for `random()`; the caller supplies it so the
/// world's RNG stays the one source of chance.
pub fn select(
    table: &'static DirectionTable,
    relative_angle: f32,
    roll: usize,
) -> &'static AttackAnimConfig {
    table.0[group_index(relative_angle)].pick(roll)
}

/// Whether `slot` in the Perfect Dark hunter template holds a fire animation — the
/// three per-stance defaults plus the whole directional set. `is_fire_clip` needs
/// this to tell a burst from a hit/death one-shot now that a burst can play any of
/// eighteen clips.
pub fn slot_is_fire(slot: usize) -> bool {
    ALL_ROWS.iter().any(|r| r.slot == slot)
}

/// A hunter's resolved fire timing, in **seconds** into its fire animation — what
/// the combat pump and the aim layers actually read each frame.
///
/// Built either from a Perfect Dark [`AttackAnimConfig`] or from the legacy
/// `anim_set::FIRE_TIMING` guess, so the runtime code has one shape and the choice
/// is made once, at spawn.
#[derive(Clone, Copy, Debug)]
pub struct FireTiming {
    /// Shots are pumped while the burst clock is inside this window.
    pub shoot: (f32, f32),
    /// The chest tracks the target while inside this window. Wider than
    /// [`Self::shoot`] under the PD rows; the whole burst under the legacy one.
    pub aim: (f32, f32),
    /// Recoil kicks on shots inside this window; `None` kicks on every shot (the
    /// legacy behaviour).
    pub recoil: Option<(f32, f32)>,
    /// Where the burst clock starts — a PD row may trim the first frames off.
    pub start: f32,
    /// Where the burst ends and the hunter drops back to locomotion.
    pub end: f32,
    /// How far the chest may swing to point the barrel.
    pub cone: AimCone,
    /// How far this animation's aim-zero sits off the body's facing (rad, `+` left).
    /// The body is turned to `bearing - this` while the clip plays, which is what
    /// makes a hard-left animation read as one. `0` on every forward row and on the
    /// legacy path.
    pub angle_offset: f32,
    /// Which template slot the burst plays. `None` on the legacy path, whose clip is
    /// fixed at spawn.
    pub slot: Option<usize>,
    /// Whether this came from Perfect Dark's authored table (for the debug overlay
    /// and for tests that need to tell the two apart).
    pub authored: bool,
}

impl FireTiming {
    /// Resolve a PD row against the clip it configures. `clip_duration` closes an
    /// open (`-1`) `end_frame`, and clamps a trim that runs past a clip we exported
    /// at a different length than the ROM's.
    pub fn from_pd(cfg: &AttackAnimConfig, clip_duration: f32) -> Self {
        let f = |frame: f32| (frame / PD_ANIM_FPS).clamp(0.0, clip_duration.max(0.0));
        let end = if cfg.end_frame < 0.0 { clip_duration } else { f(cfg.end_frame) };
        FireTiming {
            shoot: (f(cfg.shoot.0), f(cfg.shoot.1)),
            aim: (f(cfg.aim.0), f(cfg.aim.1)),
            recoil: cfg.recoil.map(|(a, b)| (f(a), f(b))),
            start: f(cfg.start_frame),
            end,
            cone: cfg.cone(),
            angle_offset: cfg.angle_offset,
            slot: Some(cfg.slot),
            authored: true,
        }
    }

    /// The pre-existing behaviour: the `FIRE_TIMING` window, aim tracking for the
    /// whole burst, recoil on every shot, one isotropic cone. Kept so GoldenEye
    /// hunters are byte-for-byte unchanged while the PD rows are evaluated in the
    /// lab.
    pub fn legacy(window: (f32, f32), cone: f32) -> Self {
        FireTiming {
            shoot: window,
            aim: (0.0, f32::INFINITY),
            recoil: None,
            start: 0.0,
            end: window.1,
            cone: AimCone::uniform(cone),
            angle_offset: 0.0,
            slot: None,
            authored: false,
        }
    }

    /// Whether the burst clock is inside the shoot window.
    pub fn shooting(&self, t: f32) -> bool {
        t >= self.shoot.0 && t <= self.shoot.1
    }

    /// Whether the burst clock is inside the aim window (so the chest tracks).
    pub fn aiming(&self, t: f32) -> bool {
        t >= self.aim.0 && t <= self.aim.1
    }

    /// Whether a shot at burst clock `t` should kick the visual recoil.
    pub fn recoiling(&self, t: f32) -> bool {
        match self.recoil {
            Some((a, b)) => t >= a && t <= b,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three rows a hunter is spawned holding — the forward-facing member of each
    /// stance, and the only ones that existed before the direction tables.
    const DEFAULTS: [&AttackAnimConfig; 3] = [&RIFLE, &PISTOL, &DUAL];

    /// The transcribed rows obey the ordering invariant the original author wrote
    /// down (`types.h:339`). A typo in any frame number would break it, and nothing
    /// else in the game would notice — a shoot window that started before the aim
    /// window would just quietly fire without tracking.
    ///
    /// **Two rows in the real data break the recoil clause**, and they are listed
    /// here rather than quietly excused: `ANIM_004A`'s recoil window (31–38) starts
    /// where its shoot window ends and runs seven frames past it. `ANIM_004E`
    /// (kneeling, not ported) does the same. So the recoil half is asserted as
    /// "well-formed and overlapping the shoot window", not as the documented nesting.
    #[test]
    fn rows_obey_the_authored_frame_ordering() {
        for cfg in ALL_ROWS {
            let (aim_s, aim_e) = cfg.aim;
            let (shoot_s, shoot_e) = cfg.shoot;
            let end = if cfg.end_frame < 0.0 { f32::INFINITY } else { cfg.end_frame };
            assert!(cfg.start_frame <= aim_s, "{}: start <= aimstart", cfg.anim);
            assert!(aim_s <= shoot_s, "{}: aimstart <= shootstart", cfg.anim);
            assert!(shoot_s <= shoot_e, "{}: shootstart <= shootend", cfg.anim);
            assert!(shoot_e <= aim_e, "{}: shootend <= aimend", cfg.anim);
            assert!(aim_e <= end, "{}: aimend <= end", cfg.anim);
            if let Some((r_s, r_e)) = cfg.recoil {
                assert!(shoot_s <= r_s, "{}: shootstart <= recoilstart", cfg.anim);
                assert!(r_s <= r_e, "{}: recoilstart <= recoilend", cfg.anim);
                assert!(r_s <= shoot_e, "{}: the recoil window overlaps the shoot window", cfg.anim);
                if cfg.anim != "ANIM_004A" {
                    assert!(r_e <= shoot_e, "{}: recoilend <= shootend", cfg.anim);
                }
            }
        }
        // The one documented exception is real, so it cannot be "fixed" by accident.
        let (_, shoot_e) = LIGHT_LEFT_B.shoot;
        assert!(
            LIGHT_LEFT_B.recoil.expect("ANIM_004A has a recoil window").1 > shoot_e,
            "ANIM_004A's recoil really does run past its shoot window in the source"
        );
    }

    /// **Every row aims wider than it shoots.** This is the structural difference
    /// from the `FIRE_TIMING` guess, which had a single window doing both jobs — a
    /// hunter that tracks only while its trigger is down snaps onto the player at
    /// the moment it fires instead of swivelling onto them first.
    #[test]
    fn the_aim_window_is_wider_than_the_shoot_window() {
        for cfg in ALL_ROWS {
            assert!(cfg.aim.0 < cfg.shoot.0, "{}: tracks before it fires", cfg.anim);
            assert!(cfg.aim.1 >= cfg.shoot.1, "{}: tracks past the last round", cfg.anim);
        }
    }

    /// Resolution to seconds is clamped by the clip, so an authored `endframe` past
    /// our exported clip length cannot produce a window the burst never leaves.
    #[test]
    fn resolving_clamps_to_the_clip() {
        let t = FireTiming::from_pd(&PISTOL, 6.13); // ANIM_0041 is 185 frames
        assert!((t.shoot.0 - 58.0 / 30.0).abs() < 1e-4);
        assert!((t.shoot.1 - 92.0 / 30.0).abs() < 1e-4);
        assert!(t.aim.0 < t.shoot.0 && t.aim.1 > t.shoot.1, "aim brackets shoot");
        assert!(t.recoiling(70.0 / 30.0), "mid-recoil-window shot kicks");
        assert!(!t.recoiling(85.0 / 30.0), "a later shot in the same burst does not");
        assert_eq!(t.slot, Some(5), "and it names the clip it resolved against");

        // A short clip clamps rather than producing an unreachable window.
        let short = FireTiming::from_pd(&PISTOL, 1.0);
        assert!(short.shoot.1 <= 1.0 && short.end <= 1.0);

        // An open end frame resolves to the clip's own length.
        let open = FireTiming::from_pd(&RIFLE, 3.5);
        assert!((open.end - 3.5).abs() < 1e-4, "endframe -1 means the whole clip");
    }

    /// The legacy path keeps the old behaviour exactly: track for the whole burst,
    /// recoil on every shot, one cone, and no directional machinery.
    #[test]
    fn legacy_timing_tracks_and_recoils_throughout() {
        let t = FireTiming::legacy((0.9, 2.67), 1.4);
        assert!(!t.authored);
        assert!(t.aiming(0.0) && t.aiming(10.0), "legacy aims for the whole burst");
        assert!(t.recoiling(0.0) && t.recoiling(10.0), "legacy recoils on every shot");
        assert!(t.shooting(1.0) && !t.shooting(3.0));
        assert_eq!(t.angle_offset, 0.0, "and it never turns the body off the target");
        assert_eq!(t.slot, None, "its clip was fixed at spawn");
    }

    /// [`group_index`] reproduces `chr_attack`'s arithmetic, including the two edges
    /// that arithmetic has: a bearing dead ahead is slot 0, and the last sliver
    /// before a full turn overflows past 31 and falls back to slot 0 — which happens
    /// to be the same group, so PD's `if (groupindex > 31)` guard is a no-op in
    /// behaviour and a real one in memory safety.
    #[test]
    fn group_index_matches_chr_attack() {
        assert_eq!(group_index(0.0), 0);
        // Slot centres: i * BAD_TAU / 32.
        for i in 0..GROUPS {
            let centre = i as f32 * BAD_TAU / GROUPS as f32;
            assert_eq!(group_index(centre), i, "slot {i}'s centre resolves to it");
        }
        // Half a slot short of a full turn overflows to 32 → clamped to 0.
        assert_eq!(group_index(BAD_TAU - 1e-4), 0);
        // A quarter turn to the left is slot 8 (32/4).
        assert_eq!(group_index(BAD_TAU / 4.0), 8);
        // Three quarters — a quarter turn to the RIGHT — is slot 24.
        assert_eq!(group_index(BAD_TAU * 0.75), 24);
    }

    /// [`relative_angle`] wraps the way `chr_get_angle_to_pos` does, and agrees with
    /// the sign convention the source states: **positive is the body's left**. A body
    /// facing `+Z` (yaw 0) with a target on `+X` (yaw `π/2`) is looking at something
    /// on its left, because `left = up × forward = +Y × +Z = +X`.
    #[test]
    fn relative_angle_is_positive_to_the_left() {
        let quarter = std::f32::consts::FRAC_PI_2;
        assert!((relative_angle(0.0, 0.0)).abs() < 1e-5, "target dead ahead is 0");
        let left = relative_angle(quarter, 0.0);
        assert!((left - quarter).abs() < 1e-3, "target on +X is a quarter turn LEFT");
        let right = relative_angle(-quarter, 0.0);
        assert!(right > BAD_PI, "a target on −X wraps into the upper half, got {right}");
        // Turning the body onto the target zeroes it again.
        assert!(relative_angle(quarter, quarter).abs() < 1e-5);
        // Inputs more than a turn out still land in range (ours can be; PD's cannot).
        for k in -3..=3 {
            let a = relative_angle(quarter + k as f32 * BAD_TAU, 0.0);
            assert!((0.0..BAD_TAU).contains(&a), "k={k} wrapped to {a}");
        }
    }

    /// **The tables put the sideways animations on the correct side.** This is the
    /// one thing a transcription of a 32-entry pointer array can get silently
    /// backwards, and the failure mode on screen — a guard firing away from you — is
    /// exactly the kind of thing that gets "fixed" with a sign flip somewhere else.
    ///
    /// So it is asserted from the direction, not from the slot number: a target hard
    /// on the body's **left** must select a row whose `angleoffset` is *also* left,
    /// and vice versa.
    #[test]
    fn hard_left_and_hard_right_bearings_pick_the_matching_animation() {
        let deg = |d: f32| d * BAD_TAU / 360.0;
        for (name, table) in
            [("heavy", &STAND_HEAVY), ("light", &STAND_LIGHT), ("dual", &STAND_DUAL)]
        {
            // 135° left, and its mirror 135° right (== 225° in the wrapped range).
            let left = select(table, deg(135.0), 0);
            let right = select(table, deg(225.0), 0);
            let off = |c: &AttackAnimConfig| {
                // Fold `angleoffset` into (−180, 180] so left is + and right is −.
                let d = c.angle_offset.to_degrees();
                if d > 180.0 { d - 360.0 } else { d }
            };
            if name == "heavy" {
                // Heavy has one sideways animation, drawn to the left; its mirror
                // slot uses the aim-forward turn-around row instead.
                assert!(off(left) > 45.0, "heavy 135° left picks a left-drawn row");
                assert_eq!(right.anim, "ANIM_0006", "heavy 135° right spins instead");
            } else {
                assert!(
                    off(left) > 45.0,
                    "{name} 135° left picked {} at {}°",
                    left.anim,
                    off(left)
                );
                assert!(
                    off(right) < -45.0,
                    "{name} 135° right picked {} at {}°",
                    right.anim,
                    off(right)
                );
            }
        }
    }

    /// Every table's group *runs* are symmetric under `i → 31 - i`, which is how the
    /// source pairs its two halves. The obvious guess — symmetry about slot 0,
    /// `i → -i` — is wrong, because `group_index`'s `+ 0.5` puts the slot boundaries
    /// half a slot off the axis. Getting that backwards would rotate every sideways
    /// animation by 11.25°, which nobody would ever see, and this catches it.
    ///
    /// The **light and dual** tables are true mirrors: each partner slot holds the
    /// same-sized group with every `angleoffset` negated. **Heavy is not**, and that is
    /// real rather than a transcription slip: PD authored one sideways two-handed
    /// animation (`ANIM_0004`, drawn to the left) and no right-hand twin, so where the
    /// light table would mirror it the heavy table uses the aim-forward turn-around row
    /// instead. Asserted here so nobody "fixes" the asymmetry by inventing a mirror.
    #[test]
    fn the_tables_are_mirror_symmetric() {
        let fold = |x: f32| {
            let d = x.to_degrees();
            if d > 180.0 { d - 360.0 } else { d }
        };
        for (name, table, mirrored) in [
            ("heavy", &STAND_HEAVY, false),
            ("light", &STAND_LIGHT, true),
            ("dual", &STAND_DUAL, true),
        ] {
            for i in 0..GROUPS {
                let a = table.0[i];
                let b = table.0[GROUPS - 1 - i];
                // The run structure is symmetric in every table: partner slots hold
                // groups of the same size.
                assert_eq!(
                    a.0.len(),
                    b.0.len(),
                    "{name} slot {i} and its mirror have different group sizes"
                );
                if !mirrored {
                    continue;
                }
                for (ra, rb) in a.0.iter().zip(b.0) {
                    assert!(
                        (fold(ra.angle_offset) + fold(rb.angle_offset)).abs() < 1e-3,
                        "{name} slot {i}: {} ({}°) mirrors {} ({}°)",
                        ra.anim,
                        fold(ra.angle_offset),
                        rb.anim,
                        fold(rb.angle_offset),
                    );
                }
            }
        }
        // Heavy's documented asymmetry, pinned: hard left has a sideways animation,
        // its mirror slot spins around instead, and no heavy row is drawn to the right.
        assert_eq!(fold(STAND_HEAVY.0[12].0[0].angle_offset), 90.0);
        assert_eq!(fold(STAND_HEAVY.0[19].0[0].angle_offset), 0.0);
        assert_eq!(STAND_HEAVY.0[19].0[0].anim, "ANIM_0006");
        assert!(
            STAND_HEAVY.rows().iter().all(|r| fold(r.angle_offset) >= 0.0),
            "PD authored no right-drawn two-handed attack animation"
        );
    }

    /// **The bearing buys time.** The whole reason the direction table is worth
    /// fifteen extra animations: how long a guard takes to get its first round off is
    /// a function of where you are standing when it starts, and it is authored, not
    /// tuned. Coming at a rifleman from behind is worth 16 frames — over half a
    /// second at 30 fps.
    #[test]
    fn a_target_behind_a_rifleman_takes_longer_to_shoot() {
        let deg = |d: f32| d * BAD_TAU / 360.0;
        let front = select(&STAND_HEAVY, deg(0.0), 0);
        let flank = select(&STAND_HEAVY, deg(45.0), 0);
        let behind = select(&STAND_HEAVY, deg(180.0), 0);
        assert_eq!((front.anim, flank.anim, behind.anim), ("ANIM_0002", "ANIM_0032", "ANIM_0006"));
        assert!(
            front.shoot.0 < flank.shoot.0 && flank.shoot.0 < behind.shoot.0,
            "front {} < flank {} < behind {}",
            front.shoot.0,
            flank.shoot.0,
            behind.shoot.0,
        );
        assert!(behind.shoot.0 - front.shoot.0 >= 16.0, "and it is a real margin");
    }

    /// A group's rows are all reachable, and the roll is PD's `random() % len` — so a
    /// caller handing it a raw RNG word cannot land out of bounds.
    #[test]
    fn every_row_in_a_group_is_reachable() {
        for table in [&STAND_HEAVY, &STAND_LIGHT, &STAND_DUAL] {
            for slot in 0..GROUPS {
                let g = table.0[slot];
                let mut seen = vec![false; g.0.len()];
                for roll in 0..(g.0.len() * 3 + 7) {
                    let picked = g.pick(roll);
                    let idx = g.0.iter().position(|r| std::ptr::eq(*r, picked)).expect("in group");
                    seen[idx] = true;
                }
                assert!(seen.iter().all(|s| *s), "every row of slot {slot} can be picked");
            }
        }
    }

    /// Every row names a distinct template slot per animation, and the spawn defaults
    /// are the three the `FIRE_*_IDX` constants point at. `world::tests` pins the
    /// slots to filenames; this pins the arithmetic they are used in.
    #[test]
    fn rows_agree_on_slots_and_the_defaults_are_the_forward_ones() {
        for a in ALL_ROWS {
            for b in ALL_ROWS {
                if a.anim == b.anim {
                    assert_eq!(a.slot, b.slot, "{} is in two slots", a.anim);
                } else {
                    assert_ne!(a.slot, b.slot, "{} and {} share a slot", a.anim, b.anim);
                }
            }
            assert!(slot_is_fire(a.slot), "{} is recognised as a fire clip", a.anim);
        }
        assert_eq!((RIFLE.slot, PISTOL.slot, DUAL.slot), (4, 5, 6));
        for d in DEFAULTS {
            assert_eq!(d.angle_offset, 0.0, "{} is a forward row", d.anim);
        }
        assert!(!slot_is_fire(0), "idle is not a fire clip");
        assert!(!slot_is_fire(7), "nor is the first hit reaction");
    }

    /// `table_for` makes the same three-way choice `chr_choose_attack_animation`
    /// does, and each table's dead-ahead group contains the default that stance is
    /// spawned holding — so a hunter that never turns keeps playing the clip its
    /// layer stack was built and measured against.
    #[test]
    fn table_for_matches_the_stance_and_contains_its_default() {
        let contains = |t: &DirectionTable, anim: &str| {
            t.0[0].0.iter().any(|r| r.anim == anim)
        };
        assert!(std::ptr::eq(table_for(EnemyWeaponClass::Rifle, true), &STAND_DUAL));
        assert!(std::ptr::eq(table_for(EnemyWeaponClass::Pistol, true), &STAND_DUAL));
        assert!(std::ptr::eq(table_for(EnemyWeaponClass::Pistol, false), &STAND_LIGHT));
        assert!(std::ptr::eq(table_for(EnemyWeaponClass::Rifle, false), &STAND_HEAVY));
        assert!(contains(&STAND_LIGHT, PISTOL.anim), "light slot 0 holds ANIM_0041");
        assert!(contains(&STAND_DUAL, DUAL.anim), "dual slot 0 holds ANIM_007A");
        // Heavy is the exception, and deliberately so: PD gives the dead-ahead slot
        // its own faster animation (ANIM_0002) and puts ANIM_0032 on the flanks.
        assert!(!contains(&STAND_HEAVY, RIFLE.anim), "heavy slot 0 is ANIM_0002, not ANIM_0032");
        assert!(contains(&STAND_HEAVY, HEAVY_FRONT.anim));
    }
}
