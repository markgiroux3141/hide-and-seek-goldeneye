//! The firing-range window: mouse/keyboard → [`PdInput`], fixed-rate PD frames,
//! the world through the engine renderer, the guns through [`PdRenderer`] in the
//! engine's render hook, PD's sight and ammo readout, and a debug panel (F1).
//!
//! Controls follow the PC port's defaults where it has them:
//! WASD move · mouse look · LMB fire · RMB (hold) aim · R reload ·
//! E / MMB hold = B (hold to switch gun function) · wheel / Q = cycle weapons
//! (while aiming a zooming gun, wheel / arrow up-down = C-up / C-down zoom) ·
//! 1–0 = pick weapon · Ctrl = crouch down, Space = crouch up · Esc frees the mouse.
//!
//! A USB N64 pad plays PD's own control style 1.1 (see [`N64State`]): stick
//! walk + turn (aimed: crosshair), Z fire, R/L aim, A tap next gun / A+Z
//! previous, B tap reload / hold gun function, C-left/right strafe, C-up/down
//! look (aimed: crouch, lean on C-left/right, zoom on the sniper/Farsight),
//! Start toggles the F1 panel.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use engine::audio::AudioManager;
use engine::platform::gamepad::{Gamepads, PadAxis};
use engine::geometry::csg_runtime::Region;
use engine::render::renderer::{EguiFrame, Renderer};

use super::bgun::*;
use super::font::{split, Canvas};
use super::gset::*;
use super::n64video::{self, Mask, N64Video, Preset, Resolution, Signal, VideoSettings};
use super::player::PdInput;
use super::render::PdRenderer;
use super::sim::{Sim, SoundReq, HAND_MODELS};

/// PD's frame pacing: `lvupdate240` quarter-ticks per frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rate {
    Hz60,
    Hz30,
    Hz20,
}

impl Rate {
    fn lvupdate240(self) -> i32 {
        match self {
            Rate::Hz60 => 4,
            Rate::Hz30 => 8,
            Rate::Hz20 => 12,
        }
    }
    fn seconds(self) -> f32 {
        self.lvupdate240() as f32 / 240.0
    }
    fn label(self) -> &'static str {
        match self {
            Rate::Hz60 => "60 Hz (PC port)",
            Rate::Hz30 => "30 Hz",
            Rate::Hz20 => "20 Hz (N64-ish)",
        }
    }
}

struct Sfx {
    file: String,
    volume: f32,
}

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pd: Option<PdRenderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    audio: Option<AudioManager>,
    sfx: HashMap<u16, Sfx>,
    loops: HashMap<usize, u64>,

    sim: Sim,
    hand_model: usize,
    rate: Rate,
    acc: f32,
    last: Instant,
    fps: f32,

    keys: HashSet<KeyCode>,
    buttons: HashSet<MouseButton>,
    mouse: Vec2,
    wheel: i32,
    /// Sim frames of held manual zoom left from wheel notches (+ in, − out).
    zoom_hold: i32,
    pending_select: Option<(i32, bool)>,
    pending_reload: bool,
    crouch_down: bool,
    crouch_up: bool,
    captured: bool,
    show_panel: bool,
    always_sight: bool,
    /// Pad aiming: stick up moves the crosshair down (flight-style). Walking
    /// is never inverted.
    invert_pad_aim: bool,
    grid: bool,
    /// The HUD canvas's texture.
    hud_tex: Option<egui::TextureHandle>,
    /// N64 video + CRT (the VIDEO section of the panel).
    video: VideoSettings,
    n64v: Option<N64Video>,
    /// The left panel's width in physical pixels (0 when hidden): the tube
    /// is centred in the rest of the window.
    panel_px: f32,
    pads: Option<Gamepads>,
    n64: N64State,
    prev_start: bool,
}

// The USB-N64 adapter's raw button codes, as verified on the user's pad for the
// main game (`crate::gamepad`): gilrs's semantic names mis-map this adapter, so
// everything is read by raw code.
const CODE_C_LEFT: u32 = 0;
const CODE_B: u32 = 1;
const CODE_A: u32 = 2;
const CODE_C_DOWN: u32 = 3;
const CODE_L: u32 = 4;
const CODE_R: u32 = 5;
const CODE_Z: u32 = 6;
const CODE_C_RIGHT: u32 = 8;
const CODE_C_UP: u32 = 9;
const CODE_START: u32 = 12;

/// One poll of the N64 pad: the stick in N64 units (±80, +y up) after a radial
/// deadzone, and the buttons.
#[derive(Clone, Copy, Debug, Default)]
struct N64State {
    stick_x: i32,
    stick_y: i32,
    z: bool,
    aim: bool,
    a: bool,
    b: bool,
    c_up: bool,
    c_down: bool,
    c_left: bool,
    c_right: bool,
    start: bool,
}

impl N64State {
    fn read(p: &Gamepads) -> Self {
        if !p.connected() {
            return N64State::default();
        }
        let (mut x, mut y) = (p.axis(PadAxis::LeftStickX), p.axis(PadAxis::LeftStickY));
        let mag = (x * x + y * y).sqrt();
        if mag < crate::world::STICK_DEADZONE {
            x = 0.0;
            y = 0.0;
        } else {
            let scale = ((mag - crate::world::STICK_DEADZONE) / (1.0 - crate::world::STICK_DEADZONE)).min(1.0) / mag;
            x *= scale;
            y *= scale;
        }
        N64State {
            stick_x: (x * 80.0).round() as i32,
            stick_y: (y * 80.0).round() as i32,
            z: p.pressed_raw(CODE_Z),
            aim: p.pressed_raw(CODE_R) || p.pressed_raw(CODE_L),
            a: p.pressed_raw(CODE_A),
            b: p.pressed_raw(CODE_B),
            c_up: p.pressed_raw(CODE_C_UP),
            c_down: p.pressed_raw(CODE_C_DOWN),
            c_left: p.pressed_raw(CODE_C_LEFT),
            c_right: p.pressed_raw(CODE_C_RIGHT),
            start: p.pressed_raw(CODE_START),
        }
    }

