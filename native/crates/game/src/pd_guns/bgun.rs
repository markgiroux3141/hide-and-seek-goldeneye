//! `game/bondgun.c` — the player's hands and weapons — ported function by function.
//!
//! PD reaches everything through `g_Vars.currentplayer`; here that is [`Bgun`]
//! (the hands, `gunctrl`, and the player fields the gun code reads and writes)
//! plus a [`Lv`] of frame timing. Side effects PD performs directly — sounds,
//! beams, casings, smoke, shots — are queued as [`GunEvent`]s for the world layer.
//!
//! NTSC final throughout: every `#if VERSION >= VERSION_PAL_BETA` is the `#else`.
//! Line numbers cite `reference/pd-decomp/src/game/bondgun.c`.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};

use super::anim::{Anim, AnimCtx};
use super::animdata::AnimBank;
use super::gset::*;
use super::model::{Model, ModelDef};
use super::pdmtx;
use crate::pd_spike::pdmath::{baddtor, Rng};

// ─── constants ───────────────────────────────────────────────────────────────

pub const HAND_RIGHT: usize = 0;
pub const HAND_LEFT: usize = 1;

pub const HANDSTATE_IDLE: i32 = 0;
pub const HANDSTATE_RELOAD: i32 = 1;
pub const HANDSTATE_2: i32 = 2;
pub const HANDSTATE_ATTACKEMPTY: i32 = 3;
pub const HANDSTATE_ATTACK: i32 = 4;
pub const HANDSTATE_CHANGEGUN: i32 = 5;
pub const HANDSTATE_CHANGEFUNC: i32 = 7;
pub const HANDSTATE_AUTOSWITCH: i32 = 8;

pub(crate) const HANDSTATEMINOR_ATTACK_MELEE_0: i32 = 0;
pub(crate) const HANDSTATEMINOR_ATTACK_MELEE_1: i32 = 1;
pub(crate) const HANDSTATEMINOR_ATTACK_MELEE_2: i32 = 2;
pub(crate) const HANDSTATEMINOR_ATTACK_MELEE_3: i32 = 3;
pub(crate) const HANDSTATEMINOR_ATTACK_SHOOT_0: i32 = 0;
pub(crate) const HANDSTATEMINOR_ATTACK_SHOOT_1: i32 = 1;
pub(crate) const HANDSTATEMINOR_ATTACK_SHOOT_2: i32 = 2;
pub(crate) const HANDSTATEMINOR_ATTACK_SPECIAL_START: i32 = 0;
pub(crate) const HANDSTATEMINOR_ATTACK_SPECIAL_EXECUTE: i32 = 1;
pub(crate) const HANDSTATEMINOR_ATTACK_SPECIAL_RECOVER: i32 = 2;
pub(crate) const HANDSTATEMINOR_ATTACK_THROW_0: i32 = 0;
pub(crate) const HANDSTATEMINOR_ATTACK_THROW_1: i32 = 1;
pub(crate) const HANDSTATEMINOR_ATTACK_THROW_2: i32 = 2;
pub(crate) const HANDSTATEMINOR_ATTACK_THROW_GRENADEWAIT: i32 = 55;
pub(crate) const HANDSTATEMINOR_AUTOSWITCH_UNEQUIP: i32 = 0;
pub(crate) const HANDSTATEMINOR_AUTOSWITCH_DELETE: i32 = 1;
pub(crate) const HANDSTATEMINOR_AUTOSWITCH_2: i32 = 2;
pub(crate) const HANDSTATEMINOR_CHANGEGUN_UNEQUIP: i32 = 0;
pub(crate) const HANDSTATEMINOR_CHANGEGUN_LOWER: i32 = 1;
pub(crate) const HANDSTATEMINOR_CHANGEGUN_LOAD: i32 = 2;
pub(crate) const HANDSTATEMINOR_CHANGEGUN_RAISE: i32 = 3;
pub(crate) const HANDSTATEMINOR_CHANGEGUN_EQUIP: i32 = 4;
pub(crate) const HANDSTATEMINOR_RELOAD_MAIN: i32 = 0;
pub(crate) const HANDSTATEMINOR_RELOAD_LOWER: i32 = 1;
pub(crate) const HANDSTATEMINOR_RELOAD_SOUND: i32 = 2;
pub(crate) const HANDSTATEMINOR_RELOAD_RAISE: i32 = 3;
pub(crate) const HANDSTATEMINOR_RELOAD_WAIT: i32 = 9;

pub(crate) const HANDSTATEFLAG_00000001: u32 = 0x01;
pub(crate) const HANDSTATEFLAG_BUSY: u32 = 0x10;
pub(crate) const HANDSTATEFLAG_FIRED: u32 = 0x20;
pub(crate) const HANDSTATEFLAG_00000040: u32 = 0x40;
pub(crate) const HANDSTATEFLAG_00000080: u32 = 0x80;

pub const HANDMODE_NONE: u32 = 0;
pub const HANDMODE_ATTACK: u32 = 1;
pub const HANDMODE_6: u32 = 6;
pub const HANDMODE_7: u32 = 7;
pub const HANDMODE_EQUIP: u32 = 8;
pub const HANDMODE_RELOAD: u32 = 9;
pub const HANDMODE_11: u32 = 11;
pub const HANDMODE_12: u32 = 12;
pub const HANDMODE_13: u32 = 13;

pub const HANDANIMMODE_IDLE: i32 = 0;
pub const HANDANIMMODE_BUSY: i32 = 2;

pub const HANDATTACKTYPE_SHOOT: i32 = 1;
pub const HANDATTACKTYPE_SHOOTPROJECTILE: i32 = 2;
pub const HANDATTACKTYPE_THROWPROJECTILE: i32 = 3;
pub const HANDATTACKTYPE_MELEE: i32 = 4;
pub const HANDATTACKTYPE_DETONATE: i32 = 5;
pub const HANDATTACKTYPE_BOOST: i32 = 6;
pub const HANDATTACKTYPE_REVERTBOOST: i32 = 7;
pub const HANDATTACKTYPE_CROUCH: i32 = 8;
pub const HANDATTACKTYPE_RCP120CLOAK: i32 = 9;
pub const HANDATTACKTYPE_MELEENOUNCLOAK: i32 = 10;

pub const GUNAMMOSTATE_DEPLETED: i32 = -1;
pub const GUNAMMOSTATE_NEEDRELOAD: i32 = 0;
pub const GUNAMMOSTATE_CLIPYES_HELDYES: i32 = 1;
pub const GUNAMMOSTATE_CLIPYES_HELDNO: i32 = 2;
pub const GUNAMMOSTATE_CLIPFULL: i32 = 3;

pub const EJECTSTATE_INACTIVE: i32 = 0;
pub const EJECTSTATE_INIT: i32 = 1;
pub const EJECTSTATE_AIRBORNE: i32 = 2;
pub const EJECTSTATE_FINISHED: i32 = 3;
pub const EJECTTYPE_GUN: i32 = 0;

pub const USETIMER_CONTINUE: i32 = 0;
pub const USETIMER_STOP: i32 = 1;
pub const USETIMER_REPEAT: i32 = 2;

pub const CROUCHPOS_SQUAT: i32 = 0;
pub const CROUCHPOS_DUCK: i32 = 1;
pub const CROUCHPOS_STAND: i32 = 2;

/// `MAX_PITCH` (`bondgun.c:70`): how far a gun tips down when lowered.
fn max_pitch() -> f32 {
    baddtor(50.0)
}

/// SFX ids the gun code starts directly.
pub const SFXMAP_804F_RELOAD_DEFAULT: u16 = 0x804f;
pub const SFXMAP_8052_FIREEMPTY: u16 = 0x8052;
pub const SFXNUM_00E8_PICKUP_GUN: u16 = 0x00e8;

/// Frame timing (`g_Vars.lvupdate240` et al., `lv.c`).
#[derive(Clone, Copy, Debug)]
pub struct Lv {
    pub lvupdate240: i32,
    pub lvupdate60: i32,
    pub lvupdate60freal: f32,
    pub lvframe60: i32,
    pub lvframenum: i32,
}

impl Lv {
    /// A frame at `lvupdate240` quarter-ticks (4 = 60 fps, 8 = 30 fps, 12 = 20 fps).
    pub fn step(lvupdate240: i32, lvframe60: i32, lvframenum: i32) -> Lv {
        Lv {
            lvupdate240,
            lvupdate60: (lvupdate240 + 3) / 4,
            lvupdate60freal: lvupdate240 as f32 / 4.0,
            lvframe60,
            lvframenum,
        }
    }
}

