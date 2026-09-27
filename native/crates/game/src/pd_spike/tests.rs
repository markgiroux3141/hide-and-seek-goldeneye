//! Headless behaviour tests: the match runs with no window and no clip data.

use glam::Vec2;

use super::sim::{LevelChoice, Sim, SimConfig};

/// The arena match these tests were written against, whatever `PD_LEVEL` says.
fn arena_config() -> SimConfig {
    let mut cfg = SimConfig::from_env();
    cfg.level = LevelChoice::Arena;
    cfg
}

fn sim(n: usize) -> Sim {
    let mut cfg = arena_config();
    cfg.bots.truncate(1);
    let first = cfg.bots[0];
    cfg.bots = vec![first; n];
    Sim::new(cfg)
}

#[test]
fn a_match_runs_and_bots_kill_each_other() {
    let mut s = sim(4);
    for _ in 0..60 * 90 {
        s.frame();
    }
    let kills: u32 = s.chrs.iter().map(|c| c.kills).sum();
    assert!(kills > 0, "no kills in 90 s");
}

/// Diagnostic timeline — `cargo test --release -p game pd_spike::tests::probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn probe() {
    let mut s = sim(4);
    let mut shots = 0usize;
    let mut hits = 0usize;
    let mut moving = vec![0u32; 4];
    let mut backwards = vec![0u32; 4];
    let mut frames = 0u32;
    for f in 0..60 * 60 {
        s.frame();
        frames += 1;
        for s2 in &s.shots {
            if s2.age == 0 {
                shots += 1;
                if s2.hit_chr.is_some() {
                    hits += 1;
                }
            }
        }
        for (k, c) in s.chrs.iter().enumerate() {
            if c.is_moving() {
                moving[k] += 1;
            }
            if c.model.speed < 0.0 {
                backwards[k] += 1;
            }
        }
        if f % 60 == 0 {
            let line: Vec<String> = s
                .chrs
                .iter()
                .map(|c| {
                    format!(
                        "{:>10} ({:>5.0},{:>5.0}) {:<8} {:>5}cm hp{:.1} {}",
                        c.label_line(),
                        c.pos.x,
                        c.pos.z,
                        format!("{:?}", c.actiontype),
                        c.last_dist as i32,
                        c.health(),
                        c.target.map_or("-".into(), |t| t.to_string())
                    )
                })
                .collect();
            println!("t={:>3}s | {}", f / 60, line.join(" | "));
        }
    }
    println!("shots {shots} hits {hits} ({:.0}%)", 100.0 * hits as f32 / shots.max(1) as f32);
    for (k, c) in s.chrs.iter().enumerate() {
        println!(
            "{}: K{} D{}  moving {:.0}%  backwards-anim {:.0}%",
            c.name,
            c.kills,
            c.deaths,
            100.0 * moving[k] as f32 / frames as f32,
            100.0 * backwards[k] as f32 / frames as f32
        );
    }
}

/// PD scales the clip's root translation by the body's `animscale`; with that and
/// nothing else, every body's lowest vertex should sit on the floor — standing and
/// mid-stride. (Loads the real GLBs; CPU skinning, no GPU.)
#[test]
fn every_body_stands_on_the_floor_with_pds_animscale() {
    use super::anims::*;
    use super::model::{evaluate, Anim, CallbackJoints, ClipBank, JointFx};
    let dir = format!("{}/../../assets/enemies/pd", env!("CARGO_MANIFEST_DIR"));
    let s = sim(6);
    for c in &s.chrs {
        let body = super::sim::BODIES[c.body];
        let model = engine::skeletal::gltf_skin::load(&format!("{dir}/characters/{body}.glb")).unwrap();
        let bank = ClipBank::load(&format!("{dir}/bot_anims"), &model.skeleton).unwrap();
        let joints = CallbackJoints::resolve(&model.skeleton).unwrap();
        for (anim, frames) in [(ANIM_0002, 35..41), (ANIM_0031, 0..21), (ANIM_0055, 0..23)] {
            let mut lo = f32::INFINITY;
            for f in frames {
                let mut a = Anim::default();
                a.set_animation(anim, f as f32, 0.5, 0.0);
                let g = evaluate(&a, &bank, &model.skeleton, &joints, &JointFx::default(), 0.0, c.animscale);
                let skin: Vec<_> = g.iter().zip(&model.skeleton.inverse_bind).map(|(g, ib)| *g * *ib).collect();
                lo = lo.min(model.skinned_y_extent(&skin).0 * 0.000_832);
            }
            println!("{body:>13} {:<28} lowest point {:+.3} m", info(anim).name, lo);
            assert!(lo.abs() < 0.06, "{body} {} floats/sinks {lo:.3} m", info(anim).name);
        }
    }
}

/// The leg twist: the model is drawn at `theta - angleoffset` (legs towards the
/// travel direction) and the waist joint turns back by `+angleoffset`, so the
/// shoulders must face `theta` while the hips face `theta - angleoffset`.
#[test]
fn the_waist_counter_twist_squares_the_shoulders_to_theta() {
    use super::anims::*;
    use super::model::{evaluate, Anim, CallbackJoints, ClipBank, JointFx, JointRot};
    use glam::{Mat4, Vec3};
    let dir = format!("{}/../../assets/enemies/pd", env!("CARGO_MANIFEST_DIR"));
    let model = engine::skeletal::gltf_skin::load(&format!("{dir}/characters/pd_a51guard.glb")).unwrap();
    let bank = ClipBank::load(&format!("{dir}/bot_anims"), &model.skeleton).unwrap();
    let j = CallbackJoints::resolve(&model.skeleton).unwrap();
    let sk = &model.skeleton;
    let (lsh, rsh) = (j.lshoulder, j.rshoulder);
    let (lhip, rhip) = (sk.index_of("Bone_10").unwrap(), sk.index_of("Bone_11").unwrap());
    let facing = |g: &[Mat4], l: usize, r: usize, yaw: f32| -> f32 {
        // Across the body left→right, rotated a quarter turn = facing direction.
        let m = Mat4::from_rotation_y(yaw);
        let a = m.transform_point3(g[l].w_axis.truncate());
        let b = m.transform_point3(g[r].w_axis.truncate());
        let across = b - a; // character's right is -X, so left→right runs -X in model space
        let fwd = Vec3::new(-across.z, 0.0, across.x);
        fwd.x.atan2(fwd.z)
    };
    let mut a = Anim::default();
    a.set_animation(ANIM_0002, 35.0, 0.05, 0.0);
    let theta = 0.7f32;
    let off = 0.9f32; // ~52°
    let fx = JointFx { waist: JointRot { x: 0.0, y: off, z: 0.0 }, aimangle: theta, ..Default::default() };
    let g = evaluate(&a, &bank, sk, &j, &fx, theta - off, 0.93);
    let shoulders = facing(&g, lsh, rsh, theta - off);
    let hips = facing(&g, lhip, rhip, theta - off);
    let base_sh = facing(&evaluate(&a, &bank, sk, &j, &JointFx::default(), 0.0, 0.93), lsh, rsh, 0.0);
    let base_hip = facing(&evaluate(&a, &bank, sk, &j, &JointFx::default(), 0.0, 0.93), lhip, rhip, 0.0);
    println!("shoulders {:.1}° (want {:.1}°)  hips {:.1}° (want {:.1}°)",
        (shoulders - base_sh).to_degrees(), theta.to_degrees(), (hips - base_hip).to_degrees(), (theta - off).to_degrees());
    assert!(((shoulders - base_sh) - theta).abs() < 0.05);
    assert!(((hips - base_hip) - (theta - off)).abs() < 0.05);
}

