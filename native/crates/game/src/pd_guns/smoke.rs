//! `game/smoke.c` — gun smoke, bullet-impact puffs, rocket/grenade trails and
//! explosion smoke, ported function by function (NTSC final).
//!
//! A smoke is a prop that spawns up to ten billboarded cloud parts over its
//! `duration`, one every `spreadspeed` ticks; each part rises, grows, spins and
//! fades on its own (`smoke_tick`). Parts are drawn with texture 0x002a through
//! `G_CC_MODULATEIA` in `G_RM_ZB_CLD_SURF` (z-tested, no z-write, alpha blend) —
//! `g_TcGdl1` (`textureconfig.c:7`).

use glam::Vec3;

use super::bgun::Lv;
use super::fx::{FxBatch, FxKind, FxVert};
use crate::pd_spike::pdmath::{baddtor, Rng};

pub const SMOKETYPE_NONE: usize = 0;
pub const SMOKETYPE_ELECTRICAL: usize = 1;
pub const SMOKETYPE_MINI: usize = 2;
pub const SMOKETYPE_SMALL: usize = 4;
pub const SMOKETYPE_MEDIUM: usize = 5;
pub const SMOKETYPE_LARGE: usize = 6;
pub const SMOKETYPE_BULLETIMPACT: usize = 7;
pub const SMOKETYPE_ROCKETTAIL: usize = 8;
pub const SMOKETYPE_GRENADETAIL: usize = 9;
pub const SMOKETYPE_HOMINGTAIL: usize = 11;
pub const SMOKETYPE_MUZZLE_PISTOL: usize = 15;
pub const SMOKETYPE_MUZZLE_REAPER: usize = 16;
pub const SMOKETYPE_MUZZLE_AUTOMATIC: usize = 17;
pub const SMOKETYPE_MUZZLE_SHOTGUN: usize = 18;
pub const SMOKETYPE_PINBALL: usize = 19;

/// `struct smoketype` (`types.h:4360`). The field names are the decomp's; the
/// column comments above `g_SmokeTypes` in `smoke.c` are misaligned with them, so
/// what each one *does* is noted here from `smoke_tick` / `smoke_render_part`.
#[derive(Clone, Copy, Debug)]
pub struct SmokeType {
    /// Ticks during which new parts are spawned.
    pub duration: i16,
    /// Ticks a new part takes to fade in.
    pub fadespeed: i16,
    /// A part spawns when `age % spreadspeed == 1`.
    pub spreadspeed: i16,
    /// Part size (cm, half-diagonal); 0 = the 0.33-scale "tiny" parts.
    pub size: i16,
    /// Spin speed range.
    pub bgrotatespeed: f32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// Alpha lost per tick.
    pub fgrotatespeed: f32,
    /// Parts spawned in the last `numclouds` ticks start fainter.
    pub numclouds: i16,
    /// Growth per tick.
    pub unk18: f32,
    /// Rise per tick.
    pub unk1c: f32,
    /// Sideways wobble amplitude.
    pub unk20: f32,
}

const fn st(
    duration: i16,
    fadespeed: i16,
    spreadspeed: i16,
    size: i16,
    bgrotatespeed: f32,
    r: u8,
    g: u8,
    b: u8,
    fgrotatespeed: f32,
    numclouds: i16,
    unk18: f32,
    unk1c: f32,
    unk20: f32,
) -> SmokeType {
    SmokeType { duration, fadespeed, spreadspeed, size, bgrotatespeed, r, g, b, fgrotatespeed, numclouds, unk18, unk1c, unk20 }
}