/// What the gun code asks the world to do.
#[derive(Clone, Debug)]
pub enum GunEvent {
    /// `snd_start` of a sound id (SFXNUM or SFXMAP), with an optional pitch.
    Sound { id: u16, speed: f32 },
    /// Stop a looping hand sound (Reaper spin, Mauler charge).
    StopLoop { hand: usize },
    /// `beam_create_for_hand` — a tracer from the muzzle to the shot's hit point.
    Beam { hand: usize },
    /// `casing_create_for_hand`: `mtx` is the eject node's WORLD matrix.
    Casing { hand: usize, mtx: Mat4, casing: i32 },
    /// `smoke_create_for_hand`.
    Smoke { hand: usize, pos: Vec3, kind: i32 },
    /// `bgun_free_held_rocket` (`:4552`).
    FreeHeldRocket { hand: usize },
    /// `chr_uncloak_temporarily` for a thrown or fired projectile
    /// (`bgun_create_fx`, `:7273`).
    UncloakTemporarily,
    /// `bgun_update_rocket_launcher` (`:6997`): make/place the rocket in the
    /// launcher (the world owns the object).
    UpdateRocketLauncher { hand: usize },
}

// ─── the hand ────────────────────────────────────────────────────────────────

/// `struct hand` (`types.h:2086`) — the fields the ported functions use.
pub struct Hand {
    // gset
    pub weaponnum: i32,
    pub weaponfunc: usize,
    pub upgradewant: u8,

    pub firing: bool,
    pub flashon: bool,
    pub visible: bool,
    pub inuse: bool,
    pub triggeron: bool,
    pub triggerprev: bool,
    pub triggerreleased: bool,
    pub count: i32,
    pub count60: i32,
    pub mode: u32,
    pub modenext: u32,
    pub numfires: u32,
    pub pausetime60: i32,
    pub pausechange: u32,
    pub posstart: Vec3,
    pub rotxstart: f32,
    pub posend: Vec3,
    pub rotxend: f32,
    pub posoffset: Vec3,
    pub rotxoffset: f32,
    pub posrotmtx: Mat4,
    pub useposrot: bool,
    pub damppos: Vec3,
    pub damplook: Vec3,
    pub dampup: Vec3,
    pub damppossum: Vec3,
    pub damplooksum: Vec3,
    pub dampupsum: Vec3,
    pub blendpos: [Vec3; 4],
    pub blendlook: [Vec3; 4],
    pub blendup: [Vec3; 4],
    pub curblendpos: i32,
    pub dampt: f32,
    pub blendscale: f32,
    pub blendscale1: f32,
    pub sideflag: i32,
    pub adjustdamp: Vec3,
    pub adjustpos: Vec3,
    pub xshift: f32,
    pub aimpos: Vec3,
    pub allowshootframe: i32,
    pub lastshootframe60: i32,
    pub noiseradius: f32,
    pub slidetrans: f32,
    pub slideinc: bool,
    pub loadedammo: [i32; 2],
    pub clipsizes: [i32; 2],
    /// The `matmot1/2/3` union: mm_maulercharge / mm_reaperrot / mm_shotgunfrac…
    pub matmot1: f32,
    pub matmot2: f32,
    pub matmot3: f32,
    pub loadslide: f32,
    pub upgrademult: [f32; 2],
    pub finalmult: [f32; 2],
    pub cammtx: Mat4,
    pub posmtx: Mat4,
    pub prevmtx: Mat4,
    pub muzzlepos: Vec3,
    pub muzzlez: f32,
    pub muzzlemat: Mat4,
    pub burstbullets: i32,
    pub hitpos: Vec3,
    pub lastdirvalid: bool,
    pub shotstotake: i32,
    pub shotremainder: f32,
    pub state: i32,
    pub stateminor: i32,
    pub stateflags: u32,
    pub stateframes: i32,
    pub statecycles: i32,
    pub statelastframe: i32,
    pub statevar1: i32,
    /// `gs_float1` aka `gs_barrelspeedfrac`.
    pub gs_barrelspeedfrac: f32,
    pub animload: i32,
    pub animframeinc: i32,
    pub animmode: i32,
    pub unk0cc8_01: bool,
    pub unk0cc8_02: bool,
    pub incrementalreloading: bool,
    pub ejectcount: i32,
    pub unk0cc8_07: bool,
    pub unk0cc8_08: bool,
    pub animloopcount: i32,
    pub crosspos: [f32; 2],
    pub guncrosspossum: [f32; 2],
    pub attacktype: i32,
    pub animcmd: Option<CmdPtr>,
    pub animcmd2: Option<CmdPtr>,
    pub gangstarot: f32,
    pub primetimer60: i32,
    pub ejectstate: i32,
    pub ejecttype: i32,
    pub unk0d0e_07: bool,
    pub createsmoke: bool,
    pub forcecreatesmoke: bool,
    pub unk0d0f_02: bool,
    pub activatesecondary: bool,
    pub gunroundsspent: [u16; 4],
    /// `ispare1` — the gangsta delay timer.
    pub ispare1: i32,
    pub gunsmokepoint: f32,
    pub fspare1: f32,
    pub fspare2: f32,
    pub lastrotangx: f32,
    pub lastrotangy: f32,
    /// Is a looping per-hand sound (Reaper spin / Mauler charge) playing?
    pub audiohandle: bool,
    /// `gs_int1` / `gs_int2`, used by reload.
    pub gs_int1: i32,
    pub gs_int2: i32,

    /// `hand->anim`, shared by `gunmodel` and `handmodel`.
    pub anim: Anim,
    /// `hand->gunmodel` (+ its `unk0a6c` toggle state).
    pub gunmodel: Option<Model>,
    /// `hand->handmodel` (+ its `handsavedata` toggle state).
    pub handmodel: Option<Model>,
    /// The flash-toggle node indices (parts 0x5a..0x5c) found this frame.
    pub flash_toggles: Vec<usize>,
    /// Whether this frame's flash quads should be drawn (`bgun_update_shotgun`'s arg2).
    pub star_flash: bool,
    /// Render-side extras computed in `bgun0f0a5550`.
    pub dualflip: bool,
    /// `hand->rocket`: the rocket sitting in the launcher (a world object id).
    pub rocket: Option<u32>,
    /// `hand->firedrocket`.
    pub firedrocket: bool,
}

impl Hand {
    /// `bgun_reset`'s positional initializer (`bondgunreset.c:17`).
    fn new() -> Self {
        Hand {
            weaponnum: 0,
            weaponfunc: 0,
            upgradewant: 0,
            firing: false,
            flashon: false,
            visible: false,
            inuse: false,
            triggeron: false,
            triggerprev: false,
            triggerreleased: false,
            count: 0,
            count60: 0,
            mode: 0,
            modenext: 0,
            numfires: 0,
            pausetime60: 0,
            pausechange: 0,
            posstart: Vec3::ZERO,
            rotxstart: 0.0,
            posend: Vec3::ZERO,
            rotxend: 0.0,
            posoffset: Vec3::ZERO,
            rotxoffset: 0.0,
            posrotmtx: Mat4::IDENTITY,
            useposrot: false,
            damppos: Vec3::ZERO,
            damplook: Vec3::new(0.0, 0.0, -1.0),
            dampup: Vec3::new(0.0, 1.0, 0.0),
            damppossum: Vec3::ZERO,
            damplooksum: Vec3::new(0.0, 0.0, -19.999_996),
            dampupsum: Vec3::new(0.0, 19.999_996, 0.0),
            blendpos: [Vec3::ZERO; 4],
            blendlook: [Vec3::new(0.0, 0.0, -1.0); 4],
            blendup: [Vec3::new(0.0, 1.0, 0.0); 4],
            curblendpos: 0,
            dampt: 0.0,
            blendscale: 1.0,
            blendscale1: 1.0,
            sideflag: 0,
            adjustdamp: Vec3::ZERO,
            adjustpos: Vec3::ZERO,
            xshift: 0.0,
            aimpos: Vec3::new(0.0, 0.0, 1000.0),
            allowshootframe: 0,
            lastshootframe60: 0,
            noiseradius: 0.0,
            slidetrans: 0.0,
            slideinc: false,
            loadedammo: [0; 2],
            clipsizes: [0; 2],
            matmot1: 0.0,
            matmot2: 0.0,
            matmot3: 0.0,
            loadslide: 0.0,
            upgrademult: [1.0; 2],
            finalmult: [1.0; 2],
            cammtx: Mat4::IDENTITY,
            posmtx: Mat4::IDENTITY,
            prevmtx: Mat4::IDENTITY,
            muzzlepos: Vec3::ZERO,
            muzzlez: 0.0,
            muzzlemat: Mat4::IDENTITY,
            burstbullets: 0,
            hitpos: Vec3::ZERO,
            lastdirvalid: false,
            shotstotake: 0,
            shotremainder: 0.0,
            state: HANDSTATE_IDLE,
            stateminor: 0,
            stateflags: 0,
            stateframes: 0,
            statecycles: 0,
            statelastframe: 0,
            statevar1: 0,
            gs_barrelspeedfrac: 0.0,
            animload: -1,
            animframeinc: 0,
            animmode: HANDANIMMODE_IDLE,
            unk0cc8_01: false,
            unk0cc8_02: false,
            incrementalreloading: false,
            ejectcount: 0,
            unk0cc8_07: false,
            unk0cc8_08: false,
            animloopcount: 0,
            crosspos: [0.0; 2],
            guncrosspossum: [0.0; 2],
            attacktype: 0,
            animcmd: None,
            animcmd2: None,
            gangstarot: 0.0,
            primetimer60: 0,
            ejectstate: EJECTSTATE_INACTIVE,
            ejecttype: EJECTTYPE_GUN,
            unk0d0e_07: false,
            createsmoke: false,
            forcecreatesmoke: false,
            unk0d0f_02: false,
            activatesecondary: false,
            gunroundsspent: [0; 4],
            ispare1: 0,
            gunsmokepoint: 0.0,
            fspare1: 0.0,
            fspare2: 0.0,
            lastrotangx: 0.0,
            lastrotangy: 0.0,
            audiohandle: false,
            gs_int1: 0,
            gs_int2: 0,
            anim: Anim::default(),
            gunmodel: None,
            handmodel: None,
            flash_toggles: Vec::new(),
            star_flash: false,
            dualflip: false,
            rocket: None,
            firedrocket: false,
        }
    }
}

