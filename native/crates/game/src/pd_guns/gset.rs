//! Perfect Dark's weapon table (`game/invitems.c`) resolved into typed Rust, plus
//! the `gset_*` accessors (`game/gset.c`) the hand code calls.
//!
//! `g_Weapons[]` is indexed by `WEAPON_*`; we build a sparse table from the MP
//! set `pd_fpgun.py` exports (every gun in the Combat Simulator). Gun scripts are
//! compiled once into [`GunCmd`] lists; a script reference is a [`ScriptId`] and a
//! running command position is `(ScriptId, index)` — PD's `struct guncmd *`.

use std::collections::HashMap;

use serde_json::Value;

use super::data::{self, NoiseSettings, RecoilSettings, WeaponsFile};

// ─── constants (include/constants.h) ─────────────────────────────────────────

pub const WEAPON_NONE: i32 = 0x00;
pub const WEAPON_UNARMED: i32 = 0x01;
pub const WEAPON_FALCON2: i32 = 0x02;
pub const WEAPON_FALCON2_SILENCER: i32 = 0x03;
pub const WEAPON_FALCON2_SCOPE: i32 = 0x04;
pub const WEAPON_MAGSEC4: i32 = 0x05;
pub const WEAPON_MAULER: i32 = 0x06;
pub const WEAPON_PHOENIX: i32 = 0x07;
pub const WEAPON_DY357MAGNUM: i32 = 0x08;
pub const WEAPON_DY357LX: i32 = 0x09;
pub const WEAPON_CMP150: i32 = 0x0a;
pub const WEAPON_CYCLONE: i32 = 0x0b;
pub const WEAPON_CALLISTO: i32 = 0x0c;
pub const WEAPON_RCP120: i32 = 0x0d;
pub const WEAPON_LAPTOPGUN: i32 = 0x0e;
pub const WEAPON_DRAGON: i32 = 0x0f;
pub const WEAPON_K7AVENGER: i32 = 0x10;
pub const WEAPON_AR34: i32 = 0x11;
pub const WEAPON_SUPERDRAGON: i32 = 0x12;
pub const WEAPON_SHOTGUN: i32 = 0x13;
pub const WEAPON_REAPER: i32 = 0x14;
pub const WEAPON_SNIPERRIFLE: i32 = 0x15;
pub const WEAPON_FARSIGHT: i32 = 0x16;
pub const WEAPON_DEVASTATOR: i32 = 0x17;
pub const WEAPON_ROCKETLAUNCHER: i32 = 0x18;
pub const WEAPON_SLAYER: i32 = 0x19;
pub const WEAPON_COMBATKNIFE: i32 = 0x1a;
pub const WEAPON_CROSSBOW: i32 = 0x1b;
pub const WEAPON_TRANQUILIZER: i32 = 0x1c;
pub const WEAPON_LASER: i32 = 0x1d;
pub const WEAPON_GRENADE: i32 = 0x1e;
pub const WEAPON_NBOMB: i32 = 0x1f;
pub const WEAPON_TIMEDMINE: i32 = 0x20;
pub const WEAPON_PROXIMITYMINE: i32 = 0x21;
pub const WEAPON_REMOTEMINE: i32 = 0x22;
pub const WEAPON_COMBATBOOST: i32 = 0x23;
pub const WEAPON_PP9I: i32 = 0x24;
pub const WEAPON_CC13: i32 = 0x25;

pub const FUNC_PRIMARY: usize = 0;
pub const FUNC_SECONDARY: usize = 1;

pub const INVENTORYFUNCTYPE_NONE: u32 = 0x0000;
pub const INVENTORYFUNCTYPE_SHOOT: u32 = 0x0001;
pub const INVENTORYFUNCTYPE_SHOOT_AUTOMATIC: u32 = 0x0101;
pub const INVENTORYFUNCTYPE_SHOOT_PROJECTILE: u32 = 0x0201;
pub const INVENTORYFUNCTYPE_0200: u32 = 0x0200;
pub const INVENTORYFUNCTYPE_THROW: u32 = 0x0002;
pub const INVENTORYFUNCTYPE_MELEE: u32 = 0x0003;
pub const INVENTORYFUNCTYPE_SPECIAL: u32 = 0x0004;
pub const INVENTORYFUNCTYPE_DEVICE: u32 = 0x0005;

