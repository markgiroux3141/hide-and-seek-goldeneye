//! Headless snapshots of the match (offscreen GPU, no window): the player's
//! view from Complex's spawn pads with the textured BG drawn through the guns'
//! N64 renderer, a frame firing, and one taking damage (the red flash and the
//! health bar). The simulants are drawn by the engine's character path, which
//! needs the window's surface, so they are not in these pictures.
//!
//! `cargo run --release --bin pd_complex_snapshot -- <outdir>`

use std::path::Path;

use glam::{Mat4, Vec3};

use crate::pd_guns::player::PdInput;
use crate::pd_guns::render::PdRenderer;
use crate::pd_guns::snapshot::{gpu, render_png};

use super::bg;
use super::fight::Fight;

pub fn run(out: &Path) {
    std::fs::create_dir_all(out).unwrap();
    let g = gpu();
    let mut f = Fight::from_env().expect("match");
    f.bots.brains = false;
    let def = bg::load().expect("bg");
    f.guns.range.xray_tris = Some(std::sync::Arc::new(bg::triangles(&def)));
    f.guns.models.insert(bg::BG_MODEL.to_string(), def);
    f.guns.aspect = 960.0 / 540.0;
    let mut pd = PdRenderer::new(&g.device, &g.queue, wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureFormat::Depth32Float);
    pd.load_models(&g.device, &g.queue, &f.guns);
    let stage = f.bots.stage.clone().unwrap();
    let idle = PdInput::default();
    // The BG in the world pass, and the match's HUD (the health bar) over the gun HUD.
    let bgdraw = |f: &mut Fight| {
        f.guns.host_world_models = vec![(bg::BG_MODEL.to_string(), vec![Mat4::IDENTITY], None)];
        if let Some(h) = f.guns.hud.as_ref() {
            let mut cv = crate::pd_guns::font::Canvas { w: h.w, h: h.h, px: h.px.clone() };
            super::health::draw(f, &mut cv);
            f.guns.hud = Some(cv);
        }
    };
    for (k, &p) in stage.spawn_pads.iter().enumerate().take(8) {
        let pad = &stage.pads.pads[p];
        let floor = crate::pd_spike::sim::drop_to_ground(&f.bots.level, pad.pos);
        let a = pad.look_angle();
        let theta = (std::f32::consts::TAU - a).to_degrees().rem_euclid(360.0);
        f.guns.player.place(floor, theta);
        for _ in 0..40 {
            f.frame(&idle);
        }
        bgdraw(&mut f);
        render_png(&g, &mut pd, &f.guns, Some(&out.join(format!("complex_spawn{k}_pad{p:04x}.png"))));
    }
    // Firing the Falcon at a wall: tracer, sparks, a bullet hole on the BG.
    for t in 0..12 {
        f.frame(&PdInput { fire: t % 6 < 3, ..PdInput::default() });
    }
    bgdraw(&mut f);
    render_png(&g, &mut pd, &f.guns, Some(&out.join("complex_fire.png")));
    // Taking a hit: the red flash and PD's health bar opening.
    f.player_damage(3.0, Vec3::X, None);
    for _ in 0..4 {
        f.frame(&idle);
    }
    bgdraw(&mut f);
    render_png(&g, &mut pd, &f.guns, Some(&out.join("complex_hurt_flash.png")));
    for _ in 0..40 {
        f.frame(&idle);
    }
    bgdraw(&mut f);
    render_png(&g, &mut pd, &f.guns, Some(&out.join("complex_hurt_bar.png")));
    // The Farsight aimed: PD's x-ray of Complex's BG.
    f.hp.reset();
    f.guns.host_fade = None;
    f.frame(&PdInput { select: Some((crate::pd_guns::gset::WEAPON_FARSIGHT, false)), ..PdInput::default() });
    for _ in 0..150 {
        f.frame(&idle);
    }
    f.guns.host_world_models.clear();
    // The x-ray smear feeds back the last frame, so draw every frame.
    for _ in 0..60 {
        f.frame(&PdInput { aim: true, ..PdInput::default() });
        render_png(&g, &mut pd, &f.guns, None);
    }
    render_png(&g, &mut pd, &f.guns, Some(&out.join("complex_xray.png")));
    eprintln!("snapshots in {}", out.display());
}
