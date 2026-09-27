//! PD's gun HUD, `bgun_draw_hud` (`bondgun.c:9930`), one player, full screen:
//! the function square, the weapon and function names sliding in with the
//! wave highlight, the magazine and reserve gauges with their counts
//! (`bgun_draw_hud_gauge`, `bondgun.c:9687`, animated by the `abmag`
//! tracker, `bgun0f0a9da8`), and the Combat Boost timer. Drawn into a
//! [`Canvas`] of PD screen pixels, which the window scales over the frame.

use super::bgun::*;
use super::font::{Canvas, Font};
use super::gset::*;

/// `AMMOFLAG_EQUIPPEDISRESERVE` (`constants.h:240`).
const AMMOFLAG_EQUIPPEDISRESERVE: i64 = 2;
/// `HUDHALIGN_*` / `HUDVALIGN_*` (`constants.h:1423`, `:1466`).
const HUDHALIGN_RIGHT: i32 = 0;
const HUDHALIGN_LEFT: i32 = 1;
const HUDVALIGN_BOTTOM: i32 = 0;
const HUDVALIGN_TOP: i32 = 1;

/// `struct abmag`: the gauge's animated view of a clip.
#[derive(Clone, Copy, Debug, Default)]
pub struct Abmag {
    pub loadedammo: i32,
    pub change: i32,
    pub ref_: i32,
    pub timer60: i32,
}

impl Abmag {
    /// `bgun0f0a9da8` (`bondgun.c:9606`).
    fn tick(&mut self, mut remaining: i32, mut capacity: i32, height: i32, lvupdate60: i32) {
        if capacity > 20 {
            let mut newremaining = height * remaining / capacity;
            if remaining > 0 && newremaining < 1 {
                newremaining = 1;
            }
            capacity = height;
            if newremaining == self.ref_ && self.loadedammo > remaining {
                self.ref_ += 1;
            }
            self.loadedammo = remaining;
            remaining = newremaining;
        }
        let mut newchange = remaining - self.ref_;
        if (self.change < 0 && newchange > 0) || (self.change > 0 && newchange < 0) {
            self.ref_ += self.change;
            self.change = 0;
            self.timer60 = 0;
            newchange = remaining - self.ref_;
        }
        if self.change < 0 && self.change > newchange && self.timer60 > -self.change * 64 {
            self.timer60 = -self.change * 64;
        }
        self.change = newchange;
        let speed = if self.change > 0 {
            capacity.max(6)
        } else {
            let mut h = 8;
            if self.change < -3 {
                h += -self.change * 2;
            }
            h
        };
        if self.change != 0 {
            self.timer60 += lvupdate60 * speed;
            if self.timer60 > 255 {
                if self.change > 0 {
                    while self.timer60 > 255 && self.change > 0 {
                        self.change -= 1;
                        self.ref_ += 1;
                        self.timer60 -= 64;
                    }
                } else {
                    while self.timer60 > 255 && self.change < 0 {
                        self.change += 1;
                        self.ref_ -= 1;
                        self.timer60 -= 64;
                    }
                }
            }
        } else {
            self.timer60 = 0;
        }
    }
}

/// The `gunctrl` fields `bgun_draw_hud` keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct HudState {
    pub abmag: [Abmag; 2],
    pub ctrl_abmag: Abmag,
    pub fnfader: i32,
    pub guntypetimer: i32,
    pub curgunstr: i32,
    pub fnstrtimer: i32,
    pub curfnstr: Option<String>,
    pub lastmag: i32,
}

pub struct HudFonts {
    pub numeric: Font,
    pub handelgothicxs: Font,
}

impl HudFonts {
    pub fn load() -> Result<Self, String> {
        let dir = super::data::assets_dir().join("fonts");
        Ok(HudFonts { numeric: Font::load(&dir.join("numeric.bin"))?, handelgothicxs: Font::load(&dir.join("handelgothicxs.bin"))? })
    }
}

/// What `bgun_draw_hud` reads from outside the gun.
pub struct HudIn {
    pub lvframenum: i32,
    pub lvupdate60: i32,
    pub lvupdate240: i32,
    pub interval_frac: f32,
    pub speedpilltime: i32,
}

/// `bgun_draw_hud_string` (`bondgun.c:9542`): aligned, black box (alpha 0, so
/// invisible), then `text_render_v1` in the numeric font with a 0x000000a0
/// glow.
fn bgun_draw_hud_string(cv: &mut Canvas, f: &Font, text: &str, x: i32, halign: i32, y: i32, valign: i32, colour: u32) {
    let (textheight, textwidth) = f.measure(text);
    let x1 = match halign {
        HUDHALIGN_LEFT => x,
        HUDHALIGN_RIGHT => x - textwidth,
        _ => x + textwidth / 2 - textwidth,
    };
    let y1 = match valign {
        HUDVALIGN_TOP => y,
        HUDVALIGN_BOTTOM => y - textheight,
        _ => y + textheight / 2 - textheight,
    };
    f.render_v1(cv, x1, y1, text, colour, 0x000000a0);
}