#[test]
fn every_spike_gun_and_flash_loads() {
    let dir = format!("{}/../../assets/weapons", env!("CARGO_MANIFEST_DIR"));
    for w in super::weapons::WEAPONS {
        crate::combat::load_gun(&format!("{dir}/{}", w.tp_glb)).unwrap_or_else(|e| panic!("{}: {e}", w.name));
        crate::combat::load_flash(&format!("{dir}/{}", w.flash_glb)).unwrap_or_else(|e| panic!("{}: {e}", w.name));
    }
}

/// `3600 / 750` truncates to 4 ticks per shot, and the burst logic only runs for
/// single-shot weapons — so an AR34 bot fires every 4th tick, uninterrupted, for as
/// long as its trigger conditions hold (`bot.c:3685`).
#[test]
fn an_automatic_bot_fires_every_fourth_tick_with_no_burst_pause() {
    let mut cfg = arena_config();
    cfg.bots = vec![super::chr::BotConfig { difficulty: super::bot::Difficulty::Dark, weapon: Some(super::weapons::AR34), dual: false }; 2];
    let mut s = Sim::new(cfg);
    let mut last: Option<i32> = None;
    let mut gaps = Vec::new();
    for _ in 0..60 * 20 {
        s.frame();
        for sh in &s.shots {
            if sh.age == 0 && sh.shooter == 0 {
                if let Some(l) = last {
                    gaps.push(s.g.lvframe60 - l);
                }
                last = Some(s.g.lvframe60);
            }
        }
    }
    let min = *gaps.iter().min().expect("bot 0 fired");
    let longest_run = gaps.split(|&g| g != 4).map(|r| r.len()).max().unwrap();
    println!("gaps min {min}, longest 4-tick run {longest_run}");
    assert_eq!(min, 4);
    assert!(longest_run >= 5, "an automatic should hold the trigger past 3 rounds");
}

/// `forcezerominspeed` keeps a floor under the zeroing speed even when fully
/// zeroed: a NormalSim's aim never settles below ±5° of residual error.
#[test]
fn a_zeroed_normalsim_still_carries_up_to_five_degrees_of_error() {
    // Watch both bots for up to 2 minutes: fights can end before either is fully
    // zeroed (180 ticks in sight), so one seed's first 30 s may never get there.
    let mut s = sim(2);
    let (mut worst, mut zeroed_frames) = (0.0f32, 0);
    for _ in 0..60 * 120 {
        s.frame();
        for c in &s.chrs {
            let a = &c.aibot;
            if a.targetinsight && a.curzerotimer60 >= 180.0 {
                worst = worst.max(a.zeroangle.abs());
                zeroed_frames += 1;
            }
        }
    }
    println!("worst residual zeroangle {:.2}° over {zeroed_frames} fully-zeroed frames", worst.to_degrees());
    assert!(zeroed_frames > 0, "no bot ever stayed zeroed for 180 ticks");
    assert!(worst > 0.0 && worst.to_degrees() <= 5.01);
}

// ─── Complex ─────────────────────────────────────────────────────────────────

/// Complex with one NormalSim (brainless on Complex until stage 3).
fn complex_sim() -> Sim {
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    cfg.bots.truncate(1);
    Sim::try_new(cfg).expect("reference/pd-decomp (or PD_DECOMP_DIR) must be present")
}

/// Walk every link of PD's own graph (each allowed direction) with the real
/// movement code and print what happened.
/// `cargo test --release -p game --lib pd_spike::tests::probe_walk_pd_graph -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_walk_pd_graph() {
    use super::walk::{pd_links, pd_pad_floor, walk, Outcome};
    let mut s = complex_sim();
    let stage = s.stage.clone().unwrap();
    let links = pd_links(&stage);
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut slow = 0;
    let mut ledges = 0;
    let mut worst_drop = 0.0f32;
    let mut float_hist = [0usize; 5]; // max (manground - ground) on walks that stayed under a 69 cm drop
    for &(a, b, one_way) in &links {
        let (pa, pb) = (stage.waypoint_pos(a), stage.waypoint_pos(b));
        let to_floor = pd_pad_floor(&s.level, pb).map(|f| f.0);
        let w = walk(&mut s, 0, pa, pb, to_floor);
        *counts.entry(format!("{:?}{}", w.outcome, if one_way { " (one-way)" } else { "" })).or_default() += 1;
        if w.max_drop > 69.0 {
            ledges += 1;
        } else {
            float_hist[match w.max_drop {
                d if d < 1.0 => 0,
                d if d < 10.0 => 1,
                d if d < 25.0 => 2,
                d if d < 50.0 => 3,
                _ => 4,
            }] += 1;
        }
        worst_drop = worst_drop.max(if one_way { 0.0 } else { w.max_drop });
        if w.outcome == Outcome::Arrived && w.ticks > w.nominal * 2 {
            slow += 1;
        }
        if w.outcome == Outcome::Stuck && w.blocked_frames > 30 {
            // What's touching the chr where it stopped.
            let c = &s.chrs[0];
            let prop_y = c.prop_pos().y;
            for (poly, v) in s.level.walls_touching(c.prop_pos(), c.radius, c.pos.y + c.height - prop_y, c.pos.y + 20.0 - prop_y) {
                let p = &s.level.geom.polys[poly];
                let (lo, hi) = (p.min_y(), p.max_y());
                println!(
                    "      touching wall poly {poly} room {:?} edge {v} y {lo}..{hi} verts {:?}",
                    p.room,
                    p.verts.iter().map(|q| (q.x as i32, q.y as i32, q.z as i32)).collect::<Vec<_>>()
                );
            }
        }
        if w.outcome != Outcome::Arrived || w.max_drop > 69.0 {
            println!(
                "{a:#04x} -> {b:#04x}{} {:?}: {} of {} ticks, drop {:.0} cm, blocked {} frames, ended ({:.0}, {:.0}, {:.0})",
                if one_way { " one-way" } else { "" },
                w.outcome,
                w.ticks,
                w.nominal,
                w.max_drop,
                w.blocked_frames,
                w.end.x,
                w.end.y,
                w.end.z
            );
        }
    }
    println!("\n{} directed links walked", links.len());
    for (k, n) in &counts {
        println!("  {k:<24} {n}");
    }
    println!("  arrived but > 2x nominal time: {slow}");
    println!("  walks with a drop > 69 cm: {ledges}; worst drop on a two-way link: {worst_drop:.0} cm");
    println!("  the rest, by how far manground got above the floor: <1 cm {}, 1-10 {}, 10-25 {}, 25-50 {}, 50-69 {}",
        float_hist[0], float_hist[1], float_hist[2], float_hist[3], float_hist[4]);
}