/// `struct gunctrl` (the fields used).
pub struct GunCtrl {
    pub weaponnum: i32,
    pub prevweaponnum: i32,
    pub switchtoweaponnum: i32,
    pub dualwielding: bool,
    pub prevwasdualwielding: bool,
    pub invertgunfunc: bool,
    pub wantammo: bool,
    pub throwing: bool,
    pub gangsta: bool,
    pub ammotypes: [i32; 2],
    // gun memory loading (bgun_tick_master_load), emulated by step counts
    pub gunmemtype: i32,
    pub gunmemnew: i32,
    pub load_steps: i32,
    pub handfilenum: String,
}

/// The `g_Vars.currentplayer` fields the gun code touches, plus camera state it
/// reads through `cam_get_*`.
pub struct PlayerGun {
    pub crosspos: [f32; 2],
    pub crosspossum: [f32; 2],
    pub oldcrosspos: [f32; 2],
    pub guncrossdamp: f32,
    pub crosspos2: [f32; 2],
    pub crosssum2: [f32; 2],
    pub gunaimdamp: f32,
    pub gunposamplitude: f32,
    pub gunxamplitude: f32,
    pub gunampsum: f32,
    pub cyclesum: f32,
    pub synccount: f32,
    pub syncchange: f32,
    pub gunsync: f32,
    pub syncoffset: i32,
    pub guncloseroffset: f32,
    pub bondbreathing: f32,
    pub crouchpos: i32,
    pub playertriggeron: bool,
    pub playertriggerprev: bool,
    pub playertrigtime240: i32,
    pub curguntofire: usize,
    pub doautoselect: bool,
    /// `gunshadecol` RGBA.
    pub gunshadecol: [u8; 4],
    pub insightaimmode: bool,
    pub ammoheldarr: [i32; 40],
    pub gunzoomfovs: [f32; 3],
    /// `g_PlayerConfigsArray[].gunfuncs` — per-weapon "use secondary" bits.
    pub gunfuncs: [u8; 8],
    pub isdead: bool,
    /// Inventory: which weapon numbers the player holds (single / double).
    pub inventory: Vec<(i32, bool)>,
    pub unlimited_ammo: bool,

    // camera (cam_get_screen_*, cam_get_projection_mtxf, c_scalex/y)
    pub screen_width: f32,
    pub screen_height: f32,
    pub screen_left: f32,
    pub screen_top: f32,
    pub c_scalex: f32,
    pub c_scaley: f32,
    /// `c_lodscalez` (`cam_set_scale`, `camera.c:86`).
    pub c_lodscalez: f32,
    pub fovy: f32,
    pub aspect: f32,
    /// `cam_get_projection_mtxf()`: camera space → world (mtx00016b58 of the camera).
    pub projection: Mat4,
    /// `cam_get_world_to_screen_mtxf()`: world → camera space.
    pub world_to_screen: Mat4,
}

/// `g_AmmoTypes[].capacity` (`bondgun.c:9316`).
pub const AMMO_CAPACITY: [i32; 33] = [
    0, 800, 800, 69, 400, 100, 100, 12, 3, 10, 200, 40, 10, 10, 10, 800, 15, 50, 10, 200, 18000, 4, 200, 2,
    10, 10, 10, 1000, 10, 50, 1, 200, 10,
];

pub struct Bgun {
    pub gset: Arc<Gset>,
    pub bank: Arc<AnimBank>,
    pub models: HashMap<String, Arc<ModelDef>>,
    pub hands: [Hand; 2],
    pub ctrl: GunCtrl,
    pub p: PlayerGun,
    pub rng: Rng,
    pub events: Vec<GunEvent>,
    /// The player's hand model stem (`g_HeadsAndBodies[].handfilenum`).
    pub hand_model: String,
    /// `var8009d140`: the Reaper barrel angle fed to the joint callback.
    pub(crate) reaper_rot: f32,
    /// This frame's timing.
    pub lv: Lv,
    /// `speedtheta * 0.3 + gunextraaimx`, `-speedverta * 0.1 + gunextraaimy` —
    /// the unarmed fists' swivel target (`bgun_swivel`, `:4912`), set by bmove.
    pub swivel_extra: [f32; 2],
    /// `g_Vars.normmplayerisrunning` — Combat Simulator rules (shorter raise).
    pub mp: bool,
}

impl Bgun {
    pub fn new(gset: Arc<Gset>, bank: Arc<AnimBank>, models: HashMap<String, Arc<ModelDef>>, hand_model: &str) -> Self {
        let mut b = Bgun {
            gset,
            bank,
            models,
            hands: [Hand::new(), Hand::new()],
            ctrl: GunCtrl {
                weaponnum: WEAPON_NONE,
                prevweaponnum: WEAPON_UNARMED,
                switchtoweaponnum: -1,
                dualwielding: false,
                prevwasdualwielding: false,
                invertgunfunc: false,
                wantammo: false,
                throwing: false,
                gangsta: false,
                ammotypes: [-1; 2],
                gunmemtype: 0,
                gunmemnew: -1,
                load_steps: 0,
                handfilenum: String::new(),
            },
            p: PlayerGun {
                crosspos: [0.0; 2],
                crosspossum: [0.0; 2],
                oldcrosspos: [0.0; 2],
                guncrossdamp: 0.9,
                crosspos2: [0.0; 2],
                crosssum2: [0.0; 2],
                gunaimdamp: 0.9,
                gunposamplitude: 1.0,
                gunxamplitude: 1.0,
                gunampsum: 0.0,
                cyclesum: 0.0,
                synccount: 0.0,
                syncchange: 0.0,
                gunsync: 0.0,
                syncoffset: 0,
                guncloseroffset: 0.0,
                bondbreathing: 0.0,
                crouchpos: CROUCHPOS_STAND,
                playertriggeron: false,
                playertriggerprev: false,
                playertrigtime240: 0,
                curguntofire: 0,
                doautoselect: false,
                gunshadecol: [0xff, 0xff, 0xff, 0],
                insightaimmode: false,
                ammoheldarr: [0; 40],
                gunzoomfovs: [15.0, 60.0, 30.0],
                gunfuncs: [0; 8],
                isdead: false,
                inventory: Vec::new(),
                unlimited_ammo: true,
                screen_width: 320.0,
                screen_height: 240.0,
                screen_left: 0.0,
                screen_top: 0.0,
                c_scalex: 1.0,
                c_scaley: 1.0,
                c_lodscalez: 1.0,
                fovy: 60.0,
                aspect: 4.0 / 3.0,
                projection: Mat4::IDENTITY,
                world_to_screen: Mat4::IDENTITY,
            },
            rng: Rng::new(0x1234_5678),
            events: Vec::new(),
            hand_model: hand_model.to_owned(),
            reaper_rot: 0.0,
            lv: Lv::step(4, 0, 0),
            swivel_extra: [0.0; 2],
            mp: true,
        };
        // bgun_reset: bgun_calculate_blend x3 per hand.
        for _ in 0..3 {
            b.bgun_calculate_blend(HAND_RIGHT);
        }
        for _ in 0..3 {
            b.bgun_calculate_blend(HAND_LEFT);
        }
        b
    }

