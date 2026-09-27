//! The `pd_combat_sim` window: keyboard / N64 pad → PD's controller state,
//! PD frames at 60 Hz, the 320×220 menu framebuffer scaled to a 4:3 picture
//! (the VI's 220 of 240 lines, black bars above and below), menu sounds on the
//! engine's audio, and an F1 panel.
//!
//! Keyboard (drives controller 1 unless the panel says otherwise):
//! arrows / WASD = D-pad · Enter = A · Esc = B · Space = START · Z = Z ·
//! Q / E = L / R (caps on the name keyboard) · Backspace = delete on the name
//! keyboard · F1 = panel · F2 / F3 / F4 = press START on controller 2 / 3 / 4
//! (join a second player without a second pad).
//!
//! A USB N64 pad plays controller 1: stick, D-pad (where the adapter reports
//! one), A, B, Z, START, L/R, C-buttons, by the raw codes the main game uses
//! (see `crate::gamepad`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use glam::Mat4;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use engine::audio::AudioManager;
use engine::platform::gamepad::{Gamepads, PadAxis, PadButton};
use engine::render::renderer::{EguiFrame, Renderer};

use super::mp::Profile;
use super::types::*;
use super::Pd;

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

struct Sfx {
    file: String,
    volume: f32,
}

fn load_sfx() -> HashMap<i32, Sfx> {
    let path = format!("{}/../../assets/audio/pd/menu_sfx/sfx_manifest.json", env!("CARGO_MANIFEST_DIR"));
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(&path) else {
        log::warn!("pd_combat_sim: no menu sound manifest at {path}");
        return out;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
    let Some(obj) = v.as_object() else { return out };
    for (k, e) in obj {
        let id = if let Some(rest) = k.strip_prefix("SFXMAP_") {
            i32::from_str_radix(&rest[..4.min(rest.len())], 16).ok()
        } else if k.len() == 4 {
            i32::from_str_radix(k, 16).ok()
        } else {
            None
        };
        let (Some(id), Some(file)) = (id, e.get("file").and_then(|f| f.as_str())) else { continue };
        let volume = e.get("volume").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
        out.insert(id, Sfx { file: format!("pd/menu_sfx/{file}"), volume });
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rate {
    Hz60,
    Hz30,
    Hz20,
}

impl Rate {
    fn ticks(self) -> i32 {
        match self {
            Rate::Hz60 => 1,
            Rate::Hz30 => 2,
            Rate::Hz20 => 3,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Rate::Hz60 => "60 Hz (PC port)",
            Rate::Hz30 => "30 Hz",
            Rate::Hz20 => "20 Hz (N64 in a busy frame)",
        }
    }
}

pub struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    audio: Option<AudioManager>,
    sfx: HashMap<i32, Sfx>,
    pd: Pd,
    keys: HashSet<KeyCode>,
    /// F2-F4 START taps waiting for the next frame.
    start_taps: [bool; 4],
    backspace: bool,
    kb_player: usize,
    pads: Option<Gamepads>,
    rate: Rate,
    acc: f32,
    last: Instant,
    fps: f32,
    show_panel: bool,
    n64_colour: bool,
    tex: Option<egui::TextureHandle>,
    profile: Profile,
}

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,engine=info,game=info")).try_init();
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("run app");
}

impl App {
    fn new() -> Self {
        let profile = if std::env::args().any(|a| a == "--fresh") { Profile::Fresh } else { Profile::Complete };
        let mut pd = Pd::new(profile).unwrap_or_else(|e| panic!("pd_combat_sim: {e} — run tools/pd-assets/pd_menu_gen.py"));
        if std::env::args().any(|a| a == "--combat") {
            pd.open_combat_simulator();
        } else {
            pd.open_main_menu();
        }
        App {
            window: None,
            renderer: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            audio: AudioManager::new(),
            sfx: load_sfx(),
            pd,
            keys: HashSet::new(),
            start_taps: [false; 4],
            backspace: false,
            kb_player: 0,
            pads: Gamepads::new(),
            rate: Rate::Hz60,
            acc: 0.0,
            last: Instant::now(),
            fps: 0.0,
            show_panel: true,
            n64_colour: true,
            tex: None,
            profile,
        }
    }

    /// Fold keyboard + pad into `pd.joy` (PD reads buttons + "pressed this frame").
    fn poll_input(&mut self) {
        let k = |c: KeyCode| self.keys.contains(&c);
        let mut kb: u16 = 0;
        if k(KeyCode::ArrowUp) || k(KeyCode::KeyW) {
            kb |= U_JPAD;
        }
        if k(KeyCode::ArrowDown) || k(KeyCode::KeyS) {
            kb |= D_JPAD;
        }
        if k(KeyCode::ArrowLeft) || k(KeyCode::KeyA) {
            kb |= L_JPAD;
        }
        if k(KeyCode::ArrowRight) || k(KeyCode::KeyD) {
            kb |= R_JPAD;
        }
        if k(KeyCode::Enter) || k(KeyCode::NumpadEnter) {
            kb |= A_BUTTON;
        }
        if k(KeyCode::Escape) {
            kb |= B_BUTTON;
        }
        if k(KeyCode::Space) {
            kb |= START_BUTTON;
        }
        if k(KeyCode::KeyZ) {
            kb |= Z_TRIG;
        }
        if k(KeyCode::KeyQ) {
            kb |= L_TRIG;
        }
        if k(KeyCode::KeyE) {
            kb |= R_TRIG;
        }
        let mut pad: u16 = 0;
        let (mut sx, mut sy) = (0i8, 0i8);
        let mut connected = 1u32;
        if let Some(p) = self.pads.as_mut() {
            p.poll();
            if p.connected() {
                let (mut x, mut y) = (p.axis(PadAxis::LeftStickX), p.axis(PadAxis::LeftStickY));
                let mag = (x * x + y * y).sqrt();
                let dz = crate::world::STICK_DEADZONE;
                if mag < dz {
                    x = 0.0;
                    y = 0.0;
                } else {
                    let s = ((mag - dz) / (1.0 - dz)).min(1.0) / mag;
                    x *= s;
                    y *= s;
                }
                sx = (x * 80.0).round() as i8;
                sy = (y * 80.0).round() as i8;
                let raw = |c: u32| p.pressed_raw(c);
                for (code, bit) in [
                    (CODE_A, A_BUTTON),
                    (CODE_B, B_BUTTON),
                    (CODE_Z, Z_TRIG),
                    (CODE_START, START_BUTTON),
                    (CODE_L, L_TRIG),
                    (CODE_R, R_TRIG),
                    (CODE_C_UP, U_CBUTTONS),
                    (CODE_C_DOWN, D_CBUTTONS),
                    (CODE_C_LEFT, L_CBUTTONS),
                    (CODE_C_RIGHT, R_CBUTTONS),
                ] {
                    if raw(code) {
                        pad |= bit;
                    }
                }
                for (b, bit) in [(PadButton::DPadUp, U_JPAD), (PadButton::DPadDown, D_JPAD), (PadButton::DPadLeft, L_JPAD), (PadButton::DPadRight, R_JPAD)] {
                    if p.pressed(b) {
                        pad |= bit;
                    }
                }
                let (dx, dy) = (p.axis(PadAxis::DPadX), p.axis(PadAxis::DPadY));
                if dy > 0.5 {
                    pad |= U_JPAD;
                }
                if dy < -0.5 {
                    pad |= D_JPAD;
                }
                if dx < -0.5 {
                    pad |= L_JPAD;
                }
                if dx > 0.5 {
                    pad |= R_JPAD;
                }
            }
        }
        for i in 0..4 {
            let mut b = 0u16;
            let (mut x, mut y) = (0i8, 0i8);
            if i == 0 {
                b |= pad;
                x = sx;
                y = sy;
            }
            if i == self.kb_player {
                b |= kb;
            }
            if std::mem::take(&mut self.start_taps[i]) {
                b |= START_BUTTON;
            }
            if b != 0 || i == 0 || i == self.kb_player {
                connected |= 1 << i;
            }
            let j = &mut self.pd.joy[i];
            j.buttons = b;
            j.stick_x = x;
            j.stick_y = y;
            if i == self.kb_player {
                j.back2 = std::mem::take(&mut self.backspace);
            }
        }
        // Controllers the user has joined with F2-F4 stay "connected".
        for i in 1..4 {
            if self.pd.mp.setup.chrslots & (1 << i) != 0 || self.pd.vars.waitingtojoin[i] {
                connected |= 1 << i;
            }
        }
        self.pd.connected_pads = connected;
    }

    fn play_sounds(&mut self) {
        let reqs = std::mem::take(&mut self.pd.sounds);
        let Some(audio) = self.audio.as_mut() else { return };
        for (id, pitch, vol) in reqs {
            let Some(s) = self.sfx.get(&id) else {
                log::debug!("pd_combat_sim: no sample for sound {id:#06x}");
                continue;
            };
            audio.play_voice(&s.file, s.volume * vol, pitch as f64, 0.0, false);
        }
    }

    fn step(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.25);
        self.last = now;
        self.fps = self.fps * 0.9 + (1.0 / dt.max(1e-4)) * 0.1;
        self.acc += dt;
        let tick = self.rate.ticks() as f32 / 60.0;
        if self.acc >= tick {
            self.acc -= tick;
            if self.acc > tick {
                self.acc = 0.0;
            }
            self.poll_input();
            if self.pd.match_started.is_some() {
                let j = self.pd.joy[self.kb_player];
                let p0 = self.pd.joy[0];
                if (j.buttons & !j.prev | p0.buttons & !p0.prev) & (START_BUTTON | A_BUTTON) != 0 {
                    self.pd.return_from_match();
                }
            }
            self.pd.frame(self.rate.ticks());
            self.play_sounds();
        }
    }

    fn ui(&mut self) -> Option<EguiFrame> {
        let window = self.window.as_ref()?.clone();
        let state = self.egui_state.as_mut()?;
        let raw = state.take_egui_input(&window);
        let img = egui::ColorImage::from_rgba_unmultiplied([self.pd.gfx.w, self.pd.gfx.h], &self.pd.gfx.rgba8(self.n64_colour));
        let mut tex = self.tex.take();
        let show_panel = self.show_panel;
        let mut rate = self.rate;
        let mut n64 = self.n64_colour;
        let mut kb_player = self.kb_player;
        let mut profile = self.profile;
        let mut taps = [false; 4];
        let mut reset = false;
        let fps = self.fps;
        let pad = self.pads.as_ref().is_some_and(|p| p.connected());
        let summary = self.pd.match_started.clone();
        let root = self.pd.menudata.root;
        let dialog = self.pd.menus[0].curdialog.map(|d| self.pd.menus[0].dialogs[d].def().name).unwrap_or("-");
        let out = self.egui_ctx.run(raw, |ctx| {
            if show_panel {
                egui::SidePanel::left("pdmenu").resizable(false).default_width(240.0).show(ctx, |ui| {
                    ui.heading("PD COMBAT SIMULATOR");
                    ui.label(egui::RichText::new("Perfect Dark's menus, ported").weak());
                    ui.label(format!("{fps:.0} fps · {}", if pad { "N64 pad connected" } else { "no pad" }));
                    ui.label(format!("root {root} · {dialog}"));
                    ui.separator();
                    ui.label("Keyboard: arrows/WASD d-pad · Enter A · Esc B · Space START · Z Z · Q/E L/R · Backspace delete (name keyboard) · F1 panel");
                    ui.separator();
                    egui::ComboBox::from_label("frame rate").selected_text(rate.label()).show_ui(ui, |ui| {
                        for r in [Rate::Hz60, Rate::Hz30, Rate::Hz20] {
                            ui.selectable_value(&mut rate, r, r.label());
                        }
                    });
                    ui.checkbox(&mut n64, "RGBA5551 framebuffer");
                    ui.separator();
                    ui.label("Save file (unlocks)");
                    ui.radio_value(&mut profile, Profile::Complete, "Complete (everything unlocked)");
                    ui.radio_value(&mut profile, Profile::Fresh, "Fresh (new file)");
                    if ui.button("Restart at the Perfect Menu").clicked() {
                        reset = true;
                    }
                    ui.separator();
                    ui.label("Players");
                    egui::ComboBox::from_label("keyboard drives").selected_text(format!("controller {}", kb_player + 1)).show_ui(ui, |ui| {
                        for i in 0..4 {
                            ui.selectable_value(&mut kb_player, i, format!("controller {}", i + 1));
                        }
                    });
                    ui.horizontal(|ui| {
                        for i in 1..4 {
                            if ui.button(format!("START on {}", i + 1)).clicked() {
                                taps[i] = true;
                            }
                        }
                    });
                    ui.label(egui::RichText::new("F2/F3/F4 press START on controllers 2-4. In the Combat Simulator a second player joins by pressing START (then 'keyboard drives' that controller).").weak());
                });
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                // 4:3 picture, the 220 lines inside a 240-line frame.
                let (w, h) = if avail.width() / avail.height() > 4.0 / 3.0 { (avail.height() * 4.0 / 3.0, avail.height()) } else { (avail.width(), avail.width() * 3.0 / 4.0) };
                let pic = egui::Rect::from_center_size(avail.center(), egui::vec2(w, h));
                let fb = egui::Rect::from_center_size(pic.center(), egui::vec2(w, h * 220.0 / 240.0));
                let t = match tex.take() {
                    Some(mut t) => {
                        t.set(img.clone(), egui::TextureOptions::NEAREST);
                        t
                    }
                    None => ctx.load_texture("pdmenu", img.clone(), egui::TextureOptions::NEAREST),
                };
                ui.painter().image(t.id(), fb, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                tex = Some(t);
                if let Some(s) = &summary {
                    let mut y = fb.top() + 40.0;
                    ui.painter().text(egui::pos2(fb.center().x, y), egui::Align2::CENTER_CENTER, "MATCH WOULD START (spike: no match)", egui::FontId::monospace(20.0), egui::Color32::from_rgb(255, 220, 90));
                    y += 36.0;
                    for l in &s.lines {
                        ui.painter().text(egui::pos2(fb.left() + 40.0, y), egui::Align2::LEFT_CENTER, l, egui::FontId::monospace(15.0), egui::Color32::WHITE);
                        y += 20.0;
                    }
                    ui.painter().text(egui::pos2(fb.center().x, fb.bottom() - 30.0), egui::Align2::CENTER_CENTER, "START / Enter: back to the Combat Simulator", egui::FontId::monospace(16.0), egui::Color32::LIGHT_GRAY);
                }
            });
        });
        self.tex = tex;
        self.show_panel = show_panel;
        self.rate = rate;
        self.n64_colour = n64;
        self.kb_player = kb_player;
        for i in 1..4 {
            if taps[i] {
                self.start_taps[i] = true;
            }
        }
        if profile != self.profile {
            self.profile = profile;
            self.pd.set_profile(profile);
        }
        if reset {
            if let Ok(pd) = Pd::new(self.profile) {
                self.pd = pd;
                self.pd.open_main_menu();
            }
        }
        let state = self.egui_state.as_mut()?;
        state.handle_platform_output(&window, out.platform_output);
        let paint_jobs = self.egui_ctx.tessellate(out.shapes, out.pixels_per_point);
        Some(EguiFrame { textures_delta: out.textures_delta, paint_jobs, pixels_per_point: out.pixels_per_point })
    }

    fn frame(&mut self) {
        self.step();
        let egui_frame = self.ui();
        if let Some(r) = self.renderer.as_mut() {
            r.render(Mat4::IDENTITY, egui_frame);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title("PD Combat Simulator (spike)").with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()));
        renderer.set_grid_mode(false);
        renderer.set_crosshair_offset(None);
        self.egui_state = Some(egui_winit::State::new(self.egui_ctx.clone(), egui::ViewportId::ROOT, &*window, None, None, None));
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.last = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let consumed = match (self.egui_state.as_mut(), self.window.as_ref()) {
            (Some(s), Some(w)) => s.on_window_event(w, &event).consumed,
            _ => false,
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::Focused(false) => self.keys.clear(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                match event.state {
                    ElementState::Pressed => {
                        let fresh = self.keys.insert(code);
                        if !fresh || consumed && self.egui_ctx.wants_keyboard_input() {
                            return;
                        }
                        match code {
                            KeyCode::F1 => self.show_panel = !self.show_panel,
                            KeyCode::F2 => self.start_taps[1] = true,
                            KeyCode::F3 => self.start_taps[2] = true,
                            KeyCode::F4 => self.start_taps[3] = true,
                            KeyCode::Backspace => self.backspace = true,
                            _ => {}
                        }
                    }
                    ElementState::Released => {
                        self.keys.remove(&code);
                    }
                }
            }
            WindowEvent::RedrawRequested => self.frame(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}