/// Stage 2's check: with the ported collision and ground code, a bot walks every
/// link of PD's own Complex graph (each allowed direction: 401 walks, ladders,
/// the crawl space and ledge drops included), except exactly these three:
/// * `0x01 → 0x03` and `0x88 → 0x8a` climb 276 cm straight out of the pit. PD's
///   data marks them two-way, but the pit wall is sheer, so they only work
///   downward, and a PD bot would strand on them too. The pit's way out is
///   `0x01 → 0x84 →` the corridor at z > 1706.
/// * `0x0f → 0x0e` starts at a walkway-edge pad; a spawn there lands on the
///   floor below (`chr_adjust_pos_for_spawn` + the floor under the centre). A
///   bot passing `0x0f` on a route is on the walkway, and `0x0e → 0x0f` walks.
#[test]
fn a_bot_walks_every_link_of_pds_complex_graph_but_three_known_ones() {
    use super::walk::{pd_links, pd_pad_floor, walk, Outcome};
    let mut s = complex_sim();
    let stage = s.stage.clone().unwrap();
    let mut failed = Vec::new();
    for (a, b, _) in pd_links(&stage) {
        let (pa, pb) = (stage.waypoint_pos(a), stage.waypoint_pos(b));
        let to_floor = pd_pad_floor(&s.level, pb).map(|f| f.0);
        let w = walk(&mut s, 0, pa, pb, to_floor);
        if w.outcome != Outcome::Arrived {
            failed.push((a, b, w.outcome));
        }
    }
    let known = [(0x01, 0x03), (0x0f, 0x0e), (0x88, 0x8a)];
    let got: Vec<(usize, usize)> = failed.iter().map(|&(a, b, _)| (a, b)).collect();
    assert_eq!(got, known, "{failed:x?}");
}

/// PD requires "any two directly connected waypoints must be in the same or
/// neighbouring rooms" (`padhalllv.c:67`). Our substitutes (a pad's room is the
/// floor room under it; rooms neighbour when their polygons share an edge, see
/// `infer_room_neighbours`) honour that on PD's own graph, except for 6 links
/// (12 directed) that pass through a small doorway room (1–3 floor tiles) between
/// their two ends. PD's portals may join those rooms directly; without the BSP
/// that can't be checked. Nothing worse than one doorway room apart is allowed.
#[test]
fn every_pd_waypoint_link_joins_the_same_or_neighbouring_rooms() {
    use super::pd_nav::NavGraph;
    use super::pd_tiles::wpseg_get_id;
    let s = complex_sim();
    let nav = NavGraph::from_pd_stage(s.stage.as_ref().unwrap(), &s.level);
    let roomless: Vec<usize> = (0..nav.waypoints.len()).filter(|&w| nav.waypoint_room(w).is_none()).collect();
    assert!(roomless.is_empty(), "waypoints with no room: {roomless:x?}");
    let mut via_doorway = 0;
    for (a, w) in nav.waypoints.iter().enumerate() {
        for &seg in &w.neighbours {
            let b = wpseg_get_id(seg);
            let (ra, rb) = (nav.waypoint_room(a).unwrap(), nav.waypoint_room(b).unwrap());
            if ra == rb || s.level.rooms_are_neighbours(ra, rb) {
                continue;
            }
            let doorway = s.level.room_neighbours(ra).into_iter().any(|r| {
                s.level.rooms_are_neighbours(r, rb) && s.level.geom.polys.iter().filter(|p| p.room == Some(r) && p.floor).count() <= 3
            });
            assert!(doorway, "link {a:#x} -> {b:#x} joins rooms {ra:#x} and {rb:#x}, which are more than a doorway apart");
            via_doorway += 1;
        }
    }
    assert_eq!(via_doorway, 12);
}

/// A free-for-all on Complex with PD's own graph: a readable timeline + summary.
/// `cargo test --release -p game --lib pd_spike::tests::probe_complex_match -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_complex_match() {
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    let n = cfg.bots.len();
    let mut s = Sim::try_new(cfg).unwrap();
    let band = |y: f32| if y < -100.0 { 0 } else if y < 140.0 { 1 } else if y < 400.0 { 2 } else { 3 };
    let mut floor_frames = [0u32; 4];
    let mut insight = 0u32;
    let mut alive = 0u32;
    let mut still_since = vec![0i32; n];
    let mut stalls = 0u32;
    let mut first_contact: Vec<Option<i32>> = vec![None; n];
    for f in 0..60 * 180 {
        s.frame();
        for (k, c) in s.chrs.iter().enumerate() {
            if c.is_dead() {
                still_since[k] = s.g.lvframe60;
                continue;
            }
            alive += 1;
            floor_frames[band(c.pos.y)] += 1;
            if c.aibot.targetinsight {
                insight += 1;
                first_contact[k].get_or_insert(s.g.lvframe60);
            }
            let moved = Vec2::new(c.pos.x - c.prevpos.x, c.pos.z - c.prevpos.z).length() > 0.5;
            if c.actiontype != super::chr::Act::GoPos || moved {
                still_since[k] = s.g.lvframe60;
            } else if s.g.lvframe60 - still_since[k] == 180 {
                stalls += 1;
                println!("  stall: {} at ({:.0}, {:.0}, {:.0}) room {:?}", c.name, c.pos.x, c.pos.y, c.pos.z, c.floorroom);
            }
        }
        if f % 600 == 0 {
            let line: Vec<String> =
                s.chrs.iter().map(|c| format!("{:>8} y{:>4.0} K{}D{}", c.label_line(), c.pos.y, c.kills, c.deaths)).collect();
            println!("t={:>3}s | {}", f / 60, line.join(" | "));
        }
    }
    let kills: u32 = s.chrs.iter().map(|c| c.kills).sum();
    let total: u32 = floor_frames.iter().sum();
    println!("\nkills {kills} in 3 min ({:.1}/min)", kills as f32 / 3.0);
    println!("target in sight {:.0}% of alive time", 100.0 * insight as f32 / alive.max(1) as f32);
    println!(
        "floor share: pit {:.0}%  ground {:.0}%  first {:.0}%  top {:.0}%",
        100.0 * floor_frames[0] as f32 / total as f32,
        100.0 * floor_frames[1] as f32 / total as f32,
        100.0 * floor_frames[2] as f32 / total as f32,
        100.0 * floor_frames[3] as f32 / total as f32
    );
    println!("first contact (s): {:?}", first_contact.iter().map(|t| t.map(|t| t as f32 / 60.0)).collect::<Vec<_>>());
    println!("stalls (GoPos, no movement for 3 s): {stalls}");
}

/// Stage 3's baseline, headless: four NormalSims on Complex routing on PD's own
/// graph fight (kills), climb (time on the first floor), and never stand in a
/// go-to for 3 s. Deterministic for the default seed; the probe above prints the
/// full timeline.
#[test]
fn bots_fight_across_complex_on_pds_graph_without_stalling() {
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    let n = cfg.bots.len();
    let mut s = Sim::try_new(cfg).unwrap();
    let (mut upstairs, mut alive, mut stalls) = (0u32, 0u32, 0u32);
    let mut still_since = vec![0i32; n];
    for _ in 0..60 * 120 {
        s.frame();
        for (k, c) in s.chrs.iter().enumerate() {
            if c.is_dead() {
                still_since[k] = s.g.lvframe60;
                continue;
            }
            alive += 1;
            if c.pos.y > 140.0 {
                upstairs += 1;
            }
            let moved = Vec2::new(c.pos.x - c.prevpos.x, c.pos.z - c.prevpos.z).length() > 0.5;
            if c.actiontype != super::chr::Act::GoPos || moved {
                still_since[k] = s.g.lvframe60;
            } else if s.g.lvframe60 - still_since[k] == 180 {
                stalls += 1;
            }
        }
    }
    let kills: u32 = s.chrs.iter().map(|c| c.kills).sum();
    let up = upstairs as f32 / alive as f32;
    println!("kills {kills}, upstairs {:.0}% of alive time, stalls {stalls}", up * 100.0);
    assert!(kills >= 5, "only {kills} kills in 2 minutes");
    assert!(up > 0.05, "bots spent only {:.1}% of their time above the ground floor", up * 100.0);
    assert_eq!(stalls, 0);
}

