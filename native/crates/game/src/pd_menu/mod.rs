//! **SPIKE: Perfect Dark's Combat Simulator menus, replicated literally.**
//!
//! A standalone window (`cargo run --release --bin pd_combat_sim`) running PD's
//! own menu system: the dialog stack and its open / populate / redraw
//! animations (`menu.c`), every item widget (`menuitem.c`), the dialog chrome,
//! shimmer comets and the rotating cone background (`menugfx.c`), the text
//! renderer with its glow, waves and hologram sweep (`text.c`), and the
//! Combat Simulator's ~56 dialogs with their handlers (`mplayer/setup.c`,
//! `scenarios.c`, `mainmenu.c`), driven by PD's input and key-repeat code.
//! Sister spike to [`crate::pd_guns`] (the guns) and [`crate::pd_spike`] (the
//! simulants).
//!
//! Ground rules (same as the other PD spikes):
//!
//! * **Port functions, not behaviours.** Each PD function is a Rust function
//!   with the same name and a `file:line` citation, in PD's units — 320×220
//!   screen pixels, 60 Hz frames (`diffframe60`), PD's colour words.
//! * **The data is PD's.** The menu and MP tables are generated from the decomp
//!   by `tools/pd-assets/pd_menu_gen.py` ([`generated`]); fonts, strings,
//!   textures, presets and challenges come from the ROM (NTSC 1.1, which is the
//!   decomp's `ntsc-final`).
//! * **Where we must substitute, say so at the call site.** No Controller Pak,
//!   no N64 music sequencer, no solo game file ([`mp::Profile`]), no match to
//!   start — see [`Pd::start_match`].
//! * **Rendering is a software RDP** ([`gfx`]) into a 320×220 framebuffer the
//!   window scales up, so draw order and blending are PD's own.
//!
//! Module map: [`types`] (the C structs), [`generated`] (the tables), [`lang`],
//! [`text`], [`gfx`], [`menugfx`], [`menu`], [`menuitem`], [`mp`],
//! [`handlers`], [`defs`] (stub dialogs), [`model`] (menu models), [`app`]
//! (the window), [`snapshot`] (headless PNGs).

pub mod app;
pub mod defs;
pub mod generated;
pub mod gfx;
pub mod handlers;
pub mod lang;
pub mod menu;
pub mod menugfx;
pub mod menuitem;
pub mod model;
pub mod mp;
pub mod pdmodel;
pub mod snapshot;
pub mod text;
pub mod types;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use crate::pd_spike::pdmath::Rng;
use gfx::{Addr, Gfx, Texture};
use lang::Lang;
use menu::{Menu, MenuData};
use mp::{MpConfig, MpState, Profile};
use text::{Fonts, TextCtx, TextState};
use types::*;

/// `native/assets/pd_menu/`.
pub fn assets_dir() -> PathBuf {
    // `native/target/<profile>/<exe>` → `native/assets/pd_menu`, so a binary
    // finds the repo it sits in wherever it was compiled.
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent()?.parent()?.parent().map(|p| p.join("assets").join("pd_menu"))) {
        if dir.join("lang_en.json").exists() {
            return dir;
        }
    }
    PathBuf::from(format!("{}/../../assets/pd_menu", env!("CARGO_MANIFEST_DIR")))
}

/// The `g_Vars` fields the menus touch (varsinit.c:57-71 for the defaults).
#[derive(Clone, Debug)]
pub struct Vars {
    pub diffframe60: i32,
    pub diffframe60f: f32,
    pub diffframe240f: f32,
    pub mpsetupmenu: i32,
    pub mpquickteam: i32,
    pub usingadvsetup: bool,
    pub waitingtojoin: [bool; 4],
    pub mpquickteamnumsims: i32,
    pub mpsimdifficulty: i32,
    pub unk0004a0: i32,
    pub mpplayerteams: [u8; 4],
    pub mphilltime: u8,
    pub unk000498: i32,
    pub screenratio: u8,
    pub screensplit: u8,
}

impl Default for Vars {
    fn default() -> Self {
        Vars {
            diffframe60: 1,
            diffframe60f: 1.0,
            diffframe240f: 4.0,
            mpsetupmenu: 0,
            mpquickteam: generated::MPQUICKTEAM_NONE,
            usingadvsetup: false,
            waitingtojoin: [false; 4],
            mpquickteamnumsims: 1,
            mpsimdifficulty: generated::BOTDIFF_NORMAL,
            unk0004a0: 1,
            mpplayerteams: [0, 1, 2, 3],
            mphilltime: 10,
            unk000498: 0,
            screenratio: 0,
            screensplit: 0,
        }
    }
}