    pub(crate) fn randomfrac(&mut self) -> f32 {
        self.rng.randomfrac()
    }

    pub(crate) fn weapon(&self, weaponnum: i32) -> Option<&WeaponDef> {
        self.gset.weapon(weaponnum)
    }

    pub(crate) fn func_of(&self, h: usize) -> Option<FuncDef> {
        let hand = &self.hands[h];
        self.gset.func(hand.weaponnum, hand.weaponfunc).cloned()
    }

    pub(crate) fn func_by(&self, h: usize, which: usize) -> Option<FuncDef> {
        self.gset.func(self.hands[h].weaponnum, which).cloned()
    }

    pub(crate) fn sound(&mut self, id: u16, speed: f32) {
        self.events.push(GunEvent::Sound { id, speed });
    }

    /// `bgun_get_weapon_num` (`:5430`).
    pub fn bgun_get_weapon_num(&self, h: usize) -> i32 {
        if !self.hands[h].inuse {
            WEAPON_NONE
        } else {
            self.ctrl.weaponnum
        }
    }

    // ─── visibility (339-445) ────────────────────────────────────────────────

    /// `bgun_set_part_visible` (`:364`): hand parts go to the hand model.
    pub fn bgun_set_part_visible(&mut self, h: usize, partnum: i32, visible: bool) {
        let hand = &mut self.hands[h];
        if partnum == MODELPART_HAND_LEFT || partnum == MODELPART_HAND_RIGHT {
            if let Some(m) = hand.handmodel.as_mut() {
                m.set_part_visible(partnum, visible);
            }
        } else if let Some(m) = hand.gunmodel.as_mut() {
            m.set_part_visible(partnum, visible);
        }
    }

    /// `bgun_execute_gun_vis_commands` (`:389`) + `bgun_test_gun_vis_command`.
    pub(crate) fn bgun_execute_gun_vis_commands(&mut self, h: usize) {
        let Some(w) = self.weapon(self.hands[h].weaponnum) else { return };
        let cmds = w.gunviscmds.clone();
        for cmd in cmds {
            let result = match cmd.ctype {
                4 => ((self.hands[h].upgradewant >> cmd.param) & 1) != 0,
                5 => h == HAND_LEFT,
                6 => h == HAND_RIGHT,
                _ => true,
            };
            if result {
                match cmd.op {
                    0 | 3 => self.bgun_set_part_visible(h, cmd.partnum, true),
                    1 => self.bgun_set_part_visible(h, cmd.partnum, false),
                    _ => {}
                }
            } else if cmd.op == 3 {
                self.bgun_set_part_visible(h, cmd.partnum, false);
            }
        }
    }

    /// `bgun_update_ammo_visibility` (`:425`).
    pub(crate) fn bgun_update_ammo_visibility(&mut self, h: usize) {
        self.bgun_execute_gun_vis_commands(h);
        self.bgun_set_part_visible(h, MODELPART_0042, false);
        let Some(w) = self.weapon(self.hands[h].weaponnum).cloned() else { return };
        for i in 0..2 {
            if let Some(a) = &w.ammos[i] {
                if a.flags & AMMOFLAG_QTYAFFECTSPARTVIS != 0 {
                    for j in 0..self.hands[h].clipsizes[i] {
                        let vis = j < self.hands[h].loadedammo[i];
                        self.bgun_set_part_visible(h, j + 100, vis);
                    }
                }
            }
        }
    }

    // ─── gun scripts (447-843) ───────────────────────────────────────────────

    pub(crate) fn anim_num_frames(&self, h: usize) -> i32 {
        self.hands[h].anim.num_frames(&self.bank)
    }

    /// `bgun_get_current_keyframe` (`:447`).
    pub fn bgun_get_current_keyframe(&self, h: usize) -> f32 {
        let hand = &self.hands[h];
        if hand.animmode == HANDANIMMODE_BUSY {
            if let Some(p) = hand.animcmd {
                if let GunCmd::PlayAnimation { params, .. } = self.gset.cmd(p) {
                    if *params < 0 {
                        return self.anim_num_frames(h) as f32 - hand.anim.cur_frame();
                    }
                }
                return hand.anim.cur_frame();
            }
        }
        0.0
    }

