//! One player in the firing range, stepped in PD's frame order:
//!
//! 1. `bmove_tick` — input, the hand state machines (`bgun_tick_gameplay`),
//!    walking ([`Player::tick`]);
//! 2. `player_allocate_matrices` — the camera for this frame;
//! 3. world ticks — beams (`player.c:5257`), sparks, casings;
//! 4. `hands_tick_attack` (`prop.c:1382`) — the shots, from the camera through
//!    the crosshair plus spread, which set `hand->hitpos`;
//! 5. `bgun_tick_gameplay2` — the gun poses; its fx (beam from the muzzle to
//!    `hitpos`, casings, smoke) come back as [`GunEvent`]s.
//!
//! PD's room/portal geometry, prop hit tests and room lighting are substituted by
//! [`Range`]; that is the only non-PD layer here.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use glam::Vec3;

use super::animdata::AnimBank;
use super::bgun::*;
use super::data;
use super::explosions::{self, ExpOut, Explosions, Victim, VictimId};
use super::fx::{self, Beam, Casing, FxBatch, FxKind, Sparks, Wallhit};
use super::gset::*;
use super::model::ModelDef;
use super::player::{PdInput, Player};
use super::nbomb::{NbombOut, Nbombs};
use super::props::{self, ObjCtx, ObjOut, WorldObj};
use super::range::{HitKind, Range};
use super::smoke::{self, Smokes};
use super::xray::{self, Eraser};
use super::font::Canvas;
use super::hud::{HudFonts, HudIn, HudState};

/// A sound the frame wants played (PD sound number, pitch, pan -1..1, volume).
#[derive(Clone, Debug)]
pub struct SoundReq {
    pub id: u16,
    pub speed: f32,
    pub pan: f32,
    pub volume: f32,
    /// `Some(hand)` for the looping per-hand sounds (Reaper spin, Mauler charge).
    pub loop_hand: Option<usize>,
}

/// The range's one room's lighting (`struct room`'s `br_flash` +
/// `br_settled_regional`, `dlights.c`). The settled level is the "room light"
/// slider; explosions flash it up and `lights_tick` lets it back down.
///
/// Substituted: only the PD layer (guns, smoke, bullet holes) sees the flash —
/// the engine draws the range's walls flat-lit.
#[derive(Clone, Copy, Debug)]
pub struct RoomLight {
    pub br_settled_regional: f32,
    pub br_flash: i32,
}

impl RoomLight {
    /// `room_get_final_brightness_for_player` (`dlights.c:106`).
    pub fn final_brightness(&self) -> f32 {
        (self.br_flash as f32 + self.br_settled_regional).clamp(0.0, 255.0)
    }

    /// `room_flash_lighting(room, start, limit)` (`dlights.c:1567`) for the room
    /// itself: its light-transfer value to itself is full, so the increment is
    /// `start` — then `room_flash_local_lighting` (`:1599`).
    pub fn flash(&mut self, start: f32, limit: i32) {
        // value (255 for the room itself) / 255 * start * 5, capped at start.
        let v = start * 5.0;
        let increment = if start > 0.0 { v.min(start) } else { v.max(start) } as i32;
        if increment > 0 {
            if self.br_flash < limit {
                self.br_flash = (self.br_flash + increment).min(limit);
            }
        } else if self.br_flash > limit {
            self.br_flash = (self.br_flash + increment).max(limit);
        }
    }

    /// The flash decay in `lights_tick` (`dlights.c:1411`).
    pub fn tick(&mut self, lv: Lv) {
        if self.br_flash != 0 {
            let mut increment = lv.lvupdate240 * 2;
            if self.br_flash > 0 {
                increment = increment.min(self.br_flash);
                self.br_flash -= increment;
            } else {
                // PD's @bug branch, kept: br_flash is <= 0 here.
                if increment < self.br_flash {
                    increment = self.br_flash;
                }
                self.br_flash += increment;
            }
        }
    }
}

/// `vi_shake` / `vi_handle_retrace` (`lib/vi.c:453`, `:252`): the whole picture
/// jumps up and down by `intensity` half-lines, alternating every retrace.
#[derive(Clone, Copy, Debug)]
pub struct ViShake {
    pub intensity: f32,
    pub timer: i32,
    pub direction: i32,
    /// This frame's vertical offset in half-lines.
    pub offset: f32,
}

impl Default for ViShake {
    fn default() -> Self {
        ViShake { intensity: 0.0, timer: 0, direction: 1, offset: 0.0 }
    }
}

impl ViShake {
    pub fn shake(&mut self, intensity: f32) {
        self.intensity = intensity.clamp(0.0, 14.0);
        self.timer = 10;
    }

    /// One 60 Hz retrace.
    pub fn retrace(&mut self) {
        if self.timer != 0 {
            self.timer -= 1;
            if self.timer == 0 {
                self.intensity = 0.0;
            }
        }
        self.offset = self.direction as f32 * self.intensity;
        self.direction = -self.direction;
    }

    /// The offset as a clip-space y shift: half-lines of the 240-line picture.
    pub fn clip_dy(&self) -> f32 {
        -self.offset / 240.0
    }
}

/// Hand models by `g_HeadsAndBodies[].handfilenum` (`modeldata/robot.c`). The
/// first is Joanna's combat suit (BODY_DARK_COMBAT → FILE_GCOMBATHANDSLOD), the
/// Combat Simulator's default Joanna.
pub const HAND_MODELS: [&str; 9] = [
    "combathandslod",
    "hand_jofrock",
    "hand_jotrench",
    "hand_jopilot",
    "hand_jowetsuit",
    "hand_josnow",
    "hand_joaf1",
    "hand_mrblonde",
    "hand_carrington",
];

/// The loop key the N-Bomb hum plays under (hands use 0 and 1).
pub const NBOMB_HUM_LOOP: usize = 2;

/// `g_Vars.currentplayer->visionmode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisionMode {
    Normal,
    /// Riding a Slayer rocket (fly-by-wire).
    SlayerRocket,
    /// The frame of static when the rocket's signal is lost (`lv.c:1456`).
    SlayerRocketStatic,
    /// The Farsight's x-ray.
    Xray,
}

/// Loop keys for `lv_set_misc_sfx_state` (`lv.c:175`): `g_MiscSfxSounds`.
pub const MISCSFX_LOOP_BASE: usize = 3;
const MISCSFX_BOOSTHEARTBEAT: usize = 0;
const MISCSFX_SLAYERROCKETHUM: usize = 1;
const MISCSFX_SLAYERROCKETBEEP: usize = 2;
const G_MISC_SFX_SOUNDS: [u16; 3] = [0x05c8, 0x8068, 0x01c8];

/// The combat boost: `g_Vars.speedpilltime` / `speedpillwant` /
/// `speedpillon` / `speedpillchange` (`bondgun.c:10325`, `lv.c:1478`).
#[derive(Clone, Copy, Debug, Default)]
pub struct SpeedPill {
    pub time: i32,
    pub want: bool,
    pub on: bool,
    pub change: i32,
}

/// The player chr's cloak: `CHRHFLAG_CLOAKED`, `cloakfadefrac`,
/// `cloakfadefinished`, `cloakpause` (`chr.c:2043`-`:2265`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ChrCloak {
    pub cloaked: bool,
    pub fadefrac: i32,
    pub fadefinished: bool,
    pub pause: i32,
}

impl ChrCloak {
    /// The cloakfade half of `chr_update_cloak` (`chr.c:2209`).
    fn update_fade(&mut self, lv: Lv) {
        if self.cloaked {
            if !self.fadefinished {
                let fadefrac = self.fadefrac + (lv.lvupdate240 * 5) / 8;
                if fadefrac >= 128 {
                    self.fadefinished = true;
                    self.fadefrac = 0;
                } else {
                    self.fadefrac = fadefrac;
                }
            } else {
                self.fadefrac = (self.fadefrac + lv.lvupdate60) % 127;
            }
        } else {
            if self.fadefinished {
                self.fadefinished = false;
                let f = 1.0 - ((self.fadefrac as f32 / 127.0 + self.fadefrac as f32 / 127.0) * std::f32::consts::PI).cos();
                self.fadefrac = (254 - (f * 20.0 * 0.5) as i32) / 2;
            }
            if self.fadefrac > 0 {
                self.fadefrac = (self.fadefrac - (lv.lvupdate240 * 5) / 8).max(0);
            }
        }
    }

    /// `chr_get_cloak_alpha` (`chr.c:2245`): 255 visible; while fully
    /// cloaked a slow shimmer between 1 and 20.
    pub fn alpha(&self) -> i32 {
        let mut alpha = 255;
        if self.fadefrac > 0 || self.fadefinished {
            if !self.fadefinished {
                alpha = 255 - self.fadefrac * 2;
            } else {
                let f = ((self.fadefrac as f32 / 127.0 + self.fadefrac as f32 / 127.0) * std::f32::consts::PI).cos();
                alpha = ((1.0 - f) * 20.0 * 0.5) as i32;
            }
            if alpha == 0 {
                alpha = 1;
            }
        }
        alpha
    }
}

/// `player->slayerrocket` + `badrockettime`.
#[derive(Clone, Copy, Debug)]
pub struct SlayerCam {
    pub rocket: u32,
    pub badrockettime: i32,
}