    /// The pad is being used (so it owns look/walk this frame; an idle pad
    /// leaves the keyboard and mouse alone).
    fn active(&self) -> bool {
        self.stick_x != 0
            || self.stick_y != 0
            || self.z
            || self.aim
            || self.a
            || self.b
            || self.c_up
            || self.c_down
            || self.c_left
            || self.c_right
    }
}

/// Fold this frame's pad state into the keyboard/mouse input (only when the
/// pad is in use, so an idle pad leaves the mouse alone). Aim is merged first:
/// the up/down inversion depends on it.
fn merge_pad(inp: &mut PdInput, n: N64State, invert_aim: bool) {
    if !n.active() {
        return;
    }
    inp.pad = true;
    inp.aim |= n.aim;
    inp.fire |= n.z;
    inp.use_held |= n.b;
    inp.a_held = n.a;
    inp.look_x = n.stick_x;
    // Aimed, the stick's y is the crosshair (and the edge pitch); walking is
    // never inverted.
    inp.look_y = if inp.aim && invert_aim { -n.stick_y } else { n.stick_y };
    inp.c_up = n.c_up;
    inp.c_down = n.c_down;
    inp.c_left = n.c_left;
    inp.c_right = n.c_right;
}

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,engine=info,game=info")).try_init();
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("run app");
}

fn load_sfx() -> HashMap<u16, Sfx> {
    let path = format!("{}/../../assets/audio/pd/sfx/sfx_manifest.json", env!("CARGO_MANIFEST_DIR"));
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        log::warn!("pd_range: no sound manifest at {path}");
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
    let Some(obj) = v.as_object() else { return out };
    for (k, e) in obj {
        // "0066" (SFXNUM) or "SFXMAP_804D_…".
        let id = if let Some(rest) = k.strip_prefix("SFXMAP_") {
            u16::from_str_radix(&rest[..4.min(rest.len())], 16).ok()
        } else {
            u16::from_str_radix(k, 16).ok()
        };
        let (Some(id), Some(file)) = (id, e.get("file").and_then(|f| f.as_str())) else { continue };
        let volume = e.get("volume").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        out.insert(id, Sfx { file: format!("pd/sfx/{file}"), volume });
    }
    out
}

impl App {
    fn new() -> Self {
        let sim = Sim::new(HAND_MODELS[0]).unwrap_or_else(|e| panic!("pd_range: {e} — export with tools/pd-assets/pd_fpgun.py all"));
        App {
            window: None,
            renderer: None,
            pd: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            audio: AudioManager::new(),
            sfx: load_sfx(),
            loops: HashMap::new(),
            sim,
            hand_model: 0,
            rate: Rate::Hz60,
            acc: 0.0,
            last: Instant::now(),
            fps: 0.0,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            mouse: Vec2::ZERO,
            wheel: 0,
            zoom_hold: 0,
            pending_select: None,
            pending_reload: false,
            crouch_down: false,
            crouch_up: false,
            captured: false,
            show_panel: true,
            always_sight: false,
            invert_pad_aim: true,
            grid: false,
            hud_tex: None,
            video: VideoSettings::default(),
            n64v: None,
            panel_px: 0.0,
            pads: Gamepads::new(),
            n64: N64State::default(),
            prev_start: false,
        }
    }