/// Stage 4: generate our graph on Complex and print the static checks S1–S4 next
/// to PD's graph.
/// `cargo test --release -p game --lib pd_spike::tests::probe_navgen -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_navgen() {
    use super::navcheck::*;
    use super::navgen::{generate, GenParams};
    use super::walk::Outcome;
    use super::pd_nav::NavGraph;
    let mut s = complex_sim();
    let stage = s.stage.clone().unwrap();
    let t0 = std::time::Instant::now();
    let mut params = GenParams::default();
    let envf = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    if let Some(v) = envf("NAVGEN_SPACING") {
        params.spacing = v;
    }
    if let Some(v) = envf("NAVGEN_MARGIN") {
        params.margin = v;
    }
    if let Some(v) = envf("NAVGEN_COVER") {
        params.cover = v;
    }
    if let Some(v) = envf("NAVGEN_GROUP") {
        params.group_radius = v;
    }
    if let Some(v) = envf("NAVGEN_PRUNE") {
        params.prune = v;
    }
    params.crouch_zones = std::env::var("NAVGEN_NO_CROUCH").is_err();
    println!("{params:?}");
    let _ = &params;
    let gen = generate(&s.level, &params);
    println!(
        "generated in {:.2} s: {} samples, {} lattice links, {} nodes, {} node links, {} groups, {} iterations, {} unresolved",
        t0.elapsed().as_secs_f32(),
        gen.samples.len(),
        gen.links.len(),
        gen.graph.waypoints.len(),
        gen.node_links.len(),
        gen.graph.waygroups.len(),
        gen.iterations,
        gen.unresolved
    );
    println!("node counts: {:?}", gen.node_counts);
    if let Ok(v) = std::env::var("NAVGEN_EXPLAIN") {
        for pt in v.split(';') {
            let xz: Vec<f32> = pt.split(',').map(|t| t.trim().parse().unwrap()).collect();
            println!("  explain ({}, {}):", xz[0], xz[1]);
            for line in super::navgen::explain_point(&s.level, xz[0], xz[1], &params) {
                println!("    {line}");
            }
        }
    }
    if let Ok(v) = std::env::var("NAVGEN_EXPLAIN") {
        for pt in v.split(';') {
            let xz: Vec<f32> = pt.split(',').map(|t| t.trim().parse().unwrap()).collect();
            println!("  explain ({}, {}):", xz[0], xz[1]);
            for line in super::navgen::explain_point(&s.level, xz[0], xz[1], &params) {
                println!("    {line}");
            }
        }
    }
    // Why node links fail: re-probe every pair of nodes 150-400 cm apart on similar floors.
    {
        let mut fails = 0;
        let mut shown = 0;
        let ns = &gen.node_samples;
        for (i, &a) in ns.iter().enumerate().take(120) {
            for &b in ns.iter().skip(i + 1) {
                let (pa, pb) = (gen.samples[a].pos, gen.samples[b].pos);
                let d = Vec2::new(pa.x - pb.x, pa.z - pb.z).length();
                if !(150.0..400.0).contains(&d) || (pa.y - pb.y).abs() > 10.0 || !s.level.los_autoflags(pa + glam::Vec3::Y * 53.0, pb + glam::Vec3::Y * 53.0) {
                    continue;
                }
                if let Err(why) = super::navgen::probe_walk_why(&s.level, pa, pb, &params) {
                    fails += 1;
                    if shown < 12 {
                        shown += 1;
                        println!("  visible pair ({:.0},{:.0},{:.0}) -> ({:.0},{:.0},{:.0}) fails: {why}", pa.x, pa.y, pa.z, pb.x, pb.y, pb.z);
                    }
                }
            }
        }
        println!("  {fails} visible same-floor node pairs fail the probe");
    }
    // Debug dump for plotting: samples with their lattice component, nodes, node links.
    if let Ok(path) = std::env::var("NAVGEN_DUMP") {
        let n = gen.samples.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut Vec<usize>, x: usize) -> usize {
            if p[x] != x {
                let r = find(p, p[x]);
                p[x] = r;
            }
            p[x]
        }
        for &(a, b, _) in &gen.links {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
        }
        let samples: Vec<String> = (0..n)
            .map(|i| {
                let p = gen.samples[i].pos;
                format!("[{:.0},{:.0},{:.0},{}]", p.x, p.y, p.z, find(&mut parent, i))
            })
            .collect();
        let nodes: Vec<String> = gen.node_samples.iter().map(|&s| s.to_string()).collect();
        let links: Vec<String> = gen.node_links.iter().map(|(u, v, k)| format!("[{u},{v},\"{k:?}\"]")).collect();
        let lat: Vec<String> = gen.links.iter().map(|(a, b, k)| format!("[{a},{b},\"{k:?}\"]")).collect();
        std::fs::write(
            &path,
            format!("{{\"samples\":[{}],\"nodes\":[{}],\"links\":[{}],\"lattice\":[{}]}}", samples.join(","), nodes.join(","), links.join(","), lat.join(",")),
        )
        .unwrap();
    }
    // Samples whose closest waypoint (as a go-to from there picks it) a chr can't
    // walk to in a straight line: a go-to started there heads into a wall.
    {
        let mut bad = Vec::new();
        for smp in &gen.samples {
            let prop = smp.pos + glam::Vec3::Y * 50.0;
            let rooms: Vec<u16> = s.level.floor_room(prop, 20.0).into_iter().collect();
            if let Some(w) = gen.graph.waypoint_find_closest_to_pos(&s.level, prop, &rooms) {
                let node = gen.graph.waypoint_pos(w) - glam::Vec3::Y * 53.0;
                if super::navgen::probe_walk(&s.level, smp.pos, node, &params).is_none() {
                    bad.push((smp.pos, w, node));
                }
            }
        }
        println!("  {} of {} samples: closest waypoint not walkable straight", bad.len(), gen.samples.len());
        for (p, w, n) in bad.iter().take(15) {
            println!("    ({:.0},{:.0},{:.0}) -> {w} ({:.0},{:.0},{:.0}): {:?}", p.x, p.y, p.z, n.x, n.y, n.z, super::navgen::probe_walk_why(&s.level, *p, *n, &params).err());
        }
    }
    let pd = NavGraph::from_pd_stage(&stage, &s.level);
    for (name, g) in [("PD", &pd), ("ours", &gen.graph)] {
        let (misses, total) = s1_coverage(&s.level, &stage, g);
        let (weak, strong) = s2_components(g);
        println!("{name:>5}: S1 {} of {total} pads uncovered; S2 {weak} weak / {strong} strong components", misses.len());
        if name == "ours" {
            for c in s2_strong_components(g).1.iter().skip(1) {
                let pts: Vec<String> = c.iter().map(|&w| { let p = g.waypoint_pos(w); format!("{w}@({:.0},{:.0},{:.0})", p.x, p.y, p.z) }).collect();
                println!("        small strong component: {}", pts.join(" "));
                for &w in c {
                    let nb: Vec<String> = g.waypoints[w].neighbours.iter().map(|&x| format!("{}{}", super::pd_tiles::wpseg_get_id(x), if x & 0x4000 != 0 { "o" } else if x & 0x8000 != 0 { "i" } else { "" })).collect();
                    println!("          {w} neighbours {}", nb.join(","));
                }
            }
            for m in misses.iter().take(20) {
                println!("        uncovered pad {:#x} at ({:.0}, {:.0}, {:.0}), nearest same-floor node {:.0} cm", m.pad, m.pos.x, m.pos.y, m.pos.z, m.nearest);
            }
        }
    }
    let rp = s3_route_lengths(&s.level, &stage, &pd);
    let ro = s3_route_lengths(&s.level, &stage, &gen.graph);
    let mut ratios = Vec::new();
    let (mut miss_pd, mut miss_ours) = (0, 0);
    for ((pair, lp), (_, lo)) in rp.iter().zip(&ro) {
        match (lp, lo) {
            (Some(lp), Some(lo)) => ratios.push((lo / lp, *pair)),
            (None, _) => miss_pd += 1,
            (_, None) => miss_ours += 1,
        }
    }
    ratios.sort_by(|a, b| a.0.total_cmp(&b.0));
    let median = ratios[ratios.len() / 2].0;
    let over: Vec<String> = ratios.iter().filter(|r| r.0 > 1.5).map(|r| format!("{:?} {:.2}", r.1, r.0)).collect();
    println!("S3 pairs over 1.5: {} {:?}", over.len(), over);
    println!(
        "S3: {} pairs; missing routes PD {miss_pd}, ours {miss_ours}; ours/PD length median {median:.2}, best {:.2}, worst {:.2} {:?}",
        rp.len(),
        ratios[0].0,
        ratios.last().unwrap().0,
        ratios.last().unwrap().1
    );
    let walks = s4_walk(&mut s, &gen.graph);
    let bad: Vec<_> = walks
        .iter()
        // A fall: over 100 cm between manground and the floor on a two-way link.
        // (Going down a steep ramp, a bot floats up to ~75 cm: PD's gravity branch.)
        // Ending within a step (69 cm) of the goal floor still counts: two nodes 50 cm
        // apart across a step, and PD's 30 cm arrival fires on the upper one.
        .filter(|(_, b, one_way, w)| {
            let step_ok = w.outcome == Outcome::WrongFloor && (w.end.y - (gen.graph.waypoint_pos(*b).y - 53.0)).abs() <= 69.0;
            (w.outcome != Outcome::Arrived && !step_ok) || w.ticks > 2 * w.nominal + 60 || (!*one_way && w.max_drop > 100.0)
        })
        .collect();
    println!("S4: {} directed links walked, {} bad", walks.len(), bad.len());
    for (a, b, one_way, w) in bad.iter().take(30) {
        let (pa, pb) = (gen.graph.waypoint_pos(*a), gen.graph.waypoint_pos(*b));
        println!(
            "        {a} -> {b}{}: {:?} {} of {} ticks, drop {:.0}  node ({:.0},{:.0},{:.0}) -> ({:.0},{:.0},{:.0}); started ({:.0},{:.0},{:.0}) ended ({:.0},{:.0},{:.0})",
            if *one_way { " one-way" } else { "" }, w.outcome, w.ticks, w.nominal, w.max_drop,
            pa.x, pa.y, pa.z, pb.x, pb.y, pb.z, w.start.x, w.start.y, w.start.z, w.end.x, w.end.y, w.end.z
        );
    }
}

