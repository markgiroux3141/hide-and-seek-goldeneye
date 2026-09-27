//! The window: renders the arena, the bots and the debug overlays, and hosts the
//! egui panels. It *reads* the simulation and never reaches into bot logic — the
//! only things it can do to [`Sim`] are the controls a tester needs (pause, step,
//! speed, reset, per-bot difficulty/weapon).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use glam::{Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use engine::geometry::csg_runtime::Region;
use engine::render::mesh::TexturedMesh;
use engine::render::renderer::{EguiFrame, Renderer};

use super::arena::UNITS_PER_M;
use super::camera::OrbitCam;
use super::debug_draw::*;
use super::greybox::{self, GreyboxOpts};
use super::level_geom::{FloorKind, GeomPoly};
use super::pd_tiles::{PdStage, WPSEGFLAG_INWARDSONLY, WPSEGFLAG_OUTWARDSONLY};
use super::sim::{LevelChoice, NavChoice};
use super::gunpos::{self, assets_dir, BodyRig};
use super::sim::{FrameRate, Sim};
use super::weapons::{self, WeaponId};
use super::{bot, botcmd};

/// PD centimetres → render metres.
fn m(p: Vec3) -> Vec3 {
    p / UNITS_PER_M
}

struct Overlays {
    waypoints: bool,
    headings: bool,
    aim: bool,
    targets: bool,
    bands: bool,
    paths: bool,
    tracers: bool,
    cones: bool,
    labels: bool,
    xray: bool,
    guns: bool,
    // Complex: the level, PD's own nav data, and our generated graph.
    pd_graph: bool,
    our_graph: bool,
    pad_drops: bool,
    spawns: bool,
    cover: bool,
    room_labels: bool,
}

impl Default for Overlays {
    fn default() -> Self {
        Overlays {
            waypoints: false,
            headings: true,
            aim: true,
            targets: true,
            bands: true,
            paths: true,
            tracers: true,
            cones: true,
            labels: true,
            xray: false,
            guns: true,
            pd_graph: true,
            our_graph: false,
            pad_drops: false,
            spawns: true,
            cover: false,
            room_labels: false,
        }
    }
}

/// Complex's display state: the greybox options and what the overlays need,
/// computed once from the PD stage the match loaded ([`Sim::stage`]).
struct ComplexView {
    lo: Vec3,
    hi: Vec3,
    clip_on: bool,
    clip_y: f32,
    room_tint: bool,
    /// The options the uploaded greybox was built with; `None` = upload needed.
    uploaded: Option<GreyboxOpts>,
    /// Floor height under each pad (cm), for the drop lines.
    pad_floor: Vec<Option<f32>>,
    /// Centroid of each room's floor tiles (cm), for the room labels.
    room_centres: Vec<(u16, Vec3)>,
    stats: Vec<(&'static str, String)>,
}

impl ComplexView {
    fn new(stage: &PdStage) -> Self {
        let (lo, hi) = stage.geom.bounds();
        let g = &stage.geom;
        let pad_floor = stage.pads.pads.iter().map(|p| g.floor_below(p.pos.x, p.pos.z, p.pos.y + 10.0).map(|f| f.0)).collect();
        let mut sums: std::collections::BTreeMap<u16, (Vec3, f32)> = Default::default();
        for p in g.polys.iter().filter(|p| p.floor) {
            let Some(r) = p.room else { continue };
            let e = sums.entry(r).or_insert((Vec3::ZERO, 0.0));
            e.0 += p.verts.iter().copied().sum::<Vec3>() / p.verts.len() as f32;
            e.1 += 1.0;
        }
        let room_centres = sums.into_iter().map(|(r, (s, n))| (r, s / n)).collect();
        let count = |f: &dyn Fn(&GeomPoly) -> bool| g.polys.iter().filter(|p| f(p)).count();
        let kind = |k: FloorKind| count(&|p| p.floor_kind() == Some(k));
        let rooms_used = g.polys.iter().filter_map(|p| p.room).collect::<HashSet<_>>().len();
        let stats = vec![
            ("polygons", g.polys.len().to_string()),
            (
                "floors",
                format!("{} flat, {} ramp, {} riser", kind(FloorKind::Flat), kind(FloorKind::Ramp), kind(FloorKind::Vertical)),
            ),
            (
                "walls",
                format!("{} solid, {} see-through", count(&|p| p.wall && p.blocks_sight), count(&|p| p.wall && !p.blocks_sight)),
            ),
            ("rooms", format!("{} ({} with tiles)", g.room_names.len(), rooms_used)),
            ("PD graph", format!("{} waypoints, {} groups", stage.pads.waypoints.len(), stage.pads.waygroups.len())),
            (
                "pads",
                format!("{} ({} spawn, {} cover)", stage.pads.pads.len(), stage.spawn_pads.len(), stage.pads.cover.len()),
            ),
            (
                "size",
                format!(
                    "{:.1} × {:.1} m, y {:.1}…{:.1} m",
                    (hi.x - lo.x) / UNITS_PER_M,
                    (hi.z - lo.z) / UNITS_PER_M,
                    lo.y / UNITS_PER_M,
                    hi.y / UNITS_PER_M
                ),
            ),
        ];
        ComplexView { lo, hi, clip_on: false, clip_y: 400.0, room_tint: false, uploaded: None, pad_floor, room_centres, stats }
    }

    fn opts(&self) -> GreyboxOpts {
        GreyboxOpts { clip_y: self.clip_on.then_some(self.clip_y), room_tint: self.room_tint }
    }

