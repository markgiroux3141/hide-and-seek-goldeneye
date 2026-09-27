//! The Perfect Dark animations a combat-simulator bot can play, with the two facts
//! PD's animation engine needs about each: its frame count and its `ANIMFLAG_LOOP`.
//!
//! Source of truth: `assets/ntsc-final/animations.json` in the decomp (the table
//! `g_Anims` is built from). The clips themselves are exported to
//! `assets/enemies/pd/bot_anims/<ID>.glb` by
//! `tools/pd-assets/pd_gltf.py clip <ID> <out>` at 30 fps, i.e. **one PD animation
//! frame = 1/30 s of glTF time** (see `pd_gltf.py`'s `DEFAULT_FPS` note: bot
//! locomotion plays at `speed 0.5`, which advances 0.5 frames per 60 Hz tick).
//!
//! This is the whole set `player_choose_third_person_animation` (`player.c:5472`)
//! can pick for a live bot — standing, ducking (135 cm) and squatting (90 cm) — plus
//! the eight deaths in `g_DeathAnimations` (`player.c:187`) and `ANIM_0029`, the run
//! whose stride defines the shove unit.

/// One PD animation number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AnimId(pub u16);

pub struct AnimInfo {
    pub id: AnimId,
    /// The decomp's symbol, which is also the exported file stem.
    pub name: &'static str,
    pub num_frames: u32,
    /// `ANIMFLAG_LOOP`: frame numbers wrap instead of clamping.
    pub looped: bool,
}

pub const ANIM_0002: AnimId = AnimId(0x0002); // heavy-gun stand/fire (idle breathing loop 35-40)
pub const ANIM_0029: AnimId = AnimId(0x0029); // run — HEADANIM_MOVING, the shove unit
pub const ANIM_0030: AnimId = AnimId(0x0030); // heavy-gun walk
pub const ANIM_0031: AnimId = AnimId(0x0031); // heavy-gun run
pub const ANIM_0041: AnimId = AnimId(0x0041); // pistol stand/fire (idle loop 79-87)
pub const ANIM_0052: AnimId = AnimId(0x0052); // pistol walk
pub const ANIM_0055: AnimId = AnimId(0x0055); // pistol run
pub const ANIM_RUNNING_ONEHANDGUN: AnimId = AnimId(0x0059); // unarmed run
pub const ANIM_006A: AnimId = AnimId(0x006a); // unarmed stand
pub const ANIM_006B: AnimId = AnimId(0x006b); // unarmed walk
pub const ANIM_006C: AnimId = AnimId(0x006c); // dual walk
pub const ANIM_006E: AnimId = AnimId(0x006e); // dual run
pub const ANIM_007A: AnimId = AnimId(0x007a); // dual stand/fire (idle loop 32-42)
/// Duck walk (`CROUCHPOS_DUCK`, `chr->height <= 135`), per wield mode.
pub const ANIM_0280: AnimId = AnimId(0x0280); // unarmed duck
pub const ANIM_0281: AnimId = AnimId(0x0281); // pistol duck
pub const ANIM_0282: AnimId = AnimId(0x0282); // heavy duck
pub const ANIM_0283: AnimId = AnimId(0x0283); // dual duck
/// Squat walk (`CROUCHPOS_SQUAT`, `chr->height <= 90`: crawl spaces), per wield mode.
pub const ANIM_0284: AnimId = AnimId(0x0284); // unarmed squat
pub const ANIM_0285: AnimId = AnimId(0x0285); // pistol squat
pub const ANIM_0286: AnimId = AnimId(0x0286); // heavy squat
pub const ANIM_0287: AnimId = AnimId(0x0287); // dual squat
pub const ANIM_DEATH_001A: AnimId = AnimId(0x001a);
pub const ANIM_DEATH_001C: AnimId = AnimId(0x001c);
pub const ANIM_DEATH_0020: AnimId = AnimId(0x0020);
pub const ANIM_DEATH_0021: AnimId = AnimId(0x0021);
pub const ANIM_DEATH_0022: AnimId = AnimId(0x0022);
pub const ANIM_DEATH_0023: AnimId = AnimId(0x0023);
pub const ANIM_DEATH_0024: AnimId = AnimId(0x0024);
pub const ANIM_DEATH_0025: AnimId = AnimId(0x0025);