/// `g_SmokeTypes[]` (`smoke.c:24`), the NTSC (`#else`) half.
pub const SMOKE_TYPES: [SmokeType; 23] = [
    /*00*/ st(1, 60, 99, 0, 0.0, 0x80, 0x80, 0x80, 0.3, 120, 0.15, 0.3, 1.0),
    /*01*/ st(220, 60, 45, 60, 0.02, 0x50, 0x50, 0x60, 0.3, 120, 0.15, 0.3, 1.0),
    /*02*/ st(220, 60, 50, 20, 0.01, 0x80, 0x80, 0x80, 0.3, 120, 0.15, 0.3, 1.0),
    /*03*/ st(280, 60, 120, 100, 0.01, 0xc0, 0xc0, 0xc0, 0.3, 120, 0.15, 0.3, 1.0),
    /*04*/ st(280, 60, 60, 80, 0.02, 0x40, 0x40, 0x40, 0.3, 120, 0.15, 0.3, 1.0),
    /*05*/ st(340, 60, 50, 190, 0.015, 0x40, 0x40, 0x40, 0.3, 120, 0.15, 0.3, 1.0),
    /*06*/ st(380, 60, 70, 300, 0.01, 0x40, 0x40, 0x40, 0.3, 120, 0.15, 0.3, 1.0),
    /*07*/ st(60, 60, 8, 15, 0.03, 0xff, 0xff, 0xff, 0.3, 120, 0.15, 0.3, 1.0),
    /*08*/ st(20, 1, 6, 30, 0.03, 0xff, 0xff, 0xff, 2.0, 30, 0.15, 0.3, 1.0),
    /*09*/ st(25, 1, 7, 16, 0.03, 0xe0, 0xe0, 0xe0, 3.0, 30, 0.15, 0.3, 1.0),
    /*10*/ st(900, 60, 70, 900, 0.01, 0x40, 0x40, 0x40, 0.3, 180, 0.15, 0.3, 1.0),
    /*11*/ st(20, 1, 6, 30, 0.03, 0x18, 0x20, 0x40, 2.0, 30, 0.15, 0.3, 1.0),
    /*12*/ st(50, 25, 7, 2, 0.03, 0xff, 0xff, 0xbf, 0.3, 150, 0.15, 0.3, 1.0),
    /*13*/ st(12, 15, 7, 5, 0.03, 0x66, 0x40, 0x40, 1.0, 18, 0.15, 0.3, 1.0),
    /*14*/ st(12, 15, 7, 5, 0.03, 0x66, 0x66, 0x00, 1.0, 18, 0.15, 0.3, 1.0),
    /*15*/ st(50, 5, 5, 3, 0.03, 0xff, 0xff, 0xff, 0.3, 150, 0.0, 0.45, 0.0),
    /*16*/ st(50, 5, 6, 3, 0.03, 0xaf, 0xff, 0xaf, 0.3, 150, 0.09, 0.3, 0.0),
    /*17*/ st(50, 5, 3, 3, 0.03, 0xff, 0xff, 0xff, 0.3, 150, 0.0, 0.35, 0.0),
    /*18*/ st(50, 5, 3, 3, 0.03, 0xaf, 0x8f, 0x6f, 0.3, 150, 0.1, 0.3, 0.0),
    /*19*/ st(50, 1, 2, 16, 0.03, 0xff, 0xff, 0x80, 3.0, 30, 0.15, 0.3, 1.0),
    /*20*/ st(180, 10, 8, 18, 0.06, 0xff, 0xff, 0xff, 0.3, 0, 0.19, 0.07, 1.0),
    /*21*/ st(220, 40, 45, 60, 0.02, 0x20, 0x20, 0x20, 0.3, 30, 1.5, 1.8, 6.0),
    /*22*/ st(220, 5, 8, 60, 0.03, 0xaf, 0x8f, 0x6f, 0.3, 30, 1.5, 0.3, 1.0),
];

/// Smoke texture (`g_TcGdl1`: TEXTURE_002A, IA8 56×56, tile 0..55, wrap).
pub const TEX_SMOKE: u16 = 0x002a;

/// `struct smokepart`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SmokePart {
    pub pos: Vec3,
    pub size: f32,
    pub rot: f32,
    pub deltarot: f32,
    pub offset1: f32,
    pub offset2: f32,
    pub alpha: f32,
    pub count: i16,
}

/// What a smoke follows (`smoke->source` / `sourceprop`, `smoke->option`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmokeSource {
    /// `source == NULL`: `option` is the hand for muzzle smoke.
    None,
    /// A world object (`smoke_create_at_prop`): parts spawn at its position.
    Prop(u32),
}

/// `struct smoke` + its prop's position.
#[derive(Clone, Debug)]
pub struct Smoke {
    pub pos: Vec3,
    pub age: i16,
    pub ty: usize,
    /// Hand number for muzzle smoke (`option`, `source == NULL`).
    pub option: usize,
    pub source: SmokeSource,
    pub parts: [SmokePart; 10],
}

/// `g_Smokes[g_MaxSmokes]` (`smokereset.c:13`: 20 on an expanded console).
pub struct Smokes {
    pub slots: Vec<Option<Smoke>>,
}

impl Default for Smokes {
    fn default() -> Self {
        Smokes { slots: vec![None; 20] }
    }
}

fn ty(t: usize) -> &'static SmokeType {
    &SMOKE_TYPES[t.min(SMOKE_TYPES.len() - 1)]
}

impl Smokes {
    /// `smoke_create` (`smoke.c:299`), one player: creating muzzle smoke retires
    /// the fourth bullet-impact smoke found.
    pub fn smoke_create(&mut self, pos: Vec3, t: usize) -> Option<usize> {
        let mut free = None;
        let mut count = 0;
        for i in 0..self.slots.len() {
            match &mut self.slots[i] {
                None => {
                    free = Some(i);
                    break;
                }
                Some(s) => {
                    if (SMOKETYPE_MUZZLE_PISTOL..=SMOKETYPE_MUZZLE_SHOTGUN).contains(&t) && s.ty == SMOKETYPE_BULLETIMPACT {
                        if count == 3 {
                            s.age = ty(s.ty).duration;
                        }
                        count += 1;
                    }
                }
            }
        }
        let i = free?;
        self.slots[i] = Some(Smoke { pos, age: 0, ty: t, option: 0, source: SmokeSource::None, parts: [SmokePart::default(); 10] });
        Some(i)
    }