pub const FUNCFLAG_00000001: u32 = 0x0000_0001;
pub const FUNCFLAG_BURST3: u32 = 0x0000_0002;
pub const FUNCFLAG_BURST50: u32 = 0x0000_0020;
pub const FUNCFLAG_BURST2: u32 = 0x0000_1000;
pub const FUNCFLAG_NOMUZZLEFLASH: u32 = 0x0000_2000;
pub const FUNCFLAG_EXPLOSIVESHELLS: u32 = 0x0000_4000;
pub const FUNCFLAG_BURST5: u32 = 0x0002_0000;
pub const FUNCFLAG_DISCARDWEAPON: u32 = 0x0004_0000;
pub const FUNCFLAG_STICKTOWALL: u32 = 0x0000_0100;
pub const FUNCFLAG_FLYBYWIRE: u32 = 0x0000_0800;
pub const FUNCFLAG_CALCULATETRAJECTORY: u32 = 0x0080_0000;
pub const FUNCFLAG_PROJECTILE_POWERED: u32 = 0x0800_0000;
pub const FUNCFLAG_HOMINGROCKET: u32 = 0x4000_0000;
pub const FUNCFLAG_PROJECTILE_LIGHTWEIGHT: u32 = 0x8000_0000;
pub const FUNCFLAG_AUTOSWITCHUNSELECTABLE: u32 = 0x0010_0000;

pub const WEAPONFLAG_THROWABLE: u32 = 0x0000_0001;
pub const WEAPONFLAG_DUALFLIP: u32 = 0x0000_0020;
pub const WEAPONFLAG_00000040: u32 = 0x0000_0040;
pub const WEAPONFLAG_00000080: u32 = 0x0000_0080;
pub const WEAPONFLAG_DUALWIELD: u32 = 0x0000_1000;
pub const WEAPONFLAG_HASGUNSCRIPT: u32 = 0x0000_2000;
pub const WEAPONFLAG_00004000: u32 = 0x0000_4000;
pub const WEAPONFLAG_BRIGHTER: u32 = 0x0000_8000;
pub const WEAPONFLAG_HASHANDS: u32 = 0x0002_0000;
pub const WEAPONFLAG_GANGSTA: u32 = 0x0008_0000;
pub const WEAPONFLAG_RESETMATRICES: u32 = 0x0200_0000;
pub const WEAPONFLAG_KEEPFUNCWHENEMPTY: u32 = 0x0400_0000;
pub const WEAPONFLAG_FIRETOACTIVATE: u32 = 0x8000_0000;

pub const AMMOFLAG_NORESERVE: i64 = 1;
pub const AMMOFLAG_INCREMENTALRELOAD: i64 = 4;
pub const AMMOFLAG_QTYAFFECTSPARTVIS: i64 = 8;

pub const INVAIMFLAG_MANUALZOOM: u32 = 0x1;
pub const INVAIMFLAG_AUTOAIM: u32 = 0x2;
pub const INVAIMFLAG_ACCURATESINGLESHOT: u32 = 0x4;

pub const GUNFEATURE_RELOAD: i32 = 1;
pub const GUNFEATURE_ATTACK: i32 = 2;
pub const GUNFEATURE_ATTACKAGAIN: i32 = 3;
pub const GUNFEATURE_CLICK: i32 = 5;

