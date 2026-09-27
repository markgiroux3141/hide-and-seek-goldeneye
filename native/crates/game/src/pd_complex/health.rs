//! The player's health on screen: the damage flash and the health bar
//! (`player.c:2390`–`:2700`, `healthbar.c`), and the death fades.
//!
//! * `player_display_damage` / the first half of `player_tick_damage_and_health`:
//!   the red fade, faster and fainter at low health (`g_DamageTypes`).
//! * `player_display_health` / the second half: the bar opens, shows the old
//!   health, slides to the new, holds, closes (`g_HealthDamageTypes`).
//! * `player_render_health_bar` + `healthbar_draw`: PD's curved bar is a small
//!   mesh in the y = 0 plane looked down on from (0, 370, 0) through the scene's
//!   perspective; here it is projected on the CPU into the HUD canvas (PD pixels)
//!   and drawn with the canvas's shaded-triangle blend (`G_CC_SHADE`, XLU).
//! * death (`player_tick` dead branch, `player.c:4546`): the red fade at
//!   0x96/0.706, then — once the death animation has finished — a 60-tick fade to
//!   black. SUBSTITUTION: the player has no third-person model here, so "the death
//!   animation finished" is a fixed [`DEATH_ANIM_TICKS`].

use glam::{Mat4, Vec3, Vec4};

use crate::pd_guns::app::shade_tri;
use crate::pd_guns::font::Canvas;
use crate::pd_guns::pdmtx;

use super::fight::Fight;

/// How long the stand-in death animation lasts, in 60 Hz ticks.
pub const DEATH_ANIM_TICKS: f32 = 90.0;

/// `struct damagetype` (`types.h`), `g_DamageTypes` (`player.c:2390`).
#[derive(Clone, Copy)]
struct DamageType {
    flashstartframe: f32,
    flashfullframe: f32,
    flashendframe: f32,
    maxalpha: f32,
}

const G_DAMAGE_TYPES: [DamageType; 8] = [
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 40.0, maxalpha: 0.7 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 40.0, maxalpha: 0.7 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 30.0, maxalpha: 0.65 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 25.0, maxalpha: 0.6 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 22.0, maxalpha: 0.55 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 19.0, maxalpha: 0.5 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 17.0, maxalpha: 0.45 },
    DamageType { flashstartframe: 0.0, flashfullframe: 5.0, flashendframe: 15.0, maxalpha: 0.4 },
];
/// Every damage type's fade colour: 0x96, 0, 0.
const DAMAGE_RGB: [f32; 3] = [0x96 as f32 / 255.0, 0.0, 0.0];