pub struct Sim {
    pub gset: Arc<Gset>,
    pub bank: Arc<AnimBank>,
    pub models: HashMap<String, Arc<ModelDef>>,
    pub bgun: Bgun,
    pub player: Player,
    pub range: Range,
    pub beams: [Beam; 2],
    pub sparks: Sparks,
    pub wallhits: VecDeque<Wallhit>,
    pub casings: Vec<Casing>,
    pub smokes: Smokes,
    pub explosions: Explosions,
    /// The weapon objects in the world (thrown and fired projectiles, mines,
    /// the Laptop sentry), in prop-list order.
    pub objs: Vec<WorldObj>,
    pub next_obj_id: u32,
    pub nbombs: Nbombs,
    /// `g_PlayersDetonatingMines & 1`: the detonator was pressed this frame.
    pub detonating_mines: bool,
    /// `bondprevpos`: where the player was before this frame's move.
    pub prev_player_pos: Vec3,
    /// This frame's timing, for code that runs outside `frame`'s arguments.
    pub lv_cur: Lv,
    /// `g_20SecIntervalFrac` (`game_006900.c:46`).
    pub interval_frac: f32,
    /// N-Bomb dizziness the player has taken (recorded only).
    pub player_dizzy: f32,
    pub visionmode: VisionMode,
    pub slayer: Option<SlayerCam>,
    /// `player->erasertime`: quarter-ticks since x-ray came on.
    pub erasertime: i32,
    /// The eraser sphere (`bg.c:5253`), set every frame; used in x-ray.
    pub eraser: Eraser,
    pub speedpill: SpeedPill,
    /// This frame's boost wipe: (zoom blur alpha 0..1, blur scale, white fade 0..1).
    pub boost_fx: Option<(f32, f32, f32)>,
    pub cloak: ChrCloak,
    /// `devicesactive & DEVICE_CLOAKRCP120`.
    pub rcp120_cloak: bool,
    /// `hand->mm_rcpremainder`: cloak ammo owed.
    pub rcpremainder: f32,
    /// `g_MiscSfxActiveTypes`: which misc loops are playing.
    misc_sfx: [bool; 3],
    /// `bgun_draw_hud`'s state, its fonts, and this frame's HUD (PD pixels).
    pub hud_state: HudState,
    hud_fonts: Option<Arc<HudFonts>>,
    pub hud: Option<Canvas>,
    /// This frame drew the static (`bview_draw_static` alpha 0..1).
    pub static_alpha: f32,
    /// The trigger was down last frame (for "pressed this frame").
    prev_fire: bool,
    pub room: RoomLight,
    pub vi: ViShake,
    /// Explosion damage the player has taken (PD damage units; the range has no
    /// health to take it from).
    pub player_damage: f32,
    pub sounds: Vec<SoundReq>,
    /// Loops to stop this frame (by hand).
    pub stop_loops: Vec<usize>,
    pub lvframe60: i32,
    pub lvframenum: i32,
    /// Melee: `hand->unk0d0f_02` — resolve a punch against the world next tick.
    pending_melee: [bool; 2],
    /// Shots fired / hits on boards, for the HUD.
    pub shots_fired: u32,
    pub last_hit: Option<(usize, i32)>,
    casing_cooldown240: i32,
    pub aspect: f32,
}

fn anim_id(w: &data::WeaponsFile, name: &str) -> u16 {
    w.anims
        .iter()
        .find(|(_, m)| m.id == name)
        .map(|(k, _)| k.parse().unwrap_or(0))
        .unwrap_or(0)
}

impl Sim {
    pub fn new(hand_model: &str) -> Result<Self, String> {
        let dir = data::assets_dir();
        let w = data::load_weapons(&dir)?;
        let bank = Arc::new(AnimBank::load(&dir.join("anims"), &w.anims)?);
        let gset = Arc::new(Gset::from_file(&w));
        let mut models = HashMap::new();
        let mut stems: Vec<String> = gset.weapons.values().filter_map(|w| w.model.clone()).collect();
        stems.extend(HAND_MODELS.iter().map(|s| s.to_string()));
        stems.extend(fx::CART_MODELS.iter().map(|s| s.to_string()));
        stems.extend([0x0ff, 0x10f, 0x110, 0x112, 0x113, 0x114, 0x115, 0x11f, 0x120, 0x121, 0x122, 0x123, 0x157].iter().filter_map(|&m| props::projectile_model_stem(m)).map(str::to_string));
        stems.sort();
        stems.dedup();
        for s in stems {
            match data::load_model(&dir, &s) {
                Ok(f) => {
                    models.insert(s, Arc::new(ModelDef::from_file(f)));
                }
                Err(e) => log::warn!("pd_guns: model {s}: {e}"),
            }
        }
        let mut bgun = Bgun::new(gset.clone(), bank.clone(), models.clone(), hand_model);
        let range = Range::standard();
        let (pos, theta) = range.spawn();
        let head = (
            anim_id(&w, "ANIM_002B"),
            anim_id(&w, "ANIM_0029"),
            anim_id(&w, "ANIM_TWO_GUN_HOLD"),
        );
        let player = Player::new(bank.clone(), head, pos, theta);
        // A Combat Simulator loadout: every MP gun the spike supports, plenty of
        // ammo (the range is for handling, not scavenging).
        for &wn in &gset.order {
            if wn == WEAPON_UNARMED || gset.weapon(wn).is_none() {
                continue;
            }
            let dual = gset.has_flag(wn, WEAPONFLAG_DUALWIELD);
            bgun.give_weapon(wn, dual);
        }
        bgun.give_weapon(WEAPON_UNARMED, false);
        bgun.p.unlimited_ammo = true;
        bgun.bgun_equip_weapon(WEAPON_FALCON2);
        let mut sim = Sim {
            gset,
            bank,
            models,
            bgun,
            player,
            range,
            beams: [Beam::default(), Beam::default()],
            sparks: Sparks::default(),
            wallhits: VecDeque::new(),
            casings: Vec::new(),
            smokes: Smokes::default(),
            explosions: Explosions::default(),
            objs: Vec::new(),
            next_obj_id: 0,
            nbombs: Nbombs::default(),
            detonating_mines: false,
            prev_player_pos: Vec3::ZERO,
            lv_cur: Lv::step(4, 0, 0),
            interval_frac: 0.0,
            player_dizzy: 0.0,
            visionmode: VisionMode::Normal,
            slayer: None,
            erasertime: 0,
            eraser: Eraser::farsight(Vec3::ZERO),
            speedpill: SpeedPill::default(),
            boost_fx: None,
            cloak: ChrCloak::default(),
            rcp120_cloak: false,
            rcpremainder: 0.0,
            misc_sfx: [false; 3],
            hud_state: HudState::default(),
            hud_fonts: match HudFonts::load() {
                Ok(f) => Some(Arc::new(f)),
                Err(e) => {
                    log::warn!("pd_guns: no HUD fonts ({e})");
                    None
                }
            },
            hud: None,
            static_alpha: 0.0,
            prev_fire: false,
            room: RoomLight { br_settled_regional: 230.0, br_flash: 0 },
            vi: ViShake::default(),
            player_damage: 0.0,
            sounds: Vec::new(),
            stop_loops: Vec::new(),
            lvframe60: 0,
            lvframenum: 0,
            pending_melee: [false; 2],
            shots_fired: 0,
            last_hit: None,
            casing_cooldown240: 0,
            aspect: 16.0 / 9.0,
        };
        sim.update_camera();
        Ok(sim)
    }

    /// Give back every weapon (a range convenience: deployed Laptops and
    /// Dragons leave the inventory, as in PD).
    pub fn restock(&mut self) {
        for &wn in &self.gset.order {
            if wn == WEAPON_UNARMED || self.gset.weapon(wn).is_none() {
                continue;
            }
            let dual = self.gset.has_flag(wn, WEAPONFLAG_DUALWIELD);
            self.bgun.give_weapon(wn, dual);
        }
    }

    /// PD's screen: 320 wide in its own pixels, height from the window aspect,
    /// so the crosshair/spread maths runs in PD's units at any window size.
    fn update_camera(&mut self) {
        let p = &mut self.bgun.p;
        p.aspect = self.aspect;
        p.screen_width = 320.0;
        p.screen_height = 320.0 / self.aspect;
        p.screen_left = 0.0;
        p.screen_top = 0.0;
        p.fovy = self.player.zoominfovy;
        self.bgun.cam_set_scale();
        let (proj, view) = match self.slayer_camera() {
            Some((pos, look, up)) => (super::pdmtx::look_basis(pos, look, up), super::pdmtx::view_matrix(pos, look, up)),
            None => self.player.camera(),
        };
        self.bgun.p.projection = proj;
        self.bgun.p.world_to_screen = view;
    }

    /// `bview_draw_zoom_blur(0xffffffff, xraything, 1.05, 1.05)` in x-ray
    /// (`lv.c:1462`): the last frame, zoomed 5 % about the centre, laid over
    /// this one at 249/255 falling to 99/255 over the first 200 ticks (the
    /// Farsight's smear). Returns (alpha 0..1, sx, sy).
    pub fn xray_zoom_blur(&self) -> Option<(f32, f32, f32)> {
        if self.visionmode != VisionMode::Xray {
            return None;
        }
        let xraything = if self.erasertime < 200 { 249 - ((self.erasertime * 3) >> 2) } else { 99 };
        Some((xraything as f32 / 255.0, 1.05, 1.05))
    }

    /// The Slayer rocket's camera: its position, local +z and +y
    /// (`player.c:3402`: `mtx00016208(sp2b8, &sp2f0)`).
    fn slayer_camera(&self) -> Option<(Vec3, Vec3, Vec3)> {
        if self.visionmode != VisionMode::SlayerRocket {
            return None;
        }
        let s = self.slayer?;
        let o = self.objs.iter().find(|o| o.id == s.rocket && !o.deleting)?;
        let r = o.realrot;
        let n = r.x_axis.length().max(1e-6);
        let r = glam::Mat3::from_cols(r.x_axis / n, r.y_axis / n, r.z_axis / n);
        Some((o.pos, r * Vec3::Z, r * Vec3::Y))
    }

