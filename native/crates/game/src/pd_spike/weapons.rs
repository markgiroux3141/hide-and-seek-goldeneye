//! The spike's weapon table — only the fields a bot's firing code reads.
//!
//! Values are transcribed from `tools/pd-assets/pd_weapons.json`, which is itself
//! generated from the decomp (`invitems.c` funcdefs + ammodefs, `botinv.c`
//! `g_BotWeaponConfigs`). The game's `combat::pd_weapons` table drops `unk24/25`
//! (the bot's shot interval) and `reloaddelay`, which are exactly what a bot needs,
//! hence this small local copy.
//!
//! Deliberately left out: the Shotgun (pellet fan + distance damage multiplier not
//! yet ported), explosives and the Reaper (spin-up + melee). Everything here is a
//! plain hitscan gun.

/// `WEAPON_*` number (`constants.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WeaponId(pub u8);

pub const FALCON2: WeaponId = WeaponId(0x02);
pub const MAGSEC4: WeaponId = WeaponId(0x05);
pub const DY357: WeaponId = WeaponId(0x08);
pub const CMP150: WeaponId = WeaponId(0x0a);
pub const LAPTOPGUN: WeaponId = WeaponId(0x0e);
pub const DRAGON: WeaponId = WeaponId(0x0f);
pub const K7AVENGER: WeaponId = WeaponId(0x10);
pub const AR34: WeaponId = WeaponId(0x11);

/// `FUNCFLAG_BURST3` / `BURST2` (`constants.h`).
pub const FUNCFLAG_BURST3: u32 = 0x0000_0002;
pub const FUNCFLAG_BURST2: u32 = 0x0000_1000;

#[derive(Clone, Copy, Debug)]
pub struct WeaponDef {
    pub id: WeaponId,
    pub name: &'static str,
    /// Third-person model under `assets/weapons/`.
    pub tp_glb: &'static str,
    /// Muzzle-flash GLB under `assets/weapons/` (PD hides `MUZZLEFLASH` until firing).
    pub flash_glb: &'static str,
    /// The authored `CHRGUNFIRE` position in the third-person model's own space
    /// (export units) — where `chr_get_gun_pos` starts a shot.
    pub tp_muzzle: [f32; 3],
    /// `WEAPONFLAG_ONEHANDED` — picks the pistol wield rows over the heavy ones.
    pub one_handed: bool,
    /// `funcdef_shootauto` (true) vs `funcdef_shootsingle`.
    pub automatic: bool,
    /// Primary function flags (only the burst bits matter to a bot).
    pub funcflags: u32,
    /// `funcdef_shoot.damage`.
    pub damage: f32,
    /// `funcdef_shoot.spread` — fed to `bgun_calculate_bot_shot_spread`.
    pub spread: f32,
    /// `funcdef_shoot.unk24` / `unk25` — `botact_get_shoot_interval60` sums them.
    pub unk24: u8,
    pub unk25: u8,
    /// `funcdef_shootauto.maxrpm` (0 for single-shot).
    pub maxrpm: f32,
    /// `ammodef.clipsize`.
    pub clip: i32,
    /// `g_BotWeaponConfigs[].reloaddelay` — seconds.
    pub reloaddelay: u8,
    /// `g_BotWeaponConfigs[].pridistconfig` — index into `g_BotDistConfigs`.
    pub pridistconfig: u8,
}