    /// `smoke_create_simple` (`smoke.c:459`).
    pub fn smoke_create_simple(&mut self, pos: Vec3, t: usize) -> bool {
        self.smoke_create(pos, t).is_some()
    }

    /// `smoke_create_for_hand` (`smoke.c:363`): refuse while this hand's muzzle
    /// smoke is still spawning and has a free part slot.
    pub fn smoke_create_for_hand(&mut self, pos: Vec3, t: usize, handnum: usize) -> bool {
        for s in self.slots.iter().flatten() {
            if s.option == handnum
                && s.source == SmokeSource::None
                && (SMOKETYPE_MUZZLE_PISTOL..=SMOKETYPE_MUZZLE_SHOTGUN).contains(&s.ty)
                && s.age < ty(s.ty).duration
                && s.parts.iter().any(|p| p.size == 0.0)
            {
                return false;
            }
        }
        match self.smoke_create(pos, t) {
            Some(i) => {
                if let Some(s) = self.slots[i].as_mut() {
                    s.option = handnum;
                }
                true
            }
            None => false,
        }
    }

    /// `smoke_create_with_source` (`smoke.c:401`) / `smoke_create_at_prop`.
    pub fn smoke_create_at_prop(&mut self, propid: u32, pos: Vec3, t: usize) -> bool {
        if t != 22 {
            for s in self.slots.iter().flatten() {
                if s.source == SmokeSource::Prop(propid) && s.age < ty(s.ty).duration && s.parts.iter().any(|p| p.size == 0.0) {
                    return false;
                }
            }
        }
        match self.smoke_create(pos, t) {
            Some(i) => {
                if let Some(s) = self.slots[i].as_mut() {
                    s.source = SmokeSource::Prop(propid);
                    s.option = 0;
                }
                true
            }
            None => false,
        }
    }

    /// `smoke_clear_for_prop` (`smoke.c:445`).
    pub fn smoke_clear_for_prop(&mut self, propid: u32) {
        for s in self.slots.iter_mut().flatten() {
            if s.source == SmokeSource::Prop(propid) && s.option == 0 {
                s.age = ty(s.ty).duration;
                s.source = SmokeSource::None;
            }
        }
    }

    /// `smoke_tick` (`smoke.c:464`) for every live smoke. `muzzles` are the
    /// hands' `muzzlepos`; `prop_pos` resolves a source prop (None once it's gone).
    pub fn tick(&mut self, rng: &mut Rng, lv: Lv, muzzles: [Vec3; 2], prop_pos: &dyn Fn(u32) -> Option<Vec3>) {
        if lv.lvupdate240 == 0 {
            return;
        }
        // "These tick values aren't adjusted for PAL".
        let lvupdate = lv.lvupdate60.min(15);
        for slot in self.slots.iter_mut() {
            let Some(smoke) = slot else { continue };
            let t = *ty(smoke.ty);
            for _ in 0..lvupdate {
                smoke.age += 1;
                for part in smoke.parts.iter_mut() {
                    if part.size != 0.0 {
                        part.pos.y += t.unk1c;
                        part.size += t.unk18;
                        if part.size < 0.0 {
                            part.size = 0.0;
                        }
                        part.alpha -= t.fgrotatespeed;
                        part.count += 1;
                        part.rot += part.deltarot;
                        part.offset1 += 0.02 + rng.randomfrac() * 0.01;
                        part.offset2 += 0.02 + rng.randomfrac() * 0.01;
                        if part.alpha < 4.0 {
                            part.size = 0.0;
                        }
                    }
                }
                if smoke.age < t.duration && smoke.age % t.spreadspeed == 1 {
                    if let Some(j) = smoke.parts.iter().position(|p| p.size == 0.0) {
                        let part = &mut smoke.parts[j];
                        part.size = if t.size == 0 {
                            (rng.randomfrac() * 0.5 + 1.0) * 0.33
                        } else {
                            t.size as f32 * (rng.randomfrac() * 0.5 + 1.0)
                        };
                        part.alpha = (rng.random() % 70) as f32 + 110.0;
                        part.count = 0;
                        part.rot = baddtor(360.0) * rng.randomfrac();
                        part.deltarot = (0.5 - rng.randomfrac()) * t.bgrotatespeed;
                        part.pos = if (SMOKETYPE_MUZZLE_PISTOL..=SMOKETYPE_MUZZLE_SHOTGUN).contains(&smoke.ty) {
                            muzzles[smoke.option & 1]
                        } else if let (SmokeSource::Prop(id), 0) = (smoke.source, smoke.option) {
                            prop_pos(id).unwrap_or(smoke.pos)
                        } else {
                            smoke.pos
                        };
                        if smoke.ty == 20 {
                            part.pos.x += rng.randomfrac() * 70.0 - 35.0;
                            part.pos.y += rng.randomfrac() * 40.0 - 25.0;
                            part.pos.z += rng.randomfrac() * 40.0 - 20.0;
                            part.alpha *= 0.23;
                            part.size *= rng.randomfrac() + 1.0;
                        }
                        part.offset1 = rng.randomfrac() * 0.5;
                        part.offset2 = rng.randomfrac() * 0.5;
                        if smoke.age > t.duration - t.numclouds {
                            part.alpha *= (t.duration - smoke.age) as f32 / t.numclouds as f32;
                        }
                    }
                }
            }
            // Free once past the first spread and every part has faded.
            let free = smoke.age > t.spreadspeed && !smoke.parts.iter().any(|p| p.size > 0.0);
            if free {
                *slot = None;
            }
        }
    }