#[test]
#[ignore]
fn probe_root_heights() {
    let mut s = sim(4);
    let mut moving: Vec<f32> = Vec::new();
    for _ in 0..60 * 30 {
        s.frame();
        for c in &s.chrs {
            if !c.is_dead() && c.is_moving() {
                moving.push(c.root_height());
            }
        }
    }
    moving.sort_by(f32::total_cmp);
    let q = |f: f32| moving[((moving.len() - 1) as f32 * f) as usize];
    println!("root height while moving: min {:.1}, p5 {:.1}, median {:.1}, p95 {:.1}, max {:.1}", q(0.0), q(0.05), q(0.5), q(0.95), q(1.0));
    for (b, name) in super::sim::BODIES.iter().zip(0..) {
        let _ = (b, name);
    }
}

/// Stage 4's static checks, pinned: our generated graph on Complex against PD's.
/// * S1: no more PD pads (waypoint + spawn) without one of our nodes within 150 cm,
///   on the same floor, in sight, than PD's own graph leaves (9 of 163). The plan's
///   "all 163" is not met by PD's graph itself.
/// * S2: the same weak/strong component counts as PD's (1 / 1).
/// * S3: every spawn-to-spawn route exists; ours/PD length median <= 1.15, worst <= 1.5.
/// * S4: every allowed link walks with the real movement code, ending on its floor
///   within a step, in under 2x nominal time + 1 s, with no fall over 100 cm on a
///   two-way link.
#[test]
fn our_generated_graph_passes_the_static_checks_against_pds() {
    use super::navcheck::*;
    use super::navgen::{generate, GenParams};
    use super::pd_nav::NavGraph;
    use super::walk::Outcome;
    let mut s = complex_sim();
    let stage = s.stage.clone().unwrap();
    let gen = generate(&s.level, &GenParams::default());
    let ours = &gen.graph;
    let pd = NavGraph::from_pd_stage(&stage, &s.level);

    let (pd_misses, total) = s1_coverage(&s.level, &stage, &pd);
    let (our_misses, _) = s1_coverage(&s.level, &stage, ours);
    println!("S1: ours {} / PD {} of {total} uncovered", our_misses.len(), pd_misses.len());
    assert!(our_misses.len() <= pd_misses.len());

    assert_eq!(s2_components(ours), s2_components(&pd));

    let rp = s3_route_lengths(&s.level, &stage, &pd);
    let ro = s3_route_lengths(&s.level, &stage, ours);
    let mut ratios: Vec<f32> = Vec::new();
    for ((pair, lp), (_, lo)) in rp.iter().zip(&ro) {
        let lo = lo.unwrap_or_else(|| panic!("no route for spawn pair {pair:?} on our graph"));
        ratios.push(lo / lp.expect("PD route"));
    }
    ratios.sort_by(f32::total_cmp);
    let (median, worst) = (ratios[ratios.len() / 2], *ratios.last().unwrap());
    println!("S3: median {median:.2}, worst {worst:.2}");
    assert!(median <= 1.15 && worst <= 1.5);

    let walks = s4_walk(&mut s, ours);
    let bad: Vec<_> = walks
        .iter()
        .filter(|(_, b, one_way, w)| {
            let step_ok = w.outcome == Outcome::WrongFloor && (w.end.y - (ours.waypoint_pos(*b).y - 53.0)).abs() <= 69.0;
            (w.outcome != Outcome::Arrived && !step_ok) || w.ticks > 2 * w.nominal + 60 || (!*one_way && w.max_drop > 100.0)
        })
        .map(|(a, b, _, w)| (*a, *b, w.outcome))
        .collect();
    println!("S4: {} links, {} bad", walks.len(), bad.len());
    assert!(bad.is_empty(), "{bad:?}");
}

