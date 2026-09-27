//! The `pd_complex` window: the guns spike's window ([`crate::pd_guns::app`]) with
//! a [`Fight`] as its host. The player's controls, audio, N64 video and CRT are
//! the range's; on top of that this draws the simulants (PD bodies, their guns
//! and muzzle flashes, through the engine's character path, posed on the CPU as
//! the simulant spike's viewer does) and Complex's BG (through the PD renderer),
//! and adds the match to the panel.

use std::collections::HashSet;
use std::sync::Arc;

use glam::Mat4;

use engine::render::renderer::Renderer;

use crate::pd_guns::app::{run_host, Host};
use crate::pd_guns::font::Canvas;
use crate::pd_guns::model::ModelDef;
use crate::pd_guns::player::PdInput;
use crate::pd_guns::sim::Sim as GunSim;
use crate::pd_spike::bot::{Difficulty, Hand};
use crate::pd_spike::gunpos::{self, assets_dir};
use crate::pd_spike::sim::FrameRate;
use crate::pd_spike::weapons::{self, WeaponId};

use super::bg;
use super::fight::{Fight, RESPAWN_MIN_TICKS};

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,engine=info,game=info")).try_init();
    let host = ComplexHost::new().unwrap_or_else(|e| panic!("pd_complex: {e}"));
    run_host(host);
}

pub struct ComplexHost {
    pub fight: Fight,
    guns_loaded: HashSet<WeaponId>,
    bg: Option<Arc<ModelDef>>,
    /// Draw Complex's collision tiles as a greybox too (or instead, with no BG).
    greybox: bool,
    greybox_uploaded: Option<bool>,
}

impl ComplexHost {
    pub fn new() -> Result<Self, String> {
        let mut fight = Fight::from_env()?;
        let bg = match bg::load() {
            Ok(def) => {
                fight.guns.models.insert(bg::BG_MODEL.to_string(), def.clone());
                fight.guns.range.xray_tris = Some(Arc::new(bg::triangles(&def)));
                Some(def)
            }
            Err(e) => {
                log::warn!("pd_complex: no textured BG ({e}); drawing the collision greybox");
                None
            }
        };
        if fight.rigs.is_empty() {
            return Err("no PD bodies — export them with tools/pd-assets/pd_gltf.py batch".into());
        }
        let greybox = bg.is_none();
        Ok(ComplexHost { fight, guns_loaded: HashSet::new(), bg, greybox, greybox_uploaded: None })
    }

    fn upload_greybox(&mut self, renderer: &mut Renderer) {
        if self.greybox_uploaded == Some(self.greybox) {
            return;
        }
        if self.greybox {
            let geom = &self.fight.bots.level.geom;
            let opts = crate::pd_spike::greybox::GreyboxOpts { clip_y: None, room_tint: false };
            renderer.set_nav_overlay_mesh(Some(&crate::pd_spike::greybox::build(geom, &opts)));
        } else {
            renderer.set_nav_overlay_mesh(None);
        }
        self.greybox_uploaded = Some(self.greybox);
    }
}

fn frame_rate(lvupdate240: i32) -> FrameRate {
    match lvupdate240 {
        8 => FrameRate::Fps30,
        12 => FrameRate::Fps20,
        16 => FrameRate::Fps15,
        _ => FrameRate::Fps60,
    }
}

impl Host for ComplexHost {
    fn sim(&self) -> &GunSim {
        &self.fight.guns
    }

    fn sim_mut(&mut self) -> &mut GunSim {
        &mut self.fight.guns
    }