/// One controller as `joy.c` reports it: N64 button mask and stick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Joy {
    pub buttons: u16,
    pub prev: u16,
    pub stick_x: i8,
    pub stick_y: i8,
    /// The keyboard item's delete (PD's `inputs.back2`).
    pub back2: bool,
}

pub struct Resources {
    pub fonts: Fonts,
    pub lang: Lang,
    /// `TEX_GENERAL_MENURAY0` (TEXTURE_01E5, 64×64 IA8, wrap).
    pub menuray0: Texture,
    /// `TEX_GENERAL_ENVSTAR` (TEXTURE_084E, 11×11 IA8, clamp).
    pub envstar: Texture,
    /// `g_BlurBuffer`: 40×30, made by `menugfx_create_blur` from the frame
    /// behind the menu. See [`Resources::blur_from_image`].
    pub blur: Option<Texture>,
    pub mpconfigs: Vec<MpConfig>,
}

impl Resources {
    pub fn load() -> Result<Resources, String> {
        let dir = assets_dir();
        let tex = dir.join("textures");
        Ok(Resources {
            fonts: Fonts::load(&dir.join("fonts"))?,
            lang: Lang::load()?,
            menuray0: Texture::load_png(&tex.join("tex_01e5.png"), Addr::Wrap, Addr::Wrap)?,
            envstar: Texture::load_png(&tex.join("tex_084e.png"), Addr::Clamp, Addr::Clamp)?,
            blur: None,
            mpconfigs: mp::load_mpconfigs(&dir)?,
        })
    }

    /// `menugfx_create_blur` (menugfx.c:45) over a picture of "the game behind
    /// the menu": the image is resampled to the 320×220 framebuffer, each 8×8
    /// block averaged in RGB555, into a 40×30 RGBA5551 texture.
    ///
    /// **Substitution:** PD blurs the Carrington Institute it is running
    /// (`STAGE_CITRAINING`); the spike blurs `assets/pd_menu/bg_source.png`
    /// (a PD scene) if present, else a dim gradient.
    pub fn blur_from_image(&mut self, img: Option<&image::RgbaImage>) {
        let (w, h) = (320usize, 220usize);
        let sample = |x: usize, y: usize| -> [u32; 3] {
            match img {
                Some(im) => {
                    let sx = (x * im.width() as usize / w).min(im.width() as usize - 1) as u32;
                    let sy = (y * im.height() as usize / h).min(im.height() as usize - 1) as u32;
                    let p = im.get_pixel(sx, sy);
                    [p[0] as u32 >> 3, p[1] as u32 >> 3, p[2] as u32 >> 3]
                }
                None => {
                    let t = y as f32 / h as f32;
                    [(4.0 + 6.0 * t) as u32, (5.0 + 4.0 * t) as u32, (9.0 - 3.0 * t) as u32]
                }
            }
        };
        let mut px = vec![[0.0f32; 4]; 40 * 30];
        for dy in 0..30 {
            for dx in 0..40 {
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                for sx in 0..8 {
                    for sy in 0..8 {
                        let (x, y) = (dx * 8 + sx, dy * 8 + sy);
                        // Rows past the 220-line framebuffer read the next buffer
                        // in PD; clamp here.
                        let c = sample(x.min(w - 1), y.min(h - 1));
                        r += c[0];
                        g += c[1];
                        b += c[2];
                    }
                }
                let (r, g, b) = (r / 64, g / 64, b / 64);
                let f = |v: u32| ((v << 3) | (v >> 2)) as f32 / 255.0;
                px[dy * 40 + dx] = [f(r), f(g), f(b), 1.0];
            }
        }
        self.blur = Some(Texture::from_rgba(40, 30, px, Addr::Clamp, Addr::Clamp));
    }
}

/// What the Ready dialog would have launched (`mp_start_match`).
#[derive(Clone, Debug)]
pub struct MatchSummary {
    pub lines: Vec<String>,
}