pub const MODELPART_GUN_CARTEJECTPOS: i32 = 0x3c;
pub const MODELPART_GUN_CARTFLAPCLOSED: i32 = 0x46;
pub const MODELPART_GUN_CARTFLAPOPEN: i32 = 0x47;
pub const MODELPART_GUN_HOLDPOS: i32 = 0x37;
pub const MODELPART_GUN_LASERSIGHT: i32 = 0x34;
pub const MODELPART_GUN_MUZZLEFLASH1: i32 = 0x5a;
pub const MODELPART_GUN_MUZZLEPOS: i32 = 0x32;
pub const MODELPART_GUN_SLIDE: i32 = 0x33;
pub const MODELPART_HAND_LEFT: i32 = 0x35;
pub const MODELPART_HAND_RIGHT: i32 = 0x36;
pub const MODELPART_0042: i32 = 0x42;
pub const MODELPART_REAPER_001E: i32 = 0x1e;
pub const MODELPART_REAPER_002C: i32 = 0x2c;
pub const MODELPART_REAPER_002D: i32 = 0x2d;
pub const MODELPART_REAPER_002E: i32 = 0x2e;
pub const MODELPART_REAPER_002F: i32 = 0x2f;
pub const MODELPART_REAPER_CARTEJECTPOS1: i32 = 0x30;
pub const MODELPART_REAPER_CARTEJECTPOS2: i32 = 0x31;
pub const MODELPART_SHOTGUN_0050: i32 = 0x50;
pub const MODELPART_SNIPERRIFLE_SCOPE1: i32 = 0x2a;
pub const MODELPART_DEVASTATOR_0028: i32 = 0x28;

// ─── scripts ─────────────────────────────────────────────────────────────────

/// Index into [`Gset::scripts`].
pub type ScriptId = usize;
/// PD's `struct guncmd *`: a script and an index into it.
pub type CmdPtr = (ScriptId, usize);

/// `struct guncmd` (`gunscript.h`), decoded.
#[derive(Clone, Debug, PartialEq)]
pub enum GunCmd {
    End,
    PlayAnimation { condition: u8, anim: u16, params: i32 },
    ShowPart { keyframe: i32, part: i32 },
    HidePart { keyframe: i32, part: i32 },
    WaitForZReleased { keyframe: i32 },
    AllowFeature { keyframe: i32, feature: i32 },
    PlaySound { keyframe: i32, sound: u16 },
    Include { condition: u8, target: ScriptId },
    Random { probability: i32, target: ScriptId },
    RepeatUntilFull { keyframe: i32, gotokeyframe: i32 },
    PopOutSackOfPills { keyframe: i32 },
    SetSoundSpeed { keyframe: i32, speed: i32 },
}

impl GunCmd {
    /// The `keyframe` field (the union's first u16), for the commands that have one.
    pub fn keyframe(&self) -> i32 {
        match *self {
            GunCmd::ShowPart { keyframe, .. }
            | GunCmd::HidePart { keyframe, .. }
            | GunCmd::WaitForZReleased { keyframe }
            | GunCmd::AllowFeature { keyframe, .. }
            | GunCmd::PlaySound { keyframe, .. }
            | GunCmd::RepeatUntilFull { keyframe, .. }
            | GunCmd::PopOutSackOfPills { keyframe }
            | GunCmd::SetSoundSpeed { keyframe, .. } => keyframe,
            _ => 0,
        }
    }
}

// ─── definitions ─────────────────────────────────────────────────────────────