/// `bgun_draw_hud_gauge` (`bondgun.c:9687`): blocks per round up to 20, a
/// single split bar above that; newly loaded rounds flash white then settle,
/// spent ones fade out. `flip` draws it top-down (the reserve).
#[allow(clippy::too_many_arguments)]
fn bgun_draw_hud_gauge(
    cv: &mut Canvas,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    abmag: &mut Abmag,
    remaining: i32,
    capacity: i32,
    emptycolour: u32,
    filledcolour: u32,
    flip: bool,
    lvupdate60: i32,
) {
    use super::font::colour_blend;
    let mut gaugeheight = y2 - y1;
    let mut numunits = capacity;
    abmag.tick(remaining, numunits, gaugeheight, lvupdate60);
    let refv = abmag.ref_;
    let (unitheight, gaugetop);
    if numunits > 20 {
        unitheight = 1;
        numunits = gaugeheight;
        gaugetop = y2 - gaugeheight;
    } else {
        let mut uh = gaugeheight / numunits.max(1);
        let r1 = (uh * numunits - gaugeheight).abs();
        let r2 = ((uh + 1) * numunits - gaugeheight).abs();
        if r2 < r1 {
            uh += 1;
        }
        unitheight = uh;
        let mut gt = y2 - unitheight * capacity + 1;
        if unitheight <= 2 {
            gt -= 1;
        }
        gaugetop = gt;
    }
    let rect = |cv: &mut Canvas, top: i32, bottom: i32, colour: u32| {
        if flip {
            cv.fill_rect(x1, y2 - bottom + y1, x2, y2 - top + y1, colour);
        } else {
            cv.fill_rect(x1, top, x2, bottom, colour);
        }
    };
    if unitheight == 0 {
        // Unreachable in PD (see the comment at bondgun.c:9738).
        gaugeheight = y2 - gaugetop;
        let partitiony = y2 - gaugeheight * refv / numunits.max(1);
        if partitiony > gaugetop {
            rect(cv, gaugetop, partitiony, emptycolour);
        }
        rect(cv, partitiony, y2, filledcolour);
        return;
    }
    // The RDP prim colour: text_begin_boxmode(emptycolour), then set only
    // when a unit's state changes; a merged gauge flushes its previous run in
    // the colour that was current before the change.
    let mut prim = emptycolour;
    let mut colour = emptycolour;
    let mut unittop = gaugetop;
    let mut unitbottom = -1;
    for i in 0..numunits {
        let mut newstate = false;
        if abmag.change > 0 {
            if i >= numunits - refv - abmag.change && i < numunits - refv {
                let fadeamount = abmag.timer60 - (numunits - refv - i - 1) * 64;
                if fadeamount >= 0 {
                    if fadeamount >= 64 {
                        let weight = (((fadeamount * 4 - 252) / 3) as u32).min(255);
                        colour = colour_blend(filledcolour, 0xffffffbf, weight);
                    } else {
                        let weight = (fadeamount * 4) as u32;
                        colour = colour_blend(0xffffffbf, emptycolour, weight);
                    }
                    newstate = true;
                }
            }
        } else if abmag.change < 0 && i < numunits - refv - abmag.change && i >= numunits - refv {
            let fadeamount = abmag.timer60 - (i - numunits + refv) * 64;
            if fadeamount >= 0 {
                let weight = fadeamount as u32;
                colour = if weight > 255 { emptycolour } else { colour_blend(emptycolour, filledcolour | 0xff, weight) };
                newstate = true;
            }
        }
        if abmag.change < 0 {
            if i == numunits - refv - abmag.change {
                colour = filledcolour;
                newstate = true;
            }
        } else if i == numunits - refv {
            colour = filledcolour;
            newstate = true;
        }
        if unitheight <= 2 {
            if newstate {
                if unitbottom >= 0 {
                    rect(cv, unittop, unitbottom, prim);
                }
                unittop = gaugetop + i * unitheight;
            }
            unitbottom = gaugetop + i * unitheight + unitheight;
        } else {
            unittop = gaugetop + i * unitheight;
            unitbottom = gaugetop + i * unitheight + unitheight - 1;
        }
        if newstate {
            prim = colour;
        }
        if unitbottom >= y2 - 1 && unitheight >= 2 {
            unitbottom = y2;
        }
        if unitheight >= 3 {
            rect(cv, unittop, unitbottom, prim);
        }
    }
    if unitheight <= 2 {
        rect(cv, unittop, unitbottom, prim);
    }
}