    pub fn live(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// `smoke_render` + `smoke_render_part` (`smoke.c:574`, `:66`): one batch
    /// per smoke (PD sorts props back to front; the caller does), with the
    /// camera's world right/up (`cam_get_projection_mtxf()` columns 0 and 1).
    /// `brightness` is `room_get_final_brightness_for_player` (0..255).
    pub fn geometry(&self, campos: Vec3, right: Vec3, up: Vec3, brightness: f32, xray: Option<&super::xray::Eraser>) -> Vec<(Vec3, FxBatch)> {
        let mut out = Vec::new();
        for smoke in self.slots.iter().flatten() {
            let t = ty(smoke.ty);
            let mut verts = Vec::new();
            for part in smoke.parts.iter().filter(|p| p.size > 0.0) {
                let alpha: u8 = if t.fadespeed as f32 >= part.count as f32 {
                    (part.alpha / t.fadespeed as f32 * part.count as f32) as u8
                } else {
                    part.alpha as u8
                };
                let mut c78 = part.rot.cos() * part.size;
                let mut s74 = part.rot.sin() * part.size;
                let p = Vec3::new(
                    part.pos.x + 7.0 * part.offset1.sin() * t.unk20,
                    part.pos.y,
                    part.pos.z + 7.0 * part.offset2.sin() * t.unk20,
                );
                let d = p - campos;
                let distance = d.length();
                if distance > 30000.0 {
                    continue;
                }
                // Pull the cloud up to 1 m towards the camera (scaled to keep its
                // apparent size) so it doesn't cut into the wall it sits on.
                let range = (distance * 0.5).min(100.0);
                let mult = if distance == 0.0 { 0.0 } else { (distance - range) / distance };
                c78 *= mult;
                s74 *= mult;
                let c = campos + d * mult;
                let spa0 = right * c78;
                let sp94 = right * s74;
                let sp88 = up * c78;
                let sp7c = up * s74;
                // SMOKETYPE_PINBALL ignores the room light.
                let frac = if smoke.ty != SMOKETYPE_PINBALL { (brightness / 255.0).min(1.0) } else { 1.0 };
                let col = match xray {
                    // smoke.c:180: measured from the pulled-in centre.
                    Some(e) => match e.smoke_colour(c, alpha as f32) {
                        Some(col) => col,
                        None => continue,
                    },
                    None => [
                        ((t.r as f32 * frac) as u32 & 0xff) as f32 / 255.0,
                        ((t.g as f32 * frac) as u32 & 0xff) as f32 / 255.0,
                        ((t.b as f32 * frac) as u32 & 0xff) as f32 / 255.0,
                        alpha as f32 / 255.0,
                    ],
                };
                // s,t 1760 = 55 texels (the 56-texel tile's last texel edge).
                let v = [
                    FxVert { pos: c - spa0 - sp7c, st: [55.0, 0.0], col },
                    FxVert { pos: c + sp94 - sp88, st: [0.0, 0.0], col },
                    FxVert { pos: c + spa0 + sp7c, st: [0.0, 55.0], col },
                    FxVert { pos: c - sp94 + sp88, st: [55.0, 55.0], col },
                ];
                // gSPTri2(0, 1, 2, 0, 2, 3)
                verts.extend_from_slice(&[v[0], v[1], v[2], v[0], v[2], v[3]]);
            }
            if !verts.is_empty() {
                out.push((smoke.pos, FxBatch { kind: FxKind::Smoke, verts }));
            }
        }
        out
    }
}