    /// Should an overlay at height `y` (cm) show under the current clip? Pads sit
    /// ~53 cm above their floor, hence the allowance.
    fn shown(&self, y: f32) -> bool {
        !self.clip_on || y <= self.clip_y + 60.0
    }

    fn camera(&self) -> OrbitCam {
        let centre = (self.lo + self.hi) * 0.5;
        OrbitCam { target: Vec3::new(centre.x, 0.0, centre.z) / UNITS_PER_M, dist: 58.0, pitch: 1.05, ..OrbitCam::default() }
    }
}

pub struct Viewer {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    bodies: Vec<BodyRig>,
    guns_loaded: HashSet<WeaponId>,

    cam: OrbitCam,
    follow: bool,
    keys: HashSet<KeyCode>,
    cursor: Vec2,
    drag: Option<(MouseButton, Vec2)>,
    press_at: Option<Vec2>,

    last: Instant,
    acc: f32,
    paused: bool,
    time_scale: f32,
    steps_requested: u32,

    sim: Sim,
    selected: Option<usize>,
    show: Overlays,
    new_bot_count: usize,

    complex: Option<ComplexView>,
    level_error: Option<String>,
}

pub fn run() {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,engine=info,game=info"))
        .try_init();
    let event_loop = EventLoop::new().expect("create event loop");
    let mut viewer = Viewer::new();
    event_loop.run_app(&mut viewer).expect("run app");
}

impl Viewer {
    fn new() -> Self {
        let mut config = super::sim::SimConfig::from_env();
        let mut level_error = None;
        let sim = match Sim::try_new(config.clone()) {
            Ok(sim) => sim,
            Err(e) => {
                log::error!("pd_arena: can't load {:?}: {e}", config.level);
                level_error = Some(e);
                config.level = LevelChoice::Arena;
                Sim::new(config)
            }
        };
        let new_bot_count = sim.chrs.len();
        let complex = sim.stage.as_ref().map(ComplexView::new);
        let cam = complex.as_ref().map_or_else(OrbitCam::default, ComplexView::camera);
        Viewer {
            window: None,
            renderer: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            bodies: Vec::new(),
            guns_loaded: HashSet::new(),
            cam,
            follow: false,
            keys: HashSet::new(),
            cursor: Vec2::ZERO,
            drag: None,
            press_at: None,
            last: Instant::now(),
            acc: 0.0,
            paused: false,
            time_scale: 1.0,
            steps_requested: 0,
            sim,
            selected: Some(0),
            show: Overlays::default(),
            new_bot_count,
            complex,
            level_error,
        }
    }

    /// Switch level: a new match on it with the same bot setup. If the level can't
    /// be loaded, the match stays where it is and the panel says why.
    fn set_level(&mut self, choice: LevelChoice) {
        let mut config = self.sim.config.clone();
        config.level = choice;
        self.restart(config, true);
    }

    /// Switch the route graph (a new match on the same seed, camera kept).
    fn set_nav(&mut self, nav: NavChoice) {
        let mut config = self.sim.config.clone();
        config.nav = nav;
        self.restart(config, false);
    }

    fn restart(&mut self, config: super::sim::SimConfig, reframe: bool) {
        let choice = config.level;
        match Sim::try_new(config) {
            Ok(sim) => {
                self.sim = sim;
                self.level_error = None;
                let old = self.complex.take();
                self.complex = self.sim.stage.as_ref().map(ComplexView::new);
                if reframe {
                    self.cam = self.complex.as_ref().map_or_else(OrbitCam::default, ComplexView::camera);
                    self.follow = false;
                } else if let (Some(new), Some(old)) = (self.complex.as_mut(), old) {
                    new.clip_on = old.clip_on;
                    new.clip_y = old.clip_y;
                    new.room_tint = old.room_tint;
                }
                self.acc = 0.0;
                if self.selected.map_or(false, |s| s >= self.sim.chrs.len()) {
                    self.selected = Some(0);
                }
            }
            Err(e) => {
                log::error!("pd_arena: can't load {choice:?}: {e}");
                self.level_error = Some(e);
            }
        }
        self.upload_level();
    }

    fn on_complex(&self) -> bool {
        self.sim.config.level == LevelChoice::Complex
    }

    /// Put the current level's static geometry on the GPU (no-op before the window).
    fn upload_level(&mut self) {
        if self.renderer.is_none() {
            return;
        }
        match self.sim.config.level {
            LevelChoice::Arena => {
                self.upload_arena();
                self.renderer.as_mut().unwrap().set_nav_overlay_mesh(None);
            }
            LevelChoice::Complex => {
                self.renderer.as_mut().unwrap().set_region_textured(0, &TexturedMesh::default());
                self.upload_greybox_if_changed();
            }
        }
    }

    /// The greybox rides the renderer's static nav-overlay channel (unlit, depth
    /// tested, uploaded only on change), so it is rebuilt only when its options move.
    fn upload_greybox_if_changed(&mut self) {
        let (Some(c), Some(r), Some(stage)) = (self.complex.as_mut(), self.renderer.as_mut(), self.sim.stage.as_ref()) else {
            return;
        };
        let opts = c.opts();
        if c.uploaded == Some(opts) {
            return;
        }
        r.set_nav_overlay_mesh(Some(&greybox::build(&stage.geom, &opts)));
        c.uploaded = Some(opts);
    }