pub const WEAPONS: &[WeaponDef] = &[
    WeaponDef {
        id: FALCON2,
        flash_glb: "pd/01-falcon-2-flash.glb",
        tp_muzzle: [-143.028_85, 0.0, 28.846_154],
        name: "Falcon 2",
        tp_glb: "pd/01-falcon-2-tp.glb",
        one_handed: true,
        automatic: false,
        funcflags: 0,
        damage: 1.0,
        spread: 1.0,
        unk24: 3,
        unk25: 5,
        maxrpm: 0.0,
        clip: 8,
        reloaddelay: 1,
        pridistconfig: 1,
    },
    WeaponDef {
        id: MAGSEC4,
        flash_glb: "pd/04-magsec-4-flash.glb",
        tp_muzzle: [-188.701_92, 0.0, 20.432_692],
        name: "MagSec 4",
        tp_glb: "pd/04-magsec-4-tp.glb",
        one_handed: true,
        automatic: false,
        funcflags: 0,
        damage: 1.1,
        spread: 6.0,
        unk24: 4,
        unk25: 8,
        maxrpm: 0.0,
        clip: 9,
        reloaddelay: 1,
        pridistconfig: 1,
    },
    WeaponDef {
        id: DY357,
        flash_glb: "pd/07-dy357-magnum-flash.glb",
        tp_muzzle: [-240.985_58, 0.600_961_5, 30.649_038],
        name: "DY357 Magnum",
        tp_glb: "pd/07-dy357-magnum-tp.glb",
        one_handed: true,
        automatic: false,
        funcflags: 0,
        damage: 2.0,
        spread: 0.0,
        unk24: 8,
        unk25: 16,
        maxrpm: 0.0,
        clip: 6,
        reloaddelay: 3,
        pridistconfig: 1,
    },
    WeaponDef {
        id: CMP150,
        flash_glb: "pd/09-cmp150-flash.glb",
        tp_muzzle: [-198.918_27, -0.600_961_5, 30.048_077],
        name: "CMP150",
        tp_glb: "pd/09-cmp150-tp.glb",
        one_handed: true,
        automatic: true,
        funcflags: 0,
        damage: 1.0,
        spread: 9.0,
        unk24: 6,
        unk25: 18,
        maxrpm: 900.0,
        clip: 32,
        reloaddelay: 2,
        pridistconfig: 2,
    },
    WeaponDef {
        id: LAPTOPGUN,
        flash_glb: "pd/0d-laptop-gun-flash.glb",
        tp_muzzle: [-402.644_23, 0.0, 125.0],
        name: "Laptop Gun",
        tp_glb: "pd/0d-laptop-gun-tp.glb",
        one_handed: false,
        automatic: true,
        funcflags: FUNCFLAG_BURST3,
        damage: 1.15,
        spread: 6.0,
        unk24: 6,
        unk25: 18,
        maxrpm: 1000.0,
        clip: 50,
        reloaddelay: 3,
        pridistconfig: 2,
    },
    WeaponDef {
        id: DRAGON,
        flash_glb: "pd/0e-dragon-flash.glb",
        tp_muzzle: [-558.894_23, 0.0, 13.221_154],
        name: "Dragon",
        tp_glb: "pd/0e-dragon-tp.glb",
        one_handed: false,
        automatic: true,
        funcflags: 0,
        damage: 1.1,
        spread: 6.0,
        unk24: 6,
        unk25: 18,
        maxrpm: 700.0,
        clip: 30,
        reloaddelay: 1,
        pridistconfig: 2,
    },
    WeaponDef {
        id: K7AVENGER,
        flash_glb: "pd/0f-k7-avenger-flash.glb",
        tp_muzzle: [-488.581_73, 1.201_923, 39.663_46],
        name: "K7 Avenger",
        tp_glb: "pd/0f-k7-avenger-tp.glb",
        one_handed: false,
        automatic: true,
        funcflags: FUNCFLAG_BURST3,
        damage: 1.5,
        spread: 6.0,
        unk24: 6,
        unk25: 18,
        maxrpm: 950.0,
        clip: 25,
        reloaddelay: 2,
        pridistconfig: 2,
    },
    WeaponDef {
        id: AR34,
        flash_glb: "pd/10-ar34-flash.glb",
        tp_muzzle: [-498.197_1, 0.0, 7.8125],
        name: "AR34",
        tp_glb: "pd/10-ar34-tp.glb",
        one_handed: false,
        automatic: true,
        funcflags: FUNCFLAG_BURST3,
        damage: 1.4,
        spread: 8.0,
        unk24: 6,
        unk25: 18,
        maxrpm: 750.0,
        clip: 30,
        reloaddelay: 2,
        pridistconfig: 2,
    },
];

pub fn get(id: WeaponId) -> Option<&'static WeaponDef> {
    WEAPONS.iter().find(|w| w.id == id)
}

pub fn by_name(name: &str) -> Option<&'static WeaponDef> {
    let n = name.to_ascii_lowercase().replace([' ', '-', '_'], "");
    WEAPONS.iter().find(|w| w.name.to_ascii_lowercase().replace([' ', '-', '_'], "") == n)
}