/// `struct funcdef_shoot` fields beyond the base.
#[derive(Clone, Debug, Default)]
pub struct ShootDef {
    pub recoil: Option<RecoilSettings>,
    pub recoverytime60: i32,
    pub damage: f32,
    pub spread: f32,
    /// `unk24..unk27`: recoil rise ticks, recoil fall ticks, earliest refire tick,
    /// refire blend ticks (bgun_tick_recoil, `bondgun.c:1851`).
    pub unk24: i32,
    pub unk25: i32,
    pub unk26: i32,
    pub unk27: i32,
    pub recoildist: f32,
    pub recoilangle: f32,
    pub slidemax: f32,
    pub impactforce: f32,
    pub duration60: i32,
    pub shootsound: u16,
    pub penetration: i32,
    // funcdef_shootauto
    pub initialrpm: f32,
    pub maxrpm: f32,
    pub turretaccel: f32,
    pub turretdecel: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FuncDef {
    pub symbol: String,
    pub name: String,
    /// `funcdef.type`.
    pub ftype: u32,
    pub ammoindex: i32,
    pub noise: NoiseSettings,
    pub fire_animation: Option<ScriptId>,
    pub flags: u32,
    pub shoot: Option<ShootDef>,
    /// melee/throw/special fields
    pub damage: f32,
    pub range: f32,
    pub recoverytime60: i32,
    pub activatetime60: i32,
    /// `HANDATTACKTYPE_*` for specials (the export names it; resolved here).
    pub specialfunc: i32,
    /// `funcdef_throw` / `funcdef_shootprojectile` projectile fields.
    pub proj: Option<ProjDef>,
    /// `funcdef_special.soundnum` / `funcdef_shootprojectile.soundnum`.
    pub soundnum: u16,
}

/// The projectile half of `struct funcdef_throw` / `funcdef_shootprojectile`.
#[derive(Clone, Debug, Default)]
pub struct ProjDef {
    pub projectilemodelnum: i32,
    pub scale: f32,
    pub speed: f32,
    pub speeddecel: f32,
    pub traveldist: f32,
    pub timer60: i32,
    pub hitspeedpreservationfrac: f32,
}

impl FuncDef {
    pub fn kind(&self) -> u32 {
        self.ftype & 0xff
    }
    pub fn is_auto(&self) -> bool {
        self.ftype & 0xff00 == 0x100
    }
}

#[derive(Clone, Debug, Default)]
pub struct AmmoDef {
    pub ammotype: i32,
    pub casingeject: i32,
    pub clipsize: i32,
    pub reload_animation: Option<ScriptId>,
    pub flags: i64,
}

#[derive(Clone, Debug)]
pub struct GunVisCmd {
    /// GUNVISCMD_* (1 always, 4 upgrade, 5 in left, 6 in right)
    pub ctype: i32,
    pub param: i32,
    pub op: i32,
    pub partnum: i32,
}

#[derive(Clone, Debug)]
pub struct AimDef {
    pub zoomfov: f32,
    pub guntransup: f32,
    pub guntransdown: f32,
    pub guntransside: f32,
    pub aimdamp: f32,
    pub flags: u32,
}

impl Default for AimDef {
    /// `invaimsettings_default` (`invitems.c:90`).
    fn default() -> Self {
        AimDef { zoomfov: 0.0, guntransup: 3.0, guntransdown: 8.0, guntransside: 15.0, aimdamp: 0.9767, flags: INVAIMFLAG_AUTOAIM }
    }
}

/// `struct weapondef` (`types.h:3023`).
#[derive(Clone, Debug)]
pub struct WeaponDef {
    pub weaponnum: i32,
    pub name: String,
    pub short_name: String,
    /// Model stem under `models/` (`hi_model`).
    pub model: Option<String>,
    pub equip_animation: Option<ScriptId>,
    pub unequip_animation: Option<ScriptId>,
    pub pritosec_animation: Option<ScriptId>,
    pub sectopri_animation: Option<ScriptId>,
    pub functions: [Option<FuncDef>; 2],
    pub ammos: [Option<AmmoDef>; 2],
    pub aim: AimDef,
    pub muzzlez: f32,
    pub posx: f32,
    pub posy: f32,
    pub posz: f32,
    pub sway: f32,
    pub gunviscmds: Vec<GunVisCmd>,
    pub flags: u32,
    /// `g_MpWeapons[]` starting ammo: (type, qty) per function.
    pub mp_ammo: [(i32, i32); 2],
}

/// The whole table plus the scripts (`g_Weapons` + every `invanim_*`).
pub struct Gset {
    pub weapons: HashMap<i32, WeaponDef>,
    /// Weapon numbers in MP menu order.
    pub order: Vec<i32>,
    pub scripts: Vec<Vec<GunCmd>>,
    pub script_names: Vec<String>,
    pub sfx_names: HashMap<u16, String>,
    /// `var80070200` (`bondgun.c:6391`): the detonator press, a script built in C.
    pub detonate_script: Option<ScriptId>,
}

fn aimflags(v: &Value) -> u32 {
    v.as_u64().map(|x| x as u32).unwrap_or(0)
}

impl Gset {
    pub fn from_file(w: &WeaponsFile) -> Self {
        // Scripts first, so references resolve to indices.
        let mut names: Vec<String> = w.scripts.keys().cloned().collect();
        names.sort();
        let index: HashMap<String, usize> = names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect();
        let scripts: Vec<Vec<GunCmd>> = names
            .iter()
            .map(|n| w.scripts[n].cmds.iter().map(|c| decode_cmd(c, &index)).collect())
            .collect();

        let script = |s: &Option<String>| -> Option<ScriptId> { s.as_ref().and_then(|n| index.get(n).copied()) };

        let mut weapons = HashMap::new();
        let mut order = Vec::new();
        for rw in &w.weapons {
            let aim = rw
                .aimsettings
                .as_ref()
                .and_then(|n| w.aimsettings.get(n))
                .map(|a| AimDef {
                    zoomfov: a.zoomfov,
                    guntransup: a.guntransup,
                    guntransdown: a.guntransdown,
                    guntransside: a.guntransside,
                    aimdamp: a.aimdamp,
                    flags: aimflags(&a.flags),
                })
                .unwrap_or_default();
            let mut functions: [Option<FuncDef>; 2] = [None, None];
            for (i, f) in rw.functions.iter().enumerate().take(2) {
                let Some(f) = f else { continue };
                let noise = data::string(f, "noisesettings").and_then(|n| w.noisesettings.get(&n).copied()).unwrap_or_default();
                let ftype = data::int(f, "type") as u32;
                let mut fd = FuncDef {
                    symbol: data::string(f, "symbol").unwrap_or_default(),
                    name: data::string(f, "name_text").unwrap_or_default(),
                    ftype,
                    ammoindex: data::int(f, "ammoindex") as i32,
                    noise,
                    fire_animation: script(&data::string(f, "fire_animation")),
                    flags: data::int(f, "flags") as u32,
                    shoot: None,
                    damage: data::num(f, "damage"),
                    range: data::num(f, "range"),
                    recoverytime60: data::int(f, "recoverytime60") as i32,
                    activatetime60: data::int(f, "activatetime60") as i32,
                    specialfunc: handattacktype(f.get("specialfunc")),
                    proj: f.get("projectilemodelnum").and_then(Value::as_i64).map(|m| ProjDef {
                        projectilemodelnum: m as i32,
                        scale: if f.get("scale").is_some() { data::num(f, "scale") } else { 1.0 },
                        speed: data::num(f, "speed"),
                        speeddecel: data::num(f, "speeddecel"),
                        traveldist: data::num(f, "traveldist"),
                        timer60: data::int(f, "timer60") as i32,
                        hitspeedpreservationfrac: data::num(f, "hitspeedpreservationfrac"),
                    }),
                    soundnum: match f.get("soundnum") {
                        Some(Value::Number(n)) if n.as_i64().unwrap_or(0) > 0 => n.as_i64().unwrap_or(0) as u16,
                        Some(v @ Value::String(_)) => resolve_sfx(Some(v), w),
                        _ => 0,
                    },
                };
                if ftype & 0xff == INVENTORYFUNCTYPE_SHOOT {
                    fd.shoot = Some(ShootDef {
                        recoil: data::string(f, "recoilsettings").and_then(|n| w.recoilsettings.get(&n).copied()),
                        recoverytime60: data::int(f, "recoverytime60") as i32,
                        damage: data::num(f, "damage"),
                        spread: data::num(f, "spread"),
                        unk24: data::int(f, "unk24") as i32,
                        unk25: data::int(f, "unk25") as i32,
                        unk26: data::int(f, "unk26") as i32,
                        unk27: data::int(f, "unk27") as i32,
                        recoildist: data::num(f, "recoildist"),
                        recoilangle: data::num(f, "recoilangle"),
                        slidemax: data::num(f, "slidemax"),
                        impactforce: data::num(f, "impactforce"),
                        duration60: data::int(f, "duration60") as i32,
                        shootsound: resolve_sfx(f.get("shootsound"), w),
                        penetration: data::int(f, "penetration") as i32,
                        initialrpm: data::num(f, "initialrpm"),
                        maxrpm: data::num(f, "maxrpm"),
                        turretaccel: data::num(f, "turretaccel"),
                        turretdecel: data::num(f, "turretdecel"),
                    });
                }
                functions[i] = Some(fd);
            }
            let mut ammos: [Option<AmmoDef>; 2] = [None, None];
            for (i, a) in rw.ammo.iter().enumerate().take(2) {
                if let Some(a) = a {
                    ammos[i] = Some(AmmoDef {
                        ammotype: a.ammotype,
                        casingeject: a.casingeject,
                        clipsize: a.clipsize,
                        reload_animation: script(&a.reload_animation),
                        flags: a.flags,
                    });
                }
            }
            let gunviscmds = rw
                .gunviscmds_symbol
                .as_ref()
                .and_then(|n| w.gunviscmds.get(n))
                .map(|cmds| cmds.iter().map(decode_gunvis).collect())
                .unwrap_or_default();
            let model = rw
                .assets
                .as_ref()
                .and_then(|a| a.fp_model.as_ref())
                .map(|p| p.trim_start_matches("guns/").trim_end_matches(".bin").to_owned());
            let mp_ammo = rw.mp.as_ref().map_or([(0, 0); 2], |m| [(m.pri_ammo_type, m.pri_ammo_qty), (m.sec_ammo_type, m.sec_ammo_qty)]);
            let def = WeaponDef {
                weaponnum: rw.weaponnum,
                name: rw.name_text.clone().unwrap_or_else(|| rw.weapon.clone()),
                short_name: rw.short_text.clone().unwrap_or_else(|| rw.weapon.clone()),
                model,
                equip_animation: script(&rw.equip_animation),
                unequip_animation: script(&rw.unequip_animation),
                pritosec_animation: script(&rw.pritosec_animation),
                sectopri_animation: script(&rw.sectopri_animation),
                functions,
                ammos,
                aim,
                muzzlez: rw.muzzlez,
                posx: rw.posx,
                posy: rw.posy,
                posz: rw.posz,
                sway: rw.sway,
                gunviscmds,
                flags: rw.weapon_flags,
                mp_ammo,
            };
            order.push(rw.weaponnum);
            weapons.insert(rw.weaponnum, def);
        }
        let sfx_names = w.sfx.iter().map(|(k, v)| (*v as u16, k.clone())).collect();
        let mut scripts = scripts;
        let mut names = names;
        // var80070200 = { PLAYANIMATION(ANIM_0434, 10000), END }.
        let anim0434 = w.anims.iter().find(|(_, m)| m.id == "ANIM_0434").and_then(|(k, _)| k.parse::<u16>().ok());
        let detonate_script = anim0434.map(|anim| {
            scripts.push(vec![GunCmd::PlayAnimation { condition: 0, anim, params: 10000 }, GunCmd::End]);
            names.push("var80070200".into());
            scripts.len() - 1
        });
        Gset { weapons, order, scripts, script_names: names, sfx_names, detonate_script }
    }