/// The whole of PD's menu state (see the module doc for the global mapping).
pub struct Pd {
    pub gfx: Gfx,
    pub text: TextState,
    pub res: Resources,
    /// `g_20SecIntervalFrac`.
    pub frac20: f32,
    pub vars: Vars,
    pub menus: [Menu; 4],
    pub menudata: MenuData,
    pub mpplayernum: usize,
    /// `g_MpNumJoined`.
    pub mp_num_joined: i32,
    pub mp: MpState,
    pub rng: Rng,
    /// Menu sounds queued this frame: (sound id, pitch, volume).
    pub sounds: Vec<(i32, f32, f32)>,
    pub joy: [Joy; 4],
    /// `joy_get_connected_controllers` as a bit mask.
    pub connected_pads: u32,
    /// `g_LineHeight` (menuitem.c:37).
    pub line_height: i32,
    /// `g_MenuCThresh` (menu.c:3581).
    pub menu_cthresh: i32,
    /// `g_MenuScissorX1..Y2`.
    pub scissor_menu: [i32; 4],
    /// `g_MpSelectedPlayersForStats`.
    pub mp_selected_for_stats: [usize; 4],
    pub match_started: Option<MatchSummary>,
    /// Model files + the menu animations (`pdmodel`).
    pub models: pdmodel::ModelStore,
    /// Each menumodel's `bodymodel` (players 0-3, then the hudpiece).
    pub model_inst: [Option<pdmodel::Inst>; 5],
}

impl Pd {
    pub fn new(profile: Profile) -> Result<Pd, String> {
        let mut res = Resources::load()?;
        let bg = image::open(assets_dir().join("bg_source.png")).ok().map(|i| i.to_rgba8());
        res.blur_from_image(bg.as_ref());
        let mut pd = Pd {
            gfx: Gfx::new(320, 220),
            text: TextState::default(),
            res,
            frac20: 0.0,
            vars: Vars::default(),
            menus: std::array::from_fn(|_| Menu::default()),
            menudata: MenuData::default(),
            mpplayernum: 0,
            mp_num_joined: 1,
            mp: MpState { profile, ..MpState::default() },
            rng: Rng::new(0x1234_5678),
            sounds: Vec::new(),
            joy: [Joy::default(); 4],
            connected_pads: 1,
            line_height: LINEHEIGHT,
            menu_cthresh: 120,
            scissor_menu: [0, 0, 320, 220],
            mp_selected_for_stats: [0, 1, 2, 3],
            match_started: None,
            models: pdmodel::ModelStore::load(),
            model_inst: Default::default(),
        };
        for m in pd.menus.iter_mut() {
            m.menumodel.zoom = -1.0;
        }
        // menu_reset (menu.c:3801): the hudpiece's resting place.
        let hp = &mut pd.menudata.hudpiece;
        hp.newparams = generated::FILE_GHUDPIECE as u32;
        hp.curroty = -std::f32::consts::PI;
        hp.newroty = hp.curroty;
        hp.curposx = -205.5;
        hp.newposx = -205.5;
        hp.curposy = 244.7;
        hp.newposy = 244.7;
        hp.curposz = 68.3;
        hp.newposz = 68.3;
        hp.curscale = 0.12209;
        hp.newscale = 0.12209;
        hp.zoom = -1.0;
        hp.headnum = -1;
        hp.bodynum = -1;
        pd.mp_init();
        Ok(pd)
    }

    /// Open the Perfect Menu (the CI main menu) as PD does after file select.
    pub fn open_main_menu(&mut self) {
        self.mpplayernum = 0;
        self.menu_push_root_dialog(&generated::G_CI_MENU_VIA_PC_MENU_DIALOG, MENUROOT_MAINMENU);
    }

    /// Straight into the Combat Simulator (what "Combat Simulator" on the
    /// Perfect Menu does, via `menu_save_and_push_root_dialog`).
    pub fn open_combat_simulator(&mut self) {
        self.mpplayernum = 0;
        self.challenge_determine_unlocked_features();
        self.vars.mpsetupmenu = generated::MPSETUPMENU_GENERAL;
        self.menu_push_root_dialog(&generated::G_COMBAT_SIMULATOR_MENU_DIALOG, MENUROOT_MPSETUP);
        self.sounds.push((generated::SFXMAP_8098_EXPLOSION, 1.0, 1.0));
    }