    fn set_captured(&mut self, on: bool) {
        let Some(w) = self.window.as_ref() else { return };
        if on {
            let ok = w.set_cursor_grab(CursorGrabMode::Locked).or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined)).is_ok();
            w.set_cursor_visible(false);
            self.captured = ok;
        } else {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor_visible(true);
            self.captured = false;
        }
    }

    fn upload_range(&mut self) {
        let mut region = Region::new(0);
        region.brushes = self.sim.range.brushes();
        let (_, tex) = region.evaluate_both(&[]);
        if let Some(r) = self.renderer.as_mut() {
            r.set_region_textured(0, &tex);
        }
    }

    fn input(&mut self, first: bool) -> PdInput {
        let k = |c: KeyCode| self.keys.contains(&c);
        let walk_y = (k(KeyCode::KeyW) as i32 - k(KeyCode::KeyS) as i32) * 127;
        let walk_x = (k(KeyCode::KeyD) as i32 - k(KeyCode::KeyA) as i32) * 127;
        let mut inp = PdInput {
            walk_x,
            walk_y,
            fire: self.captured && self.buttons.contains(&MouseButton::Left),
            aim: self.captured && self.buttons.contains(&MouseButton::Right),
            use_held: k(KeyCode::KeyE) || self.buttons.contains(&MouseButton::Middle),
            ..PdInput::default()
        };
        merge_pad(&mut inp, self.n64, self.invert_pad_aim);
        inp.zoom_in = k(KeyCode::ArrowUp) || self.zoom_hold > 0;
        inp.zoom_out = k(KeyCode::ArrowDown) || self.zoom_hold < 0;
        self.zoom_hold -= self.zoom_hold.signum();
        if first {
            if self.captured {
                inp.mouse_dx = self.mouse.x;
                inp.mouse_dy = self.mouse.y;
            }
            self.mouse = Vec2::ZERO;
            inp.reload = std::mem::take(&mut self.pending_reload);
            inp.cycle_next = self.wheel < 0;
            inp.cycle_prev = self.wheel > 0;
            self.wheel = 0;
            inp.select = self.pending_select.take();
            inp.crouch_down = std::mem::take(&mut self.crouch_down);
            inp.crouch_up = std::mem::take(&mut self.crouch_up);
        }
        inp
    }

    fn step(&mut self, dt: f32) {
        if let Some(p) = self.pads.as_mut() {
            p.poll();
            self.n64 = N64State::read(p);
            if self.n64.start && !self.prev_start {
                self.show_panel = !self.show_panel;
            }
            self.prev_start = self.n64.start;
        }
        self.acc += dt;
        let frame_dt = self.rate.seconds();
        let mut n = 0;
        while self.acc >= frame_dt && n < 6 {
            let inp = self.input(n == 0);
            self.sim.frame(&inp, self.rate.lvupdate240());
            self.acc -= frame_dt;
            n += 1;
            self.flush_sounds();
        }
        if n == 6 {
            self.acc = 0.0;
        }
    }

    fn flush_sounds(&mut self) {
        let reqs: Vec<SoundReq> = std::mem::take(&mut self.sim.sounds);
        let stops: Vec<usize> = std::mem::take(&mut self.sim.stop_loops);
        let Some(audio) = self.audio.as_mut() else { return };
        for h in stops {
            if let Some(id) = self.loops.remove(&h) {
                audio.stop_voice(id);
            }
        }
        for r in reqs {
            let Some(s) = self.sfx.get(&r.id) else {
                log::debug!("pd_range: no sample for sound {:#06x}", r.id);
                continue;
            };
            if r.volume <= 0.0 {
                continue;
            }
            let looping = r.loop_hand.is_some();
            if let Some(id) = audio.play_voice(&s.file, s.volume * r.volume, r.speed as f64, r.pan, looping) {
                if let Some(h) = r.loop_hand {
                    if let Some(old) = self.loops.insert(h, id) {
                        audio.stop_voice(old);
                    }
                }
            }
        }
    }

    /// The engine's world view-projection (metres), with `vi_shake` applied.
    fn world_vp(&self, aspect: f32) -> Mat4 {
        world_vp(&self.sim, aspect)
    }

    fn ui(&mut self) -> Option<EguiFrame> {
        let window = self.window.as_ref()?.clone();
        let state = self.egui_state.as_mut()?;
        let raw = state.take_egui_input(&window);
        let mut select: Option<(i32, bool)> = None;
        let mut hand_model = self.hand_model;
        let mut rate = self.rate;
        let mut show_panel = self.show_panel;
        let mut always_sight = self.always_sight;
        let mut invert_pad_aim = self.invert_pad_aim;
        let mut grid = self.grid;
        let mut brightness = self.sim.room.br_settled_regional;
        let mut sens = self.sim.player.mouse_sens;
        let mut reset_targets = false;
        let mut restock = false;
        let sim = &self.sim;
        let captured = self.captured;
        let fps = self.fps;
        let pad_connected = self.pads.as_ref().is_some_and(|p| p.connected());
        let mut hud_tex = self.hud_tex.take();
        let mut video = self.video;
        let mut panel_px = 0.0f32;

        let out = self.egui_ctx.run(raw, |ctx| {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new("pdhud")));
            let p = &sim.bgun.p;
            let sx = screen.width() / p.screen_width;
            let sy = screen.height() / p.screen_height;
            let to_px = |x: f32, y: f32| egui::pos2(screen.left() + x * sx, screen.top() + y * sy);

            // sight_draw_default (sight.c:615): only while R is held (or with
            // "always show target"); red when a board is under the crosshair.
            // N64 video draws the sight and the HUD into the frame itself
            // (`App::n64_hud`), so they go through the VI and the tube too.
            let sighton = (p.insightaimmode || always_sight) && !video.n64;
            if sighton && sim.bgun.hands[HAND_RIGHT].weaponnum != WEAPON_UNARMED {
                let (x, y) = (p.crosspos[0], p.crosspos[1]);
                let dir = sim.bgun.cam_screen_dir([x, y], 1.0);
                let world_dir = p.projection.transform_vector3(dir);
                let on_target = matches!(
                    sim.range.raycast(sim.player.pos, world_dir, 65536.0).map(|h| h.kind),
                    Some(super::range::HitKind::Target(_))
                );
                // gset_get_sight (gset.c:586): the Farsight has SIGHT_MAIAN; the
                // range draws the default sight for everything else.
                if sim.bgun.hands[HAND_RIGHT].weaponnum == WEAPON_FARSIGHT {
                    sight_draw_maian(&painter, &to_px, p.screen_width, p.screen_height, x, y, on_target, sx);
                } else {
                    sight_draw_default(&painter, &to_px, p.screen_width, p.screen_height, x, y, on_target, sx);
                }
            }

            // PD's gun HUD (bgun_draw_hud), drawn by the sim in PD pixels and
            // scaled up without filtering.
            if let Some(cv) = sim.hud.as_ref().filter(|_| !video.n64) {
                let img = egui::ColorImage::from_rgba_premultiplied([cv.w, cv.h], &cv.rgba8());
                let tex = match hud_tex.take() {
                    Some(mut t) => {
                        t.set(img, egui::TextureOptions::NEAREST);
                        t
                    }
                    None => ctx.load_texture("pdhud", img, egui::TextureOptions::NEAREST),
                };
                painter.image(tex.id(), screen, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                hud_tex = Some(tex);
            }
            if !captured {
                painter.text(
                    screen.center() + egui::vec2(0.0, 60.0),
                    egui::Align2::CENTER_CENTER,
                    "click to capture the mouse  ·  Esc to release",
                    egui::FontId::proportional(18.0),
                    egui::Color32::from_white_alpha(200),
                );
            }

            if show_panel {
                let panel = egui::SidePanel::left("pdguns").resizable(false).default_width(250.0).show(ctx, |ui| {
                    egui::ScrollArea::vertical().id_salt("pdpanel").show(ui, |ui| {
                    ui.heading("PD RANGE");
                    ui.label(egui::RichText::new("Perfect Dark's guns, ported").weak());
                    ui.label(format!("{fps:.0} fps"));
                    ui.label(if pad_connected { "N64 pad connected" } else { "no pad" });
                    ui.separator();
                    egui::ComboBox::from_label("rate").selected_text(rate.label()).show_ui(ui, |ui| {
                        for r in [Rate::Hz60, Rate::Hz30, Rate::Hz20] {
                            ui.selectable_value(&mut rate, r, r.label());
                        }
                    });
                    egui::ComboBox::from_label("hands").selected_text(HAND_MODELS[hand_model]).show_ui(ui, |ui| {
                        for (i, n) in HAND_MODELS.iter().enumerate() {
                            ui.selectable_value(&mut hand_model, i, *n);
                        }
                    });
                    ui.add(egui::Slider::new(&mut brightness, 60.0..=255.0).text("room light"));
                    ui.add(egui::Slider::new(&mut sens, 0.5..=6.0).text("mouse"));
                    ui.checkbox(&mut always_sight, "always show target");
                    ui.checkbox(&mut invert_pad_aim, "invert pad aim (up/down)");
                    ui.checkbox(&mut grid, "grid walls");
                    ui.separator();
                    video_panel(ui, &mut video);
                    ui.horizontal(|ui| {
                        if ui.button("clear targets").clicked() {
                            reset_targets = true;
                        }
                        // Deploying the Laptop or the Dragon's self-destruct takes
                        // it out of the inventory (inv_remove_item_by_num).
                        if ui.button("restock weapons").clicked() {
                            restock = true;
                        }
                    });
                    ui.separator();
                    ui.label("Weapons");
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        for &(w, dual) in &sim.bgun.p.inventory {
                            let Some(def) = sim.gset.weapon(w) else { continue };
                            ui.horizontal(|ui| {
                                if ui.selectable_label(sim.bgun.bgun_get_weapon_num(HAND_RIGHT) == w, &def.name).clicked() {
                                    select = Some((w, false));
                                }
                                if dual && ui.small_button("×2").clicked() {
                                    select = Some((w, true));
                                }
                            });
                        }
                    });
                    ui.separator();
                    let h = &sim.bgun.hands[HAND_RIGHT];
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(k).monospace().weak());
                            ui.label(egui::RichText::new(v).monospace());
                        });
                    };
                    row(ui, "state", format!("{} / {}", h.state, h.stateminor));
                    row(ui, "func", format!("{}", if h.weaponfunc == 0 { "primary" } else { "secondary" }));
                    row(ui, "anim", format!("{} f{:.1}", h.anim.animnum, h.anim.frame));
                    row(ui, "crouch", format!("{}", sim.bgun.p.crouchpos));
                    row(ui, "fov", format!("{:.1}", sim.player.zoominfovy));
                    row(ui, "shots", format!("{}", sim.shots_fired));
                    let hits: u32 = sim.range.targets.iter().map(|t| t.hits).sum();
                    row(ui, "board hits", format!("{hits}"));
                    row(ui, "smoke/exp", format!("{} / {}", sim.smokes.live(), sim.explosions.live()));
                    row(ui, "blast dmg", format!("{:.2} (you)", sim.player_damage));
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "WASD move · mouse look · LMB fire\nRMB hold aim (W/S crouch, A/D lean)\nR reload · E/MMB hold: gun function\nwheel/Q cycle · 1–0 pick · Ctrl/Space crouch\nF1 panel · Esc free mouse

N64 pad (PD 1.1): stick walk/turn · Z fire
R/L aim: stick = crosshair, C-up/dn crouch
(zoom on sniper/Farsight), C-lt/rt lean
A tap next gun · A+Z previous
B tap reload · B hold gun function
C-lt/rt strafe · C-up/dn look · Start panel",
                        )
                        .small()
                        .weak(),
                    );
                    });
                });
                panel_px = panel.response.rect.right() * ctx.pixels_per_point();
            }
        });
        state.handle_platform_output(&window, out.platform_output);
        let paint_jobs = self.egui_ctx.tessellate(out.shapes, out.pixels_per_point);

        if let Some(s) = select {
            self.pending_select = Some(s);
        }
        self.rate = rate;
        self.show_panel = show_panel;
        self.always_sight = always_sight;
        self.invert_pad_aim = invert_pad_aim;
        self.hud_tex = hud_tex;
        self.video = video;
        self.panel_px = panel_px;
        self.sim.player.mouse_sens = sens;
        self.sim.room.br_settled_regional = brightness;
        if grid != self.grid {
            self.grid = grid;
            if let Some(r) = self.renderer.as_mut() {
                r.set_grid_mode(grid);
            }
        }
        if restock {
            self.sim.restock();
        }
        if reset_targets {
            for t in &mut self.sim.range.targets {
                t.hits = 0;
                t.damage = 0.0;
            }
            self.sim.wallhits.clear();
        }
        if hand_model != self.hand_model {
            self.hand_model = hand_model;
            self.sim.bgun.hand_model = HAND_MODELS[hand_model].to_string();
            // Re-instantiate the models on the next load by re-equipping.
            let w = self.sim.bgun.bgun_get_weapon_num(HAND_RIGHT);
            let def = self.sim.models.get(HAND_MODELS[hand_model]).cloned();
            for h in 0..2 {
                if self.sim.bgun.hands[h].handmodel.is_some() {
                    self.sim.bgun.hands[h].handmodel = def.clone().map(super::model::Model::new);
                }
            }
            let _ = w;
        }
        Some(EguiFrame { textures_delta: out.textures_delta, paint_jobs, pixels_per_point: out.pixels_per_point })
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;
        if dt > 0.0 {
            self.fps = self.fps * 0.95 + (1.0 / dt) * 0.05;
        }
        let n64 = self.video.n64;
        if let Some(r) = self.renderer.as_mut() {
            // N64 video: the world renders at PD's resolution (320×220 is
            // square pixels; the picture keeps that 320:220 shape at 4:3 with
            // the VI's black bars in every mode), whatever the window.
            r.set_scene_size(n64.then_some(self.video.resolution.size()));
            let three_point = n64 && self.video.three_point;
            if self.pd.as_ref().is_some_and(|p| p.three_point != three_point) {
                r.set_three_point_filter(three_point);
            }
            if let Some(pd) = self.pd.as_mut() {
                pd.three_point = three_point;
            }
            self.sim.aspect = if n64 { n64video::N64_W as f32 / n64video::N64_H as f32 } else { r.aspect() };
        }
        self.step(dt);
        let egui_frame = self.ui();
        let hud = if n64 { self.n64_hud() } else { None };
        let (Some(renderer), Some(pd), Some(nv)) = (self.renderer.as_mut(), self.pd.as_mut(), self.n64v.as_mut()) else { return };
        let aspect = self.sim.aspect;
        let vp = world_vp(&self.sim, aspect);
        let sim = &self.sim;
        let video = self.video;
        let panel_px = self.panel_px;
        renderer.render_with_hook(vp, egui_frame, &mut |h| {
            let n64 = n64 && h.depth_texture.is_some();
            pd.world_depth_copy = match (n64 && video.aa, h.depth_texture) {
                (true, Some(src)) => Some((src.clone(), nv.world_depth(h.device, h.width, h.height))),
                _ => None,
            };
            pd.draw(h.device, h.queue, h.encoder, h.color, h.depth, aspect, vp, sim);
            let fx = PdRenderer::post_fx(sim);
            pd.post(h.device, h.queue, h.encoder, h.color_texture, h.color, h.width, h.height, fx);
            if n64 {
                nv.upload_hud(h.device, h.queue, hud.as_ref());
                let rect = n64video::tube_rect(h.present_width, h.present_height, panel_px);
                nv.run(h.device, h.queue, h.encoder, h.color, h.depth, (h.width, h.height), h.present, rect, &video);
            }
        });
        let _ = self.world_vp(aspect);
    }

    /// N64 video's HUD layer (see [`n64_hud_canvas`]).
    fn n64_hud(&self) -> Option<Canvas> {
        Some(n64_hud_canvas(&self.sim, self.always_sight))
    }

    fn weapon_by_index(&self, i: usize) -> Option<(i32, bool)> {
        self.sim.bgun.p.inventory.get(i).map(|&(w, _)| (w, false))
    }
}