    /// `gset_get_weapondef`.
    pub fn weapon(&self, weaponnum: i32) -> Option<&WeaponDef> {
        self.weapons.get(&weaponnum)
    }

    /// `gset_get_funcdef_by_weaponnum_funcnum`.
    pub fn func(&self, weaponnum: i32, which: usize) -> Option<&FuncDef> {
        self.weapon(weaponnum).and_then(|w| w.functions.get(which)).and_then(|f| f.as_ref())
    }

    /// `gset_has_weapon_flag`.
    pub fn has_flag(&self, weaponnum: i32, flag: u32) -> bool {
        self.weapon(weaponnum).is_some_and(|w| w.flags & flag != 0)
    }

    /// `gset_has_aim_flag`.
    pub fn has_aim_flag(&self, weaponnum: i32, flag: u32) -> bool {
        self.weapon(weaponnum).is_some_and(|w| w.aim.flags & flag != 0)
    }

    /// `gset_get_filenum2` != 0 — the weapon has a first-person model.
    pub fn has_model(&self, weaponnum: i32) -> bool {
        self.weapon(weaponnum).is_some_and(|w| w.model.is_some())
    }

    pub fn cmd(&self, p: CmdPtr) -> &GunCmd {
        self.scripts[p.0].get(p.1).unwrap_or(&GunCmd::End)
    }
}

/// `HANDATTACKTYPE_*` (`constants.h:1282`) by name or number.
fn handattacktype(v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) as i32,
        Some(Value::String(s)) => match s.as_str() {
            "HANDATTACKTYPE_SHOOT" => 1,
            "HANDATTACKTYPE_SHOOTPROJECTILE" => 2,
            "HANDATTACKTYPE_THROWPROJECTILE" => 3,
            "HANDATTACKTYPE_MELEE" => 4,
            "HANDATTACKTYPE_DETONATE" => 5,
            "HANDATTACKTYPE_BOOST" => 6,
            "HANDATTACKTYPE_REVERTBOOST" => 7,
            "HANDATTACKTYPE_CROUCH" => 8,
            "HANDATTACKTYPE_RCP120CLOAK" => 9,
            "HANDATTACKTYPE_MELEENOUNCLOAK" => 10,
            "HANDATTACKTYPE_UPLINK" => 12,
            _ => 0,
        },
        _ => 0,
    }
}