/// `struct healthdamagetype`, `g_HealthDamageTypes` (`player.c:2409`):
/// openend, updatestart, updateend, closestart, closeend.
const G_HEALTH_DAMAGE_TYPES: [[f32; 5]; 8] = [
    [20.0, 34.0, 46.0, 270.0, 285.0],
    [20.0, 37.0, 52.0, 250.0, 265.0],
    [20.0, 40.0, 58.0, 230.0, 245.0],
    [20.0, 43.0, 64.0, 210.0, 225.0],
    [20.0, 46.0, 70.0, 190.0, 205.0],
    [20.0, 49.0, 76.0, 170.0, 185.0],
    [20.0, 52.0, 82.0, 150.0, 165.0],
    [20.0, 55.0, 88.0, 130.0, 145.0],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthShowMode {
    Hidden,
    Opening,
    Previous,
    Updating,
    Current,
    Closing,
}

/// The player fields these functions keep.
#[derive(Clone, Debug)]
pub struct HealthShow {
    pub damageshowtime: f32,
    damagetype: usize,
    pub healthshowmode: HealthShowMode,
    pub healthshowtime: f32,
    healthdamagetype: usize,
    oldhealth: f32,
    pub apparenthealth: f32,
    /// `colourscreenred/green/blue/frac` (`player_set_fade_colour`).
    pub fade: ([f32; 3], f32),
    /// The death fades: ticks since death.
    dead_ticks: f32,
}

impl Default for HealthShow {
    fn default() -> Self {
        HealthShow {
            damageshowtime: -1.0,
            damagetype: 7,
            healthshowmode: HealthShowMode::Hidden,
            healthshowtime: -1.0,
            healthdamagetype: 7,
            oldhealth: 1.0,
            apparenthealth: 1.0,
            fade: ([1.0, 1.0, 1.0], 0.0),
            dead_ticks: 0.0,
        }
    }
}

impl HealthShow {
    /// `player_display_health` (`player.c:2430`), called before the health drops.
    pub fn display_health(&mut self, bondhealth: f32) {
        use HealthShowMode::*;
        match self.healthshowmode {
            Hidden | Closing => self.oldhealth = bondhealth,
            Updating | Current => self.oldhealth = self.apparenthealth,
            Opening | Previous => {}
        }
        match self.healthshowmode {
            Hidden => {
                self.healthshowtime = 0.0;
                self.healthshowmode = Opening;
            }
            Opening | Previous => {}
            Updating | Current => {
                self.healthshowtime = G_HEALTH_DAMAGE_TYPES[self.healthdamagetype][1];
                self.healthshowmode = Updating;
            }
            Closing => {
                self.healthshowtime = G_HEALTH_DAMAGE_TYPES[self.healthdamagetype][0] * self.bar_height_frac();
                self.healthshowmode = Opening;
            }
        }
    }

    /// `player_display_damage` (`player.c:2657`), with PD's `@bug`: it indexes
    /// `g_DamageTypes` by `healthdamagetype`.
    pub fn display_damage(&mut self) {
        let full = G_DAMAGE_TYPES[self.healthdamagetype].flashfullframe;
        if self.damageshowtime >= full {
            self.damageshowtime = full;
            return;
        }
        if self.damageshowtime < 0.0 {
            self.damageshowtime = 0.0;
        }
    }

    /// `player_tick_damage_and_health` (`player.c:2477`), no shield, no menus.
    pub fn tick(&mut self, bondhealth: f32, isdead: bool, lv60: f32) {
        if self.damageshowtime >= 0.0 {
            if self.damageshowtime == 0.0 {
                self.damagetype = ((bondhealth * 8.0) as i32).clamp(0, 7) as usize;
            }
            let dt = G_DAMAGE_TYPES[self.damagetype];
            if !isdead && self.damageshowtime <= dt.flashendframe {
                let inc = lv60.min(5.0);
                self.damageshowtime += inc;
                if self.damageshowtime >= dt.flashstartframe && self.damageshowtime <= dt.flashendframe {
                    let done = self.damageshowtime - dt.flashstartframe;
                    let total = dt.flashendframe - dt.flashstartframe;
                    let alpha = if done < dt.flashfullframe {
                        dt.maxalpha * done / dt.flashfullframe
                    } else {
                        dt.maxalpha * (total - done) / (total - dt.flashfullframe)
                    };
                    self.fade = (DAMAGE_RGB, alpha);
                }
            } else {
                self.damageshowtime = -1.0;
                self.fade = ([1.0, 1.0, 1.0], 0.0);
            }
        }

        if self.healthshowmode != HealthShowMode::Hidden {
            use HealthShowMode::*;
            if self.healthshowmode == Opening {
                self.healthdamagetype = ((bondhealth * 8.0) as i32).clamp(0, 7) as usize;
            }
            let h = G_HEALTH_DAMAGE_TYPES[self.healthdamagetype];
            if !isdead {
                match self.healthshowmode {
                    Opening => {
                        self.apparenthealth = self.oldhealth;
                        self.healthshowtime += lv60;
                        if self.healthshowtime >= h[0] {
                            self.healthshowmode = Previous;
                        }
                    }
                    Previous => {
                        self.apparenthealth = self.oldhealth;
                        self.healthshowtime += lv60;
                        if self.healthshowtime >= h[1] {
                            self.healthshowmode = Updating;
                        }
                    }
                    Updating => {
                        self.healthshowtime += lv60;
                        let frac = ((self.healthshowtime - h[1]) / (h[2] - h[1])).clamp(0.0, 1.0);
                        let healthdiff = self.oldhealth - bondhealth;
                        self.apparenthealth = self.oldhealth - frac * healthdiff;
                        if self.healthshowtime >= h[2] {
                            self.healthshowmode = Current;
                        }
                    }
                    Current => {
                        self.apparenthealth = bondhealth;
                        self.healthshowtime += lv60;
                        if self.healthshowtime >= h[3] {
                            self.healthshowmode = Closing;
                            self.healthshowtime = h[3];
                        }
                    }
                    Closing => {
                        self.healthshowtime += lv60;
                        if self.healthshowtime >= h[4] {
                            self.healthshowtime = -1.0;
                            self.healthshowmode = Hidden;
                        }
                    }
                    Hidden => {}
                }
            } else {
                self.healthshowtime = -1.0;
                self.healthshowmode = Hidden;
            }
        }
    }

    /// The dead branch's fades (`player.c:4546`): red at 0.706 while the death
    /// animation plays, then `player_adjust_fade(60, 0, 0, 0, 1)` to black.
    /// Returns true once the black fade is complete.
    pub fn tick_dead(&mut self, lv60: f32) -> bool {
        self.dead_ticks += lv60;
        if self.dead_ticks < DEATH_ANIM_TICKS {
            self.fade = (DAMAGE_RGB, 0.705_882_37);
            return false;
        }
        let t = ((self.dead_ticks - DEATH_ANIM_TICKS) / 60.0).clamp(0.0, 1.0);
        // player_adjust_fade tweens colour and frac from the red to black, full.
        let rgb = [DAMAGE_RGB[0] * (1.0 - t), 0.0, 0.0];
        let frac = 0.705_882_37 + (1.0 - 0.705_882_37) * t;
        self.fade = (rgb, frac);
        t >= 1.0
    }

    /// Back to a fresh life (`player_spawn` → `player_load_defaults`).
    pub fn reset(&mut self) {
        *self = HealthShow::default();
    }

    /// `player_get_health_bar_height_frac` (`player.c:4885`).
    pub fn bar_height_frac(&self) -> f32 {
        let h = G_HEALTH_DAMAGE_TYPES[self.healthdamagetype];
        match self.healthshowmode {
            HealthShowMode::Hidden => 0.0,
            HealthShowMode::Opening => self.healthshowtime / h[0],
            HealthShowMode::Closing => 1.0 - (self.healthshowtime - h[3]) / (h[4] - h[3]),
            _ => 1.0,
        }
    }
}

/// `struct marker` (`healthbar.c:13`).
#[derive(Clone, Copy, Default)]
struct Marker {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    frac: f32,
}

/// `healthbar_maybe_insert_marker` (`healthbar.c:21`).
fn insert_marker(markers: &mut [Marker], indexes: &mut [i32], fillfrac: f32) -> usize {
    let fillfrac = fillfrac.clamp(0.0, 1.0);
    let mut len = 0i32;
    for &i in indexes.iter() {
        len = len.max(i);
    }
    let len = (len + 1) as usize;
    for i in 0..len {
        let (i1, i2) = (indexes[i], indexes[i + 1]);
        if i1 < 0 || i2 < 0 {
            continue;
        }
        let (a, b) = (markers[i1 as usize], markers[i2 as usize]);
        if a.frac < fillfrac && b.frac > fillfrac {
            let t = (fillfrac - a.frac) / (b.frac - a.frac);
            markers[len] = Marker {
                x1: a.x1 + t * (b.x1 - a.x1),
                y1: a.y1 + t * (b.y1 - a.y1),
                x2: a.x2 + t * (b.x2 - a.x2),
                y2: a.y2 + t * (b.y2 - a.y2),
                frac: fillfrac,
            };
            for j in (i + 1..len).rev() {
                indexes[j + 1] = indexes[j];
            }
            indexes[i + 1] = len as i32;
            return 1;
        }
    }
    0
}

/// `healthbar_choose_colour` (`healthbar.c:80`).
fn choose_colour(fillcol: u32, bgcol: u32, exc: f32, inc: f32, frac: f32) -> u32 {
    if frac >= inc {
        return bgcol;
    }
    if frac <= exc {
        return fillcol;
    }
    let mult = (frac - exc) / (inc - exc);
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as i32;
    let mix = |s: u32| (ch(fillcol, s) + ((ch(bgcol, s) - ch(fillcol, s)) as f32 * mult) as i32) as u32 & 0xff;
    mix(24) << 24 | mix(16) << 16 | mix(8) << 8 | mix(0)
}

/// `healthbar_draw(gdl, NULL, 0, 0)` (`healthbar.c:133`) as triangle strips:
/// the armour (green) and trauma (red) parts; the shield ring is drawn too, empty.
fn healthbar_strips(apparenthealth: f32, heightfrac: f32) -> Vec<Vec<(f32, f32, u32)>> {
    let (radmax, radmed, radmin) = (30.0f32, 18.0f32, 12.0f32);
    let (len1, len2, len3) = (170.0f32, 47.0f32, 40.0f32);
    let (shieldcol, armourcol, traumacol, bgcol) = (0x10500090u32, 0x00c00060u32, 0xff000060u32, 0x00000080u32);
    let (offx, offy) = (-85.0f32, -185.0f32);
    let (shieldfade, armourfade, traumafade) = (100.0f32, 100.0f32, 200.0f32);
    let hf = heightfrac;
    let shieldfrac = 0.0f32;
    let armourfrac = ((apparenthealth - 0.25) / 0.75).max(0.0);
    let traumafrac = ((0.25 - apparenthealth) * 4.0).max(0.0);
    let m = |x1: f32, y1: f32, x2: f32, y2: f32, frac: f32| Marker { x1, y1, x2, y2, frac };

    let mut shield = [Marker::default(); 12];
    let sh = [
        m(len1 + radmax * 1.08, 0.0, len1 + radmed, 0.0, 0.0),
        m(len1 + radmax * 0.924 * 1.04, hf * radmax * 0.383, len1 + radmed * 0.924, hf * radmed * 0.383, 0.05),
        m(len1 + radmax * 0.707 * 1.02, hf * radmax * 0.707, len1 + radmed * 0.707, hf * radmed * 0.707, 0.1),
        m(len1 + radmax * 0.383, hf * radmax * 0.924, len1 + radmed * 0.383, hf * radmed * 0.924, 0.15),
        m(len1, hf * radmax, len1, hf * radmed, 0.2),
        m(0.0, hf * radmax, 0.0, hf * radmed, 0.8),
        m(-radmax * 0.383, hf * radmax * 0.924, -radmed * 0.383, hf * radmed * 0.924, 0.85),
        m(-radmax * 0.707 * 1.02, hf * radmax * 0.707, -radmed * 0.707, hf * radmed * 0.707, 0.9),
        m(-radmax * 0.924 * 1.04, hf * radmax * 0.383, -radmed * 0.924, hf * radmed * 0.383, 0.95),
        m(-radmax * 1.08, 0.0, -radmed, 0.0, 1.0),
    ];
    shield[..10].copy_from_slice(&sh);
    let mut armour = [Marker::default(); 8];
    let ar = [
        m(len2, hf * radmin, len2, -hf * radmin, 0.0),
        m(len1, hf * radmin, len1, -hf * radmin, 0.9),
        m(len1 + radmin * 0.342, hf * radmin * 0.94, len1 + radmin * 0.342, -hf * radmin * 0.94, 0.94),
        m(len1 + radmin * 0.643, hf * radmin * 0.766, len1 + radmin * 0.643, -hf * radmin * 0.766, 0.97),
        m(len1 + radmin * 0.866, hf * radmin * 0.5, len1 + radmin * 0.866, -hf * radmin * 0.5, 0.99),
        m(len1 + radmin * 0.985, hf * radmin * 0.174, len1 + radmin * 0.985, -hf * radmin * 0.174, 1.0),
    ];
    armour[..6].copy_from_slice(&ar);
    let mut trauma = [Marker::default(); 8];
    let tr = [
        m(len3, hf * radmin, len3, -hf * radmin, 0.0),
        m(0.0, hf * radmin, 0.0, -hf * radmin, 0.8),
        m(-radmin * 0.383, hf * radmin * 0.924, -radmin * 0.383, -hf * radmin * 0.924, 0.85),
        m(-radmin * 0.707, hf * radmin * 0.7070, -radmin * 0.707, -hf * radmin * 0.7070, 0.9),
        m(-radmin * 0.924, hf * radmin * 0.383, -radmin * 0.924, -hf * radmin * 0.383, 0.95),
        m(-radmin, 0.0, -radmin, 0.0, 1.0),
    ];
    trauma[..6].copy_from_slice(&tr);

    let mut out = Vec::new();
    // Shield: shielddir = 1, so it fills the other way.
    let sfrac = 1.0 - shieldfrac;
    let inc = (1.0 + shieldfade * 0.001) * sfrac;
    let exc = inc - shieldfade * 0.001;
    let mut sidx: [i32; 12] = std::array::from_fn(|i| if i < 10 { i as i32 } else { -1 });
    let mut n = 10;
    n += insert_marker(&mut shield, &mut sidx, exc);
    n += insert_marker(&mut shield, &mut sidx, inc);
    out.push(strip(&shield, &sidx[..n], offx, offy, |f| choose_colour(bgcol, shieldcol, exc, inc, f)));
    for (markers, frac, fade, col) in [(&mut armour, armourfrac, armourfade, armourcol), (&mut trauma, traumafrac, traumafade, traumacol)] {
        let inc = (1.0 + fade * 0.001) * frac;
        let exc = inc - fade * 0.001;
        let mut idx: [i32; 8] = std::array::from_fn(|i| if i < 6 { i as i32 } else { -1 });
        let mut n = 6;
        n += insert_marker(markers, &mut idx, exc);
        n += insert_marker(markers, &mut idx, inc);
        out.push(strip(markers, &idx[..n], offx, offy, |f| choose_colour(col, bgcol, exc, inc, f)));
    }
    out
}

/// A marker list as strip vertices `(x, z, colour)`: each marker's pair, in order.
fn strip(markers: &[Marker], idx: &[i32], offx: f32, offy: f32, colour: impl Fn(f32) -> u32) -> Vec<(f32, f32, u32)> {
    let mut v = Vec::new();
    for &i in idx {
        let mk = markers[i as usize];
        let c = colour(mk.frac);
        v.push(((mk.x1 as i32) as f32 + offx, (mk.y1 as i32) as f32 + offy, c));
        v.push(((mk.x2 as i32) as f32 + offx, (mk.y2 as i32) as f32 + offy, c));
    }
    v
}

/// `player_render_health_bar` (`player.c:2682`) into the HUD canvas: the bar's
/// y = 0 plane seen from (0, 370, 0) looking at the origin with −z up the screen,
/// through the scene's perspective.
pub fn draw_health_bar(apparenthealth: f32, heightfrac: f32, fovy: f32, cv: &mut Canvas) {
    let (w, h) = (cv.w as f32, cv.h as f32);
    let view = pdmtx::view_matrix(Vec3::new(0.0, 370.0, 0.0), Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
    let proj = Mat4::perspective_rh(fovy.to_radians(), w / h, 10.0, 10000.0);
    let pv = proj * view;
    let to_px = |x: f32, z: f32| -> Option<(f32, f32)> {
        let c = pv * Vec4::new(x, 0.0, z, 1.0);
        if c.w <= 0.0 {
            return None;
        }
        Some(((c.x / c.w * 0.5 + 0.5) * w, (0.5 - c.y / c.w * 0.5) * h))
    };
    for s in healthbar_strips(apparenthealth, heightfrac) {
        for k in 0..s.len().saturating_sub(2) {
            let tri = [s[k], s[k + 1], s[k + 2]];
            let mut pts = Vec::new();
            for (x, z, c) in tri {
                let Some((px, py)) = to_px(x, z) else { break };
                pts.push((px, py, c));
            }
            if pts.len() == 3 {
                shade_tri(cv, [pts[0], pts[1], pts[2]]);
            }
        }
    }
}

/// The match's health HUD into the canvas.
pub fn draw(f: &Fight, cv: &mut Canvas) {
    let hs = &f.hp;
    if hs.healthshowmode != HealthShowMode::Hidden {
        draw_health_bar(hs.apparenthealth, hs.bar_height_frac(), f.guns.bgun.p.fovy, cv);
    }
}