/// `g_DeathAnimations` (`player.c:187`) — what a dead bot picks from at random.
pub const DEATH_ANIMS: [AnimId; 8] = [
    ANIM_DEATH_001A,
    ANIM_DEATH_001C,
    ANIM_DEATH_0020,
    ANIM_DEATH_0021,
    ANIM_DEATH_0022,
    ANIM_DEATH_0023,
    ANIM_DEATH_0024,
    ANIM_DEATH_0025,
];

pub const ANIMS: &[AnimInfo] = &[
    AnimInfo { id: ANIM_0002, name: "ANIM_0002", num_frames: 81, looped: false },
    AnimInfo { id: ANIM_0029, name: "ANIM_0029", num_frames: 19, looped: true },
    AnimInfo { id: ANIM_0030, name: "ANIM_0030", num_frames: 34, looped: true },
    AnimInfo { id: ANIM_0031, name: "ANIM_0031", num_frames: 21, looped: true },
    AnimInfo { id: ANIM_0041, name: "ANIM_0041", num_frames: 185, looped: false },
    AnimInfo { id: ANIM_0052, name: "ANIM_0052", num_frames: 35, looped: true },
    AnimInfo { id: ANIM_0055, name: "ANIM_0055", num_frames: 23, looped: true },
    AnimInfo { id: ANIM_RUNNING_ONEHANDGUN, name: "ANIM_RUNNING_ONEHANDGUN", num_frames: 26, looped: true },
    AnimInfo { id: ANIM_006A, name: "ANIM_006A", num_frames: 40, looped: true },
    AnimInfo { id: ANIM_006B, name: "ANIM_006B", num_frames: 35, looped: true },
    AnimInfo { id: ANIM_006C, name: "ANIM_006C", num_frames: 34, looped: true },
    AnimInfo { id: ANIM_006E, name: "ANIM_006E", num_frames: 24, looped: true },
    AnimInfo { id: ANIM_007A, name: "ANIM_007A", num_frames: 100, looped: false },
    AnimInfo { id: ANIM_DEATH_001A, name: "ANIM_DEATH_001A", num_frames: 89, looped: false },
    AnimInfo { id: ANIM_DEATH_001C, name: "ANIM_DEATH_001C", num_frames: 69, looped: false },
    AnimInfo { id: ANIM_DEATH_0020, name: "ANIM_DEATH_0020", num_frames: 69, looped: false },
    AnimInfo { id: ANIM_DEATH_0021, name: "ANIM_DEATH_0021", num_frames: 118, looped: false },
    AnimInfo { id: ANIM_DEATH_0022, name: "ANIM_DEATH_0022", num_frames: 118, looped: false },
    AnimInfo { id: ANIM_DEATH_0023, name: "ANIM_DEATH_0023", num_frames: 86, looped: false },
    AnimInfo { id: ANIM_DEATH_0024, name: "ANIM_DEATH_0024", num_frames: 88, looped: false },
    AnimInfo { id: ANIM_DEATH_0025, name: "ANIM_DEATH_0025", num_frames: 76, looped: false },
    AnimInfo { id: ANIM_0280, name: "ANIM_0280", num_frames: 29, looped: true },
    AnimInfo { id: ANIM_0281, name: "ANIM_0281", num_frames: 29, looped: true },
    AnimInfo { id: ANIM_0282, name: "ANIM_0282", num_frames: 29, looped: true },
    AnimInfo { id: ANIM_0283, name: "ANIM_0283", num_frames: 29, looped: true },
    AnimInfo { id: ANIM_0284, name: "ANIM_0284", num_frames: 28, looped: true },
    AnimInfo { id: ANIM_0285, name: "ANIM_0285", num_frames: 28, looped: true },
    AnimInfo { id: ANIM_0286, name: "ANIM_0286", num_frames: 28, looped: true },
    AnimInfo { id: ANIM_0287, name: "ANIM_0287", num_frames: 28, looped: true },
];

pub fn info(id: AnimId) -> &'static AnimInfo {
    ANIMS.iter().find(|a| a.id == id).unwrap_or_else(|| panic!("anim {:#06x} not in the spike's table", id.0))
}

/// `anim_get_num_frames`.
pub fn num_frames(id: AnimId) -> u32 {
    info(id).num_frames
}

pub fn index_of(id: AnimId) -> usize {
    ANIMS.iter().position(|a| a.id == id).expect("anim in table")
}