/// Stage 5's dynamic checks D1–D4: the same seeds on PD's graph and ours.
/// `cargo test --release -p game --lib pd_spike::tests::probe_ab -- --ignored --nocapture`
/// (`AB_SEEDS`, default 10; `AB_SECONDS`, default 180.)
#[test]
#[ignore]
fn probe_ab() {
    use super::abtest::*;
    use super::sim::NavChoice;
    let seeds: u64 = std::env::var("AB_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let secs: u32 = std::env::var("AB_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(180);
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    let cells = floor_cells(&Sim::try_new(cfg.clone()).unwrap());
    // Posed guns by default (as on screen); AB_FALLBACK_GUNS=1 for PD's off-screen fallback.
    let poser = if std::env::var("AB_FALLBACK_GUNS").is_ok() { None } else { Some(super::gunpos::GunPoser::load().unwrap()) };
    let mut pooled = [Pooled::default(), Pooled::default()];
    for (k, nav) in [NavChoice::Pd, NavChoice::Ours].into_iter().enumerate() {
        for seed in 1..=seeds {
            let mut c = cfg.clone();
            c.nav = nav;
            c.seed = seed;
            let m = run_match(c, secs, poser.as_ref());
            pooled[k].add(&m, &cells);
        }
    }
    let [pd, ours] = &pooled;
    let row = |name: &str, a: f32, b: f32| println!("{name:<34} {a:>9.3} {b:>9.3}   ratio {:.2}", b / a);
    println!("\n{seeds} seeds x {secs} s, 4 bots                 PD      ours");
    row("D1 first contact, median (s)", pd.median_first_contact(), ours.median_first_contact());
    row("D2 kills per minute", pd.kills_per_min(), ours.kills_per_min());
    row("D2 target in sight (share)", pd.insight_share(), ours.insight_share());
    row("D2 in a go-to (share)", pd.gopos_share(), ours.gopos_share());
    row("D3 re-paths per bot-minute", pd.per_bot_minute(pd.repaths), ours.per_bot_minute(ours.repaths));
    row("D3 3-s stalls per bot-minute", pd.per_bot_minute(pd.stalls), ours.per_bot_minute(ours.stalls));
    row("D3 ledge falls per bot-minute", pd.per_bot_minute(pd.ledge_falls), ours.per_bot_minute(ours.ledge_falls));
    println!("go-to calls / no start / no end / no route: PD {:?}, ours {:?}", pd.gotos, ours.gotos);
    println!("kills per match: PD {:?}, ours {:?}", pd.kills_each, ours.kills_each);
    row("shots per minute", pd.shots as f32 / pd.minutes, ours.shots as f32 / ours.minutes);
    row("hit rate", pd.hits as f32 / pd.shots as f32, ours.hits as f32 / ours.shots as f32);
    let modes = |p: &Pooled| {
        let t: u32 = p.modes.iter().sum();
        p.modes.iter().map(|&m| format!("{:.2}", m as f32 / t as f32)).collect::<Vec<_>>().join(" ")
    };
    println!("distance modes none/backup/ok/advance/goto: PD {}, ours {}", modes(pd), modes(ours));
    row("mean distance while in sight (cm)", (pd.insight_dist / pd.insight_frames as f64) as f32, (ours.insight_dist / ours.insight_frames as f64) as f32);
    for (b, &(_, _, name)) in BANDS.iter().enumerate() {
        row(&format!("D4 coverage, {name} ({} cells)", cells[b].len()), pd.coverage(b), ours.coverage(b));
    }
}

/// Where bots on our graph get stuck: one Complex match, the stuck re-routes listed.
#[test]
#[ignore]
fn probe_repaths_on_ours() {
    use super::sim::NavChoice;
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    cfg.nav = if std::env::var("AB_PD").is_ok() { NavChoice::Pd } else { NavChoice::Ours };
    // `AB_SEEDS` seeds from 1, as `probe_ab` runs them; without it, the one `PD_SEED` match.
    let seeds: Vec<u64> = match std::env::var("AB_SEEDS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(n) => (1..=n).collect(),
        None => vec![cfg.seed],
    };
    // Posed guns, as `probe_ab` (AB_FALLBACK_GUNS=1 for PD's off-screen fallback,
    // which puts a crouched bot's gun above the crawl space's ceiling).
    let poser = if std::env::var("AB_FALLBACK_GUNS").is_ok() { None } else { Some(super::gunpos::GunPoser::load().unwrap()) };
    let mut total = 0;
    for seed in seeds {
        let mut c = cfg.clone();
        c.seed = seed;
        let mut s = Sim::try_new(c).unwrap();
        for _ in 0..60 * 180 {
            s.frame();
            if let Some(p) = &poser {
                p.apply(&mut s);
            }
        }
        let mut v = s.stats.repath_at.clone();
        v.sort_by(|a, b| (a.0.x as i32, a.0.z as i32).cmp(&(b.0.x as i32, b.0.z as i32)));
        for (p, aim) in &v {
            println!("seed {seed}: stuck at ({:.0},{:.0},{:.0}) heading for ({:.0},{:.0},{:.0}), {:.0} cm away", p.x, p.y, p.z, aim.x, aim.y, aim.z, p.distance(*aim));
        }
        total += v.len();
    }
    println!("{total} stuck re-routes");
}

/// One match on our graph with a per-second line for each bot (for a bad seed).
/// `PD_SEED=2 cargo test --release -p game --lib pd_spike::tests::probe_ours_timeline -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_ours_timeline() {
    use super::sim::NavChoice;
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    cfg.nav = if std::env::var("AB_PD").is_ok() { NavChoice::Pd } else { NavChoice::Ours };
    let mut s = Sim::try_new(cfg).unwrap();
    for f in 0..60 * 90 {
        s.frame();
        if f % 300 == 0 {
            for c in &s.chrs {
                println!(
                    "t={:>3} {:<11} ({:>5.0},{:>4.0},{:>5.0}) h{:>3.0} {:<8} anim {:<10} insight {} tgt {:?} dist {:>4.0} K{} shots",
                    f / 60, c.name, c.pos.x, c.pos.y, c.pos.z, c.height, format!("{:?}", c.actiontype),
                    c.model.animnum.map_or("-".to_string(), |a| super::anims::info(a).name.to_string()),
                    c.aibot.targetinsight, c.target, c.last_dist, c.kills
                );
            }
            let n = s.shots.iter().filter(|x| x.age == 0).count();
            println!("  shots this frame {n}, re-paths so far {}", s.stats.repaths);
        }
    }
}