fn resolve_sfx(v: Option<&Value>, w: &WeaponsFile) -> u16 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u16,
        Some(Value::String(s)) => w.sfx.get(s).copied().unwrap_or(0) as u16,
        _ => 0,
    }
}

fn decode_gunvis(c: &data::RawGunVis) -> GunVisCmd {
    // The operator arrives by name (`GUNVISOP_*`, `constants.h:1272`); reading it
    // as a number turned SETVISIBILITY into IFTRUE_SETVISIBLE, so the
    // in-left/in-right hand checks never hid anything (both remote-mine hands
    // drew the mine and the detonator).
    let arg = |i: usize| match c.args.get(i) {
        Some(Value::String(s)) => match s.as_str() {
            "GUNVISOP_IFTRUE_SETVISIBLE" => 0,
            "GUNVISOP_IFTRUE_SETHIDDEN" => 1,
            "GUNVISOP_SETVISIBILITY" => 3,
            _ => 0,
        },
        Some(v) => v.as_i64().unwrap_or(0) as i32,
        None => 0,
    };
    match c.op.as_str() {
        // gunviscmd_sethidden(part) = { ALWAYSTRUE, 0, IFTRUE_SETHIDDEN, part }
        "sethidden" => GunVisCmd { ctype: 1, param: 0, op: 1, partnum: arg(0) },
        // gunviscmd_checkupgrade(upgrade, op, part)
        "checkupgrade" => GunVisCmd { ctype: 4, param: arg(0), op: arg(1), partnum: arg(2) },
        "checkinlefthand" => GunVisCmd { ctype: 5, param: 0, op: arg(0), partnum: arg(1) },
        "checkinrighthand" => GunVisCmd { ctype: 6, param: 0, op: arg(0), partnum: arg(1) },
        _ => GunVisCmd { ctype: 0, param: 0, op: 0, partnum: 0 },
    }
}