    fn load_assets(&mut self) {
        let dir = assets_dir();
        let renderer = self.renderer.as_mut().unwrap();
        self.bodies = BodyRig::load_all()
            .unwrap_or_else(|e| panic!("pd_arena: {e} — export clips with pd_gltf.py clip"));
        for (i, body) in self.bodies.iter().enumerate() {
            renderer.upload_character(i, &body.model);
        }
        for w in weapons::WEAPONS {
            let path = format!("{dir}/weapons/{}", w.tp_glb);
            match crate::combat::load_gun(&path) {
                Ok(model) => {
                    renderer.upload_enemy_weapon(w.name, &model);
                    self.guns_loaded.insert(w.id);
                }
                Err(e) => log::warn!("pd_arena: gun {path}: {e}"),
            }
            let fpath = format!("{dir}/weapons/{}", w.flash_glb);
            match crate::combat::load_flash(&fpath) {
                Ok(model) => renderer.upload_enemy_muzzle(w.name, &model),
                Err(e) => log::warn!("pd_arena: flash {fpath}: {e}"),
            }
        }
        log::info!("pd_arena: {} bodies, {} guns loaded", self.bodies.len(), self.guns_loaded.len());
    }

    fn upload_arena(&mut self) {
        let mut region = Region::new(0);
        region.brushes = self.sim.arena.brushes();
        let (_, tex) = region.evaluate_both(&[]);
        let r = self.renderer.as_mut().unwrap();
        r.set_region_textured(0, &tex);
    }

    fn size(&self) -> Vec2 {
        self.window.as_ref().map_or(Vec2::new(1600.0, 900.0), |w| {
            let s = w.inner_size();
            Vec2::new(s.width.max(1) as f32, s.height.max(1) as f32)
        })
    }

    /// The chr under the cursor, by ray vs each bot's cylinder.
    fn pick(&self, px: Vec2) -> Option<usize> {
        let (o, d) = self.cam.ray(px, self.size());
        let mut best: Option<(f32, usize)> = None;
        for (i, c) in self.sim.chrs.iter().enumerate() {
            let p = m(c.pos);
            let r = (c.radius / UNITS_PER_M).max(0.35);
            // Closest approach of the ray to the vertical axis through p.
            let o2 = Vec2::new(o.x - p.x, o.z - p.z);
            let d2 = Vec2::new(d.x, d.z);
            let a = d2.length_squared();
            if a < 1e-8 {
                continue;
            }
            let t = -(o2.dot(d2)) / a;
            let closest = o2 + d2 * t;
            let y = o.y + d.y * t;
            if closest.length() <= r && y >= p.y - 0.1 && y <= p.y + 1.9 && t > 0.0 {
                if best.map_or(true, |(bt, _)| t < bt) {
                    best = Some((t, i));
                }
            }
        }
        best.map(|(_, i)| i)
    }

    /// The floor point under the cursor (PD units), if any.
    fn pick_floor(&self, px: Vec2) -> Option<Vec3> {
        let (o, d) = self.cam.ray(px, self.size());
        self.sim.level.raycast_floor(o * UNITS_PER_M, d, 20_000.0).map(|h| h.point)
    }

    /// Ctrl+LMB: send the selected bot to the floor point under the cursor with
    /// PD's own go-to (`chr_go_to_room_pos`: nearest waypoints, `nav_find_route`),
    /// or straight there if the graph has no route. With the brain on it will
    /// soon pick its own go-to again, so this is for the brain-off walker.
    fn command_walk(&mut self, px: Vec2) {
        let (Some(i), Some(p)) = (self.selected, self.pick_floor(px)) else { return };
        if !super::chraction::chr_go_to_room_pos(&mut self.sim, i, p) {
            self.sim.walk_straight_to(i, p);
        }
    }

    fn step_sim(&mut self, dt: f32) {
        let frame_dt = self.sim.config.rate.frame_seconds();
        if !self.paused {
            self.acc += dt * self.time_scale;
        }
        let mut n = 0;
        while self.acc >= frame_dt && n < 8 {
            self.sim.frame();
            self.acc -= frame_dt;
            n += 1;
        }
        if n == 8 {
            self.acc = 0.0;
        }
        while self.steps_requested > 0 {
            self.sim.frame();
            self.steps_requested -= 1;
        }
    }

    /// Skinned bodies, held guns, muzzle flashes, and the muzzle positions to hand
    /// back to the sim (PD reads the gun model's `CHRGUNFIRE` node as it was last
    /// drawn — `chr_get_gun_pos`).
    #[allow(clippy::type_complexity)]
    fn character_draws(
        &self,
    ) -> (Vec<(usize, Mat4, Vec<Mat4>, f32)>, Vec<(&'static str, Mat4)>, Vec<(&'static str, Mat4)>, Vec<(usize, usize, Vec3)>) {
        let mut chars = Vec::new();
        let mut guns = Vec::new();
        let mut flashes = Vec::new();
        let mut muzzles = Vec::new();
        for (ci, c) in self.sim.chrs.iter().enumerate() {
            let alpha = c.render_alpha();
            if alpha <= 0.0 {
                continue;
            }
            let Some(body) = self.bodies.get(c.body) else { continue };
            let (model_mtx, globals) = body.pose(c);
            let skin: Vec<Mat4> =
                globals.iter().zip(&body.model.skeleton.inverse_bind).map(|(g, ib)| *g * *ib).collect();
            chars.push((c.body, model_mtx, skin, alpha));
            for (hand, w) in c.held_weapons() {
                let Some(def) = weapons::get(w) else { continue };
                if !self.guns_loaded.contains(&w) {
                    continue;
                }
                let gun_mtx = body.gun_matrix(model_mtx, &globals, hand);
                if self.show.guns {
                    guns.push((def.name, gun_mtx));
                }
                let k = if hand == bot::Hand::Right { 0 } else { 1 };
                if c.gunfire_visible[k] {
                    flashes.push((def.name, gun_mtx));
                }
                muzzles.push((ci, k, gunpos::muzzle(gun_mtx, def)));
            }
        }
        (chars, guns, flashes, muzzles)
    }