/// D4 diagnosis: where each graph's bots go. Pools visited 1 m cells over seeds and
/// writes, per floor cell, how many matches on each graph visited it, plus both
/// graphs' waypoints and links, to `COVERAGE_DUMP` (default `coverage.json`).
/// `AB_SEEDS` / `AB_SECONDS` as in `probe_ab`.
#[test]
#[ignore]
fn probe_coverage_map() {
    use super::abtest::*;
    use super::sim::NavChoice;
    use std::collections::HashMap;
    let seeds: u64 = std::env::var("AB_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let secs: u32 = std::env::var("AB_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(180);
    let path = std::env::var("COVERAGE_DUMP").unwrap_or_else(|_| "coverage.json".into());
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    let s0 = Sim::try_new(cfg.clone()).unwrap();
    let cells = floor_cells(&s0);
    let poser = if std::env::var("AB_FALLBACK_GUNS").is_ok() { None } else { Some(super::gunpos::GunPoser::load().unwrap()) };
    let mut counts: [HashMap<(usize, i32, i32), u32>; 2] = Default::default();
    let mut band_frames = [[0u64; 4]; 2];
    let mut moves = [[[0u32; 4]; 4]; 2];
    let mut spawns = [[0u32; 4]; 2];
    let mut exits = [[0u32; 3]; 2];
    let mut per_seed: [Vec<(f32, f32)>; 2] = Default::default();
    for (k, nav) in [NavChoice::Pd, NavChoice::Ours].into_iter().enumerate() {
        let first_seed: u64 = std::env::var("AB_FIRST_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        for seed in first_seed..first_seed + seeds {
            let mut c = cfg.clone();
            c.nav = nav;
            c.seed = seed;
            let m = run_match(c, secs, poser.as_ref());
            for e in 0..3 {
                exits[k][e] += m.first_exits[e];
            }
            let tf: u32 = m.band_frames.iter().sum();
            per_seed[k].push((m.band_frames[2] as f32 / tf as f32, m.visited[2].intersection(&cells[2]).count() as f32 / cells[2].len() as f32));
            for b in 0..4 {
                spawns[k][b] += m.spawn_band[b];
                for t in 0..4 {
                    moves[k][b][t] += m.band_moves[b][t];
                }
                band_frames[k][b] += m.band_frames[b] as u64;
                for &(x, z) in m.visited[b].intersection(&cells[b]) {
                    *counts[k].entry((b, x, z)).or_default() += 1;
                }
            }
        }
    }
    for k in 0..2 {
        let t: u64 = band_frames[k].iter().sum();
        let share: Vec<String> = (0..4).map(|b| format!("{} {:.3}", BANDS[b].2, band_frames[k][b] as f64 / t as f64)).collect();
        println!("{} time share per band: {}", ["PD", "ours"][k], share.join(", "));
        println!("   lives starting per band {:?}; band moves from->to (pit, ground, first, top) {:?}", spawns[k], moves[k]);
        // How long a visit to the first floor lasts: frames there / entries.
        let entries: u32 = (0..4).filter(|&a| a != 2).map(|a| moves[k][a][2]).sum::<u32>() + spawns[k][2];
        println!("   first-floor visits {entries}, mean stay {:.1} s; ended walking down / falling / dying {:?}", band_frames[k][2] as f64 / entries.max(1) as f64 / 60.0, exits[k]);
        let fmt: Vec<String> = per_seed[k].iter().map(|(t, c)| format!("{t:.2}/{c:.2}")).collect();
        println!("   per seed, first-floor time share / coverage: {}", fmt.join(" "));
    }
    let mut rows = Vec::new();
    for b in 0..4 {
        for &(x, z) in &cells[b] {
            let (p, o) = (counts[0].get(&(b, x, z)).copied().unwrap_or(0), counts[1].get(&(b, x, z)).copied().unwrap_or(0));
            rows.push(format!("[{b},{x},{z},{p},{o}]"));
        }
    }
    let graph_json = |g: &super::pd_nav::NavGraph| {
        let wps: Vec<String> = (0..g.waypoints.len())
            .map(|w| {
                let p = g.waypoint_pos(w);
                let f = g.pads[g.waypoints[w].padnum].flags;
                format!("[{:.0},{:.0},{:.0},{},{},{}]", p.x, p.y, p.z, g.waypoints[w].groupnum, f.walkdirect as u8, f.crouch as u8)
            })
            .collect();
        let nbs: Vec<String> = (0..g.waypoints.len()).map(|w| format!("{:?}", g.waypoints[w].neighbours)).collect();
        format!("{{\"wps\":[{}],\"nbs\":[{}]}}", wps.join(","), nbs.join(","))
    };
    let (pd, ours) = match s0.config.nav {
        NavChoice::Pd => (&s0.nav, s0.other_nav.as_ref().unwrap()),
        NavChoice::Ours => (s0.other_nav.as_ref().unwrap(), &s0.nav),
    };
    // Where a target standing on each first-floor cell resolves to (the end
    // waypoint of a go-to at it): the node's band, per graph.
    for (name, g) in [("PD", pd), ("ours", ours)] {
        let mut to_band = [0u32; 5];
        for &(x, z) in &cells[2] {
            let (cx, cz) = ((x as f32 + 0.5) * 100.0, (z as f32 + 0.5) * 100.0);
            let Some(fy) = s0.level.geom.polys.iter().filter(|p| p.floor_kind().is_some() && p.xz_in_convex(cx, cz)).map(|p| p.find_y(cx, cz)).filter(|&y| (140.0..400.0).contains(&y)).next() else { continue };
            let pos = glam::Vec3::new(cx, fy + 50.0, cz);
            let rooms: Vec<u16> = s0.level.floor_room(pos, 20.0).into_iter().collect();
            match g.waypoint_find_closest_to_pos(&s0.level, pos, &rooms) {
                Some(w) => to_band[BANDS.iter().position(|&(lo, hi, _)| { let y = g.waypoint_pos(w).y - 53.0; y >= lo && y < hi }).unwrap_or(4)] += 1,
                None => to_band[4] += 1,
            }
        }
        println!("{name}: first-floor cells resolve to a waypoint on pit/ground/first/top/none {to_band:?}");
    }
    std::fs::write(&path, format!("{{\"seeds\":{seeds},\"cells\":[{}],\"pd\":{},\"ours\":{}}}", rows.join(","), graph_json(pd), graph_json(ours))).unwrap();
    println!("wrote {path}");
}

/// One S4-style walk between two floor points with a trace every 10 ticks.
/// `WALK_LINK="x,y,z;x,y,z"` (floor points, cm).
#[test]
#[ignore]
fn probe_walk_link() {
    let v = std::env::var("WALK_LINK").expect("WALK_LINK=\"x,y,z;x,y,z\"");
    let pts: Vec<glam::Vec3> = v
        .split(';')
        .map(|p| {
            let c: Vec<f32> = p.split(',').map(|t| t.trim().parse().unwrap()).collect();
            glam::Vec3::new(c[0], c[1], c[2])
        })
        .collect();
    let mut s = complex_sim();
    s.brains = false;
    let _nav = std::mem::replace(&mut s.nav, super::pd_nav::NavGraph::empty());
    while s.g.lvframe60 < 145 {
        s.frame();
    }
    s.place_exact(0, pts[0]);
    s.walk_straight_to(0, pts[1] + glam::Vec3::Y * 53.0);
    for c in s.chrs.iter().skip(1) {
        println!("other chr at ({:.0},{:.0},{:.0})", c.pos.x, c.pos.y, c.pos.z);
    }
    for t in 0..400 {
        s.frame();
        let c = &s.chrs[0];
        if t % 10 == 0 || c.actiontype != super::chr::Act::GoPos {
            println!("t {t:>3} pos ({:.0},{:.0},{:.0}) invalidmove {} moved {:.1}", c.pos.x, c.pos.y, c.pos.z, c.invalidmove, Vec2::new(c.pos.x - c.prevpos.x, c.pos.z - c.prevpos.z).length());
        }
        if c.actiontype != super::chr::Act::GoPos {
            break;
        }
    }
}

/// D4 diagnosis: replay the go-to requests bots made on one graph (`AB_PD` set:
/// PD's; else ours) through both graphs' routing, and compare where the routes run.
/// Separates route choice from the match's feedback (where targets happen to be).
#[test]
#[ignore]
fn probe_replay_gotos() {
    use super::abtest::BANDS;
    use super::pd_nav::{NavGraph, NavSeed};
    use super::sim::NavChoice;
    let seeds: u64 = std::env::var("AB_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    cfg.nav = if std::env::var("AB_PD").is_ok() { NavChoice::Pd } else { NavChoice::Ours };
    let mut log = Vec::new();
    let mut graphs: Option<(NavGraph, NavGraph)> = None;
    let mut level = None;
    for seed in 1..=seeds {
        let mut c = cfg.clone();
        c.seed = seed;
        let mut s = Sim::try_new(c).unwrap();
        for _ in 0..60 * 180 {
            s.frame();
        }
        log.extend(s.stats.goto_log.drain(..));
        if graphs.is_none() {
            let other = s.other_nav.take().unwrap();
            let nav = std::mem::replace(&mut s.nav, NavGraph::empty());
            graphs = Some(if cfg.nav == NavChoice::Pd { (nav, other) } else { (other, nav) });
            level = Some(s.level);
        }
    }
    let (pd, ours) = graphs.unwrap();
    let level = level.unwrap();
    let band = |y: f32| BANDS.iter().position(|&(lo, hi, _)| y >= lo && y < hi).unwrap_or(4);
    println!("{} requests replayed (made on {:?})", log.len(), cfg.nav);
    for (name, g) in [("PD", &pd), ("ours", &ours)] {
        let mut rng = super::pdmath::Rng::new(1);
        // Length per band of the whole route, and of its first 6 slots (what a chr
        // loads), both from the chr's position.
        let (mut full, mut head) = ([0f64; 5], [0f64; 5]);
        let mut ups = 0;
        for (from, rooms, to) in &log {
            let endrooms: Vec<u16> = level.floor_room(*to, 20.0).into_iter().collect();
            let (Some(a), Some(b)) = (g.waypoint_find_closest_to_pos(&level, *from, rooms), g.waypoint_find_closest_to_pos(&level, *to, &endrooms)) else { continue };
            let (route, _) = g.nav_find_route(a, b, 100_000, NavSeed(1, 1), &mut rng);
            let mut pts = vec![*from - glam::Vec3::Y * 50.0];
            pts.extend(route.iter().map(|&w| g.waypoint_pos(w) - glam::Vec3::Y * 53.0));
            for (k, w) in pts.windows(2).enumerate() {
                let l = w[0].distance(w[1]) as f64;
                let b = band(0.5 * (w[0].y + w[1].y));
                full[b] += l;
                if k < 5 {
                    head[b] += l;
                }
            }
            if band(from.y - 50.0) == 1 && pts.iter().any(|p| band(p.y) == 2) {
                ups += 1;
            }
        }
        let share = |v: &[f64; 5]| {
            let t: f64 = v.iter().sum();
            format!("pit {:.3} ground {:.3} first {:.3} top {:.3}", v[0] / t, v[1] / t, v[2] / t, v[3] / t)
        };
        println!("{name:>5} whole routes: {}
      first 6 slots: {}
      routes from the ground that climb to the first floor: {ups}", share(&full), share(&head));
    }
}

/// For requests made on PD's graph where exactly one graph's route climbs to the
/// first floor: compare lengths, and name PD's first-floor waypoints involved.
#[test]
#[ignore]
fn probe_replay_divergent() {
    use super::abtest::BANDS;
    use super::pd_nav::{NavGraph, NavSeed};
    use super::sim::NavChoice;
    use std::collections::HashMap;
    let seeds: u64 = std::env::var("AB_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let mut cfg = arena_config();
    cfg.level = LevelChoice::Complex;
    cfg.nav = NavChoice::Pd;
    let mut log = Vec::new();
    let mut keep = None;
    for seed in 1..=seeds {
        let mut c = cfg.clone();
        c.seed = seed;
        let mut s = Sim::try_new(c).unwrap();
        for _ in 0..60 * 180 {
            s.frame();
        }
        log.extend(s.stats.goto_log.drain(..));
        if keep.is_none() {
            let ours = s.other_nav.take().unwrap();
            let pd = std::mem::replace(&mut s.nav, NavGraph::empty());
            keep = Some((pd, ours, s.level));
        }
    }
    let (pd, ours, level) = keep.unwrap();
    let band = |y: f32| BANDS.iter().position(|&(lo, hi, _)| y >= lo && y < hi).unwrap_or(4);
    let mut rng = super::pdmath::Rng::new(1);
    let mut route = |g: &NavGraph, from: glam::Vec3, rooms: &[u16], to: glam::Vec3| -> Option<(Vec<usize>, f32)> {
        let endrooms: Vec<u16> = level.floor_room(to, 20.0).into_iter().collect();
        let a = g.waypoint_find_closest_to_pos(&level, from, rooms)?;
        let b = g.waypoint_find_closest_to_pos(&level, to, &endrooms)?;
        let (r, _) = g.nav_find_route(a, b, 100_000, NavSeed(1, 1), &mut rng);
        let mut pts = vec![from - glam::Vec3::Y * 50.0];
        pts.extend(r.iter().map(|&w| g.waypoint_pos(w) - glam::Vec3::Y * 53.0));
        pts.push(to - glam::Vec3::Y * 50.0);
        Some((r, pts.windows(2).map(|w| w[0].distance(w[1])).sum()))
    };
    let (mut pd_only, mut ours_only) = (Vec::new(), Vec::new());
    let mut pd_first_wps: HashMap<usize, u32> = HashMap::new();
    let mut ends: HashMap<(i32, i32, i32, i32), u32> = HashMap::new();
    for (from, rooms, to) in &log {
        let (Some((rp, lp)), Some((ro, lo))) = (route(&pd, *from, rooms, *to), route(&ours, *from, rooms, *to)) else { continue };
        let up = |g: &NavGraph, r: &[usize]| r.iter().any(|&w| band(g.waypoint_pos(w).y - 53.0) == 2);
        match (up(&pd, &rp), up(&ours, &ro)) {
            (true, false) => {
                pd_only.push(lo / lp);
                for &w in &rp {
                    if band(pd.waypoint_pos(w).y - 53.0) == 2 {
                        *pd_first_wps.entry(w).or_default() += 1;
                    }
                }
                *ends.entry(((from.x / 500.0).round() as i32, (from.z / 500.0).round() as i32, (to.x / 500.0).round() as i32, (to.z / 500.0).round() as i32)).or_default() += 1;
            }
            (false, true) => ours_only.push(lo / lp),
            _ => {}
        }
    }
    let med = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
    };
    println!("{} requests; PD climbs and ours doesn't: {} (ours/PD length median {:.2}); ours climbs and PD doesn't: {} (median {:.2})", log.len(), pd_only.len(), med(&mut pd_only), ours_only.len(), med(&mut ours_only));
    let mut w: Vec<_> = pd_first_wps.into_iter().collect();
    w.sort_by_key(|x| std::cmp::Reverse(x.1));
    for (wp, n) in w.iter().take(12) {
        let p = pd.waypoint_pos(*wp);
        println!("  PD first-floor waypoint {wp:#x} at ({:.0},{:.0},{:.0}): {n}", p.x, p.y, p.z);
    }
    let mut e: Vec<_> = ends.into_iter().collect();
    e.sort_by_key(|x| std::cmp::Reverse(x.1));
    for ((fx, fz, tx, tz), n) in e.iter().take(10) {
        println!("  from ~({},{}) to ~({},{}): {n}", fx * 500, fz * 500, tx * 500, tz * 500);
    }
}
