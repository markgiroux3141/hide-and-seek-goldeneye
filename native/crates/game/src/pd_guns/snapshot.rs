//! Headless snapshots: run the sim for a scripted sequence and render the gun
//! pass offscreen (no window, no input capture) to PNGs, so the first-person
//! view can be checked without driving the game. The world itself (engine
//! region renderer) needs a surface, so the background is a flat grey and only
//! the PD layer — guns, hands, beams, sparks, boards, bullet holes — is drawn.
//!
//! `cargo run --release --bin pd_gun_snapshot -- <outdir> [weapon ...]`

use std::path::Path;

use glam::Vec3;


use super::bgun::*;
use super::gset::*;
use super::player::PdInput;
use super::render::PdRenderer;
use super::sim::Sim;

const W: u32 = 960;
const H: u32 = 540;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Gpu {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("no GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).expect("device");
    Gpu { device, queue }
}

/// Draw one frame; with a `path`, read it back and save it. Frames drawn
/// without saving still feed the post pass's last-frame copy (the x-ray smear).
fn render_png(g: &Gpu, pd: &mut PdRenderer, sim: &Sim, path: Option<&Path>) {
    let color = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snap-color"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let depth = g.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snap-depth"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let cv = color.create_view(&Default::default());
    let dv = depth.create_view(&Default::default());
    let mut enc = g.device.create_command_encoder(&Default::default());
    {
        let _rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("snap-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &cv,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.12, g: 0.13, b: 0.15, a: 1.0 }), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
    }
    let aspect = W as f32 / H as f32;
    let vp = super::app::world_vp(sim, aspect);
    pd.draw(&g.device, &g.queue, &mut enc, &cv, &dv, aspect, vp, sim);
    let fx = PdRenderer::post_fx(sim);
    pd.post(&g.device, &g.queue, &mut enc, Some(&color), &cv, W, H, fx);
    let Some(path) = path else {
        g.queue.submit(Some(enc.finish()));
        return;
    };
    let row = (W * 4).div_ceil(256) * 256;
    let buf = g.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("snap-read"),
        size: (row * H) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &color, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(H) } },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
    g.queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    g.device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let mut img = image::RgbaImage::new(W, H);
    for y in 0..H {
        let src = &data[(y * row) as usize..(y * row + W * 4) as usize];
        for x in 0..W {
            let i = (x * 4) as usize;
            img.put_pixel(x, y, image::Rgba([src[i], src[i + 1], src[i + 2], 255]));
        }
    }
    drop(data);
    buf.unmap();
    // The HUD canvas (PD pixels, premultiplied), scaled up without filtering.
    if let Some(cv) = &sim.hud {
        for y in 0..H {
            for x in 0..W {
                let (cx, cy) = ((x as usize * cv.w) / W as usize, (y as usize * cv.h) / H as usize);
                let p = cv.px[cy.min(cv.h - 1) * cv.w + cx.min(cv.w - 1)];
                if p[3] <= 0.0 {
                    continue;
                }
                let d = img.get_pixel_mut(x, y);
                for k in 0..3 {
                    let dst = d[k] as f32 / 255.0;
                    d[k] = ((p[k] + dst * (1.0 - p[3])).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    img.save(path).expect("save png");
}

/// Equip, settle, then capture: idle, mid-fire, mid-reload, aiming.
pub fn run(out: &Path, weapons: &[String]) {
    std::fs::create_dir_all(out).unwrap();
    let g = gpu();
    let mut sim = Sim::new(super::sim::HAND_MODELS[0]).expect("sim");
    sim.aspect = W as f32 / H as f32;
    let mut pd = PdRenderer::new(&g.device, &g.queue, wgpu::TextureFormat::Rgba8UnormSrgb, wgpu::TextureFormat::Depth32Float);
    pd.load_models(&g.device, &g.queue, &sim);
    let idle = PdInput::default();
    if weapons.first().map(|s| s.as_str()) == Some("--seq") {
        let names: Vec<&str> = weapons[1..].iter().map(|s| s.as_str()).collect();
        let mut snap = Snap { g: &g, pd: &mut pd, out };
        for name in if names.is_empty() || names == ["all"] { SEQUENCES.to_vec() } else { names } {
            let mut sim = Sim::new(super::sim::HAND_MODELS[0]).expect("sim");
            sim.aspect = W as f32 / H as f32;
            eprintln!("── sequence {name}");
            run_sequence(&mut snap, &mut sim, name);
        }
        return;
    }
    if weapons.first().map(|s| s.as_str()) == Some("--all") {
        // One idle frame per weapon in the loadout (single, then dual).
        let inv = sim.bgun.p.inventory.clone();
        for (w, dual) in inv {
            for d in [false, true] {
                if d && !dual {
                    continue;
                }
                sim.frame(&PdInput { select: Some((w, d)), ..PdInput::default() }, 4);
                for _ in 0..150 {
                    sim.frame(&idle, 4);
                }
                let name = sim.gset.weapon(w).map_or(format!("w{w}"), |d| d.name.replace(' ', "_"));
                render_png(&g, &mut pd, &sim, Some(&out.join(format!("all_{w:02}_{name}{}.png", if d { "_x2" } else { "" }))));
            }
        }
        return;
    }
    let list: Vec<i32> = if weapons.is_empty() {
        vec![WEAPON_FALCON2, WEAPON_CMP150, WEAPON_DRAGON, WEAPON_SHOTGUN, WEAPON_REAPER, WEAPON_LAPTOPGUN, WEAPON_UNARMED]
    } else {
        weapons
            .iter()
            .filter_map(|n| sim.gset.weapons.values().find(|w| w.short_name.eq_ignore_ascii_case(n) || w.name.eq_ignore_ascii_case(n)).map(|w| w.weaponnum))
            .collect()
    };
    for w in list {
        let name = sim.gset.weapon(w).map_or(format!("w{w}"), |d| {
            let n = if d.short_name.is_empty() { &d.name } else { &d.short_name };
            n.replace(' ', "_")
        });
        sim.frame(&PdInput { select: Some((w, false)), ..PdInput::default() }, 4);
        for _ in 0..150 {
            sim.frame(&idle, 4);
        }
        render_png(&g, &mut pd, &sim, Some(&out.join(format!("{name}_1idle.png"))));
        let fire = PdInput { fire: true, ..PdInput::default() };
        sim.frame(&fire, 4);
        render_png(&g, &mut pd, &sim, Some(&out.join(format!("{name}_2fire.png"))));
        for _ in 0..8 {
            sim.frame(&fire, 4);
        }
        render_png(&g, &mut pd, &sim, Some(&out.join(format!("{name}_2fire_b.png"))));
        for _ in 0..40 {
            sim.frame(&idle, 4);
        }
        sim.frame(&PdInput { reload: true, ..PdInput::default() }, 4);
        for _ in 0..25 {
            sim.frame(&idle, 4);
        }
        render_png(&g, &mut pd, &sim, Some(&out.join(format!("{name}_3reload.png"))));
        for _ in 0..120 {
            sim.frame(&idle, 4);
        }
        let aim = PdInput { aim: true, ..PdInput::default() };
        for _ in 0..60 {
            sim.frame(&aim, 4);
        }
        render_png(&g, &mut pd, &sim, Some(&out.join(format!("{name}_4aim.png"))));
        for _ in 0..30 {
            sim.frame(&idle, 4);
        }
        eprintln!("{name}: state {} visible {}", sim.bgun.hands[HAND_RIGHT].state, sim.bgun.hands[HAND_RIGHT].visible);
    }
}

// ─── scripted sequences (`--seq <name>...`) for the deferred-work features ────

/// Every sequence, in the handoff's order.
pub const SEQUENCES: &[&str] = &["smoke", "explosion", "grenade", "cook", "pinball", "mines", "knife", "nbomb", "rocket", "devastator", "superdragon", "crossbow", "slayer", "phoenix", "laptop", "farsight", "boost", "cloak", "hud"];

struct Snap<'a> {
    g: &'a Gpu,
    pd: &'a mut PdRenderer,
    out: &'a Path,
}

impl Snap<'_> {
    fn png(&mut self, sim: &Sim, name: &str) {
        render_png(self.g, self.pd, sim, Some(&self.out.join(format!("seq_{name}.png"))));
        eprintln!(
            "  {name}: smokes {} explosions {} wallhits {} room {:.0} shake {:.1} objs [{}] dmg {:.2}",
            sim.smokes.live(),
            sim.explosions.live(),
            sim.wallhits.len(),
            sim.room.final_brightness(),
            sim.vi.offset,
            sim.objs
                .iter()
                .map(|o| format!("{:#x}@({:.0},{:.0},{:.0}) t{}{}", o.weaponnum, o.pos.x, o.pos.y, o.pos.z, o.timer240, if o.proj.is_some() { " fly" } else if o.attached { " stuck" } else { " rest" }))
                .collect::<Vec<_>>()
                .join(" "),
            sim.player_damage
        );
        if sim.visionmode == super::sim::VisionMode::Xray {
            eprintln!("    x-ray: erasertime {} eraser ({:.0},{:.0},{:.0}) fov {:.1} blur {:?}", sim.erasertime, sim.eraser.pos.x, sim.eraser.pos.y, sim.eraser.pos.z, sim.bgun.p.fovy, sim.xray_zoom_blur());
        }
    }

    /// Step `n` frames, drawing each (unsaved) so frame-to-frame effects run.
    fn live(&mut self, sim: &mut Sim, input: &PdInput, n: usize) {
        for _ in 0..n {
            sim.frame(input, 4);
            render_png(self.g, self.pd, sim, None);
        }
    }
}

fn frames(sim: &mut Sim, input: &PdInput, n: usize) {
    for _ in 0..n {
        sim.frame(input, 4);
    }
}

/// Equip `w` and let it come up.
fn equip(sim: &mut Sim, w: i32) {
    sim.frame(&PdInput { select: Some((w, false)), ..PdInput::default() }, 4);
    frames(sim, &PdInput::default(), 150);
}

/// Hold B (use) for `n` frames, then let go: past 25 ticks it toggles (or,
/// for the remote mine, inverts while held) the gun function.
fn hold_use(sim: &mut Sim, n: usize) {
    frames(sim, &PdInput { use_held: true, ..PdInput::default() }, n);
    frames(sim, &PdInput::default(), 2);
}

/// Tap the trigger `n` times, `gap` frames apart.
fn taps(sim: &mut Sim, n: usize, gap: usize) {
    let fire = PdInput { fire: true, ..PdInput::default() };
    for _ in 0..n {
        sim.frame(&fire, 4);
        frames(sim, &PdInput::default(), gap);
    }
}

fn run_sequence(s: &mut Snap, sim: &mut Sim, name: &str) {
    let idle = PdInput::default();
    let fire = PdInput { fire: true, ..PdInput::default() };
    match name {
        // Muzzle smoke (Falcon taps, CMP150 burst, shotgun) and the one-player
        // bullet-hole flame + puff on the floor two metres ahead.
        "smoke" => {
            equip(sim, WEAPON_FALCON2);
            sim.player.verta = -38.0;
            taps(sim, 5, 5);
            s.png(sim, "smoke_1_falcon_taps");
            frames(sim, &idle, 20);
            s.png(sim, "smoke_2_falcon_after20");
            frames(sim, &idle, 60);
            s.png(sim, "smoke_3_falcon_after80");
            equip(sim, WEAPON_CMP150);
            sim.player.verta = -38.0;
            // Automatics only smoke after 14+ rounds (`bgun_update_smoke`).
            frames(sim, &fire, 80);
            frames(sim, &idle, 6);
            s.png(sim, "smoke_4_cmp_burst");
            frames(sim, &idle, 40);
            s.png(sim, "smoke_5_cmp_after");
            equip(sim, WEAPON_SHOTGUN);
            sim.player.verta = -38.0;
            taps(sim, 1, 8);
            s.png(sim, "smoke_6_shotgun");
        }
        // A rocket-sized blast (EXPLOSIONTYPE_ROCKET) four metres ahead on the
        // floor: flare frames, the smoke it leaves, the scorch, the shake.
        "explosion" => {
            equip(sim, WEAPON_FALCON2);
            sim.player.verta = -12.0;
            let pos = sim.player.pos + Vec3::new(0.0, 20.0, 400.0);
            let pos = Vec3::new(pos.x, 20.0, pos.z);
            sim.explosion_create_simple(pos, super::explosions::EXPLOSIONTYPE_ROCKET);
            let mut t = 0;
            for (n, label) in [(2, "a"), (10, "b"), (25, "c"), (50, "d"), (90, "e"), (200, "f")] {
                frames(sim, &idle, n - t);
                t = n;
                s.png(sim, &format!("explosion_{label}_t{n}"));
            }
        }
        // A grenade thrown down the range: flight, the bounce, rest, the blast.
        "grenade" => {
            equip(sim, WEAPON_GRENADE);
            sim.player.verta = -6.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 50);
            s.png(sim, "grenade_1_thrown");
            frames(sim, &idle, 40);
            s.png(sim, "grenade_2_landed");
            frames(sim, &idle, 110);
            s.png(sim, "grenade_3_resting");
            frames(sim, &idle, 44);
            s.png(sim, "grenade_4_blast");
            frames(sim, &idle, 30);
            s.png(sim, "grenade_5_blast_b");
            frames(sim, &idle, 120);
            s.png(sim, "grenade_6_after");
        }
        // Hold the trigger past the 4 s fuse: it goes off in the hand.
        "cook" => {
            equip(sim, WEAPON_GRENADE);
            frames(sim, &fire, 262);
            s.png(sim, "cook_1_blast");
            frames(sim, &fire, 20);
            s.png(sim, "cook_2_blast_b");
        }
        // Secondary: proximity pinball (bounces at full speed, arms after 1.5 s).
        "pinball" => {
            equip(sim, WEAPON_GRENADE);
            hold_use(sim, 30);
            frames(sim, &idle, 30);
            sim.player.verta = -20.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 70);
            s.png(sim, "pinball_1");
            frames(sim, &idle, 60);
            s.png(sim, "pinball_2");
        }
        // Timed mine onto a crate, proximity mine at our feet, remote mine on the
        // right wall then detonated (hold B + fire).
        "mines" => {
            equip(sim, WEAPON_TIMEDMINE);
            sim.player.theta = 330.0;
            sim.player.verta = -15.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 60);
            s.png(sim, "mines_1_timed_stuck");
            frames(sim, &idle, 190);
            s.png(sim, "mines_2_timed_blast");
            frames(sim, &idle, 150);
            equip(sim, WEAPON_REMOTEMINE);
            sim.player.theta = 90.0;
            sim.player.verta = 5.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 70);
            s.png(sim, "mines_3_remote_stuck");
            frames(sim, &PdInput { use_held: true, ..PdInput::default() }, 30);
            frames(sim, &PdInput { use_held: true, fire: true, ..PdInput::default() }, 3);
            frames(sim, &PdInput { use_held: true, ..PdInput::default() }, 8);
            s.png(sim, "mines_4_remote_detonated");
            frames(sim, &idle, 200);
            equip(sim, WEAPON_PROXIMITYMINE);
            sim.player.theta = 0.0;
            sim.player.verta = -45.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 80);
            s.png(sim, "mines_5_proxy_floor");
        }
        // Throwing knife (secondary) into the first board.
        "knife" => {
            equip(sim, WEAPON_COMBATKNIFE);
            hold_use(sim, 30);
            frames(sim, &idle, 40);
            sim.player.verta = -3.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 12);
            s.png(sim, "knife_1_flight");
            frames(sim, &idle, 60);
            s.png(sim, "knife_2_stuck");
        }
        // N-Bomb: the storm dome from outside, then from inside.
        "nbomb" => {
            equip(sim, WEAPON_NBOMB);
            sim.player.verta = 2.0;
            frames(sim, &fire, 3);
            frames(sim, &idle, 110);
            s.png(sim, "nbomb_1_dome");
            frames(sim, &idle, 60);
            s.png(sim, "nbomb_2_dome_b");
            // Step inside.
            if let Some(n) = sim.nbombs.bombs.iter().find(|n| n.age240 >= 0) {
                let p = n.pos;
                sim.player.pos.x = p.x;
                sim.player.pos.z = p.z - 150.0;
            }
            frames(sim, &idle, 4);
            s.png(sim, "nbomb_3_inside");
        }
        // Rocket launcher: the rocket loaded in the tube, the shot, the trail,
        // the blast on the far wall.
        "rocket" => {
            equip(sim, WEAPON_ROCKETLAUNCHER);
            s.png(sim, "rocket_1_loaded");
            sim.frame(&fire, 4);
            frames(sim, &idle, 6);
            s.png(sim, "rocket_2_launch");
            frames(sim, &idle, 14);
            s.png(sim, "rocket_3_flight");
            frames(sim, &idle, 40);
            s.png(sim, "rocket_4_impact");
        }
        // Devastator: an arcing grenade round that blows on landing, then the
        // wall hugger on the right wall (sticks, drops after 2 s, blows).
        "devastator" => {
            equip(sim, WEAPON_DEVASTATOR);
            sim.player.verta = -10.0;
            sim.frame(&fire, 4);
            frames(sim, &idle, 12);
            s.png(sim, "devastator_1_round");
            frames(sim, &idle, 30);
            s.png(sim, "devastator_2_blast");
            frames(sim, &idle, 120);
            hold_use(sim, 30);
            frames(sim, &idle, 60);
            sim.player.theta = 270.0;
            sim.player.verta = 0.0;
            sim.frame(&fire, 4);
            frames(sim, &idle, 30);
            s.png(sim, "devastator_3_hugger_stuck");
            frames(sim, &idle, 110);
            s.png(sim, "devastator_4_hugger_drop");
        }
        // SuperDragon secondary: the grenade launcher (EXPLOSIONTYPE_SDGRENADE).
        "superdragon" => {
            equip(sim, WEAPON_SUPERDRAGON);
            hold_use(sim, 30);
            frames(sim, &idle, 60);
            sim.player.verta = -12.0;
            sim.frame(&fire, 4);
            frames(sim, &idle, 8);
            s.png(sim, "superdragon_1_round");
            frames(sim, &idle, 26);
            s.png(sim, "superdragon_2_blast");
        }
        // Crossbow bolt into the first board (it quivers).
        "crossbow" => {
            equip(sim, WEAPON_CROSSBOW);
            sim.frame(&fire, 4);
            frames(sim, &idle, 5);
            s.png(sim, "crossbow_1_flight");
            frames(sim, &idle, 20);
            s.png(sim, "crossbow_2_stuck");
        }
        // Slayer: a primary rocket, then fly-by-wire — the rocket camera with
        // its interlace, steered right, then blown (the frame of static).
        "slayer" => {
            equip(sim, WEAPON_SLAYER);
            sim.frame(&fire, 4);
            frames(sim, &idle, 20);
            s.png(sim, "slayer_1_rocket");
            frames(sim, &idle, 80);
            hold_use(sim, 30);
            frames(sim, &idle, 80);
            sim.frame(&fire, 4);
            frames(sim, &idle, 30);
            s.png(sim, "slayer_2_fbw_view");
            frames(sim, &PdInput { walk_x: 127, ..PdInput::default() }, 40);
            s.png(sim, "slayer_3_fbw_turned");
            sim.frame(&fire, 4);
            s.png(sim, "slayer_4_static");
            frames(sim, &idle, 3);
            s.png(sim, "slayer_5_back");
        }
        // Phoenix secondary: explosive shells on the first board.
        "phoenix" => {
            equip(sim, WEAPON_PHOENIX);
            hold_use(sim, 30);
            frames(sim, &idle, 60);
            sim.frame(&fire, 4);
            frames(sim, &idle, 12);
            s.png(sim, "phoenix_1_shell");
        }
        // Laptop Gun deployed as a sentry (hold B + fire): on the floor ahead,
        // then one stuck to the right wall; both pick off the boards.
        "laptop" => {
            equip(sim, WEAPON_LAPTOPGUN);
            sim.player.verta = -35.0;
            frames(sim, &PdInput { use_held: true, ..PdInput::default() }, 30);
            frames(sim, &PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
            frames(sim, &idle, 200);
            sim.player.verta = -10.0;
            s.png(sim, "laptop_1_sentry_floor");
            frames(sim, &idle, 1);
            s.png(sim, "laptop_2_sentry_firing");
            sim.restock();
            equip(sim, WEAPON_LAPTOPGUN);
            sim.player.theta = 270.0;
            sim.player.verta = 5.0;
            frames(sim, &PdInput { use_held: true, ..PdInput::default() }, 30);
            frames(sim, &PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
            frames(sim, &idle, 160);
            sim.player.theta = 300.0;
            s.png(sim, "laptop_3_sentry_wall");
            // Close up: stand 1.2 m from the wall sentry, looking at it.
            if let Some(o) = sim.objs.iter().find(|o| o.autogun.is_some()) {
                let p = o.pos;
                sim.player.pos.x = p.x - 120.0;
                sim.player.pos.z = p.z - 60.0;
                let d = p - sim.player.pos;
                sim.player.theta = (-d.x).atan2(d.z).to_degrees();
                sim.player.verta = (d.y / (d.x * d.x + d.z * d.z).sqrt()).atan().to_degrees();
            }
            for k in 0..4 {
                frames(sim, &idle, 1);
                s.png(sim, &format!("laptop_4_close_{k}"));
            }
        }
        // Farsight: aim → x-ray (the zoom blur smearing in over the normal
        // frame, then settling), zoomed down the hall, a round through the
        // crate, then the right wall up close.
        "farsight" => {
            equip(sim, WEAPON_FARSIGHT);
            s.png(sim, "farsight_0_normal");
            let aim = PdInput { aim: true, ..PdInput::default() };
            sim.frame(&aim, 4);
            s.png(sim, "farsight_1_xray_first");
            s.live(sim, &aim, 8);
            s.png(sim, "farsight_2_xray_smear");
            s.live(sim, &aim, 60);
            s.png(sim, "farsight_3_xray_settled");
            s.live(sim, &PdInput { aim: true, zoom_in: true, ..PdInput::default() }, 120);
            s.live(sim, &aim, 30);
            s.png(sim, "farsight_4_zoomed");
            s.live(sim, &PdInput { aim: true, fire: true, ..PdInput::default() }, 1);
            s.live(sim, &aim, 6);
            s.png(sim, "farsight_5_shot");
            s.live(sim, &PdInput { aim: true, zoom_out: true, ..PdInput::default() }, 300);
            sim.player.pos.x = 350.0;
            sim.player.pos.z = 900.0;
            sim.player.theta = 270.0;
            s.live(sim, &aim, 90);
            s.png(sim, "farsight_6_wall");
        }
        // Combat Boost: a pill, the wipe in (zoom blur + white fade) at steps
        // 8 and 15, then boosted.
        "boost" => {
            equip(sim, WEAPON_COMBATBOOST);
            s.live(sim, &idle, 2);
            s.live(sim, &fire, 1);
            s.live(sim, &idle, 7);
            s.png(sim, "boost_1_wipe_8");
            s.live(sim, &idle, 6);
            s.png(sim, "boost_2_wipe_15");
            s.live(sim, &idle, 30);
            s.png(sim, "boost_3_on");
        }
        // RC-P120 cloak (hold B + fire): the gun half faded, then fully cloaked.
        "cloak" => {
            equip(sim, WEAPON_RCP120);
            frames(sim, &PdInput { use_held: true, ..PdInput::default() }, 30);
            frames(sim, &PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
            frames(sim, &idle, 28);
            s.png(sim, "cloak_1_fading");
            frames(sim, &idle, 60);
            s.png(sim, "cloak_2_cloaked");
        }
        // PD's gun HUD: the name sliding in, a mag emptying (the spent rounds
        // fading), reloading (the new rounds flashing in), the secondary's
        // function square, dual Falcons, a big-mag bar and the boost timer.
        "hud" => {
            s.live(sim, &idle, 6);
            s.live(sim, &PdInput { select: Some((WEAPON_FALCON2, false)), ..PdInput::default() }, 1);
            s.live(sim, &idle, 20);
            s.png(sim, "hud_1_name");
            s.live(sim, &idle, 250);
            for _ in 0..4 {
                s.live(sim, &fire, 1);
                s.live(sim, &idle, 5);
            }
            s.png(sim, "hud_2_fired");
            s.live(sim, &PdInput { reload: true, ..PdInput::default() }, 1);
            s.live(sim, &idle, 60);
            s.png(sim, "hud_3_reloading");
            s.live(sim, &idle, 200);
            s.live(sim, &PdInput { select: Some((WEAPON_CMP150, true)), ..PdInput::default() }, 1);
            s.live(sim, &idle, 200);
            s.png(sim, "hud_4_dual_cmp");
            s.live(sim, &PdInput { use_held: true, ..PdInput::default() }, 30);
            s.live(sim, &idle, 20);
            s.png(sim, "hud_5_secondary");
            s.live(sim, &PdInput { select: Some((WEAPON_COMBATBOOST, false)), ..PdInput::default() }, 1);
            s.live(sim, &idle, 150);
            s.live(sim, &fire, 1);
            s.live(sim, &idle, 40);
            s.png(sim, "hud_6_boost");
        }
        other => eprintln!("  unknown sequence {other}"),
    }
}