    fn title(&self) -> &'static str {
        "PD COMPLEX — you vs Perfect Dark simulants"
    }

    fn frame(&mut self, input: &PdInput, lvupdate240: i32) {
        self.fight.bots.config.rate = frame_rate(lvupdate240);
        self.fight.frame(input);
    }

    fn gpu_ready(&mut self, renderer: &mut Renderer) {
        let dir = assets_dir();
        for (i, body) in self.fight.rigs.iter().enumerate() {
            renderer.upload_character(i, &body.model);
        }
        for w in weapons::WEAPONS {
            let path = format!("{dir}/weapons/{}", w.tp_glb);
            match crate::combat::load_gun(&path) {
                Ok(model) => {
                    renderer.upload_enemy_weapon(w.name, &model);
                    self.guns_loaded.insert(w.id);
                }
                Err(e) => log::warn!("pd_complex: gun {path}: {e}"),
            }
            let fpath = format!("{dir}/weapons/{}", w.flash_glb);
            match crate::combat::load_flash(&fpath) {
                Ok(model) => renderer.upload_enemy_muzzle(w.name, &model),
                Err(e) => log::warn!("pd_complex: flash {fpath}: {e}"),
            }
        }
        self.upload_greybox(renderer);
    }

    fn before_render(&mut self, renderer: &mut Renderer, vp: Mat4) {
        self.upload_greybox(renderer);
        // The BG in the PD layer's world pass; in x-ray PD draws its own BG instead.
        let guns = &mut self.fight.guns;
        guns.host_world_models.clear();
        if self.bg.is_some() && guns.xray().is_none() {
            guns.host_world_models.push((bg::BG_MODEL.to_string(), vec![Mat4::IDENTITY], None));
        }
        // The simulants: skinned bodies, held guns, muzzle flashes, and the muzzles
        // handed back to the sim (`chr_get_gun_pos` reads the drawn gun).
        let me = self.fight.me;
        let mut chars = Vec::new();
        let mut guns_draw = Vec::new();
        let mut flashes = Vec::new();
        let mut muzzles = Vec::new();
        for (ci, c) in self.fight.bots.chrs.iter().enumerate() {
            if ci == me {
                continue;
            }
            let alpha = c.render_alpha();
            if alpha <= 0.0 {
                continue;
            }
            let Some(body) = self.fight.rigs.get(c.body) else { continue };
            let (model_mtx, globals) = body.pose(c);
            let skin: Vec<Mat4> = globals.iter().zip(&body.model.skeleton.inverse_bind).map(|(g, ib)| *g * *ib).collect();
            chars.push((c.body, model_mtx, skin, alpha));
            for (hand, w) in c.held_weapons() {
                let Some(def) = weapons::get(w) else { continue };
                if !self.guns_loaded.contains(&w) {
                    continue;
                }
                let gun_mtx = body.gun_matrix(model_mtx, &globals, hand);
                guns_draw.push((def.name, vp * gun_mtx));
                let k = if hand == Hand::Right { 0 } else { 1 };
                if c.gunfire_visible[k] {
                    flashes.push((def.name, vp * gun_mtx));
                }
                muzzles.push((ci, k, gunpos::muzzle(gun_mtx, def)));
            }
        }
        for c in &mut self.fight.bots.chrs {
            c.gunpos_rendered = [None; 2];
        }
        for (ci, hand, p) in muzzles {
            self.fight.bots.chrs[ci].gunpos_rendered[hand] = Some(p);
        }
        let inst: Vec<(usize, Mat4, Vec<Mat4>, f32, &[f32])> = chars.into_iter().map(|(b, m, j, a)| (b, m, j, a, &[][..])).collect();
        renderer.set_character_instances(&inst);
        renderer.set_enemy_weapon_draws(&guns_draw);
        renderer.set_enemy_muzzle_draws(&flashes);
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("PD COMPLEX");
        ui.label(egui::RichText::new("you vs Perfect Dark's simulants").weak());
        let f = &mut self.fight;
        ui.label(format!("health {:.0}%", f.health * 100.0));
        ui.separator();
        egui::Grid::new("score").striped(true).show(ui, |ui| {
            ui.label(egui::RichText::new("chr").strong());
            ui.label(egui::RichText::new("kills").strong());
            ui.label(egui::RichText::new("deaths").strong());
            ui.end_row();
            let mut order: Vec<usize> = (0..f.bots.chrs.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(f.bots.chrs[i].kills as i64 * 100 - f.bots.chrs[i].deaths as i64));
            for i in order {
                let c = &f.bots.chrs[i];
                let name = if c.player { egui::RichText::new(&c.name).strong() } else { egui::RichText::new(&c.name) };
                ui.label(name);
                ui.label(c.kills.to_string());
                ui.label(c.deaths.to_string());
                ui.end_row();
            }
        });
        ui.separator();
        // The bots: difficulty and weapon for all of them, count, restart.
        let cur = f.bots.config.bots.first().copied();
        if let Some(cfg) = cur {
            let mut diff = cfg.difficulty;
            egui::ComboBox::from_label("simulants").selected_text(diff.label()).show_ui(ui, |ui| {
                for d in Difficulty::ALL {
                    ui.selectable_value(&mut diff, d, d.label());
                }
            });
            let mut weapon = cfg.weapon;
            let wname = |w: Option<WeaponId>| w.and_then(weapons::get).map_or("unarmed", |d| d.name);
            egui::ComboBox::from_label("their gun").selected_text(wname(weapon)).show_ui(ui, |ui| {
                ui.selectable_value(&mut weapon, None, "unarmed");
                for w in weapons::WEAPONS {
                    ui.selectable_value(&mut weapon, Some(w.id), w.name);
                }
            });
            if diff != cfg.difficulty || weapon != cfg.weapon {
                for i in 0..f.bots.config.bots.len() {
                    f.bots.set_bot_config(i, diff, weapon);
                }
            }
        }
        let mut n = f.bots.config.bots.len();
        ui.add(egui::Slider::new(&mut n, 1..=8).text("simulants"));
        let restart = ui.button("restart match").clicked();
        if n != f.bots.config.bots.len() {
            while f.bots.config.bots.len() < n {
                let last = *f.bots.config.bots.last().unwrap();
                f.bots.config.bots.push(last);
            }
            f.bots.config.bots.truncate(n);
            f.reset();
        } else if restart {
            f.reset();
        }
        ui.checkbox(&mut self.greybox, "collision greybox");
        ui.separator();
        for (t, line) in f.bots.feed.iter().rev().take(6) {
            ui.label(egui::RichText::new(format!("{:>5.1}s {line}", *t as f32 / 60.0)).small());
        }
        ui.separator();
    }

    fn hud(&self, cv: &mut Canvas) {
        super::health::draw(&self.fight, cv);
    }

    fn overlay(&self, painter: &egui::Painter, screen: egui::Rect) {
        if let Some(t) = self.fight.dead_for {
            let msg = if t >= RESPAWN_MIN_TICKS { "fire to respawn" } else { "" };
            painter.text(
                screen.center() + egui::vec2(0.0, 30.0),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(20.0),
                egui::Color32::from_white_alpha(200),
            );
        }
    }
}