    /// Complex's overlays: PD's own waypoint graph (yellow; orange where a link
    /// carries a `WPSEGFLAG_*` direction flag), nodes coloured by waygroup, the
    /// spawn pads and cover points. Raw data only; nothing routes on it yet.
    fn complex_overlays(&self, c: &ComplexView, dd: &mut DebugDraw) {
        let Some(st) = self.sim.stage.as_ref() else { return };
        let wp = &st.pads.waypoints;
        if self.show.pd_graph {
            for (a, w) in wp.iter().enumerate() {
                let pa = st.waypoint_pos(a);
                if !c.shown(pa.y) {
                    continue;
                }
                for &seg in &w.neighbours {
                    let b = super::pd_tiles::wpseg_get_id(seg);
                    let pb = st.waypoint_pos(b);
                    let flagged = seg & (WPSEGFLAG_OUTWARDSONLY | WPSEGFLAG_INWARDSONLY) != 0;
                    // Each link is listed at both ends; draw it once, unless this end
                    // is the one carrying a flag.
                    if !flagged && b < a && wp[b].neighbours.iter().any(|&s| super::pd_tiles::wpseg_get_id(s) == a) {
                        continue;
                    }
                    if !c.shown(pb.y) {
                        continue;
                    }
                    let col = if flagged { ORANGE } else { YELLOW };
                    dd.line(m(pa), m(pb), col, if flagged { 0.03 } else { 0.022 });
                }
                dd.cross(m(pa), 0.14, greybox::cool_hue(w.groupnum), 0.035);
            }
        }
        if self.show.our_graph {
            let ours = match self.sim.config.nav {
                NavChoice::Ours => Some(&self.sim.nav),
                NavChoice::Pd => self.sim.other_nav.as_ref(),
            };
            if let Some(g) = ours {
                const BLUE_LINK: Rgb = [0.35, 0.6, 1.0];
                const BLUE_ONE_WAY: Rgb = [0.75, 0.45, 1.0];
                for (a, w) in g.waypoints.iter().enumerate() {
                    let pa = g.waypoint_pos(a);
                    if !c.shown(pa.y) {
                        continue;
                    }
                    for &seg in &w.neighbours {
                        let b = super::pd_tiles::wpseg_get_id(seg);
                        let flagged = seg & (WPSEGFLAG_OUTWARDSONLY | WPSEGFLAG_INWARDSONLY) != 0;
                        if (!flagged && b < a) || seg & WPSEGFLAG_INWARDSONLY != 0 {
                            continue;
                        }
                        let pb = g.waypoint_pos(b);
                        if !c.shown(pb.y) {
                            continue;
                        }
                        if flagged {
                            dd.arrow(m(pa), m(pb), BLUE_ONE_WAY, 0.025);
                        } else {
                            dd.line(m(pa), m(pb), BLUE_LINK, 0.018);
                        }
                    }
                    let flags = g.pads[w.padnum].flags;
                    let col = if flags.walkdirect { [1.0, 1.0, 1.0] } else { BLUE_LINK };
                    dd.cross(m(pa), 0.1, col, 0.03);
                }
            }
        }
        if self.show.pad_drops {
            for (i, p) in st.pads.pads.iter().enumerate() {
                if !c.shown(p.pos.y) {
                    continue;
                }
                match c.pad_floor[i] {
                    Some(fy) => dd.line(m(p.pos), m(Vec3::new(p.pos.x, fy, p.pos.z)), GREY, 0.012),
                    // A tall red spike: nothing else on this level is drawn in red.
                    None => dd.line(m(p.pos) - Vec3::Y * 0.5, m(p.pos) + Vec3::Y * 2.5, RED, 0.08),
                }
            }
        }
        if self.show.spawns {
            for &pad in &st.spawn_pads {
                let p = &st.pads.pads[pad];
                if !c.shown(p.pos.y) {
                    continue;
                }
                let fy = c.pad_floor[pad].unwrap_or(p.pos.y);
                let at = m(Vec3::new(p.pos.x, fy, p.pos.z)) + Vec3::Y * 0.03;
                let look = p.look_angle();
                dd.circle(at, 0.35, WHITE, 0.035);
                dd.arrow(at, at + Vec3::new(look.sin(), 0.0, look.cos()) * 0.7, WHITE, 0.035);
            }
        }
        if self.show.cover {
            for cv in &st.pads.cover {
                if !c.shown(cv.pos.y) {
                    continue;
                }
                let at = m(cv.pos) + Vec3::Y * 0.05;
                dd.arrow(at, at + cv.dir.normalize_or_zero() * 0.45, MAGENTA, 0.02);
            }
        }
    }