impl Bgun {
    /// `bgun_draw_hud` (`bondgun.c:9930`), one player, full-screen view.
    pub fn bgun_draw_hud(&self, st: &mut HudState, fonts: &HudFonts, inp: &HudIn, cv: &mut Canvas) {
        if inp.lvframenum < 5 {
            return;
        }
        let view_h = cv.h as i32;
        let view_w = cv.w as i32;
        let bottom = view_h - 13;
        let barwidth = 9;
        let reserveheight = 36;
        let clipheight = 57;
        let ctrl = &self.ctrl;
        let hand = &self.hands[HAND_RIGHT];
        let lefthand = &self.hands[HAND_LEFT];
        let weapon = self.gset.weapon(ctrl.weaponnum);

        let mut fncolour: u32 = 0xff000040;
        let mut funcnum = hand.weaponfunc;
        let fnfaderinc = inp.lvupdate240 * 2;
        let tmpfuncnum = self.bgun_is_using_secondary_function() as usize;
        if self.bgun_get_ammo_state(tmpfuncnum, HAND_RIGHT) > GUNAMMOSTATE_DEPLETED {
            funcnum = tmpfuncnum;
        }
        let mut xpos = view_w - barwidth - 24;

        // Function square.
        if funcnum == FUNC_SECONDARY && st.fnfader < 255 {
            st.fnfader = st.fnfader.max(128);
            st.fnfader = (st.fnfader + fnfaderinc).min(255);
        }
        if funcnum == FUNC_PRIMARY && st.fnfader > 0 {
            st.fnfader = (st.fnfader - fnfaderinc).max(0);
        }
        if st.fnfader > 128 {
            fncolour = (((st.fnfader * 2) - 256) as u32) << 16 | 0xff000040;
        }
        cv.fill_rect(xpos - 13, bottom - 11, xpos - 2, bottom, fncolour);

        // Weapon name, then function name (options_get_show_gun_function on).
        let wave = Some(inp.interval_frac * 50.0);
        let f = &fonts.handelgothicxs;
        let func = self.gset.func(hand.weaponnum, funcnum);
        let nameid = hand.weaponnum;
        if st.curgunstr != nameid {
            st.guntypetimer = 0;
            st.curgunstr = nameid;
        }
        if st.guntypetimer < 255 {
            let mut colour: u32 = 0x55ffffff;
            st.guntypetimer = (st.guntypetimer + inp.lvupdate60).min(255);
            let name = weapon.map_or(String::new(), |w| w.name.clone());
            let (textheight, mut textwidth) = f.measure(&name);
            textwidth += 2;
            textwidth = textwidth.min(st.guntypetimer * 3);
            let x = xpos - textwidth - 2;
            let y = bottom - textheight - 15;
            if st.guntypetimer > 192 {
                let alpha = 255 - (st.guntypetimer - 192) as u32 * 255 / 63;
                colour = (colour & 0xffffff00) | alpha;
            }
            f.render_v2(cv, x, y, &name, colour, textwidth, wave);
        }
        if let Some(func) = func {
            let mut colour: u32 = 0xff5555ff;
            if (st.curfnstr.as_deref() != Some(func.name.as_str()) && st.fnfader > 128) || st.curfnstr.is_none() {
                st.fnstrtimer = 0;
                st.curfnstr = Some(func.name.clone());
            }
            let s = st.curfnstr.clone().unwrap_or_default();
            if st.fnstrtimer < 255 {
                st.fnstrtimer = (st.fnstrtimer + inp.lvupdate60).min(255);
                if funcnum == FUNC_SECONDARY && func.name == s {
                    colour |= 0x00ff0000;
                }
                if funcnum == FUNC_PRIMARY && func.name != s {
                    colour |= 0x00ff0000;
                }
                let (textheight, mut textwidth) = f.measure(&s);
                textwidth += 2;
                textwidth = textwidth.min(st.fnstrtimer * 3);
                let x = xpos - textwidth - 13;
                let y = bottom - textheight - 1;
                if st.fnstrtimer > 192 {
                    let alpha = 255 - (st.fnstrtimer - 192) as u32 * 255 / 63;
                    colour = (colour & 0xffffff00) | alpha;
                }
                f.render_v2(cv, x, y, &s, colour, textwidth, wave);
            }
        }

        let Some(weapon) = weapon else { return };
        let mut ammoindex = weapon.functions[hand.weaponfunc].as_ref().map_or(0, |f| f.ammoindex);
        if ammoindex == -1 {
            ammoindex = weapon.functions[1 - hand.weaponfunc].as_ref().map_or(-1, |f| f.ammoindex);
            if ammoindex == -1 {
                return;
            }
        }
        if ammoindex != st.lastmag {
            st.abmag = [Abmag::default(); 2];
            st.ctrl_abmag = Abmag::default();
            st.lastmag = ammoindex;
        }
        let ai = ammoindex as usize;
        let n = &fonts.numeric;

        // Left hand: mag.
        if lefthand.inuse && weapon.ammos[ai].is_some() && lefthand.weaponnum != WEAPON_REMOTEMINE {
            let lx = 24;
            let a = weapon.ammos[ai].as_ref().unwrap();
            if lefthand.clipsizes[ai] > 0 && a.flags & AMMOFLAG_EQUIPPEDISRESERVE == 0 {
                bgun_draw_hud_gauge(
                    cv,
                    lx,
                    bottom - reserveheight - clipheight - 3,
                    lx + barwidth,
                    bottom - reserveheight - 3,
                    &mut st.abmag[HAND_LEFT],
                    lefthand.loadedammo[ai],
                    lefthand.clipsizes[ai],
                    0x00300080,
                    0x00ff0040,
                    false,
                    inp.lvupdate60,
                );
                bgun_draw_hud_string(cv, n, &format!("{}\n", lefthand.loadedammo[ai]), lx + barwidth + 2, HUDHALIGN_LEFT, bottom - reserveheight - 8, HUDVALIGN_BOTTOM, 0x00ff00a0);
            }
        }

        // Right hand: mag, reserve, boost timer.
        if hand.inuse && ctrl.ammotypes[ai] >= 0 {
            let ammotype = ctrl.ammotypes[ai];
            xpos = view_w - barwidth - 24;
            let ammoheld = self.ammoheld(ammotype);
            let a = weapon.ammos[ai].as_ref();
            if hand.clipsizes[ai] > 0 && a.is_some_and(|a| a.flags & AMMOFLAG_EQUIPPEDISRESERVE == 0) {
                bgun_draw_hud_gauge(
                    cv,
                    xpos,
                    bottom - reserveheight - clipheight - 3,
                    xpos + barwidth,
                    bottom - reserveheight - 3,
                    &mut st.abmag[HAND_RIGHT],
                    hand.loadedammo[ai],
                    hand.clipsizes[ai],
                    0x00300080,
                    0x00ff0040,
                    false,
                    inp.lvupdate60,
                );
                bgun_draw_hud_string(cv, n, &format!("{}\n", hand.loadedammo[ai]), xpos - 2, HUDHALIGN_RIGHT, bottom - reserveheight - 8, HUDVALIGN_BOTTOM, 0x00ff00a0);
            }
            let capacity = AMMO_CAPACITY.get(ammotype as usize).copied().unwrap_or(0);
            if let Some(a) = a {
                if capacity > 0 && a.flags & AMMOFLAG_NORESERVE == 0 {
                    let mut ammototal = ammoheld;
                    if a.flags & AMMOFLAG_EQUIPPEDISRESERVE != 0 {
                        if hand.clipsizes[ai] > 0 {
                            ammototal += hand.loadedammo[ai];
                        }
                        if lefthand.clipsizes[ai] > 0 {
                            ammototal += lefthand.loadedammo[ai];
                        }
                    }
                    bgun_draw_hud_gauge(
                        cv,
                        xpos,
                        bottom - reserveheight,
                        xpos + barwidth,
                        bottom,
                        &mut st.ctrl_abmag,
                        ammototal,
                        capacity,
                        0x00403080,
                        0x00ffc040,
                        true,
                        inp.lvupdate60,
                    );
                    bgun_draw_hud_string(cv, n, &format!("{ammototal}\n"), xpos - 2, HUDHALIGN_RIGHT, bottom - reserveheight + 1, HUDVALIGN_BOTTOM, 0x00ffc0a0);
                }
            }
            if hand.weaponnum == WEAPON_COMBATBOOST {
                let t = inp.speedpilltime;
                let mins = t / 3600;
                let secs60 = t - mins * 3600;
                let text = if mins >= 1 {
                    format!("{:02}:{:02}:{:02}\n", mins, secs60 / 60, (secs60 - (secs60 / 60) * 60) * 100 / 60)
                } else {
                    format!("{:02}:{:02}\n", secs60 / 60, (secs60 - (secs60 / 60) * 60) * 100 / 60)
                };
                bgun_draw_hud_string(cv, n, &text, xpos + barwidth - 2, HUDHALIGN_RIGHT, bottom - reserveheight + 1, HUDVALIGN_BOTTOM, 0x00ffc0a0);
            }
        }
    }
}