    /// The Slayer branch of `player_tick` (`player.c:3363`): the stick turns
    /// the rocket (pitch about its horizontal right axis, yaw about world up),
    /// A/B/L/R slow it to 1 cm/tick (else 12), Z blows it. PC mapping: mouse
    /// or WASD = stick, fire = Z, aim/use = slow.
    fn slayer_control(&mut self, input: &PdInput, lv: Lv) {
        let fire_pressed = input.fire && !self.prev_fire;
        let Some(mut s) = self.slayer else { return };
        let stickx = if input.mouse_dx != 0.0 { (input.mouse_dx * 2.0).clamp(-80.0, 80.0) } else { (input.walk_x as f32 * 80.0 / 127.0).clamp(-80.0, 80.0) };
        let sticky = if input.mouse_dy != 0.0 { (-input.mouse_dy * 2.0).clamp(-80.0, 80.0) } else { (input.walk_y as f32 * 80.0 / 127.0).clamp(-80.0, 80.0) };
        let slow = input.aim || input.use_held;
        let b = self.range.bounds;
        let Some(o) = self.objs.iter_mut().find(|o| o.id == s.rocket && !o.deleting) else {
            self.slayer = None;
            self.visionmode = VisionMode::SlayerRocketStatic;
            return;
        };
        let inside = o.pos.cmpge(b.min).all() && o.pos.cmple(b.max).all();
        if !inside {
            // Out of bounds: two seconds of this, then the signal is lost.
            s.badrockettime += lv.lvupdate60;
            if s.badrockettime > 120 {
                self.visionmode = VisionMode::SlayerRocketStatic;
            }
        } else if s.badrockettime > 0 {
            s.badrockettime = (s.badrockettime - lv.lvupdate60).max(0);
        }
        let sp2a8 = o.realrot.x_axis.length();
        let sp2b8 = glam::Mat3::from_cols(o.realrot.x_axis / sp2a8, o.realrot.y_axis / sp2a8, o.realrot.z_axis / sp2a8);
        if let Some(p) = o.proj.as_mut() {
            let sp178 = sticky * lv.lvupdate60freal * 0.00025;
            let sp174 = -stickx * lv.lvupdate60freal * 0.00025;
            let right = Vec3::new(sp2b8.x_axis.x, 0.0, sp2b8.x_axis.z).normalize_or_zero();
            // PD's quaternions are (w, x, y, z) with angle/2 = sp178, sp174.
            let q14c = glam::Quat::from_xyzw(right.x * sp178.sin(), 0.0, right.z * sp178.sin(), sp178.cos());
            let yawsin = if sp2b8.y_axis.y >= 0.0 { sp174.sin() } else { -sp174.sin() };
            let q15c = glam::Quat::from_xyzw(0.0, yawsin, 0.0, sp174.cos());
            let q13c = q15c * q14c;
            p.speed = q13c * p.speed;
            p.powerlimit240 = -1;
            p.flags |= props::PROJECTILEFLAG_NOTIMELIMIT;
            p.accel = Vec3::ZERO;
            if p.flags & props::PROJECTILEFLAG_LAUNCHING == 0 {
                p.has_owner = false;
            }
            if fire_pressed {
                // rocket->team = TEAM_00: the union with timer240 — boom.
                o.timer240 = 0;
            }
            let prevspeed = p.speed.length();
            let targetspeed = if slow { 1.0 } else { 12.0 };
            let mut newspeed = prevspeed;
            if prevspeed < targetspeed {
                newspeed = (prevspeed + 0.05 * lv.lvupdate60freal).min(targetspeed);
            } else if prevspeed > targetspeed {
                newspeed = (prevspeed - 0.05 * lv.lvupdate60freal).max(targetspeed);
            }
            if prevspeed > 0.0 {
                p.speed = p.speed * newspeed / prevspeed;
            }
            let rot = glam::Mat3::from_quat(q13c * glam::Quat::from_mat3(&sp2b8));
            o.realrot = rot * sp2a8;
        }
        self.slayer = Some(s);
    }

    pub fn campos(&self) -> Vec3 {
        self.player.pos
    }

    /// One PD frame of `lvupdate240` quarter-ticks (4 = 60 Hz).
    pub fn frame(&mut self, input: &PdInput, lvupdate240: i32) {
        // lv_tick (`lv.c:2061`), slow-motion option off: a live combat boost
        // caps the frame at 4 quarter-ticks. (At 60 Hz that is every frame
        // anyway, so the boost only slows the game at 30 or 20 Hz, exactly as
        // in PD and the PC port, whose LV_SLOMO_TICK_CAP is also 4.)
        let lvupdate240 = if self.speedpill.on { lvupdate240.min(4) } else { lvupdate240 };
        let lv = Lv::step(lvupdate240, self.lvframe60, self.lvframenum);
        self.lvframe60 += lv.lvupdate60;
        self.lvframenum += 1;
        let lv = Lv::step(lvupdate240, self.lvframe60, self.lvframenum);

        self.lv_cur = lv;
        self.bgun_tick_boost(lv);
        self.interval_frac = (self.interval_frac + lv.lvupdate240 as f32 / 4800.0).fract();
        self.prev_player_pos = self.player.pos;
        // One frame of static when the rocket's signal went (`lv.c:1456`).
        self.static_alpha = 0.0;
        if self.visionmode == VisionMode::SlayerRocketStatic {
            self.static_alpha = 1.0;
            self.visionmode = VisionMode::Normal;
        }
        self.update_camera();
        let range = &self.range;
        let resolve = |pos: Vec3, delta: Vec3| range.resolve(pos, delta, 30.0);
        if self.visionmode == VisionMode::SlayerRocket {
            // bmove_tick(0, 0, 0, 1): Jo stands still while the rocket flies.
            self.player.tick(&PdInput::default(), &mut self.bgun, lv, &resolve);
            self.slayer_control(input, lv);
            if let Some(s) = self.slayer {
                self.static_alpha = (s.badrockettime as f32 / 90.0).min(1.0);
            }
        } else {
            self.player.tick(input, &mut self.bgun, lv, &resolve);
        }
        self.prev_fire = input.fire;
        self.update_camera();
        // player_update_shake (`player.c:3012`), then the retraces this frame spans.
        let intensity = self.explosions.update_shake(self.player.pos);
        self.vi.shake(intensity);
        for _ in 0..lv.lvupdate60.max(1) {
            self.vi.retrace();
        }

        // World ticks.
        self.room.tick(lv);
        for h in 0..2 {
            let rng = &mut self.bgun.rng;
            self.beams[h].tick(rng, lv);
        }
        self.sparks.tick(lv);
        self.tick_casings(lv);
        for t in &mut self.range.targets {
            t.flash = (t.flash - lv.lvupdate60freal / 20.0).max(0.0);
        }
        self.props_tick(lv);
        self.chr_update_cloak(lv);

        if lv.lvupdate240 > 0 {
            self.hand_tick_attack(HAND_RIGHT);
            self.hand_tick_attack(HAND_LEFT);
        }

        self.bgun.bgun_tick_gameplay2();
        self.xray_tick(lv);
        self.rcp120_cloak_tick(lv);
        self.process_events(lv);
        self.bg_update_eraser();
        self.lv_update_misc_sfx(lv);
        self.lv_render_boost();
        self.draw_hud(lv);
    }

    /// `bgun_draw_hud` for this frame, into a canvas the size of PD's view.
    fn draw_hud(&mut self, lv: Lv) {
        let Some(fonts) = self.hud_fonts.clone() else { return };
        let p = &self.bgun.p;
        let mut cv = Canvas::new(p.screen_width.round() as usize, p.screen_height.round() as usize);
        let inp = HudIn {
            lvframenum: self.lvframenum,
            lvupdate60: lv.lvupdate60,
            lvupdate240: lv.lvupdate240,
            interval_frac: self.interval_frac,
            speedpilltime: self.speedpill.time,
        };
        self.bgun.bgun_draw_hud(&mut self.hud_state, &fonts, &inp, &mut cv);
        self.hud = Some(cv);
    }

    // ─── combat boost (bondgun.c:10325-10380, lv.c:1478) ─────────────────────