    fn debug_mesh(&self, eye: Vec3) -> DebugDraw {
        let mut dd = DebugDraw::new(eye);
        if let (true, Some(c)) = (self.on_complex(), self.complex.as_ref()) {
            self.complex_overlays(c, &mut dd);
        }
        let w = 0.03;
        let sel = self.selected;
        for (i, c) in self.sim.chrs.iter().enumerate() {
            let p = m(c.pos);
            let is_sel = sel == Some(i);
            let col = c.color;
            if c.render_alpha() <= 0.0 {
                continue;
            }
            dd.circle(p + Vec3::Y * 0.02, c.radius / UNITS_PER_M, col, if is_sel { 0.05 } else { w });
            if c.is_dead() {
                continue;
            }
            if self.show.headings {
                let travel = Vec3::new(c.roty().sin(), 0.0, c.roty().cos());
                let face = Vec3::new(c.theta().sin(), 0.0, c.theta().cos());
                if c.is_moving() {
                    dd.arrow(p + Vec3::Y * 0.1, p + Vec3::Y * 0.1 + travel * 0.9, CYAN, w);
                }
                dd.arrow(p + Vec3::Y * 1.2, p + Vec3::Y * 1.2 + face * 0.8, YELLOW, w);
            }
            if self.show.aim && c.target().is_some() {
                let (from, dir) = c.aim_ray();
                let from = m(from);
                let len = self.sim.level.raycast_shoot(c.aim_ray().0, dir, 5000.0).map_or(10.0, |h| h.dist / UNITS_PER_M);
                let col = if c.is_firing() { RED } else { [0.6, 0.25, 0.2] };
                dd.line(from, from + dir * len, col, if c.is_firing() { 0.025 } else { 0.012 });
            }
            if self.show.cones && (is_sel || sel.is_none()) {
                let half = bot::TRIGGER_FOV_HALF;
                dd.arc(p + Vec3::Y * 0.05, c.theta(), half, 2.0, ORANGE, 0.015, true);
            }
            if let Some(t) = c.target() {
                if let Some(tc) = self.sim.chrs.get(t) {
                    let tp = m(tc.pos);
                    if self.show.targets {
                        let insight = c.target_in_sight();
                        let col = if insight { GREEN } else { GREY };
                        dd.line(p + Vec3::Y * 1.0, tp + Vec3::Y * 1.0, col, if insight { 0.02 } else { 0.01 });
                    }
                    if self.show.bands && is_sel {
                        if let Some(band) = c.dist_band() {
                            let mode = c.dist_mode();
                            let hl = |on: bool, base: Rgb| if on { base } else { [base[0] * 0.4, base[1] * 0.4, base[2] * 0.4] };
                            dd.circle(tp + Vec3::Y * 0.03, band.min / UNITS_PER_M, hl(mode == Some(botcmd::DistMode::Backup), RED), 0.02);
                            dd.circle(tp + Vec3::Y * 0.03, band.max / UNITS_PER_M, hl(mode == Some(botcmd::DistMode::Ok), GREEN), 0.02);
                        }
                    }
                }
            }
            if self.show.paths {
                if let Some(goal) = c.gopos_target_in(&self.sim) {
                    let g = m(goal);
                    dd.line(p + Vec3::Y * 0.05, g + Vec3::Y * 0.05, MAGENTA, 0.012);
                    dd.cross(g + Vec3::Y * 0.1, 0.12, MAGENTA, 0.02);
                }
                // The selected bot's loaded route: the waypoints still ahead in its
                // 6-slot array, then the go-to's end point.
                if is_sel && c.actiontype == super::chr::Act::GoPos {
                    let gp = &c.act_gopos;
                    let mut prev = p + Vec3::Y * 0.3;
                    for &wpt in gp.waypoints.iter().skip(gp.curindex) {
                        let q = m(self.sim.nav.waypoint_pos(wpt));
                        dd.line(prev, q, [1.0, 0.45, 0.8], 0.045);
                        dd.circle(q, 0.18, [1.0, 0.45, 0.8], 0.03);
                        prev = q;
                    }
                    dd.line(prev, m(gp.endpos) + Vec3::Y * 0.3, [1.0, 0.45, 0.8], 0.02);
                }
            }
        }
        if self.show.waypoints && !self.on_complex() {
            let nav = &self.sim.nav;
            for (a, w) in nav.waypoints.iter().enumerate() {
                for &seg in &w.neighbours {
                    let b = super::pd_tiles::wpseg_get_id(seg);
                    if b > a {
                        dd.line(m(nav.waypoint_pos(a)), m(nav.waypoint_pos(b)), [0.25, 0.3, 0.45], 0.01);
                    }
                }
                dd.cross(m(nav.waypoint_pos(a)), 0.06, [0.5, 0.6, 0.9], 0.015);
            }
            for (p, look) in &self.sim.spawn_pads {
                let p = m(*p) + Vec3::Y * 0.03;
                dd.circle(p, 0.3, [0.9, 0.9, 0.3], 0.015);
                dd.arrow(p, p + Vec3::new(look.sin(), 0.0, look.cos()) * 0.5, [0.9, 0.9, 0.3], 0.015);
            }
        }
        if self.show.tracers {
            for s in &self.sim.shots {
                let fade = 1.0 - s.age as f32 / super::sim::SHOT_LIFETIME as f32;
                let base = if s.hit_chr.is_some() { RED } else { WHITE };
                let col = [base[0] * fade, base[1] * fade, base[2] * fade];
                dd.line(m(s.from), m(s.to), col, 0.012);
            }
        }
        dd
    }