/// The engine world's view-projection in metres, shifted by `vi_shake`.
pub fn world_vp(sim: &Sim, aspect: f32) -> Mat4 {
    let p = &sim.bgun.p;
    let view_m = Mat4::from_scale(Vec3::splat(0.01)) * p.world_to_screen * Mat4::from_scale(Vec3::splat(100.0));
    let shake = Mat4::from_translation(Vec3::new(0.0, sim.vi.clip_dy(), 0.0));
    shake * Mat4::perspective_rh(p.fovy.to_radians(), aspect, 0.05, 300.0) * view_m
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("PD RANGE — Perfect Dark guns spike")
            .with_inner_size(winit::dpi::LogicalSize::new(1600.0, 900.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()));
        renderer.set_grid_mode(self.grid);
        renderer.set_crosshair_offset(None);
        renderer.set_lighting(&[], ([1.0, 1.0, 1.0], 1.0), false);
        let (device, queue, cf, df) = renderer.gpu();
        let mut pd = PdRenderer::new(device, queue, cf, df);
        pd.load_models(device, queue, &self.sim);
        self.n64v = Some(N64Video::new(device, cf));
        self.egui_state = Some(egui_winit::State::new(self.egui_ctx.clone(), egui::ViewportId::ROOT, &*window, None, None, None));
        self.renderer = Some(renderer);
        self.pd = Some(pd);
        self.window = Some(window);
        self.upload_range();
        self.last = Instant::now();
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.captured {
                self.mouse += Vec2::new(delta.0 as f32, delta.1 as f32);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let consumed = if self.captured {
            false
        } else {
            match (self.egui_state.as_mut(), self.window.as_ref()) {
                (Some(s), Some(w)) => s.on_window_event(w, &event).consumed,
                _ => false,
            }
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::Focused(false) => {
                self.keys.clear();
                self.buttons.clear();
                self.set_captured(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                match event.state {
                    ElementState::Pressed => {
                        let fresh = self.keys.insert(code);
                        if !fresh || (consumed && !self.captured) {
                            return;
                        }
                        match code {
                            KeyCode::Escape => self.set_captured(false),
                            KeyCode::F1 => self.show_panel = !self.show_panel,
                            KeyCode::KeyR => self.pending_reload = true,
                            KeyCode::KeyQ => self.wheel = 1,
                            KeyCode::ControlLeft | KeyCode::ControlRight | KeyCode::KeyC => self.crouch_down = true,
                            KeyCode::Space => self.crouch_up = true,
                            KeyCode::Digit1 => self.pending_select = self.weapon_by_index(0),
                            KeyCode::Digit2 => self.pending_select = self.weapon_by_index(1),
                            KeyCode::Digit3 => self.pending_select = self.weapon_by_index(2),
                            KeyCode::Digit4 => self.pending_select = self.weapon_by_index(3),
                            KeyCode::Digit5 => self.pending_select = self.weapon_by_index(4),
                            KeyCode::Digit6 => self.pending_select = self.weapon_by_index(5),
                            KeyCode::Digit7 => self.pending_select = self.weapon_by_index(6),
                            KeyCode::Digit8 => self.pending_select = self.weapon_by_index(7),
                            KeyCode::Digit9 => self.pending_select = self.weapon_by_index(8),
                            KeyCode::Digit0 => self.pending_select = self.weapon_by_index(9),
                            _ => {}
                        }
                    }
                    ElementState::Released => {
                        self.keys.remove(&code);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match state {
                ElementState::Pressed => {
                    if !self.captured {
                        if !consumed && !self.egui_ctx.is_pointer_over_area() && button == MouseButton::Left {
                            self.set_captured(true);
                        }
                        return;
                    }
                    self.buttons.insert(button);
                }
                ElementState::Released => {
                    self.buttons.remove(&button);
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.captured {
                    return;
                }
                let y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                // Aiming a manual-zoom gun, a notch is a quarter second of
                // C-up / C-down held; otherwise it cycles weapons.
                let w = self.sim.bgun.bgun_get_weapon_num(HAND_RIGHT);
                let zooms = self.buttons.contains(&MouseButton::Right) && self.sim.gset.has_aim_flag(w, INVAIMFLAG_MANUALZOOM);
                if zooms {
                    if y != 0.0 {
                        self.zoom_hold = 15 * y.signum() as i32;
                    }
                } else if y > 0.0 {
                    self.wheel = 1;
                } else if y < 0.0 {
                    self.wheel = -1;
                }
            }
            WindowEvent::RedrawRequested => self.frame(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        _event_loop.set_control_flow(ControlFlow::Poll);
    }
}

/// `sight_draw_maian` (`sight.c:1278`): four shaded triangles from the middle
/// of each view edge to an 8-pixel box round the crosshair (outer 0x00ff000f,
/// inner 0xff000060 with a prop under it, else 0x00ff0044), then the box's
/// 1-pixel border in 0x00ff0028. Coordinates are PD's 320×240 view pixels.
#[allow(clippy::too_many_arguments)]
fn sight_draw_maian(
    painter: &egui::Painter,
    to_px: &dyn Fn(f32, f32) -> egui::Pos2,
    width: f32,
    height: f32,
    x: f32,
    y: f32,
    hasprop: bool,
    sx: f32,
) {
    let (x, y) = (x.trunc(), y.trunc());
    let (vl, vt) = (0.0f32, 0.0f32);
    let (vr, vb) = (width - 1.0, height - 1.0);
    let midx = vl + (width as i32 >> 1) as f32;
    let midy = vt + (height as i32 >> 1) as f32;
    let outer = egui::Color32::from_rgba_unmultiplied(0x00, 0xff, 0x00, 0x0f);
    let inner = if hasprop {
        egui::Color32::from_rgba_unmultiplied(0xff, 0x00, 0x00, 0x60)
    } else {
        egui::Color32::from_rgba_unmultiplied(0x00, 0xff, 0x00, 0x44)
    };
    let v = [
        (to_px(midx, vt + 10.0), outer),
        (to_px(midx, vb - 10.0), outer),
        (to_px(vl + 48.0, midy), outer),
        (to_px(vr - 49.0, midy), outer),
        (to_px(x - 4.0, y - 4.0), inner),
        (to_px(x + 4.0, y - 4.0), inner),
        (to_px(x + 4.0, y + 4.0), inner),
        (to_px(x - 4.0, y + 4.0), inner),
    ];
    let mut mesh = egui::Mesh::default();
    for (p, c) in v {
        mesh.colored_vertex(p, c);
    }
    // gSPTri4(0, 4, 5, 5, 3, 6, 7, 6, 1, 4, 7, 2)
    for t in [[0, 4, 5], [5, 3, 6], [7, 6, 1], [4, 7, 2]] {
        mesh.add_triangle(t[0], t[1], t[2]);
    }
    painter.add(egui::Shape::mesh(mesh));
    let border = egui::Stroke::new(1.0_f32.max(sx * 0.5), egui::Color32::from_rgba_unmultiplied(0x00, 0xff, 0x00, 0x28));
    for (a, b) in [
        ((x - 4.0, y - 4.0), (x - 4.0, y + 4.0)),
        ((x + 4.0, y - 4.0), (x + 4.0, y + 4.0)),
        ((x - 4.0, y - 4.0), (x + 4.0, y - 4.0)),
        ((x - 4.0, y + 4.0), (x + 4.0, y + 4.0)),
    ] {
        painter.line_segment([to_px(a.0, a.1), to_px(b.0, b.1)], border);
    }
}

/// `sight_draw_default` (`sight.c:615`): the long cross and the box, red and
/// tighter with a board under the crosshair.
#[allow(clippy::too_many_arguments)]
fn sight_draw_default(
    painter: &egui::Painter,
    to_px: &dyn Fn(f32, f32) -> egui::Pos2,
    width: f32,
    height: f32,
    x: f32,
    y: f32,
    on_target: bool,
    sx: f32,
) {
    let (col, radius, gap) = if on_target {
        (egui::Color32::from_rgba_unmultiplied(255, 0, 0, 0x60), 6.0, 3.0)
    } else {
        (egui::Color32::from_rgba_unmultiplied(0, 255, 0, 0x28 * 3), 8.0, 5.0)
    };
    let dim = egui::Color32::from_rgba_unmultiplied(0, 255, 0, 0x28 * 2);
    let stroke = egui::Stroke::new(1.0_f32.max(sx * 0.5), col);
    let dstroke = egui::Stroke::new(1.0_f32.max(sx * 0.5), dim);
    let (vl, vt, vr, vb) = (0.0, 0.0, width - 1.0, height - 1.0);
    painter.line_segment([to_px(vl + 48.0, y), to_px(x - radius + 2.0, y)], dstroke);
    painter.line_segment([to_px(x + radius - 2.0, y), to_px(vr - 49.0, y)], dstroke);
    painter.line_segment([to_px(x, vt + 10.0), to_px(x, y - radius + 2.0)], dstroke);
    painter.line_segment([to_px(x, y + radius - 2.0), to_px(x, vb - 10.0)], dstroke);
    let r = radius;
    for (a, b) in [
        ((x - r, y - r), (x - r, y + r)),
        ((x + r, y - r), (x + r, y + r)),
        ((x - r, y - r), (x + r, y - r)),
        ((x - r, y + r), (x + r, y + r)),
        ((x - r, y - r), (x - r, y - gap)),
        ((x - r, y + gap), (x - r, y + r)),
        ((x + r, y - r), (x + r, y - gap)),
        ((x + r, y + gap), (x + r, y + r)),
    ] {
        painter.line_segment([to_px(a.0, a.1), to_px(b.0, b.1)], stroke);
    }
}

/// N64 video's HUD layer: PD's gun HUD with the sight drawn into it, in PD
/// pixels, as the N64 draws both into the framebuffer.
pub(super) fn n64_hud_canvas(sim: &Sim, always_sight: bool) -> Canvas {
    let p = &sim.bgun.p;
    let mut cv = match &sim.hud {
        Some(h) => Canvas { w: h.w, h: h.h, px: h.px.clone() },
        None => Canvas::new(p.screen_width.round() as usize, p.screen_height.round() as usize),
    };
    let sighton = p.insightaimmode || always_sight;
    if sighton && sim.bgun.hands[HAND_RIGHT].weaponnum != WEAPON_UNARMED {
        let (x, y) = (p.crosspos[0], p.crosspos[1]);
        let dir = sim.bgun.cam_screen_dir([x, y], 1.0);
        let world_dir = p.projection.transform_vector3(dir);
        let on_target = matches!(
            sim.range.raycast(sim.player.pos, world_dir, 65536.0).map(|h| h.kind),
            Some(super::range::HitKind::Target(_))
        );
        if sim.bgun.hands[HAND_RIGHT].weaponnum == WEAPON_FARSIGHT {
            canvas_sight_maian(&mut cv, x as i32, y as i32, on_target);
        } else {
            let (colour, radius, gap) = if on_target { (0xff000060, 6, 3) } else { (0x00ff0028, 8, 5) };
            canvas_sight_aimer(&mut cv, x as i32, y as i32, radius, gap, colour);
        }
    }
    cv
}

/// The panel's VIDEO section: N64 video on/off, each VI stage, and the tube.
fn video_panel(ui: &mut egui::Ui, v: &mut VideoSettings) {
    ui.label(egui::RichText::new("VIDEO").strong());
    ui.checkbox(&mut v.n64, "N64 video");
    ui.add_enabled_ui(v.n64, |ui| {
        egui::ComboBox::from_label("resolution").selected_text(v.resolution.label()).show_ui(ui, |ui| {
            for r in Resolution::ALL {
                ui.selectable_value(&mut v.resolution, r, r.label());
            }
        });
        ui.indent("n64stages", |ui| {
            ui.checkbox(&mut v.three_point, "3-point texture filter");
            ui.checkbox(&mut v.fb16, "16-bit colour + Bayer dither");
            ui.add_enabled(v.fb16, egui::Checkbox::new(&mut v.dither_filter, "VI dither filter"));
            ui.checkbox(&mut v.aa, "VI anti-alias (depth edges)");
            ui.checkbox(&mut v.divot, "VI divot filter");
        });
        ui.checkbox(&mut v.crt, "CRT");
        ui.add_enabled_ui(v.crt, |ui| {
            ui.indent("crtopts", |ui| {
                ui.horizontal(|ui| {
                    for p in Preset::ALL {
                        if ui.small_button(p.label()).clicked() {
                            p.apply(v);
                        }
                    }
                });
                egui::ComboBox::from_label("signal").selected_text(v.signal.label()).show_ui(ui, |ui| {
                    for s in Signal::ALL {
                        ui.selectable_value(&mut v.signal, s, s.label());
                    }
                });
                egui::ComboBox::from_label("mask").selected_text(v.mask.label()).show_ui(ui, |ui| {
                    for m in Mask::ALL {
                        ui.selectable_value(&mut v.mask, m, m.label());
                    }
                });
                ui.add(egui::Slider::new(&mut v.mask_strength, 0.0..=1.0).text("mask strength"));
                ui.add(egui::Slider::new(&mut v.scanlines, 0.0..=1.0).text("scanlines"));
                ui.add(egui::Slider::new(&mut v.sharpness, 0.5..=2.5).text("signal sharpness"));
                ui.add(egui::Slider::new(&mut v.halation, 0.0..=0.3).text("halation"));
                ui.add(egui::Slider::new(&mut v.curvature, 0.0..=0.2).text("curvature"));
                ui.add(egui::Slider::new(&mut v.overscan, 0.0..=0.1).text("overscan"));
            });
        });
        if ui.small_button("reset video").clicked() {
            *v = VideoSettings { n64: true, ..VideoSettings::default() };
        }
    });
}

/// `gDPHudRectangle` (`gbiex.h:101`) at `g_UiScaleX` 1: both corners inclusive,
/// blended `G_RM_XLU_SURF` in the prim colour (`text_begin_boxmode`).
fn hud_rect(cv: &mut Canvas, x1: i32, y1: i32, x2: i32, y2: i32, colour: u32) {
    cv.fill_rect(x1, y1, x2 + 1, y2 + 1, colour);
}

/// `sight_draw_aimer` (`sight.c:428`) into the HUD canvas, single player.
fn canvas_sight_aimer(cv: &mut Canvas, x: i32, y: i32, radius: i32, cornergap: i32, colour: u32) {
    let (vl, vt) = (0, 0);
    let (vr, vb) = (cv.w as i32 - 1, cv.h as i32 - 1);
    let line = 0x00ff0028;
    hud_rect(cv, vl + 48, y, x - radius + 2, y, line);
    hud_rect(cv, x + radius - 2, y, vr - 49, y, line);
    hud_rect(cv, x, vt + 10, x, y - radius + 2, line);
    hud_rect(cv, x, y + radius - 2, x, vb - 10, line);
    let (r, g) = (radius, cornergap);
    for (x1, y1, x2, y2) in [
        (x - r, y - r, x - r, y + r),
        (x + r, y - r, x + r, y + r),
        (x - r, y - r, x + r, y - r),
        (x - r, y + r, x + r, y + r),
        // The corners a second time.
        (x - r, y - r, x - r, y - g),
        (x - r, y + g, x - r, y + r),
        (x + r, y - r, x + r, y - g),
        (x + r, y + g, x + r, y + r),
        (x - r, y - r, x - g, y - r),
        (x + g, y - r, x + r, y - r),
        (x - r, y + r, x - g, y + r),
        (x + g, y + r, x + r, y + r),
    ] {
        hud_rect(cv, x1, y1, x2, y2, colour);
    }
}

/// `sight_draw_maian` (`sight.c:1278`) into the HUD canvas: the four
/// smooth-shaded triangles (`G_CC_SHADE`, XLU), then the inner box's border.
fn canvas_sight_maian(cv: &mut Canvas, x: i32, y: i32, hasprop: bool) {
    let (w, h) = (cv.w as i32, cv.h as i32);
    let (vr, vb) = (w - 1, h - 1);
    let outer = 0x00ff000f;
    let inner = if hasprop { 0xff000060 } else { 0x00ff0044 };
    let v = [
        ((w >> 1) as f32, 10.0, outer),
        ((w >> 1) as f32, (vb - 10) as f32, outer),
        (48.0, (h >> 1) as f32, outer),
        ((vr - 49) as f32, (h >> 1) as f32, outer),
        ((x - 4) as f32, (y - 4) as f32, inner),
        ((x + 4) as f32, (y - 4) as f32, inner),
        ((x + 4) as f32, (y + 4) as f32, inner),
        ((x - 4) as f32, (y + 4) as f32, inner),
    ];
    // gSPTri4(0, 4, 5, 5, 3, 6, 7, 6, 1, 4, 7, 2)
    for t in [[0, 4, 5], [5, 3, 6], [7, 6, 1], [4, 7, 2]] {
        shade_tri(cv, [v[t[0]], v[t[1]], v[t[2]]]);
    }
    let b = 0x00ff0028;
    hud_rect(cv, x - 4, y - 4, x - 4, y + 4, b);
    hud_rect(cv, x + 4, y - 4, x + 4, y + 4, b);
    hud_rect(cv, x - 4, y - 4, x + 4, y - 4, b);
    hud_rect(cv, x - 4, y + 4, x + 4, y + 4, b);
}

/// A gouraud-shaded triangle (vertex colours as RGBA words), sampled at pixel
/// centres and blended XLU.
fn shade_tri(cv: &mut Canvas, v: [(f32, f32, u32); 3]) {
    let col = v.map(|(_, _, c)| {
        let (rgb, a) = split(c);
        [rgb[0], rgb[1], rgb[2], a]
    });
    let (x0, y0) = (v[0].0, v[0].1);
    let (x1, y1) = (v[1].0, v[1].1);
    let (x2, y2) = (v[2].0, v[2].1);
    let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    if area.abs() < 1e-6 {
        return;
    }
    let minx = x0.min(x1).min(x2).floor().max(0.0) as i32;
    let maxx = x0.max(x1).max(x2).ceil().min(cv.w as f32 - 1.0) as i32;
    let miny = y0.min(y1).min(y2).floor().max(0.0) as i32;
    let maxy = y0.max(y1).max(y2).ceil().min(cv.h as f32 - 1.0) as i32;
    for py in miny..=maxy {
        for px in minx..=maxx {
            let (sx, sy) = (px as f32 + 0.5, py as f32 + 0.5);
            let w0 = ((x1 - sx) * (y2 - sy) - (x2 - sx) * (y1 - sy)) / area;
            let w1 = ((x2 - sx) * (y0 - sy) - (x0 - sx) * (y2 - sy)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let c: [f32; 4] = std::array::from_fn(|i| col[0][i] * w0 + col[1][i] * w1 + col[2][i] * w2);
            cv.blend(px, py, [c[0], c[1], c[2]], c[3]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_aim_inversion_applies_only_while_aiming_and_follows_the_checkbox() {
        let up = N64State { stick_y: 60, ..N64State::default() };
        let aimed = N64State { aim: true, ..up };
        let look = |n: N64State, invert: bool| {
            let mut i = PdInput::default();
            merge_pad(&mut i, n, invert);
            i.look_y
        };
        assert_eq!(look(up, true), 60, "walking is never inverted");
        assert_eq!(look(aimed, true), -60, "aimed + invert: stick up aims down");
        assert_eq!(look(aimed, false), 60, "checkbox off: stick up aims up");
    }
}