    /// `bgun_tick_anim` (`:460`), NTSC (integer keyframes, full-speed ticking).
    pub(crate) fn bgun_tick_anim(&mut self, h: usize) {
        self.hands[h].ejectcount = 0;
        if self.hands[h].animmode == HANDANIMMODE_BUSY
            && self.bgun_get_current_keyframe(h) >= (self.anim_num_frames(h) - 1) as f32
        {
            self.hands[h].animmode = HANDANIMMODE_IDLE;
        }
        if !(self.hands[h].animmode == HANDANIMMODE_BUSY || self.hands[h].animload >= 0) {
            return;
        }
        if self.hands[h].gangstarot > 0.0 {
            self.hands[h].animframeinc = 0;
        }
        if self.hands[h].animload >= 0 {
            let mut animspeedmult = 1.0;
            let params = match self.hands[h].animcmd.map(|p| self.gset.cmd(p).clone()) {
                Some(GunCmd::PlayAnimation { params, .. }) => params,
                _ => 10000,
            };
            let animspeed = params as f32 / 10000.0;
            if self.hands[h].unk0d0e_07 && self.hands[HAND_LEFT].inuse {
                animspeedmult = self.randomfrac() * 0.77 + 0.7;
            }
            let animload = self.hands[h].animload as u16;
            let bank = self.bank.clone();
            let mut ctx = AnimCtx { bank: &bank, scale: 1.0, chrinfo: None, merging_enabled: true };
            let hand = &mut self.hands[h];
            hand.anim.set_animation(&mut ctx, animload, false, 0.0, animspeedmult * animspeed, 0.0);
            if hand.animcmd.is_some() && animspeed < 0.0 {
                let n = hand.anim.num_frames(&bank) as f32;
                hand.anim.set_frame(&bank, n);
            }
            hand.animload = -1;
            hand.animmode = HANDANIMMODE_BUSY;
        }
        if self.hands[h].unk0cc8_02 {
            self.hands[h].animframeinc = 0;
        }

        let mut oldkeyframe = self.bgun_get_current_keyframe(h) as i32;
        let mut newkeyframe = oldkeyframe + self.hands[h].animframeinc;
        if oldkeyframe == 0 && newkeyframe > 0 {
            oldkeyframe -= 1;
        }

        // Pre-tick commands: part visibility, WAITFORZRELEASED, REPEATUNTILFULL.
        if let Some(start) = self.hands[h].animcmd {
            let mut parts: Vec<(i32, i32, bool)> = Vec::new(); // (partnum, frame, visible)
            let mut i = start.1;
            loop {
                let cmd = self.gset.cmd((start.0, i)).clone();
                match cmd {
                    GunCmd::End => break,
                    GunCmd::ShowPart { keyframe, part } | GunCmd::HidePart { keyframe, part } => {
                        if newkeyframe >= keyframe {
                            let show = matches!(cmd, GunCmd::ShowPart { .. });
                            match parts.iter_mut().find(|p| p.0 == part) {
                                Some(p) => {
                                    if keyframe > p.1 {
                                        p.1 = keyframe;
                                        p.2 = show;
                                    }
                                }
                                None => parts.push((part, keyframe, show)),
                            }
                        }
                    }
                    GunCmd::WaitForZReleased { keyframe } => {
                        if self.hands[h].unk0cc8_01
                            && newkeyframe >= keyframe
                            && oldkeyframe < keyframe
                            && oldkeyframe < newkeyframe
                        {
                            let mut tmp = keyframe - self.bgun_get_current_keyframe(h) as i32;
                            tmp /= 2;
                            if self.hands[h].animframeinc > tmp {
                                self.hands[h].animframeinc = tmp;
                            }
                            newkeyframe = oldkeyframe + self.hands[h].animframeinc;
                        }
                    }
                    GunCmd::RepeatUntilFull { keyframe, gotokeyframe } => {
                        if self.hands[h].incrementalreloading
                            && newkeyframe >= keyframe
                            && oldkeyframe < keyframe
                            && oldkeyframe < newkeyframe
                        {
                            let foundkeyframe =
                                gotokeyframe + ((newkeyframe - keyframe) % ((keyframe - gotokeyframe) + 1));
                            oldkeyframe = foundkeyframe;
                            self.hands[h].animframeinc = 0;
                            let bank = self.bank.clone();
                            self.hands[h].anim.set_frame(&bank, foundkeyframe as f32);
                            self.hands[h].animloopcount += 1;
                            newkeyframe = foundkeyframe;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            for (part, _, vis) in parts {
                self.bgun_set_part_visible(h, part, vis);
            }
        }

        // model_tick_anim(&hand->gunmodel, hand->animframeinc, true)
        {
            let bank = self.bank.clone();
            let mut ctx = AnimCtx { bank: &bank, scale: 1.0, chrinfo: None, merging_enabled: true };
            let inc = self.hands[h].animframeinc;
            self.hands[h].anim.tick(&mut ctx, inc, true);
        }

        // Post-tick commands: sounds, sound speed, casing ejection.
        let newkeyframe = self.bgun_get_current_keyframe(h) as i32;
        if let Some(start) = self.hands[h].animcmd {
            let mut speed = 1.0f32;
            let mut hasspeed = false;
            let mut i = start.1;
            loop {
                let cmd = self.gset.cmd((start.0, i)).clone();
                if cmd == GunCmd::End {
                    break;
                }
                let kf = cmd.keyframe();
                if newkeyframe >= kf && oldkeyframe < kf && oldkeyframe < newkeyframe {
                    match cmd {
                        GunCmd::PlaySound { sound, .. } => {
                            // snd_start_extra(..., cmd->soundnum, speed, ...)
                            self.sound(sound, if hasspeed { speed } else { 1.0 });
                            hasspeed = false;
                        }
                        GunCmd::SetSoundSpeed { speed: s, .. } => {
                            speed = s as f32 / 1000.0;
                            hasspeed = true;
                        }
                        GunCmd::PopOutSackOfPills { .. } => {
                            self.hands[h].ejectcount += 1;
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
        }
    }

    /// `bgun_test_condition` (`:709`).
    pub(crate) fn bgun_test_condition(&self, condition: u8, h: usize) -> bool {
        match condition {
            0 => true,
            1 => self.hands[HAND_LEFT].inuse,
            2 => self.hands[h].weaponfunc == FUNC_SECONDARY,
            _ => false,
        }
    }

    /// `bgun_start_animation` (`:728`).
    pub fn bgun_start_animation(&mut self, script: ScriptId, h: usize) {
        self.bgun_start_animation_at((script, 0), h);
    }

    pub(crate) fn bgun_start_animation_at(&mut self, cmd: CmdPtr, h: usize) {
        match self.gset.cmd(cmd).clone() {
            GunCmd::PlayAnimation { anim, .. } => {
                let hand = &mut self.hands[h];
                hand.animload = anim as i32;
                hand.animmode = HANDANIMMODE_IDLE;
                hand.unk0cc8_01 = false;
                hand.incrementalreloading = false;
                hand.animcmd = Some(cmd);
                hand.animloopcount = 0;
                hand.unk0cc8_02 = false;
                hand.unk0d0e_07 = false;
                hand.animcmd2 = Some(cmd);
            }
            _ => {
                let rand = self.rng.random() % 100;
                let mut done = false;
                let mut i = cmd.1;
                loop {
                    let c = self.gset.cmd((cmd.0, i)).clone();
                    if c == GunCmd::End {
                        break;
                    }
                    match c {
                        GunCmd::Include { condition, target } => {
                            if self.bgun_test_condition(condition, h) && !done && target != usize::MAX {
                                done = true;
                                self.bgun_start_animation_at((target, 0), h);
                            }
                        }
                        GunCmd::Random { probability, target } => {
                            if !done
                                && target != usize::MAX
                                && self.hands[h].animcmd2 != Some((target, 0))
                                && (rand as i32) < probability
                            {
                                done = true;
                                self.bgun_start_animation_at((target, 0), h);
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
        }
    }

    /// `bgun_anim_allows_feature` (`:763`), NTSC integer compare.
    pub fn bgun_anim_allows_feature(&self, h: usize, feature: i32) -> bool {
        let hand = &self.hands[h];
        if hand.animmode == HANDANIMMODE_IDLE {
            return hand.animload == -1;
        }
        let Some(start) = hand.animcmd else { return true };
        let mut allowkeyframe = -1;
        let mut zreleasekeyframe = -1;
        let mut i = start.1;
        loop {
            let c = self.gset.cmd((start.0, i));
            if *c == GunCmd::End || allowkeyframe != -1 {
                break;
            }
            match *c {
                GunCmd::WaitForZReleased { keyframe } => zreleasekeyframe = keyframe,
                GunCmd::AllowFeature { keyframe, feature: f } if f == feature => allowkeyframe = keyframe,
                _ => {}
            }
            i += 1;
        }
        if allowkeyframe >= 0 {
            if hand.unk0cc8_01 && (self.bgun_get_current_keyframe(h) as i32) <= zreleasekeyframe {
                return false;
            }
            return self.bgun_get_current_keyframe(h) + hand.animframeinc as f32 >= allowkeyframe as f32;
        }
        true
    }

    /// `bgun_is_anim_busy`.
    pub(crate) fn bgun_is_anim_busy(&self, h: usize) -> bool {
        self.hands[h].animmode != HANDANIMMODE_IDLE
    }

    /// `bgun_reset_anim` (`:833`).
    pub(crate) fn bgun_reset_anim(&mut self, h: usize) {
        let hand = &mut self.hands[h];
        hand.animload = -1;
        hand.animmode = HANDANIMMODE_IDLE;
        hand.unk0cc8_01 = false;
        hand.incrementalreloading = false;
        hand.animcmd = None;
        hand.animloopcount = 0;
        hand.unk0cc8_02 = false;
        hand.unk0d0e_07 = false;
    }

    // ─── ammo (862-1034) ─────────────────────────────────────────────────────

    pub(crate) fn ammoheld(&self, ammotype: i32) -> i32 {
        if ammotype < 0 {
            return 0;
        }
        self.p.ammoheldarr.get(ammotype as usize).copied().unwrap_or(0)
    }

    /// `bgun_get_ammo_state` (`:862`).
    pub fn bgun_get_ammo_state(&self, funcnum: usize, h: usize) -> i32 {
        let hand = &self.hands[h];
        let Some(func) = self.gset.func(hand.weaponnum, funcnum) else {
            return GUNAMMOSTATE_DEPLETED;
        };
        let mut state = GUNAMMOSTATE_CLIPFULL;
        if func.ammoindex != -1 {
            let ai = func.ammoindex as usize;
            if self.ctrl.ammotypes[ai] >= 0 && hand.loadedammo[ai] < hand.clipsizes[ai] {
                let mut minqty = 1;
                if hand.weaponnum == WEAPON_SHOTGUN && funcnum == FUNC_SECONDARY {
                    minqty = 2;
                }
                if hand.weaponnum == WEAPON_TRANQUILIZER && funcnum == FUNC_SECONDARY {
                    minqty = 4;
                }
                state = GUNAMMOSTATE_CLIPYES_HELDYES;
                if hand.loadedammo[ai] < minqty {
                    state = GUNAMMOSTATE_NEEDRELOAD;
                    if self.ammoheld(self.ctrl.ammotypes[ai]) == 0 {
                        state = GUNAMMOSTATE_DEPLETED;
                    }
                } else if self.ammoheld(self.ctrl.ammotypes[ai]) == 0 {
                    state = GUNAMMOSTATE_CLIPYES_HELDNO;
                }
            }
        }
        state
    }

    /// `bgun0f098df8` (`:904`): move ammo from reserve into the clip.
    pub(crate) fn bgun_load_clip(&mut self, weaponfunc: usize, h: usize, onebullet: bool, checkunequipped: bool) {
        let Some(func) = self.func_by(h, weaponfunc) else { return };
        if func.ammoindex == -1 {
            return;
        }
        let ai = func.ammoindex as usize;
        let ammotype = self.ctrl.ammotypes[ai];
        if ammotype < 0 {
            return;
        }
        let hand = &self.hands[h];
        let mut amount = hand.clipsizes[ai] - hand.loadedammo[ai];
        let reloadindex = match hand.weaponnum {
            WEAPON_CROSSBOW => 0,
            WEAPON_SHOTGUN => 1,
            WEAPON_DY357MAGNUM => 2,
            WEAPON_DY357LX => 3,
            _ => -1,
        };
        if checkunequipped && reloadindex >= 0 {
            amount -= (hand.gunroundsspent[reloadindex as usize] >> 8) as i32;
        }
        if onebullet {
            amount = 1;
        }
        let held = self.ammoheld(ammotype);
        if amount > held {
            amount = held;
        }
        let flags = self.weapon(hand.weaponnum).and_then(|w| w.ammos[ai].as_ref()).map_or(0, |a| a.flags);
        self.hands[h].loadedammo[ai] += amount;
        self.p.ammoheldarr[ammotype as usize] -= amount;
        if flags & AMMOFLAG_NORESERVE != 0 {
            self.p.ammoheldarr[ammotype as usize] = 0;
        }
    }

    /// `bgun0f098f8c` (`:957`).
    pub(crate) fn bgun_load_all_clips(&mut self, h: usize) {
        for i in 0..2 {
            if self.func_by(h, i).is_some() {
                self.bgun_load_clip(i, h, false, true);
            }
        }
    }

    /// `bgun_clip_has_ammo` (`:968`).
    pub(crate) fn bgun_clip_has_ammo(&self, h: usize) -> bool {
        self.bgun_get_ammo_state(FUNC_PRIMARY, h) > GUNAMMOSTATE_NEEDRELOAD
            || self.bgun_get_ammo_state(FUNC_SECONDARY, h) > GUNAMMOSTATE_NEEDRELOAD
    }

    /// `bgun0f0990b0` (`:985`): is this function unusable for autoswitch purposes?
    pub(crate) fn bgun_func_unusable(&self, func: Option<&FuncDef>, weaponnum: i32) -> bool {
        let Some(f) = func else { return true };
        if f.ftype == INVENTORYFUNCTYPE_NONE {
            return true;
        }
        if f.kind() == INVENTORYFUNCTYPE_MELEE {
            return true;
        }
        if f.kind() == INVENTORYFUNCTYPE_SPECIAL {
            return true;
        }
        if f.kind() == INVENTORYFUNCTYPE_THROW && f.ammoindex <= -1 {
            return true;
        }
        if f.ammoindex >= 0 {
            if let Some(a) = self.weapon(weaponnum).and_then(|w| w.ammos[f.ammoindex as usize].as_ref()) {
                if self.bgun_get_ammo_count(a.ammotype) <= 0 {
                    return true;
                }
            }
        }
        false
    }

    /// `bgun0f099188` (`:1024`).
    pub(crate) fn bgun_other_func_unusable(&self, h: usize, gunfunc: usize) -> bool {
        if self.bgun_is_using_secondary_function() as usize == gunfunc {
            return false;
        }
        let w = self.hands[h].weaponnum;
        self.bgun_func_unusable(self.gset.func(w, gunfunc), w)
    }

    /// `bgun_get_ammo_count` (`:9415`): reserve + both loaded clips.
    pub fn bgun_get_ammo_count(&self, ammotype: i32) -> i32 {
        let mut total = self.ammoheld(ammotype);
        for h in 0..2 {
            for i in 0..2 {
                if self.ctrl.ammotypes[i] == ammotype && self.hands[h].inuse {
                    total += self.hands[h].loadedammo[i];
                }
            }
        }
        total
    }

    // ─── hand state machine (1036-3131) ──────────────────────────────────────

    /// `bgun_tick_inc_idle` (`:1036`).
    pub(crate) fn bgun_tick_inc_idle(&mut self, h: usize, lvupdate: i32) -> i32 {
        let gunfunc = self.bgun_is_using_secondary_function() as usize;
        {
            let hand = &mut self.hands[h];
            hand.lastdirvalid = false;
            hand.burstbullets = 0;
            hand.shotremainder = 0.0;
        }
        if self.bgun_is_ready_to_switch(h) && self.bgun_set_state(h, HANDSTATE_CHANGEGUN) {
            return lvupdate;
        }
        if gunfunc == self.hands[h].weaponfunc {
            self.hands[h].unk0cc8_07 = false;
        }
        self.hands[h].unk0cc8_08 = false;

        if self.hands[h].inuse {
            let ammostate = self.bgun_get_ammo_state(self.hands[h].weaponfunc, h);
            let weaponnum = self.hands[h].weaponnum;

            if gunfunc != self.hands[h].weaponfunc && self.hands[h].modenext != HANDMODE_RELOAD {
                let mut changefunc = true;
                if self.hands[h].unk0cc8_07 && self.bgun_get_ammo_state(1 - self.hands[h].weaponfunc, h) < 0 {
                    changefunc = false;
                }
                if changefunc && weaponnum == WEAPON_COMBATKNIFE {
                    if ammostate == GUNAMMOSTATE_NEEDRELOAD {
                        self.hands[h].count60 = 0;
                        self.hands[h].count = 0;
                        self.hands[h].weaponfunc = gunfunc;
                        if self.bgun_set_state(h, HANDSTATE_RELOAD) {
                            return lvupdate;
                        }
                    } else if ammostate <= GUNAMMOSTATE_DEPLETED {
                        changefunc = false;
                    }
                }
                if changefunc {
                    self.hands[h].unk0cc8_07 = false;
                    if self.bgun_set_state(h, HANDSTATE_CHANGEFUNC) {
                        return lvupdate;
                    }
                }
            }

            if ammostate <= GUNAMMOSTATE_DEPLETED {
                if self.gset.has_flag(weaponnum, WEAPONFLAG_THROWABLE)
                    && (weaponnum != WEAPON_REMOTEMINE || h != HAND_LEFT)
                    && self.bgun_set_state(h, HANDSTATE_AUTOSWITCH)
                {
                    return lvupdate;
                }
                let usesec = self.funcissec() as usize;
                if usesec == gunfunc {
                    let mut ammostate2 = self.bgun_get_ammo_state(1 - self.hands[h].weaponfunc, h);
                    if self.bgun_other_func_unusable(h, 1 - self.hands[h].weaponfunc) && weaponnum != WEAPON_REAPER {
                        if self.ctrl.wantammo {
                            let f = self.func_by(h, 1 - self.hands[h].weaponfunc);
                            if f.is_none_or(|f| f.kind() != INVENTORYFUNCTYPE_MELEE) {
                                ammostate2 = GUNAMMOSTATE_DEPLETED;
                            }
                        } else {
                            ammostate2 = GUNAMMOSTATE_DEPLETED;
                        }
                    }
                    if ammostate2 <= GUNAMMOSTATE_DEPLETED {
                        self.hands[h].unk0cc8_08 = true;
                    } else if !self.gset.has_flag(weaponnum, WEAPONFLAG_KEEPFUNCWHENEMPTY)
                        || self.hands[h].weaponfunc == FUNC_SECONDARY
                    {
                        self.hands[h].unk0cc8_07 = true;
                        if self.bgun_set_state(h, HANDSTATE_CHANGEFUNC) {
                            return lvupdate;
                        }
                    }
                }
            } else if ammostate == GUNAMMOSTATE_NEEDRELOAD {
                if self.hands[h].triggeron && weaponnum != WEAPON_NONE {
                    self.hands[h].unk0cc8_01 = false;
                    if self.bgun_set_state(h, HANDSTATE_ATTACKEMPTY) {
                        return lvupdate;
                    }
                } else {
                    self.hands[h].count60 = 0;
                    self.hands[h].count = 0;
                    if self.bgun_set_state(h, HANDSTATE_RELOAD) {
                        return lvupdate;
                    }
                }
            } else {
                if (self.hands[h].triggeron || (self.hands[h].activatesecondary && self.hands[h].weaponfunc == FUNC_SECONDARY))
                    && weaponnum != WEAPON_NONE
                {
                    self.p.doautoselect = false;
                    let hand = &mut self.hands[h];
                    hand.mode = HANDMODE_ATTACK;
                    hand.count = 0;
                    hand.count60 = 0;
                    hand.triggerreleased = false;
                    hand.activatesecondary = false;
                    if self.bgun_set_state(h, HANDSTATE_ATTACK) {
                        return lvupdate;
                    }
                }
                if self.hands[h].modenext != HANDMODE_NONE {
                    let next = self.hands[h].modenext;
                    let hand = &mut self.hands[h];
                    hand.mode = hand.modenext;
                    hand.count60 = 0;
                    hand.count = 0;
                    hand.modenext = HANDMODE_NONE;
                    if next == HANDMODE_RELOAD
                        && ammostate < GUNAMMOSTATE_CLIPYES_HELDNO
                        && ammostate >= GUNAMMOSTATE_NEEDRELOAD
                        && self.bgun_set_state(h, HANDSTATE_RELOAD)
                    {
                        return lvupdate;
                    }
                }
            }
        }

        if h == HAND_RIGHT {
            if self.ctrl.wantammo {
                self.bgun_auto_switch_weapon();
            } else {
                let (r, l) = (&self.hands[0], &self.hands[1]);
                if (r.unk0cc8_08 || !r.inuse) && (l.unk0cc8_08 || !l.inuse) && (r.triggeron || l.triggeron) {
                    self.bgun_auto_switch_weapon();
                }
                self.hands[0].unk0cc8_08 = false;
                self.hands[1].unk0cc8_08 = false;
            }
        }
        0
    }

    /// `bgun_set_arm_pitch` (`:1214`).
    pub(crate) fn bgun_set_arm_pitch(&mut self, h: usize, angle: f32) {
        let hand = &mut self.hands[h];
        hand.useposrot = true;
        hand.posrotmtx = pdmtx::load_x_rotation(angle);
        hand.posrotmtx.w_axis = glam::Vec4::new(0.0, (1.0 - angle.cos()) * -80.0, angle.sin() * 15.0, 1.0);
    }

    /// `bgun_tick_inc_autoswitch` (`:1225`) — the throwables-ran-out path.
    pub(crate) fn bgun_tick_inc_autoswitch(&mut self, h: usize, lvupdate: i32) -> i32 {
        let gunfunc = self.bgun_is_using_secondary_function() as usize;
        if !self.hands[h].inuse && self.bgun_set_state(h, HANDSTATE_IDLE) {
            return lvupdate;
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_AUTOSWITCH_UNEQUIP {
            let delay = if self.mp { 12 } else { 16 };
            if self.hands[h].stateframes >= delay {
                self.hands[h].stateminor += 1;
            } else {
                let a = self.hands[h].stateframes as f32 * max_pitch() / delay as f32;
                self.bgun_set_arm_pitch(h, a);
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_AUTOSWITCH_DELETE {
            self.hands[h].lastdirvalid = false;
            self.hands[h].shotremainder = 0.0;
            if self.bgun_is_ready_to_switch(h) && self.bgun_set_state(h, HANDSTATE_CHANGEGUN) {
                self.hands[h].mode = HANDMODE_6;
                self.hands[h].stateminor = HANDSTATEMINOR_AUTOSWITCH_2;
                self.hands[h].count = 0;
                return 0;
            }
            if self.hands[h].inuse {
                let ammostate = self.bgun_get_ammo_state(gunfunc, h);
                if self.p.doautoselect {
                    self.bgun_auto_switch_weapon();
                }
                if (GUNAMMOSTATE_NEEDRELOAD..=GUNAMMOSTATE_CLIPYES_HELDYES).contains(&ammostate)
                    && self.hands[1 - h].state != HANDSTATE_RELOAD
                {
                    self.hands[h].count60 = 0;
                    self.hands[h].count = 0;
                    if self.bgun_set_state(h, HANDSTATE_RELOAD) {
                        return lvupdate;
                    }
                }
                if self.hands[h].modenext != 0 {
                    let hand = &mut self.hands[h];
                    hand.mode = hand.modenext;
                    hand.count60 = 0;
                    hand.count = 0;
                    hand.modenext = HANDMODE_NONE;
                }
            }
            self.bgun_set_arm_pitch(h, max_pitch());
        }
        0
    }

    /// `bgun_is_reloading`.
    pub fn bgun_is_reloading(&self, h: usize) -> bool {
        self.hands[h].state == HANDSTATE_RELOAD
    }

    /// `bgun_tick_inc_reload` (`:1354`).
    pub(crate) fn bgun_tick_inc_reload(&mut self, h: usize, lvupdate: i32) -> i32 {
        let func = self.func_of(h);
        let weaponnum = self.hands[h].weaponnum;
        if self.p.isdead {
            self.hands[h].animmode = HANDANIMMODE_IDLE;
            self.hands[h].animload = -1;
            if self.bgun_set_state(h, HANDSTATE_IDLE) {
                return lvupdate;
            }
        }
        if self.hands[h].statecycles == 0 {
            self.hands[h].gs_int1 = -1;
            self.hands[h].gs_int2 = 0;
            let other = &self.hands[1 - h];
            if other.state == HANDSTATE_RELOAD && other.stateframes < 20 {
                self.hands[h].stateminor = HANDSTATEMINOR_RELOAD_WAIT;
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_RELOAD_WAIT {
            let other = &self.hands[1 - h];
            if other.state == HANDSTATE_RELOAD && other.stateframes < 20 {
                return 0;
            }
            let hand = &mut self.hands[h];
            hand.stateframes = 0;
            hand.statecycles = 0;
            hand.stateminor = HANDSTATEMINOR_RELOAD_MAIN;
            hand.statelastframe = 0;
        }
        let ammodef = |b: &Bgun, f: &Option<FuncDef>| -> Option<AmmoDef> {
            let f = f.as_ref()?;
            if f.ammoindex < 0 {
                return None;
            }
            b.weapon(weaponnum).and_then(|w| w.ammos[f.ammoindex as usize].clone())
        };
        if self.hands[h].stateminor == HANDSTATEMINOR_RELOAD_MAIN {
            if self.hands[h].statecycles == 0 {
                if func.as_ref().is_some_and(|f| f.ammoindex == 0 || f.ammoindex == 1) {
                    let a = ammodef(self, &func);
                    match a.as_ref().and_then(|a| a.reload_animation) {
                        Some(script) if weaponnum != WEAPON_COMBATKNIFE => {
                            self.bgun_start_animation(script, h);
                            self.hands[h].unk0d0e_07 = true;
                            if a.as_ref().is_some_and(|a| a.flags & AMMOFLAG_INCREMENTALRELOAD != 0) {
                                self.hands[h].incrementalreloading = true;
                            }
                            if weaponnum == WEAPON_GRENADE || weaponnum == WEAPON_NBOMB {
                                self.hands[h].ejectstate = EJECTSTATE_INACTIVE;
                            }
                        }
                        _ => {
                            self.hands[h].stateminor += 1;
                        }
                    }
                } else if self.bgun_set_state(h, HANDSTATE_IDLE) {
                    return lvupdate;
                }
            } else {
                let a = ammodef(self, &func);
                let incremental = a.as_ref().is_some_and(|a| a.flags & AMMOFLAG_INCREMENTALRELOAD != 0);
                if incremental {
                    if self.bgun_anim_allows_feature(h, GUNFEATURE_RELOAD) {
                        if self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0 {
                            let wf = self.hands[h].weaponfunc;
                            self.bgun_load_clip(wf, h, true, false);
                            self.hands[h].stateflags |= HANDSTATEFLAG_BUSY;
                            let ammostate = self.bgun_get_ammo_state(wf, h);
                            if ammostate >= GUNAMMOSTATE_CLIPYES_HELDNO || ammostate == GUNAMMOSTATE_DEPLETED {
                                self.hands[h].incrementalreloading = false;
                            }
                        }
                    } else {
                        self.hands[h].stateflags = 0;
                    }
                    if self.hands[h].triggeron {
                        self.hands[h].incrementalreloading = false;
                    }
                } else if self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0
                    && self.bgun_anim_allows_feature(h, GUNFEATURE_RELOAD)
                {
                    let wf = self.hands[h].weaponfunc;
                    self.bgun_load_clip(wf, h, false, false);
                    self.hands[h].stateflags |= HANDSTATEFLAG_BUSY;
                }
                if self.hands[h].animmode != HANDANIMMODE_BUSY && self.bgun_set_state(h, HANDSTATE_IDLE) {
                    return lvupdate;
                }
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_RELOAD_LOWER {
            if self.hands[h].count60 > 15 || !self.hands[h].visible {
                let hand = &mut self.hands[h];
                hand.mode = HANDMODE_11;
                hand.stateminor += 1;
                hand.pausetime60 = 17;
                hand.count60 = 0;
                hand.count = 0;
            } else {
                let a = self.hands[h].count60 as f32 * max_pitch() / 16.0;
                self.bgun_set_arm_pitch(h, a);
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_RELOAD_SOUND {
            if self.hands[h].count == 0 {
                if weaponnum == WEAPON_COMBATKNIFE {
                    if let Some(script) = ammodef(self, &func).and_then(|a| a.reload_animation) {
                        self.bgun_start_animation(script, h);
                        self.hands[h].unk0cc8_02 = true;
                    }
                }
                if self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0 {
                    let wf = self.hands[h].weaponfunc;
                    self.bgun_load_clip(wf, h, false, false);
                }
                if !self.p.isdead
                    && !matches!(
                        weaponnum,
                        WEAPON_NONE
                            | WEAPON_UNARMED
                            | WEAPON_COMBATKNIFE
                            | WEAPON_LASER
                            | WEAPON_GRENADE
                            | WEAPON_TIMEDMINE
                            | WEAPON_PROXIMITYMINE
                            | WEAPON_REMOTEMINE
                    )
                {
                    self.sound(SFXMAP_804F_RELOAD_DEFAULT, 1.0);
                }
            }
            if self.hands[h].count60 >= self.hands[h].pausetime60 && self.hands[h].count >= 2 {
                let hand = &mut self.hands[h];
                hand.mode = HANDMODE_12;
                hand.stateminor += 1;
                hand.count60 = 0;
                hand.count = 0;
            } else {
                self.bgun_set_arm_pitch(h, max_pitch());
            }
        }
        if self.hands[h].stateminor == HANDSTATEMINOR_RELOAD_RAISE {
            if weaponnum == WEAPON_COMBATKNIFE {
                self.hands[h].animmode = HANDANIMMODE_IDLE;
            }
            if self.hands[h].count == 0 {
                self.p.doautoselect = false;
            }
            if self.hands[h].count60 >= 23
                || !self.gset.has_model(weaponnum)
                || !self.gset.has_flag(weaponnum, WEAPONFLAG_00000040)
                || self.gset.has_flag(weaponnum, WEAPONFLAG_00000080)
            {
                let hand = &mut self.hands[h];
                hand.mode = HANDMODE_NONE;
                hand.count60 = 0;
                hand.count = 0;
                if self.bgun_set_state(h, HANDSTATE_IDLE) {
                    return lvupdate;
                }
            } else {
                let a = (23 - self.hands[h].count60) as f32 * max_pitch() / 23.0;
                self.bgun_set_arm_pitch(h, a);
            }
        }
        0
    }

    /// `bgun_tick_inc_changefunc` (`:1559`).
    pub(crate) fn bgun_tick_inc_changefunc(&mut self, h: usize, lvupdate: i32) -> i32 {
        let mut more = false;
        if self.hands[h].statecycles == 0 {
            let w = self.weapon(self.hands[h].weaponnum).cloned();
            let cmd = if self.hands[h].weaponfunc == FUNC_PRIMARY {
                self.hands[h].weaponfunc = FUNC_SECONDARY;
                w.and_then(|w| w.pritosec_animation)
            } else {
                self.hands[h].weaponfunc = FUNC_PRIMARY;
                w.and_then(|w| w.sectopri_animation)
            };
            if let Some(script) = cmd {
                self.bgun_start_animation(script, h);
                more = true;
            }
        } else if self.hands[h].animmode == HANDANIMMODE_BUSY {
            more = true;
        }
        if !more && self.bgun_set_state(h, HANDSTATE_IDLE) {
            return lvupdate;
        }
        0
    }

    /// `bgun0f09a3f8` (`:1593`): may this function fire this tick?
    /// -1 = stop, 0 = wait, 1 = fire and keep the burst, 2 = fire and finish.
    pub(crate) fn bgun_should_fire(&mut self, h: usize, func: &FuncDef) -> i32 {
        let mut burst = false;
        let mut smallburst = false;
        let bb = self.hands[h].burstbullets;
        if func.flags & FUNCFLAG_BURST3 != 0 && bb < 3 && (!self.p.insightaimmode || !func.is_auto()) {
            smallburst = true;
        }
        if func.flags & FUNCFLAG_BURST2 != 0 && bb < 2 {
            smallburst = true;
        }
        if func.flags & FUNCFLAG_BURST5 != 0 && bb < 5 {
            smallburst = true;
        }
        if func.flags & FUNCFLAG_BURST50 != 0 && bb < 50 {
            burst = true;
        }
        if smallburst {
            burst = true;
        }
        let hand_trig = self.hands[h].triggeron;
        let busy = self.hands[h].stateflags & HANDSTATEFLAG_BUSY != 0;
        let shoot = func.shoot.clone().unwrap_or_default();
        if hand_trig || !busy || burst {
            if func.ammoindex >= 0
                && self.hands[h].loadedammo[func.ammoindex as usize] == 0
                && self.ctrl.ammotypes[func.ammoindex as usize] >= 0
            {
                return -1;
            }
            if func.is_auto() {
                if shoot.turretaccel > 0.0 {
                    if self.hands[h].gs_barrelspeedfrac < 1.0 {
                        self.hands[h].gs_barrelspeedfrac += self.lv.lvupdate60freal / shoot.turretaccel;
                        if self.hands[h].gs_barrelspeedfrac > 1.0 {
                            self.hands[h].gs_barrelspeedfrac = 1.0;
                        }
                    }
                } else {
                    self.hands[h].gs_barrelspeedfrac = 1.0;
                }
                return 1;
            }
            self.hands[h].gs_barrelspeedfrac = 1.0;
            if smallburst {
                if self.hands[h].burstbullets > 0 {
                    let delay = if self.hands[h].weaponnum == WEAPON_SHOTGUN { 13 } else { 3 };
                    if self.hands[h].stateframes < delay {
                        return 0;
                    }
                }
                self.hands[h].stateframes = 0;
            }
            if func.flags & FUNCFLAG_BURST3 != 0 && bb == 2 {
                smallburst = false;
            }
            if func.flags & FUNCFLAG_BURST2 != 0 && bb == 1 {
                smallburst = false;
            }
            if func.flags & FUNCFLAG_BURST5 != 0 && bb == 4 {
                smallburst = false;
            }
            return if smallburst { 1 } else { 2 };
        }
        if func.is_auto() {
            if shoot.turretdecel > 0.0 {
                if self.hands[h].gs_barrelspeedfrac > 0.0 {
                    self.hands[h].gs_barrelspeedfrac -= self.lv.lvupdate60freal / shoot.turretdecel;
                    if self.hands[h].gs_barrelspeedfrac < 0.0 {
                        self.hands[h].gs_barrelspeedfrac = 0.0;
                        return -1;
                    }
                    return 1;
                }
            } else {
                self.hands[h].gs_barrelspeedfrac = 0.0;
            }
            return -1;
        }
        -1
    }

    /// `bgun0f09a6f8` (`:1710`): fire one tick's worth of rounds.
    pub(crate) fn bgun_fire(&mut self, h: usize, func: &FuncDef) {
        let mut usesammo = true;
        self.hands[h].firing = true;
        let shoot = func.shoot.clone().unwrap_or_default();
        if func.is_auto() {
            let tmp = shoot.initialrpm + (shoot.maxrpm - shoot.initialrpm) * self.hands[h].gs_barrelspeedfrac;
            let tmp2 = tmp / 60.0 * (self.lv.lvupdate60freal / 60.0) + self.hands[h].shotremainder;
            self.hands[h].shotstotake = tmp2 as i32;
            self.hands[h].shotremainder = tmp2 - self.hands[h].shotstotake as f32;
            if self.hands[h].shotstotake <= 0 {
                if self.hands[h].stateflags & HANDSTATEFLAG_BUSY == 0 {
                    self.hands[h].shotstotake += 1;
                } else {
                    self.hands[h].firing = false;
                }
            }
        } else {
            self.hands[h].shotstotake = 1;
            if self.hands[h].weaponnum == WEAPON_LASER {
                usesammo = false;
            }
        }
        self.hands[h].burstbullets += self.hands[h].shotstotake;
        self.hands[h].flashon = func.flags & FUNCFLAG_NOMUZZLEFLASH == 0;
        self.bgun_start_slide(h);
        self.hands[h].loadslide = 0.0;

        if self.hands[h].firing {
            let hand = &mut self.hands[h];
            hand.statevar1 = hand.stateframes;
            hand.stateflags |= HANDSTATEFLAG_FIRED | HANDSTATEFLAG_BUSY;
            if usesammo && func.ammoindex >= 0 {
                let ai = func.ammoindex as usize;
                let hand = &mut self.hands[h];
                hand.loadedammo[ai] -= hand.shotstotake;
                if hand.loadedammo[ai] < 0 {
                    hand.shotstotake += hand.loadedammo[ai];
                    hand.loadedammo[ai] = 0;
                }
            }
            self.hands[h].attacktype = match func.ftype & 0xff00 {
                0x200 => HANDATTACKTYPE_SHOOTPROJECTILE,
                _ => HANDATTACKTYPE_SHOOT,
            };
            // fireslot-limited sound (duration60) or one per shot
            let mut playsound = false;
            if shoot.duration60 > 0 {
                if self.lv.lvframe60 != self.hands[1 - h].lastshootframe60 && self.lv.lvframe60 > self.hands[h].allowshootframe {
                    self.hands[h].allowshootframe = self.lv.lvframe60 + shoot.duration60;
                    playsound = true;
                }
            } else {
                playsound = true;
            }
            if playsound && shoot.shootsound != 0 {
                self.hands[h].lastshootframe60 = self.lv.lvframe60;
                let mut speed = 1.0;
                if self.hands[h].weaponnum == WEAPON_MAULER {
                    let charge = self.hands[h].matmot1 as i32;
                    let frac = (charge as f32 / 3.0).min(1.0);
                    speed = 1.0 - frac * 0.4;
                }
                self.sound(shoot.shootsound, speed);
            }
        }
    }
}