    /// `bgun_add_boost` (`bondgun.c:10325`): up to 5 minutes; Jo's "boost
    /// activate" line if it wasn't already wanted.
    pub fn bgun_add_boost(&mut self, amount: i32) {
        self.speedpill.time = (self.speedpill.time + amount).min(5 * 60 * 60);
        if !self.speedpill.want {
            self.sounds.push(SoundReq { id: 0x05c9, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
        }
        self.speedpill.want = true;
    }

    /// `bgun_subtract_boost` (`bondgun.c:10342`).
    pub fn bgun_subtract_boost(&mut self, amount: i32) {
        self.speedpill.time -= amount;
        if self.speedpill.time <= 0 {
            self.speedpill.time = 0;
            self.speedpill.want = false;
        }
    }

    /// `bgun_apply_boost` / `bgun_revert_boost` (`bondgun.c:10352`, `:10361`)
    /// with the slow-motion option off: ±10 seconds.
    fn bgun_apply_boost(&mut self) {
        self.bgun_add_boost(600);
    }

    fn bgun_revert_boost(&mut self) {
        self.bgun_subtract_boost(600);
    }

    /// `bgun_tick_boost` (`bondgun.c:10370`).
    fn bgun_tick_boost(&mut self, lv: Lv) {
        if self.speedpill.on && self.speedpill.time > 0 {
            self.speedpill.time -= lv.lvupdate60;
            if self.speedpill.time <= 0 {
                self.speedpill.time = 0;
                self.speedpill.want = false;
            }
        }
    }

    /// The boost's wipe in `lv_render` (`lv.c:1478`): `speedpillchange` runs
    /// 0 → 30 (on) or back (off) one step per drawn frame; the first and last
    /// 15 draw a zoom blur and a white fade that peak at the switch-over, where
    /// `speedpillon` flips. Jo groans as a boost starts to wear off.
    fn lv_render_boost(&mut self) {
        let sp = &mut self.speedpill;
        self.boost_fx = None;
        if (sp.change > 0 && sp.change < 30) || (sp.want && !sp.on) || (!sp.want && sp.on) {
            if sp.change == 30 && !sp.want {
                self.sounds.push(SoundReq { id: 0x02ad, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
            }
            let k = if sp.change < 15 { sp.change } else { 30 - sp.change };
            let alpha = (k * 180 / 15) as f32 / 255.0;
            let scale = k as f32 * 0.02 + 1.1;
            let fade = k as f32 * 0.006_666_667;
            self.boost_fx = Some((alpha, scale, fade));
            if sp.want {
                sp.change += 1;
            } else {
                sp.change -= 1;
            }
            sp.change = sp.change.clamp(0, 30);
        }
        sp.on = sp.change > 15;
    }

    /// `lv_update_misc_sfx` (`lv.c:205`): the boost heartbeat and the Slayer
    /// rocket's hum and beep.
    fn lv_update_misc_sfx(&mut self, lv: Lv) {
        if lv.lvupdate240 == 0 {
            for i in 0..3 {
                self.lv_set_misc_sfx_state(i, false);
            }
            return;
        }
        let usingboost = self.speedpill.on;
        self.lv_set_misc_sfx_state(MISCSFX_BOOSTHEARTBEAT, usingboost);
        let usingrocket = self.visionmode == VisionMode::SlayerRocket;
        self.lv_set_misc_sfx_state(MISCSFX_SLAYERROCKETHUM, usingrocket);
        self.lv_set_misc_sfx_state(MISCSFX_SLAYERROCKETBEEP, usingrocket);
    }

    /// `lv_set_misc_sfx_state` (`lv.c:175`): start the loop once, stop it once.
    fn lv_set_misc_sfx_state(&mut self, ty: usize, play: bool) {
        if play == self.misc_sfx[ty] {
            return;
        }
        self.misc_sfx[ty] = play;
        if play {
            self.sounds.push(SoundReq { id: G_MISC_SFX_SOUNDS[ty], speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: Some(MISCSFX_LOOP_BASE + ty) });
        } else {
            self.stop_loops.push(MISCSFX_LOOP_BASE + ty);
        }
    }

    // ─── cloak (chr.c:2043-2265, bondgun.c:8050, prop.c:1367) ────────────────

    /// `chr_cloak` (`chr.c:2043`).
    fn chr_cloak(&mut self) {
        self.cloak.cloaked = true;
        self.sounds.push(SoundReq { id: 0x005b, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
    }

    /// `chr_uncloak` (`chr.c:2054`).
    fn chr_uncloak(&mut self) {
        if self.cloak.cloaked {
            self.cloak.cloaked = false;
            self.sounds.push(SoundReq { id: 0x005c, speed: 1.0, pan: 0.0, volume: 1.0, loop_hand: None });
        }
    }

    /// `chr_uncloak_temporarily` (`chr.c:2082`): shooting drops the cloak for
    /// two seconds.
    pub fn chr_uncloak_temporarily(&mut self) {
        self.chr_uncloak();
        self.cloak.pause = 120;
    }

    /// `chr_update_cloak` (`chr.c:2090`), the player branch; the range has the
    /// RC-P120's cloak but no cloaking device.
    fn chr_update_cloak(&mut self, lv: Lv) {
        if self.cloak.pause > 0 {
            self.cloak.pause -= lv.lvupdate60;
            if self.cloak.pause < 1 {
                self.cloak.pause = 0;
            }
        }
        if self.bgun.ctrl.weaponnum == WEAPON_RCP120 && self.rcp120_cloak {
            if !self.cloak.cloaked && self.cloak.pause < 1 {
                self.chr_cloak();
            }
        } else if self.cloak.cloaked {
            self.chr_uncloak();
        }
        self.cloak.update_fade(lv);
    }

    /// The RC-P120 half of `bgun_tick_gameplay2` (`bondgun.c:8050`): while
    /// the cloak is fully in, it eats 0.4 rounds a tick from the clip, and
    /// turns off once the clip and reserve can't pay; switching away turns it
    /// off.
    fn rcp120_cloak_tick(&mut self, lv: Lv) {
        if self.rcp120_cloak {
            if self.bgun.ctrl.weaponnum == WEAPON_RCP120 {
                if self.cloak.cloaked && self.cloak.fadefinished {
                    self.rcpremainder += lv.lvupdate60freal * 0.4;
                    if self.rcpremainder > 1.0 {
                        let hand = &mut self.bgun.hands[HAND_RIGHT];
                        let usedqty = (self.rcpremainder as i32).min(hand.loadedammo[0]);
                        self.rcpremainder -= usedqty as f32;
                        hand.loadedammo[0] -= usedqty;
                        if hand.loadedammo[0] == 0 && hand.state != HANDSTATE_RELOAD {
                            let stilltogo = self.rcpremainder as i32;
                            let ammotype = self.bgun.ctrl.ammotypes[0];
                            if ammotype >= 0 && stilltogo > self.bgun.bgun_get_ammo_count(ammotype) {
                                self.rcp120_cloak = false;
                            }
                        }
                    }
                }
            } else {
                self.rcp120_cloak = false;
            }
        } else if self.bgun.ctrl.weaponnum == WEAPON_RCP120 && self.rcpremainder > 1.0 {
            let hand = &mut self.bgun.hands[HAND_RIGHT];
            let usedqty = (self.rcpremainder as i32).min(hand.loadedammo[0]);
            self.rcpremainder -= usedqty as f32;
            hand.loadedammo[0] -= usedqty;
        }
    }

    /// The vision-mode half of `bgun_tick_gameplay2` (`bondgun.c:7989`):
    /// aiming the Farsight turns x-ray on (the range has no x-ray scanner
    /// device). `gunsightoff == 0` is "aiming" here: the only sight-off
    /// reason the range can raise is `GUNSIGHTREASON_NOTAIMING`.
    fn xray_tick(&mut self, lv: Lv) {
        let sighton = self.bgun.p.insightaimmode;
        if sighton && self.bgun.hands[HAND_RIGHT].weaponnum == WEAPON_FARSIGHT {
            if self.visionmode != VisionMode::Xray {
                self.erasertime = 0;
            } else {
                self.erasertime += lv.lvupdate240;
            }
            self.visionmode = VisionMode::Xray;
            // ecol_1 16, ecol_2 24, ecol_3 8; epcol 0, 1, 2.
            self.eraser.ecol = [16, 24, 8];
            self.eraser.epcol = [0, 1, 2];
        } else if !matches!(self.visionmode, VisionMode::SlayerRocket | VisionMode::SlayerRocketStatic) {
            self.visionmode = VisionMode::Normal;
        }
    }

    /// `bg_tick`'s eraser (`bg.c:5253`): 5 m ahead of the eye, pushed out by
    /// the zoom while the Farsight is aimed; the stage's prop/BG radii.
    fn bg_update_eraser(&mut self) {
        let p = &self.bgun.p;
        let depth = if self.bgun.bgun_get_weapon_num(HAND_RIGHT) == WEAPON_FARSIGHT && p.insightaimmode {
            -500.0 / p.c_lodscalez
        } else {
            -500.0
        };
        self.eraser.pos = p.projection.transform_point3(Vec3::new(0.0, 0.0, depth));
        self.eraser.propdist = xray::STAGE_ERASERPROPDIST;
        self.eraser.bgdist = xray::STAGE_ERASERPROPDIST + xray::STAGE_UNK30;
    }

    /// The eraser when x-ray is on.
    pub fn xray(&self) -> Option<&Eraser> {
        (self.visionmode == VisionMode::Xray).then_some(&self.eraser)
    }

    /// This frame's timing.
    pub fn lv(&self) -> Lv {
        self.lv_cur
    }

    /// `props_tick` for the prop types the guns spawn: N-Bomb storms (ticked
    /// from `lv_tick` just before), the weapon objects, explosions, smoke.
    fn props_tick(&mut self, lv: Lv) {
        let mut nout = NbombOut::default();
        self.nbombs.tick(lv, self.player.pos, &mut nout);
        self.apply_nbomb_out(nout);
        self.objs_tick(lv);
        let mut victims: Vec<Victim> = self
            .range
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| Victim { id: VictimId::Board(i), pos: (t.bbox.min + t.bbox.max) * 0.5, is_chr: false })
            .collect();
        victims.push(Victim { id: VictimId::Player, pos: self.player.pos, is_chr: true });
        let mut out = ExpOut::default();
        let brightness = self.room.final_brightness();
        let rng = &mut self.bgun.rng;
        self.explosions.tick(rng, lv, &mut self.smokes, &victims, brightness, &|_| None, &mut out);
        self.apply_explosion_out(out);
        let muzzles = [self.bgun.hands[0].muzzlepos, self.bgun.hands[1].muzzlepos];
        let rng = &mut self.bgun.rng;
        self.smokes.tick(rng, lv, muzzles, &|_| None);
    }

    /// `obj_tick_player` for each weapon object: free if flagged, then
    /// `projectile_tick`, `autogun_tick`, `weapon_tick` (`propobj.c:11054`).
    fn objs_tick(&mut self, lv: Lv) {
        let mut objs = std::mem::take(&mut self.objs);
        objs.retain(|o| !o.deleting);
        let mut out = ObjOut::default();
        let campos = self.campos();
        {
            let mut c = ObjCtx {
                rng: &mut self.bgun.rng,
                lv,
                gset: &self.gset,
                world: &self.range,
                smokes: &mut self.smokes,
                explosions: &mut self.explosions,
                campos,
                lodscalez: self.bgun.p.c_lodscalez,
                playerpos: self.player.pos,
                detonating: self.detonating_mines,
                brightness: self.room.final_brightness(),
                out: &mut out,
            };
            let mut aout = super::autogun::AutogunOut::default();
            for o in objs.iter_mut() {
                if o.deleting {
                    continue;
                }
                props::projectile_tick(o, &mut c);
                match o.ty {
                    props::ObjType::Weapon => props::weapon_tick(o, &mut c),
                    props::ObjType::Autogun => {
                        let pos = o.pos;
                        if let Some(a) = o.autogun.as_mut() {
                            a.tick(pos, c.world, lv);
                            a.beam.tick(c.rng, lv);
                        }
                        super::autogun::autogun_tick_shoot(o, c.world, lv, c.rng, &mut aout);
                    }
                }
            }
            for (id, pos) in aout.sounds {
                c.out.sounds.push((id, pos, 1.0));
            }
            c.out.bg_hit_sounds.extend(aout.bg_hit_sounds);
            for (pos, ty) in aout.sparks {
                c.out.sparks.push((pos, Vec3::ZERO, Vec3::ZERO, ty));
            }
            for (b, _) in aout.board_hits {
                c.out.board_hits.push((b, 0.0));
            }
        }
        // g_PlayersDetonatingMines is cleared once the mines have seen it.
        self.detonating_mines = false;
        // A Slayer rocket that blew: the signal is lost (`weapon_tick`).
        if let Some(s) = self.slayer {
            if objs.iter().all(|o| o.id != s.rocket || o.deleting) {
                self.slayer = None;
                self.visionmode = VisionMode::SlayerRocketStatic;
            }
        }
        // New objects made while ticking (none yet) go after the old ones.
        objs.append(&mut self.objs);
        self.objs = objs;
        self.apply_obj_out(out);
        for f in std::mem::take(&mut self.explosions.flashes) {
            self.room.flash(f, 255);
        }
    }

    fn apply_obj_out(&mut self, out: ObjOut) {
        for (id, pos, speed) in out.sounds {
            let (pan, volume) = (self.pan_of(pos), self.ps_vol(id, pos));
            self.sounds.push(SoundReq { id, speed, pan, volume, loop_hand: None });
        }
        for (w, pos) in out.bg_hit_sounds {
            self.play_bg_hit_sound(w, pos);
        }
        for (pos, dir, normal, ty) in out.sparks {
            self.sparks.create(&mut self.bgun.rng, pos, dir, normal, ty);
        }
        for (b, dmg) in out.board_hits {
            if let Some(t) = self.range.targets.get_mut(b) {
                t.hits += 1;
                t.damage += dmg;
                t.flash = 1.0;
            }
        }
        for pos in out.nbombs {
            let mut nout = NbombOut::default();
            self.nbombs.create_storm(pos, &mut nout);
            self.apply_nbomb_out(nout);
        }
    }

    fn apply_nbomb_out(&mut self, out: NbombOut) {
        if out.darken {
            self.room.flash(-38.0, -180);
        }
        self.player_dizzy += out.dizzy;
        for _ in 0..out.roars {
            self.sounds.push(SoundReq { id: 0x0001, speed: 0.4, pan: 0.0, volume: 1.0, loop_hand: None });
        }
        if out.hum_start {
            self.sounds.push(SoundReq { id: 0x810c, speed: 0.4, pan: 0.0, volume: 1.0, loop_hand: Some(NBOMB_HUM_LOOP) });
        }
        if out.hum_stop {
            self.stop_loops.push(NBOMB_HUM_LOOP);
        }
    }

    fn apply_explosion_out(&mut self, out: ExpOut) {
        for (id, pos) in out.sounds {
            if id != 0 {
                let (pan, volume) = (self.pan_of(pos), self.ps_vol(id, pos));
                self.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
            }
        }
        for wh in out.wallhits {
            self.push_wallhit(wh);
        }
        for (victim, dmg, _dir, _first) in out.damage {
            match victim {
                // obj_damage_by_explosion on a target board.
                VictimId::Board(i) => {
                    if let Some(t) = self.range.targets.get_mut(i) {
                        t.damage += dmg;
                        if dmg > 0.0 {
                            t.flash = 1.0;
                        }
                    }
                }
                // chr_damage_by_explosion on the player: recorded only (the
                // range has no health, death or knockback).
                VictimId::Player => self.player_damage += dmg,
                VictimId::Prop(_) => {}
            }
        }
        for f in out.flashes {
            self.room.flash(f, 255);
        }
        for f in std::mem::take(&mut self.explosions.flashes) {
            self.room.flash(f, 255);
        }
    }

    /// `explosion_create_simple` from the gun code, with the room flash it
    /// requests applied.
    pub fn explosion_create_simple(&mut self, pos: Vec3, ty: usize) -> bool {
        let campos = self.campos();
        let lod = self.bgun.p.c_lodscalez;
        let rng = &mut self.bgun.rng;
        let ok = self.explosions.create_simple(rng, &mut self.smokes, &self.range, None, pos, ty, 0, campos, lod);
        for f in std::mem::take(&mut self.explosions.flashes) {
            self.room.flash(f, 255);
        }
        ok
    }

    fn tick_casings(&mut self, lv: Lv) {
        if self.casing_cooldown240 > 0 {
            self.casing_cooldown240 = (self.casing_cooldown240 - lv.lvupdate240).max(0);
        }
        let mut landed = 0;
        self.casings.retain_mut(|c| {
            if c.tick(lv) {
                landed += 1;
                false
            } else {
                true
            }
        });
        for _ in 0..landed {
            // casing_tick: SFXMAP_8051 at 0.98..1.23, at most one per 20 ticks.
            if self.casing_cooldown240 == 0 && lv.lvupdate240 > 0 {
                self.casing_cooldown240 = 20;
                let speed = self.bgun.rng.randomfrac() * 0.25 + 0.98;
                self.sounds.push(SoundReq { id: 0x8051, speed, pan: 0.0, volume: 1.0, loop_hand: None });
            }
        }
    }

    /// `hand_tick_attack` (`prop.c:1285`).
    fn hand_tick_attack(&mut self, h: usize) {
        if self.pending_melee[h] {
            let doit = !(self.bgun.bgun_get_weapon_num(h) == WEAPON_REAPER && self.bgun.hands[h].burstbullets % 3 != 1);
            if doit {
                self.shot_calculate_hits(h, true, true);
            }
            self.pending_melee[h] = false;
        }
        if !self.bgun.hands[h].firing {
            return;
        }
        let weaponnum = self.bgun.bgun_get_weapon_num(h);
        self.bgun.hands[h].activatesecondary = false;
        match self.bgun.hands[h].attacktype {
            HANDATTACKTYPE_SHOOT => {
                if h == HAND_RIGHT || !self.bgun.hands[HAND_RIGHT].firing {
                    self.chr_uncloak_temporarily();
                    self.shots_fired += 1;
                    if weaponnum == WEAPON_SHOTGUN {
                        for _ in 0..6 {
                            self.shot_create(h, true, 1);
                        }
                    } else {
                        let n = self.bgun.hands[h].shotstotake;
                        self.shot_create(h, true, n);
                    }
                }
            }
            HANDATTACKTYPE_MELEE | HANDATTACKTYPE_MELEENOUNCLOAK => {
                // hand_inflict_melee_damage: no chr in range, so the punch is
                // resolved against the world on the next tick (unk0d0f_02).
                if self.bgun.hands[h].attacktype == HANDATTACKTYPE_MELEE {
                    self.chr_uncloak_temporarily();
                    self.pending_melee[h] = true;
                }
            }
            HANDATTACKTYPE_THROWPROJECTILE => {
                let wf = self.bgun.hands[h].weaponfunc;
                self.bgun_create_thrown_projectile(h, weaponnum, wf);
            }
            HANDATTACKTYPE_DETONATE => self.player_activate_remote_mine_detonator(),
            HANDATTACKTYPE_BOOST => self.bgun_apply_boost(),
            HANDATTACKTYPE_REVERTBOOST => self.bgun_revert_boost(),
            HANDATTACKTYPE_SHOOTPROJECTILE => self.bgun_create_fired_projectile(h),
            // bwalk_adjust_crouch_pos(±2): the sniper rifle's crouch.
            HANDATTACKTYPE_CROUCH => {
                let p = &mut self.bgun.p;
                let d = if p.crouchpos == CROUCHPOS_SQUAT { 2 } else { -2 };
                p.crouchpos = (p.crouchpos + d).clamp(CROUCHPOS_SQUAT, CROUCHPOS_STAND);
            }
            HANDATTACKTYPE_RCP120CLOAK => self.rcp120_cloak = !self.rcp120_cloak,
            _ => {}
        }
    }

    /// `shot_create` (`prop.c:998`).
    fn shot_create(&mut self, h: usize, dorandom: bool, numshots: i32) {
        if numshots <= 0 {
            // PD still computes the spread (consuming the RNG) but casts nothing.
            let _ = self.bgun.bgun_calculate_player_shot_spread(h, dorandom);
            return;
        }
        self.shot_calculate_hits_dir(h, dorandom, false);
    }

    fn shot_calculate_hits(&mut self, h: usize, dorandom: bool, _melee_context: bool) {
        self.shot_calculate_hits_dir(h, dorandom, true);
    }

    /// `shot_calculate_hits` (`prop.c:570`) against the range.
    fn shot_calculate_hits_dir(&mut self, h: usize, dorandom: bool, from_aim: bool) {
        let dir2d = self.bgun.bgun_calculate_player_shot_spread(h, dorandom);
        let mut gunpos2d = Vec3::ZERO;
        if from_aim && self.bgun.bgun_get_weapon_num(HAND_RIGHT) == WEAPON_REAPER {
            gunpos2d.y -= 15.0 * self.bgun.rng.randomfrac();
        }
        let proj = self.bgun.p.projection;
        let gunpos3d = proj.transform_point3(gunpos2d);
        let gundir3d = proj.transform_vector3(dir2d).normalize_or_zero();
        let func = self.bgun.func_of(h);
        let weaponnum = self.bgun.bgun_get_weapon_num(h);
        let ismelee = func.as_ref().is_some_and(|f| f.kind() == INVENTORYFUNCTYPE_MELEE);
        let laserstream = weaponnum == WEAPON_LASER && self.bgun.hands[h].weaponfunc == FUNC_SECONDARY;
        let range = if laserstream {
            300.0
        } else if ismelee {
            func.as_ref().map_or(200.0, |f| if f.range > 0.0 { f.range } else { 200.0 })
        } else {
            65536.0
        };
        let penetration = func.as_ref().and_then(|f| f.shoot.as_ref()).map_or(1, |s| s.penetration.max(1));
        let damage = func.as_ref().and_then(|f| f.shoot.as_ref()).map_or(0.0, |s| s.damage);

        if ismelee {
            // The melee branch: sparks + a thud if anything is within reach.
            match self.range.raycast(gunpos3d, gundir3d, range) {
                Some(hit) => {
                    self.play_melee_hit_sound(weaponnum);
                    if weaponnum != WEAPON_UNARMED && weaponnum != WEAPON_TRANQUILIZER {
                        self.sparks.create(&mut self.bgun.rng, hit.pos, gundir3d, hit.normal, fx::SPARKTYPE_DEFAULT);
                    }
                }
                None => self.play_melee_miss_sound(weaponnum),
            }
            return;
        }

        // The Farsight's round ignores the BG for props (`prop.c:724`: a BG hit
        // doesn't shorten the shot) and in x-ray doesn't test the BG at all
        // (`prop.c:688`); outside x-ray the wall it crosses still takes the hit
        // effects, unless a board stopped the round first.
        if weaponnum == WEAPON_FARSIGHT {
            let hitbg = if self.visionmode == VisionMode::Xray {
                None
            } else {
                self.range.raycast_world(gunpos3d, gundir3d, range)
            };
            let mut origin = gunpos3d;
            let mut travelled = 0.0;
            let mut through = 0;
            let mut hitpos = hitbg.map_or(gunpos3d + gundir3d * range, |h| h.pos);
            let mut blockedbyprop = false;
            while let Some(hit) = self.range.raycast_targets(origin, gundir3d, range - travelled) {
                let HitKind::Target(i) = hit.kind else { break };
                self.board_hit(i, damage, hit.pos, hit.normal, gunpos3d);
                through += 1;
                if through >= penetration {
                    blockedbyprop = true;
                    hitpos = hit.pos;
                    break;
                }
                travelled += hit.dist + 0.5;
                origin = hit.pos + gundir3d * 0.5;
            }
            match hitbg {
                Some(bg) if !blockedbyprop => self.bg_hit(h, weaponnum, gunpos3d, gundir3d, bg.pos, bg.normal),
                _ => self.set_hit_pos(hitpos),
            }
            return;
        }

        // Walk the ray: boards slow the bullet (penetration), the world stops it.
        let mut origin = gunpos3d;
        let mut travelled = 0.0;
        let mut through = 0;
        loop {
            let Some(hit) = self.range.raycast(origin, gundir3d, range - travelled) else {
                self.set_hit_pos(gunpos3d + gundir3d * range);
                return;
            };
            match hit.kind {
                HitKind::Target(i) => {
                    self.board_hit(i, damage, hit.pos, hit.normal, gunpos3d);
                    through += 1;
                    if through >= penetration {
                        self.set_hit_pos(hit.pos);
                        // doexplosiveshells: the round that stops in a prop blows.
                        if self.explosive_shells(h) {
                            self.explosion_create_simple(hit.pos, explosions::EXPLOSIONTYPE_PHOENIX);
                        }
                        return;
                    }
                    travelled += hit.dist + 0.5;
                    origin = hit.pos + gundir3d * 0.5;
                }
                HitKind::World => {
                    self.bg_hit(h, weaponnum, gunpos3d, gundir3d, hit.pos, hit.normal);
                    return;
                }
            }
        }
    }

    /// `obj_hit` on a board: the hit counts, the prop takes a bullet hole and
    /// its hit sound.
    fn board_hit(&mut self, i: usize, damage: f32, pos: Vec3, normal: Vec3, gunpos3d: Vec3) {
        let t = &mut self.range.targets[i];
        t.hits += 1;
        t.damage += damage;
        t.flash = 1.0;
        self.last_hit = Some((i, self.lvframe60));
        let tex = if self.bgun.rng.random() % 2 == 0 { fx::WALLHITTEX_BULLET1 } else { fx::WALLHITTEX_BULLET2 };
        // Stand-in: the board's face art sits a few mm proud of its box, so the
        // hole goes on top of it.
        let b = self.room.final_brightness();
        let wh = fx::wallhit_create(&mut self.bgun.rng, pos + normal * 0.6, normal, gunpos3d, tex, b);
        self.push_wallhit(wh);
        let id = if self.bgun.rng.random() % 2 == 0 { 0x8089 } else { 0x808a };
        self.sounds.push(SoundReq { id, speed: 1.0, pan: self.pan_of(pos), volume: 1.0, loop_hand: None });
    }

    /// The `hitbg && !blockedbyprop` branch of `shot_calculate_hits`: hitpos,
    /// bullet hole, ricochet + surface sounds, sparks. The range is all
    /// `g_SurfaceTypeDefault` (stone sounds, BULLET2 holes).
    fn bg_hit(&mut self, h: usize, weaponnum: i32, gunpos: Vec3, dir: Vec3, pos: Vec3, normal: Vec3) {
        self.set_hit_pos(pos);
        // `texnum`: the wallhit made (0 for the guns that leave no hole — PD
        // then holds lights_handle_hit's result, 0 in a room with no lights).
        let mut texnum = 0;
        if !matches!(weaponnum, WEAPON_UNARMED | WEAPON_LASER | WEAPON_TRANQUILIZER | WEAPON_FARSIGHT) {
            let b = self.room.final_brightness();
            let wh = fx::wallhit_create(&mut self.bgun.rng, pos, normal, gunpos, fx::WALLHITTEX_BULLET2, b);
            texnum = wh.texnum;
            self.push_wallhit(wh);
        }
        self.play_bg_hit_sound(weaponnum, pos);
        if self.explosive_shells(h) {
            self.explosion_create_simple(pos, explosions::EXPLOSIONTYPE_PHOENIX);
        } else if texnum != 0 {
            // One player: the bullet hole's own little flame + puff.
            self.explosion_create_simple(pos, explosions::EXPLOSIONTYPE_BULLETHOLE);
        }
        let sparktype = match weaponnum {
            WEAPON_FARSIGHT => fx::SPARKTYPE_BGHIT_ORANGE,
            WEAPON_CYCLONE => fx::SPARKTYPE_ELECTRICAL,
            WEAPON_MAULER | WEAPON_PHOENIX | WEAPON_CALLISTO | WEAPON_REAPER => fx::SPARKTYPE_BGHIT_GREEN,
            WEAPON_TRANQUILIZER => fx::SPARKTYPE_BGHIT_TRANQULIZER,
            _ => fx::SPARKTYPE_DEFAULT,
        };
        self.sparks.create(&mut self.bgun.rng, pos, dir, normal, sparktype);
    }

    /// `gset_has_function_flags(&gset, FUNCFLAG_EXPLOSIVESHELLS)` — the Phoenix's
    /// secondary.
    fn explosive_shells(&self, h: usize) -> bool {
        self.bgun.func_of(h).is_some_and(|f| f.flags & FUNCFLAG_EXPLOSIVESHELLS != 0)
    }

    fn push_wallhit(&mut self, wh: Wallhit) {
        if self.wallhits.len() >= fx::MAX_WALLHITS {
            self.wallhits.pop_front();
        }
        self.wallhits.push_back(wh);
    }

    /// `bgun_set_hit_pos` (`bondgun.c:9259`): both hands share it.
    fn set_hit_pos(&mut self, pos: Vec3) {
        self.bgun.hands[0].hitpos = pos;
        self.bgun.hands[1].hitpos = pos;
    }

    /// Stereo pan of a world position relative to the camera.
    pub(crate) fn pan_of(&self, pos: Vec3) -> f32 {
        let v = self.bgun.p.world_to_screen.transform_point3(pos);
        let d = v.length();
        if d < 1.0 {
            0.0
        } else {
            (v.x / d).clamp(-1.0, 1.0)
        }
    }

    /// Volume for `ps_apply_vol_pan(…, 400, 2500, 3000, …)` and for a
    /// `ps_create` with default distances.
    pub(crate) fn vol_of(&self, pos: Vec3) -> f32 {
        ps_calculate_volume_from_distance((pos - self.player.pos).length(), [400.0, 2500.0, 3000.0])
    }

    /// Volume for `ps_create(…, soundnum, …)` (`propsnd.c:764`): a sound with an
    /// audio config (every SFXMAP) takes the config's distances instead of the
    /// call's (explosions carry out to 25–55 m).
    pub(crate) fn ps_vol(&self, id: u16, pos: Vec3) -> f32 {
        let dist = audio_config_dists().get(&id).copied().unwrap_or([400.0, 2500.0, 3000.0]);
        ps_calculate_volume_from_distance((pos - self.player.pos).length(), dist)
    }

    /// `bgun_play_bg_hit_sound` (`bondgun.c:8741`, NTSC 1.0+ path): a ricochet
    /// from the shared table, then the surface's own hit sound.
    fn play_bg_hit_sound(&mut self, weaponnum: i32, pos: Vec3) {
        const RICOCHETS: [u16; 32] = [
            0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x17, 0x18, 0x19, 0x1a, 0x17, 0x18, 0x19, 0x1a, 0x1f, 0x20,
            0x20, 0x21, 0x1f, 0x20, 0x20, 0x21, 0x1f, 0x20, 0x20, 0x21, 0x23, 0x24, 0x25, 0x26,
        ];
        // The table continues 0x27..0x2a (36 entries in total).
        const RICOCHETS_TAIL: [u16; 4] = [0x27, 0x28, 0x29, 0x2a];
        let rand1 = self.bgun.rng.random();
        let rand2 = self.bgun.rng.random();
        let pan = self.pan_of(pos);
        let volume = self.vol_of(pos);
        match weaponnum {
            WEAPON_LASER => {
                let id = if rand1 % 2 == 0 { 0x5b } else { 0x5c };
                self.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
                return;
            }
            // Knives and bolts make a metal sound.
            WEAPON_COMBATKNIFE | props::WEAPON_BOLT => {
                self.sounds.push(SoundReq { id: 0x8079, speed: 1.0, pan, volume, loop_hand: None });
                return;
            }
            // Mine landing/activation sound (SFXMAP_80AA).
            WEAPON_REMOTEMINE | WEAPON_PROXIMITYMINE | WEAPON_TIMEDMINE => {
                self.sounds.push(SoundReq { id: 0x80aa, speed: 1.0, pan, volume, loop_hand: None });
                return;
            }
            _ => {
                let i = (rand1 % 36) as usize;
                let id = if i < 32 { RICOCHETS[i] } else { RICOCHETS_TAIL[i - 32] };
                self.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
            }
        }
        // g_SurfaceTypeDefault: SFXMAP_8087 / 8088 (stone).
        let id = if rand2 % 2 == 0 { 0x8087 } else { 0x8088 };
        self.sounds.push(SoundReq { id, speed: 1.0, pan, volume, loop_hand: None });
    }

    /// `weapon_play_melee_hit_sound` (`prop.c:504`).
    fn play_melee_hit_sound(&mut self, weaponnum: i32) {
        let (id, speed) = match weaponnum {
            WEAPON_UNARMED => {
                let id = if self.bgun.rng.random() % 2 == 1 { 0x8094 } else { 0x808f };
                (id, 1.0 - self.bgun.rng.randomfrac() * 0.1)
            }
            WEAPON_TRANQUILIZER => (0x04fb, 2.78),
            _ => (0x8079, 1.0 - self.bgun.rng.randomfrac() * 0.1),
        };
        self.sounds.push(SoundReq { id, speed, pan: 0.0, volume: 1.0, loop_hand: None });
    }

    /// `weapon_play_melee_miss_sound` (`prop.c:453`).
    fn play_melee_miss_sound(&mut self, weaponnum: i32) {
        let (id, speed) = match weaponnum {
            WEAPON_TRANQUILIZER => (0x04fb, 2.78),
            WEAPON_REAPER => return,
            WEAPON_COMBATKNIFE => {
                let id = if self.bgun.rng.random() % 2 == 1 { 0x8060 } else { 0x8061 };
                (id, 1.05 - self.bgun.rng.randomfrac() * 0.2)
            }
            _ => (0x0069, 1.0 - self.bgun.rng.randomfrac() * 0.2),
        };
        self.sounds.push(SoundReq { id, speed, pan: 0.0, volume: 1.0, loop_hand: None });
    }

    /// Turn the gun code's side effects into sounds and world effects.
    fn process_events(&mut self, lv: Lv) {
        let events = std::mem::take(&mut self.bgun.events);
        for e in events {
            match e {
                GunEvent::Sound { id, speed } => {
                    let loop_hand = match id {
                        0x805e | 0x8065 => Some(0),
                        _ => None,
                    };
                    self.sounds.push(SoundReq { id, speed, pan: 0.0, volume: 1.0, loop_hand });
                }
                GunEvent::StopLoop { hand } => self.stop_loops.push(hand),
                GunEvent::FreeHeldRocket { hand } => self.bgun_free_held_rocket(hand),
                GunEvent::UpdateRocketLauncher { hand } => self.bgun_update_rocket_launcher(hand),
                GunEvent::Beam { hand } => self.beam_create_for_hand(hand),
                GunEvent::UncloakTemporarily => self.chr_uncloak_temporarily(),
                GunEvent::Casing { hand, mtx, casing } => {
                    let weaponnum = self.bgun.bgun_get_weapon_num(hand);
                    let hd = &self.bgun.hands[hand];
                    let handvel = if lv.lvupdate240 > 0 {
                        (hd.posmtx.w_axis - hd.prevmtx.w_axis).truncate() / lv.lvupdate60freal
                    } else {
                        Vec3::ZERO
                    };
                    let ground = self.player.manground;
                    if let Some(c) = fx::casing_create_for_hand(&mut self.bgun.rng, weaponnum, casing, ground, &mtx, handvel) {
                        if self.casings.len() >= 20 {
                            self.casings.remove(0);
                        }
                        self.casings.push(c);
                    }
                }
                GunEvent::Smoke { hand, pos, kind } => {
                    // smoke_create_for_hand (`bondgun.c:6631`): createsmoke stays
                    // set until a smoke is actually made.
                    let ty = match kind {
                        1 => smoke::SMOKETYPE_MUZZLE_PISTOL,
                        2 => smoke::SMOKETYPE_MUZZLE_REAPER,
                        3 => smoke::SMOKETYPE_MUZZLE_SHOTGUN,
                        _ => smoke::SMOKETYPE_MUZZLE_AUTOMATIC,
                    };
                    if self.smokes.smoke_create_for_hand(pos, ty, hand) {
                        self.bgun.hands[hand].createsmoke = false;
                    }
                }
            }
        }
    }

    /// `beam_create_for_hand` (`gunfx.c:104`).
    fn beam_create_for_hand(&mut self, h: usize) {
        let hand = &self.bgun.hands[h];
        let v = self.bgun.p.world_to_screen.transform_point3(hand.hitpos);
        if -v.z < hand.muzzlez {
            return;
        }
        let mut weaponnum = self.bgun.bgun_get_weapon_num(h);
        if weaponnum == WEAPON_LASER && hand.weaponfunc == FUNC_SECONDARY {
            weaponnum = -2;
        }
        let (from, to) = (hand.muzzlepos, hand.hitpos);
        self.beams[h].create(&mut self.bgun.rng, weaponnum, from, to);
        if self.beams[h].weaponnum == WEAPON_MAULER {
            // mm_lasertype: the charge level.
            let lasertype = self.bgun.hands[h].matmot1 as i32;
            self.beams[h].weaponnum = -3 - lasertype.clamp(0, 5);
        }
    }

    // ─── what the renderer reads ──────────────────────────────────────────────

    /// World-space effect batches drawn in the depth-tested world pass.
    pub fn world_fx(&self) -> Vec<FxBatch> {
        let mut out = Vec::new();
        let xray = self.xray();
        if let Some(e) = xray {
            // bg_render_scene_in_xray: the BG, then the props in their x-ray
            // colours; no wallhits (`wallhit.c:1332`).
            out.push(xray::bg_geometry(&self.range, e));
            let mut boards = Vec::new();
            for t in &self.range.targets {
                if let Some(col) = e.obj_colour((t.bbox.min + t.bbox.max) * 0.5) {
                    xray::board_geometry(&t.bbox, self.campos(), col, &mut boards);
                }
            }
            out.push(FxBatch { kind: FxKind::Xray, verts: boards });
        }
        // Target boards: a white face, flashing red on a hit, on a dark frame.
        let mut flat = Vec::new();
        for t in self.range.targets.iter().filter(|_| xray.is_none()) {
            let f = t.flash;
            let face = [1.0, 1.0 - 0.6 * f, 1.0 - 0.6 * f, 1.0];
            let back = [0.25, 0.22, 0.2, 1.0];
            box_tris(&mut flat, t.bbox.min, t.bbox.max, back);
            let c = t.face - Vec3::Z * 0.2;
            let (hx, hy) = (t.half.x * 0.85, t.half.y * 0.85);
            quad(&mut flat, [c + Vec3::new(-hx, -hy, 0.0), c + Vec3::new(hx, -hy, 0.0), c + Vec3::new(hx, hy, 0.0), c + Vec3::new(-hx, hy, 0.0)], face);
            // Bullseye rings.
            for (r, col) in [(0.55, [0.1, 0.1, 0.1, 1.0]), (0.35, face), (0.18, [0.8, 0.1, 0.1, 1.0])] {
                let (rx, ry) = (hx * r * 1.4, hx * r * 1.4);
                let cc = c - Vec3::Z * 0.1 * (2.0 - r);
                disc(&mut flat, cc, rx.min(hx), ry.min(hy), col);
            }
        }
        out.push(FxBatch { kind: FxKind::Flat, verts: flat });
        let mut by_tex: HashMap<u16, Vec<fx::FxVert>> = HashMap::new();
        for wh in self.wallhits.iter().filter(|_| xray.is_none()) {
            let tex = fx::WALLHIT_TEX[wh.texnum].0;
            wh.tris(by_tex.entry(tex).or_default());
        }
        for (tex, verts) in by_tex {
            out.push(FxBatch { kind: FxKind::Wallhit(tex), verts });
        }
        // The xlu props (smoke, explosions), back to front by `prop->z`
        // (`smoke_tick_player` / `explosion_tick_player`).
        let proj = self.bgun.p.projection;
        let (right, up) = (proj.x_axis.truncate(), proj.y_axis.truncate());
        let w2s = self.bgun.p.world_to_screen;
        let propz = |pos: Vec3| {
            let z = -w2s.transform_point3(pos).z;
            if z < 100.0 {
                z * 0.5
            } else {
                z - 100.0
            }
        };
        let mut xlu: Vec<(f32, Vec<FxBatch>)> = Vec::new();
        for (pos, b) in self.smokes.geometry(self.campos(), right, up, self.room.final_brightness(), xray) {
            xlu.push((propz(pos), vec![b]));
        }
        for (pos, bs) in self.explosions.geometry(right, up, xray) {
            xlu.push((propz(pos), bs));
        }
        for (pos, b) in self.nbombs.geometry(self.interval_frac) {
            xlu.push((propz(pos), vec![b]));
        }
        xlu.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, bs) in xlu {
            out.extend(bs);
        }
        // Deployed sentries: their tracer and, on a firing tick, the flash.
        for o in &self.objs {
            if let Some(a) = &o.autogun {
                if let Some(b) = a.beam.geometry(self.campos()) {
                    out.push(b);
                }
                if a.fireleft {
                    let mats = o.init_matrices();
                    let seed = (self.lvframenum as u32) ^ o.id.wrapping_mul(7919);
                    if let Some(b) = super::autogun::gunfire_geometry(&o.def, &mats, super::autogun::MODELPART_AUTOGUN_FLASHLEFT, self.campos(), seed) {
                        out.push(b);
                    }
                }
            }
        }
        if let Some(s) = self.sparks.geometry(self.campos(), self.player.look, self.bgun.p.fovy, xray) {
            out.push(s);
        }
        out
    }

    /// The weapon objects to draw in the world pass: (model name, world-space
    /// joint matrices) — `obj_render` → `model_render` of each prop.
    /// The weapon objects to draw: model, joint matrices, and in x-ray the
    /// flat colour + alpha `obj_render` gives them (hidden past the eraser).
    pub fn world_models(&self) -> Vec<(String, Vec<glam::Mat4>, Option<[f32; 4]>)> {
        let mut out = Vec::new();
        let own_rocket = if self.visionmode == VisionMode::SlayerRocket { self.slayer.map(|s| s.rocket) } else { None };
        let xray = self.xray();
        for o in &self.objs {
            if o.deleting || o.heldrocket || Some(o.id) == own_rocket {
                continue;
            }
            let tint = match xray {
                Some(e) => match e.obj_colour(o.pos) {
                    Some(c) => Some(c),
                    None => continue,
                },
                None => None,
            };
            out.push((o.def.name.clone(), o.init_matrices(), tint));
        }
        out
    }

    /// Screen overlays drawn after the guns: the N-Bomb storm when inside one
    /// (`nbomb_render_overlay`), as a quad 2 cm in front of the eye covering
    /// the view.
    pub fn overlay_fx(&self) -> Vec<FxBatch> {
        let mut out = Vec::new();
        if let Some((alpha, st)) = self.nbombs.overlay(self.campos(), self.interval_frac) {
            let p = &self.bgun.p;
            let z = 2.0;
            let hh = (p.fovy.to_radians() * 0.5).tan() * z * 1.05;
            let hw = hh * p.aspect;
            let proj = p.projection;
            let at = |x: f32, y: f32| proj.transform_point3(Vec3::new(x, y, -z));
            let col = [0.0, 0.0, 0.0, alpha];
            // s spans 5 texels across the view, t 30 down (`nbomb.c:870`).
            let v = [
                fx::FxVert { pos: at(-hw, hh), st: [st[0], st[1]], col },
                fx::FxVert { pos: at(hw, hh), st: [st[0] + 5.0, st[1]], col },
                fx::FxVert { pos: at(hw, -hh), st: [st[0] + 5.0, st[1] + 30.0], col },
                fx::FxVert { pos: at(-hw, -hh), st: [st[0], st[1] + 30.0], col },
            ];
            out.push(FxBatch { kind: FxKind::Nbomb, verts: vec![v[0], v[1], v[2], v[2], v[3], v[0]] });
        }
        out
    }

    /// Beams, drawn in the gun pass (`bgun_render` renders them first, in the
    /// gun's depth space).
    pub fn gun_fx(&self) -> Vec<FxBatch> {
        let mut out = Vec::new();
        // bgun_render returns before the beams in x-ray (`bondgun.c:8191`).
        if self.visionmode == VisionMode::Xray {
            return out;
        }
        for h in 0..2 {
            if self.bgun.hands[h].visible {
                if let Some(b) = self.beams[h].geometry(self.campos()) {
                    out.push(b);
                }
            }
        }
        out
    }
}

fn quad(out: &mut Vec<fx::FxVert>, p: [Vec3; 4], col: [f32; 4]) {
    let v = |i: usize| fx::FxVert { pos: p[i], st: [0.0, 0.0], col };
    out.extend_from_slice(&[v(0), v(1), v(2), v(0), v(2), v(3)]);
}

fn disc(out: &mut Vec<fx::FxVert>, c: Vec3, rx: f32, ry: f32, col: [f32; 4]) {
    let n = 20;
    for i in 0..n {
        let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / n as f32 * std::f32::consts::TAU;
        let p0 = c + Vec3::new(a0.cos() * rx, a0.sin() * ry, 0.0);
        let p1 = c + Vec3::new(a1.cos() * rx, a1.sin() * ry, 0.0);
        let v = |p: Vec3| fx::FxVert { pos: p, st: [0.0, 0.0], col };
        out.extend_from_slice(&[v(c), v(p1), v(p0)]);
    }
}

fn box_tris(out: &mut Vec<fx::FxVert>, mn: Vec3, mx: Vec3, col: [f32; 4]) {
    let c = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
    let faces = [
        [c(mn.x, mn.y, mn.z), c(mn.x, mx.y, mn.z), c(mx.x, mx.y, mn.z), c(mx.x, mn.y, mn.z)],
        [c(mn.x, mn.y, mx.z), c(mx.x, mn.y, mx.z), c(mx.x, mx.y, mx.z), c(mn.x, mx.y, mx.z)],
        [c(mn.x, mn.y, mn.z), c(mn.x, mn.y, mx.z), c(mn.x, mx.y, mx.z), c(mn.x, mx.y, mn.z)],
        [c(mx.x, mn.y, mn.z), c(mx.x, mx.y, mn.z), c(mx.x, mx.y, mx.z), c(mx.x, mn.y, mx.z)],
        [c(mn.x, mx.y, mn.z), c(mn.x, mx.y, mx.z), c(mx.x, mx.y, mx.z), c(mx.x, mx.y, mn.z)],
        [c(mn.x, mn.y, mn.z), c(mx.x, mn.y, mn.z), c(mx.x, mn.y, mx.z), c(mn.x, mn.y, mx.z)],
    ];
    for f in faces {
        quad(out, f, col);
    }
}

/// `ps_calculate_volume_from_distance` (`propsnd.c:80`): full inside dist1,
/// a square-root fade to 1000/32767 at dist2, linear to silence at dist3;
/// below 40/32767 is silence.
pub(crate) fn ps_calculate_volume_from_distance(playerdist: f32, dist: [f32; 3]) -> f32 {
    const FULL: f32 = 32767.0;
    let (d1, d2, d3) = (dist[0].min(5501.0), dist[1].min(5801.0), dist[2].min(6000.0));
    let mut result = 0.0;
    if playerdist < dist[2] {
        result = if playerdist < d1 {
            FULL
        } else if playerdist < d2 {
            FULL - (((playerdist - d1) / (d2 - d1)).sqrt() * (FULL - 1000.0)).trunc()
        } else {
            ((d3 - playerdist) * 1000.0 / (d3 - d2)).trunc()
        };
    }
    let result = result.min(FULL);
    if result < 40.0 {
        0.0
    } else {
        result / FULL
    }
}

/// `g_AudioConfigs[].dist1..3` per sound id, from the sound pack's manifest
/// (every SFXMAP entry carries its config).
fn audio_config_dists() -> &'static HashMap<u16, [f32; 3]> {
    static DISTS: std::sync::OnceLock<HashMap<u16, [f32; 3]>> = std::sync::OnceLock::new();
    DISTS.get_or_init(|| {
        let mut out = HashMap::new();
        let path = format!("{}/../../assets/audio/pd/sfx/sfx_manifest.json", env!("CARGO_MANIFEST_DIR"));
        let Ok(text) = std::fs::read_to_string(path) else { return out };
        let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
        for (k, e) in obj {
            let Some(rest) = k.strip_prefix("SFXMAP_") else { continue };
            let Ok(id) = u16::from_str_radix(&rest[..4.min(rest.len())], 16) else { continue };
            let Some(d) = e.pointer("/config/dist").and_then(|d| d.as_array()) else { continue };
            let v: Vec<f32> = d.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
            if v.len() == 3 {
                out.insert(id, [v[0], v[1], v[2]]);
            }
        }
        out
    })
}