    fn ui(&mut self) -> Option<EguiFrame> {
        let window = self.window.as_ref()?.clone();
        let size = self.size();
        let state = self.egui_state.as_mut()?;
        let raw = state.take_egui_input(&window);
        let ppp = window.scale_factor() as f32;

        // Snapshot the things the closure needs, and collect actions to apply after.
        let mut reset: Option<usize> = None;
        let mut step = false;
        let mut paused = self.paused;
        let mut time_scale = self.time_scale;
        let mut rate = self.sim.config.rate;
        let mut new_bot_count = self.new_bot_count;
        let mut follow = self.follow;
        let mut selected = self.selected;
        let mut brains = self.sim.brains;
        let mut edits: Vec<(usize, bot::Difficulty, Option<WeaponId>)> = Vec::new();
        let mut level = self.sim.config.level;
        let mut nav = self.sim.config.nav;
        let nav_sizes = (self.sim.nav.waypoints.len(), self.sim.other_nav.as_ref().map_or(0, |g| g.waypoints.len()));
        let on_complex = level == LevelChoice::Complex;
        let level_error = self.level_error.clone();
        let mut cx = self.complex.as_ref().map(|c| (c.clip_on, c.clip_y, c.room_tint));
        let complex = self.complex.as_ref();
        let show = &mut self.show;
        let sim = &self.sim;
        let cam = &self.cam;

        let out = self.egui_ctx.run(raw, |ctx| {
            egui::SidePanel::left("controls").resizable(false).default_width(230.0).show(ctx, |ui| {
                ui.heading("PD ARENA");
                ui.label(egui::RichText::new("Perfect Dark simulants, ported").weak());
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Level");
                    egui::ComboBox::from_id_salt("level").selected_text(level.label()).show_ui(ui, |ui| {
                        for l in [LevelChoice::Arena, LevelChoice::Complex] {
                            ui.selectable_value(&mut level, l, l.label());
                        }
                    });
                });
                if let Some(e) = &level_error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 90), e);
                }
                ui.separator();
                if let (true, Some(c), Some((clip_on, clip_y, tint))) = (on_complex, complex, cx.as_mut()) {
                    ui.label(egui::RichText::new("COMPLEX (stage ref)").strong());
                    ui.horizontal(|ui| {
                        ui.label("Route with");
                        egui::ComboBox::from_id_salt("nav").selected_text(nav.label()).show_ui(ui, |ui| {
                            for n in [NavChoice::Pd, NavChoice::Ours] {
                                ui.selectable_value(&mut nav, n, n.label());
                            }
                        });
                    });
                    let (active, other) = nav_sizes;
                    let (pd_n, our_n) = if nav == NavChoice::Pd { (active, other) } else { (other, active) };
                    ui.label(
                        egui::RichText::new(format!("PD graph {pd_n} waypoints · ours {our_n} (generated from the tiles)"))
                            .weak()
                            .small(),
                    );
                    egui::Grid::new("cx_stats").striped(true).num_columns(2).show(ui, |ui| {
                        for (k, v) in &c.stats {
                            ui.label(egui::RichText::new(*k).weak());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                    ui.separator();
                    ui.checkbox(clip_on, "hide floors above");
                    ui.add_enabled(
                        *clip_on,
                        egui::Slider::new(clip_y, c.lo.y..=c.hi.y).suffix(" cm").step_by(10.0),
                    );
                    ui.checkbox(tint, "tint by room");
                    ui.label(
                        egui::RichText::new("floors: violet pit · sand ground · teal 1st · blue top\norange ramp · pale = see-through wall")
                            .weak()
                            .small(),
                    );
                    ui.separator();
                    ui.label("Overlays");
                    ui.checkbox(&mut show.pd_graph, "PD waypoint graph (yellow; orange = one-way flag; node colour = waygroup)");
                    ui.checkbox(&mut show.our_graph, "our graph (blue; violet arrow = one-way; white node = walk-direct)");
                    ui.checkbox(&mut show.spawns, "spawn pads (white)");
                    ui.checkbox(&mut show.cover, "cover points (magenta)");
                    ui.checkbox(&mut show.pad_drops, "every pad, dropped to its floor (red spike = no floor)");
                    ui.checkbox(&mut show.room_labels, "room numbers");
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    if ui.button(if paused { "▶ Run" } else { "⏸ Pause" }).clicked() {
                        paused = !paused;
                    }
                    if ui.button("Step ⏭").clicked() {
                        step = true;
                    }
                });
                ui.add(egui::Slider::new(&mut time_scale, 0.05..=2.0).logarithmic(true).text("speed"));
                ui.horizontal(|ui| {
                    ui.label("Sim rate");
                    egui::ComboBox::from_id_salt("rate").selected_text(rate.label()).show_ui(ui, |ui| {
                        for r in FrameRate::ALL {
                            ui.selectable_value(&mut rate, r, r.label());
                        }
                    });
                });
                ui.label(format!(
                    "frame {}  lvframe60 {}  ({:.1} s)",
                    sim.frame_count,
                    sim.g.lvframe60,
                    sim.g.lvframe60 as f32 / 60.0
                ));
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut new_bot_count).range(1..=8).prefix("bots "));
                    if ui.button("Reset match").clicked() {
                        reset = Some(new_bot_count);
                    }
                });
                ui.checkbox(&mut brains, "bot brains (off = walker mode)");
                if !brains {
                    ui.label(
                        egui::RichText::new("Ctrl+LMB a floor: the selected bot goes there by PD's route").weak().small(),
                    );
                }
                ui.separator();
                ui.label("Scoreboard");
                egui::Grid::new("score").striped(true).show(ui, |ui| {
                    ui.label("");
                    ui.label("kills");
                    ui.label("deaths");
                    ui.label("hp");
                    ui.end_row();
                    for (i, c) in sim.chrs.iter().enumerate() {
                        let col = egui::Color32::from_rgb(
                            (c.color[0] * 255.0) as u8,
                            (c.color[1] * 255.0) as u8,
                            (c.color[2] * 255.0) as u8,
                        );
                        if ui.selectable_label(selected == Some(i), egui::RichText::new(&c.name).color(col)).clicked()
                        {
                            selected = Some(i);
                        }
                        ui.label(c.kills.to_string());
                        ui.label(c.deaths.to_string());
                        ui.label(format!("{:.1}", c.health()));
                        ui.end_row();
                    }
                });
                if !sim.feed.is_empty() {
                    ui.separator();
                    for (t, line) in sim.feed.iter().rev().take(5) {
                        ui.label(egui::RichText::new(format!("{:>5.1}s  {line}", *t as f32 / 60.0)).small());
                    }
                }
                ui.separator();
                ui.label("Overlays");
                if !on_complex {
                    ui.checkbox(&mut show.waypoints, "waypoint graph + spawn pads");
                }
                ui.checkbox(&mut show.headings, "travel (cyan) / facing (yellow)");
                ui.checkbox(&mut show.aim, "aim ray (red = trigger held)");
                ui.checkbox(&mut show.targets, "target link (green = in sight)");
                ui.checkbox(&mut show.cones, "trigger cone ±63°");
                ui.checkbox(&mut show.bands, "dist band around target");
                ui.checkbox(&mut show.paths, "go-to goal (magenta) + selected bot's route (pink)");
                ui.checkbox(&mut show.tracers, "shot tracers");
                ui.checkbox(&mut show.labels, "labels");
                ui.checkbox(&mut show.guns, "guns");
                ui.checkbox(&mut show.xray, "overlays through walls");
                ui.checkbox(&mut follow, "camera follows selected (F)");
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        "RMB drag orbit · MMB/Shift+RMB pan · wheel zoom\nWASD slide · LMB select · Ctrl+LMB send selected\nSpace pause · . step · R reset · F follow",
                    )
                    .weak()
                    .small(),
                );
            });

            if let Some(i) = selected {
                if let Some(c) = sim.chrs.get(i) {
                    egui::SidePanel::right("inspector").resizable(true).default_width(300.0).show(ctx, |ui| {
                        ui.heading(&c.name);
                        let mut diff = c.difficulty();
                        let mut weapon = c.loadout();
                        ui.horizontal(|ui| {
                            ui.label("difficulty");
                            egui::ComboBox::from_id_salt("diff").selected_text(diff.label()).show_ui(ui, |ui| {
                                for d in bot::Difficulty::ALL {
                                    ui.selectable_value(&mut diff, d, d.label());
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("weapon");
                            let label = weapon.and_then(weapons::get).map_or("Unarmed", |w| w.name);
                            egui::ComboBox::from_id_salt("weap").selected_text(label).show_ui(ui, |ui| {
                                ui.selectable_value(&mut weapon, None, "Unarmed");
                                for w in weapons::WEAPONS {
                                    ui.selectable_value(&mut weapon, Some(w.id), w.name);
                                }
                            });
                        });
                        if diff != c.difficulty() || weapon != c.loadout() {
                            edits.push((i, diff, weapon));
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            egui::Grid::new("fields").striped(true).num_columns(2).show(ui, |ui| {
                                for (k, v) in c.debug_rows(sim) {
                                    ui.label(egui::RichText::new(k).monospace().weak());
                                    ui.label(egui::RichText::new(v).monospace());
                                    ui.end_row();
                                }
                            });
                        });
                    });
                }
            }

            if let (true, true, Some(c)) = (on_complex, show.room_labels, complex) {
                let painter = ctx.layer_painter(egui::LayerId::background());
                for (room, at) in &c.room_centres {
                    if !c.shown(at.y) {
                        continue;
                    }
                    if let Some(px) = cam.to_screen(m(*at) + Vec3::Y * 0.3, size) {
                        painter.text(
                            egui::pos2(px.x / ppp, px.y / ppp),
                            egui::Align2::CENTER_CENTER,
                            format!("{room:02X}"),
                            egui::FontId::monospace(13.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }

            if show.labels {
                let painter = ctx.layer_painter(egui::LayerId::background());
                for (i, c) in sim.chrs.iter().enumerate() {
                    if c.render_alpha() <= 0.0 {
                        continue;
                    }
                    let head = m(c.pos) + Vec3::Y * 2.05;
                    if let Some(px) = cam.to_screen(head, size) {
                        let pos = egui::pos2(px.x / ppp, px.y / ppp);
                        let col = egui::Color32::from_rgb(
                            (c.color[0] * 255.0) as u8,
                            (c.color[1] * 255.0) as u8,
                            (c.color[2] * 255.0) as u8,
                        );
                        let text = format!("{}{}\n{}", if selected == Some(i) { "▶ " } else { "" }, c.name, c.label_line());
                        painter.text(
                            pos,
                            egui::Align2::CENTER_BOTTOM,
                            text,
                            egui::FontId::monospace(12.0),
                            col,
                        );
                    }
                }
            }
        });

        state.handle_platform_output(&window, out.platform_output);
        let paint_jobs = self.egui_ctx.tessellate(out.shapes, out.pixels_per_point);

        self.paused = paused;
        self.time_scale = time_scale;
        self.new_bot_count = new_bot_count;
        self.follow = follow;
        self.selected = selected;
        self.sim.brains = brains;
        if step {
            self.steps_requested += 1;
            self.paused = true;
        }
        if rate != self.sim.config.rate {
            self.sim.config.rate = rate;
            self.acc = 0.0;
        }
        for (i, d, w) in edits {
            self.sim.set_bot_config(i, d, w);
        }
        if let Some(n) = reset {
            self.sim.reset(n);
            if self.selected.map_or(false, |s| s >= n) {
                self.selected = Some(0);
            }
        }
        if let (Some(c), Some((clip_on, clip_y, tint))) = (self.complex.as_mut(), cx) {
            c.clip_on = clip_on;
            c.clip_y = clip_y;
            c.room_tint = tint;
        }
        if level != self.sim.config.level {
            self.set_level(level);
        } else if nav != self.sim.config.nav {
            self.set_nav(nav);
        }
        Some(EguiFrame { textures_delta: out.textures_delta, paint_jobs, pixels_per_point: out.pixels_per_point })
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.1);
        self.last = now;

        let fwd = self.keys.contains(&KeyCode::KeyW) as i32 - self.keys.contains(&KeyCode::KeyS) as i32;
        let side = self.keys.contains(&KeyCode::KeyD) as i32 - self.keys.contains(&KeyCode::KeyA) as i32;
        if fwd != 0 || side != 0 {
            self.follow = false;
            self.cam.slide(fwd as f32, side as f32, dt);
        }

        self.step_sim(dt);

        if self.follow {
            if let Some(c) = self.selected.and_then(|i| self.sim.chrs.get(i)) {
                let goal = m(c.pos) + Vec3::Y * 1.0;
                self.cam.target = self.cam.target.lerp(goal, (dt * 6.0).min(1.0));
            }
        }

        let (chars, guns, flashes, muzzles) = self.character_draws();
        for c in &mut self.sim.chrs {
            c.gunpos_rendered = [None; 2];
        }
        for (ci, hand, p) in muzzles {
            self.sim.chrs[ci].gunpos_rendered[hand] = Some(p);
        }
        let eye = self.cam.eye();
        let dd = self.debug_mesh(eye);
        let egui_frame = self.ui();
        if self.on_complex() {
            self.upload_greybox_if_changed();
        }

        let Some(renderer) = self.renderer.as_mut() else { return };
        let vp = self.cam.view_proj(renderer.aspect());
        let inst: Vec<(usize, Mat4, Vec<Mat4>, f32, &[f32])> =
            chars.into_iter().map(|(b, mm, j, a)| (b, mm, j, a, &[][..])).collect();
        renderer.set_character_instances(&inst);
        let gun_draws: Vec<(&'static str, Mat4)> = guns.into_iter().map(|(k, w)| (k, vp * w)).collect();
        renderer.set_enemy_weapon_draws(&gun_draws);
        let flash_draws: Vec<(&'static str, Mat4)> = flashes.into_iter().map(|(k, w)| (k, vp * w)).collect();
        renderer.set_enemy_muzzle_draws(&flash_draws);
        if self.show.xray {
            renderer.set_spark_mesh(None);
            renderer.set_gizmo_mesh((!dd.is_empty()).then_some(&dd.mesh));
        } else {
            renderer.set_gizmo_mesh(None);
            renderer.set_spark_mesh((!dd.is_empty()).then_some(&dd.mesh));
        }
        renderer.render(vp, egui_frame);
    }
}

impl ApplicationHandler for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("PD ARENA — Perfect Dark simulant spike")
            .with_inner_size(winit::dpi::LogicalSize::new(1600.0, 900.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let mut renderer = pollster::block_on(Renderer::new(window.clone()));
        renderer.set_grid_mode(true);
        renderer.set_crosshair_offset(None);
        renderer.set_lighting(&[], ([1.0, 1.0, 1.0], 1.0), false);
        self.egui_state = Some(egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            None,
            None,
            None,
        ));
        self.renderer = Some(renderer);
        self.window = Some(window);
        self.upload_level();
        self.load_assets();
        self.last = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let consumed = match (self.egui_state.as_mut(), self.window.as_ref()) {
            (Some(s), Some(w)) => s.on_window_event(w, &event).consumed,
            _ => false,
        };
        let over_ui = self.egui_ctx.is_pointer_over_area();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                match event.state {
                    ElementState::Pressed => {
                        let fresh = self.keys.insert(code);
                        if consumed || self.egui_ctx.wants_keyboard_input() || !fresh {
                            return;
                        }
                        match code {
                            KeyCode::Space => self.paused = !self.paused,
                            KeyCode::Period => {
                                self.paused = true;
                                self.steps_requested += 1;
                            }
                            KeyCode::KeyR => {
                                let n = self.new_bot_count;
                                self.sim.reset(n);
                            }
                            KeyCode::KeyF => self.follow = !self.follow,
                            KeyCode::Escape => self.selected = None,
                            _ => {}
                        }
                    }
                    ElementState::Released => {
                        self.keys.remove(&code);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Vec2::new(position.x as f32, position.y as f32);
                let d = p - self.cursor;
                self.cursor = p;
                if let Some((button, _)) = self.drag {
                    let shift = self.keys.contains(&KeyCode::ShiftLeft) || self.keys.contains(&KeyCode::ShiftRight);
                    match button {
                        MouseButton::Right if !shift => self.cam.orbit(d.x, d.y),
                        MouseButton::Right | MouseButton::Middle => {
                            self.follow = false;
                            self.cam.pan(d.x, d.y);
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match state {
                ElementState::Pressed => {
                    if over_ui || consumed {
                        return;
                    }
                    match button {
                        MouseButton::Left => self.press_at = Some(self.cursor),
                        MouseButton::Right | MouseButton::Middle => self.drag = Some((button, self.cursor)),
                        _ => {}
                    }
                }
                ElementState::Released => {
                    if button == MouseButton::Left {
                        if let Some(at) = self.press_at.take() {
                            if at.distance(self.cursor) < 5.0 {
                                let ctrl = self.keys.contains(&KeyCode::ControlLeft) || self.keys.contains(&KeyCode::ControlRight);
                                if ctrl {
                                    self.command_walk(self.cursor);
                                } else {
                                    self.selected = self.pick(self.cursor);
                                }
                            }
                        }
                    }
                    if self.drag.map_or(false, |(b, _)| b == button) {
                        self.drag = None;
                    }
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                if over_ui {
                    return;
                }
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                self.cam.zoom(steps);
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