    pub fn tc(&mut self) -> TextCtx<'_> {
        TextCtx { gfx: &mut self.gfx, ts: &mut self.text, fonts: &self.res.fonts, frac20: self.frac20 }
    }

    /// One PD frame at `diffframe60` 60 Hz ticks: `menu_tick` then `menu_render`.
    pub fn frame(&mut self, diffframe60: i32) {
        self.vars.diffframe60 = diffframe60;
        self.vars.diffframe60f = diffframe60 as f32;
        self.vars.diffframe240f = 4.0 * diffframe60 as f32;
        if self.match_started.is_none() {
            self.menu_tick();
        }
        self.menu_render();
        for j in self.joy.iter_mut() {
            j.prev = j.buttons;
            j.back2 = false;
        }
    }

    /// `MENUROOT_START_MP_MATCH` (menutick.c:521): `mp_start_match` + `menu_stop`.
    ///
    /// **Substitution:** there is no match here. The spike records what PD would
    /// have started (`mp_start_match` → `mp_configure_quick_team_simulants`, the
    /// arena, scenario, weapons, limits and every chr) and shows it; START comes
    /// back to the menus the way PD returns from a match (menutick.c:217).
    pub fn start_match(&mut self) {
        handlers::mp_configure_quick_team_simulants(self);
        let mut lines = Vec::new();
        let scen = self.lang(generated::MP_SCENARIO_OVERVIEWS[self.mp.setup.scenario as usize % 6].name);
        let arena = generated::MP_ARENAS.iter().find(|a| a.stagenum == self.mp.setup.stagenum as i32).map(|a| self.lang(a.name)).unwrap_or_default();
        lines.push(format!("Scenario: {}", scen.trim()));
        lines.push(format!("Arena: {}", arena.trim()));
        let ws = self.mp_get_weaponset_slotnum();
        lines.push(format!("Weapons: {}", self.mp_get_weaponset_name_by_slotnum(ws).trim()));
        let weps: Vec<String> = (0..6).map(|s| {
            let slot = self.mp_get_weapon_slot(s);
            self.mp_get_weapon_label(slot).trim().to_string()
        }).collect();
        lines.push(format!("  {}", weps.join(", ")));
        let tl = self.mp.setup.timelimit;
        lines.push(format!("Time: {}   Score: {}", if tl >= 60 { "No Limit".into() } else { format!("{} min", tl as i32 + 1) }, if self.mp.setup.scorelimit >= 100 { "No Limit".into() } else { format!("{}", self.mp.setup.scorelimit as i32 + 1) }));
        for i in 0..12 {
            if self.mp.setup.chrslots & (1 << i) != 0 {
                let c = self.mpchr(i).unwrap_or_default();
                let body = self.mp_get_body_name(c.mpbodynum as usize);
                let kind = if i < 4 {
                    format!("Player {}", i + 1)
                } else {
                    let b = &self.mp.bots[i - 4];
                    let d = if (b.difficulty as i32) < generated::BOTDIFF_DISABLED { self.lang(lang::tx(generated::B_MISC, 82).add(b.difficulty as i32)) } else { String::new() };
                    format!("Sim ({})", d.trim())
                };
                let team = if self.mp.setup.options & generated::MPOPTION_TEAMSENABLED as u32 != 0 { format!(" [{}]", self.mp.bossfile.teamnames[c.team as usize & 7].trim()) } else { String::new() };
                lines.push(format!("{}: {} — {}{}", kind, c.name.trim(), body.trim(), team));
            }
        }
        log::info!("pd_combat_sim: match would start:\n{}", lines.join("\n"));
        self.match_started = Some(MatchSummary { lines });
        for i in 0..4 {
            self.mpplayernum = i;
            self.menu_save_and_close_all();
        }
        self.mpplayernum = 0;
        self.menudata.count = 0;
    }

    /// Back from the "match" (menutick.c:217, `g_MpReturningFromMatch`).
    pub fn return_from_match(&mut self) {
        self.match_started = None;
        self.mp_num_joined = 0;
        self.vars.mpsetupmenu = if self.vars.usingadvsetup { generated::MPSETUPMENU_ADVSETUP } else { generated::MPSETUPMENU_GENERAL };
        // mp_start_match turned quick-team sims into real ones; PD reloads the
        // setup at match end, and the quick team rebuilds them next time.
        if self.vars.mpquickteam != generated::MPQUICKTEAM_NONE {
            for i in 0..8 {
                self.mp_remove_simulant(i);
            }
        }
        for i in 0..4 {
            self.vars.waitingtojoin[i] = false;
            if self.mp.setup.chrslots & (1 << i) != 0 {
                self.mpplayernum = i;
                if self.vars.mpsetupmenu == generated::MPSETUPMENU_ADVSETUP {
                    self.mp_num_joined += 1;
                    self.mp_open_advanced_setup(true);
                } else if self.mp_num_joined == 0 {
                    self.mp_num_joined += 1;
                    self.menu_push_root_dialog(&generated::G_COMBAT_SIMULATOR_MENU_DIALOG, MENUROOT_MPSETUP);
                } else {
                    self.vars.waitingtojoin[i] = true;
                }
            }
        }
        self.mpplayernum = 0;
        if self.menus.iter().all(|m| m.curdialog.is_none()) {
            self.open_combat_simulator();
        }
        self.sounds.push((generated::SFXMAP_8098_EXPLOSION, 1.0, 1.0));
    }

    /// Switch the pretend save file and re-derive the unlocks.
    pub fn set_profile(&mut self, profile: Profile) {
        self.mp.profile = profile;
        self.challenges_init();
    }
}