fn decode_cmd(c: &Value, index: &HashMap<String, usize>) -> GunCmd {
    let op = c.get("op").and_then(Value::as_str).unwrap_or("end");
    let i = |k: &str| data::int(c, k) as i32;
    let target = |k: &str| -> ScriptId {
        c.get(k).and_then(Value::as_str).and_then(|n| index.get(n).copied()).unwrap_or(usize::MAX)
    };
    match op {
        "playanimation" => GunCmd::PlayAnimation {
            condition: data::int(c, "condition") as u8,
            anim: data::int(c, "anim") as u16,
            params: data::int(c, "params") as i32,
        },
        "showpart" => GunCmd::ShowPart { keyframe: i("keyframe"), part: i("part") },
        "hidepart" => GunCmd::HidePart { keyframe: i("keyframe"), part: i("part") },
        "waitforzreleased" => GunCmd::WaitForZReleased { keyframe: i("keyframe") },
        "allowfeature" => GunCmd::AllowFeature { keyframe: i("keyframe"), feature: i("feature") },
        "playsound" => GunCmd::PlaySound { keyframe: i("keyframe"), sound: data::int(c, "sound") as u16 },
        "include" => GunCmd::Include { condition: data::int(c, "condition") as u8, target: target("target") },
        "random" => GunCmd::Random { probability: i("probability"), target: target("target") },
        "repeatuntilfull" => GunCmd::RepeatUntilFull { keyframe: i("keyframe"), gotokeyframe: i("gotokeyframe") },
        "popoutsackofpills" => GunCmd::PopOutSackOfPills { keyframe: i("keyframe") },
        "setsoundspeed" => GunCmd::SetSoundSpeed { keyframe: i("keyframe"), speed: i("speed") },
        _ => GunCmd::End,
    }
}
