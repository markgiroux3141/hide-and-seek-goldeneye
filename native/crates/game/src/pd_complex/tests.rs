//! Headless checks for the combined match: PD's player walk on Complex (every
//! link of PD's waypoint graph, walked by the player), the player's weapons
//! killing a simulant, the simulants killing the player (and the respawn), and
//! the textured BG agreeing with the collision tiles.
//!
//! `cargo test --release -p game --lib pd_complex`

use std::sync::Arc;

use glam::{Vec2, Vec3};

use super::fight::Fight;
use crate::pd_guns::bgun::{CROUCHPOS_SQUAT, CROUCHPOS_STAND};
use crate::pd_guns::gset::WEAPON_FALCON2;
use crate::pd_guns::player::PdInput;
use crate::pd_guns::range::Range;
use crate::pd_guns::sim::{Sim as GunSim, HAND_MODELS};
use crate::pd_spike::chraction::chr_adjust_pos_for_spawn;
use crate::pd_spike::pd_tiles::PdStage;
use crate::pd_spike::sim::{drop_to_ground, SimConfig};
use crate::pd_spike::tile_level::TileLevel;
use crate::pd_spike::walk::{pd_links, pd_pad_floor};

fn complex() -> (PdStage, Arc<TileLevel>) {
    let stage = PdStage::complex().expect("reference/pd-decomp (or PD_DECOMP_DIR) must be present");
    let level = Arc::new(TileLevel::new(stage.geom.clone()));
    (stage, level)
}

/// The guns' sim with Complex as its world and no chrs: the player alone.
fn walker(level: &Arc<TileLevel>) -> GunSim {
    let mut g = GunSim::new(HAND_MODELS[0]).expect("pd_fp assets");
    g.walk_level = level.clone();
    g.range = Range::for_stage(level.clone());
    g
}

/// `vv_theta` (degrees; forward = (−sin, 0, cos)) that faces from `a` to `b`.
fn theta_towards(a: Vec3, b: Vec3) -> f32 {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let t = (-dx).atan2(dz).to_degrees();
    if t < 0.0 {
        t + 360.0
    } else {
        t
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Arrived,
    WrongFloor,
    Stuck,
    Timeout,
}

#[derive(Clone, Copy, Debug)]
struct PlayerWalk {
    outcome: Outcome,
    ticks: i32,
    end: Vec3,
    /// Largest `manground − ground` seen: > 69 means it went off a ledge.
    max_drop: f32,
    landed: bool,
}

/// TEST HARNESS (not PD): drive the player from the floor under pad `from` to
/// pad `to` by holding forward and turning the view straight at the pad each tick
/// (the turn is set directly, as a perfect mouse would), crouched if the goal is
/// a crouch pad. Everything else is the ported walk: `bwalk_resolve_posdelta`,
/// `bwalk_update_vertical`, the head bob that sets the speed.
fn walk_player(g: &mut GunSim, level: &TileLevel, from: Vec3, to: Vec3, to_floor: Option<f32>, crouch: bool) -> PlayerWalk {
    // Placed as PD spawns a player: a clear 30 cm cylinder at or around the pad
    // (`chr_adjust_pos_for_spawn`), stood on its floor.
    let spot = chr_adjust_pos_for_spawn(level, 30.0, from, 0.0, &[]).unwrap_or(from);
    let start = drop_to_ground(level, spot);
    g.player.place(start, theta_towards(start, to));
    g.bgun.p.crouchpos = if crouch { CROUCHPOS_SQUAT } else { CROUCHPOS_STAND };
    // A start inside the crawl space is already squatting (standing up in there
    // is refused by the ceiling, and so is crouching while the head is in it).
    if crouch {
        g.player.set_crouch_offset(-90.0);
    }
    // PD's pad heights lie for some pads (the pit's sit 118 cm above the rim), so
    // either the pad's own floor or the floor under it counts, as for the bots.
    let under = drop_to_ground(level, to).y;
    let goal_floor = to_floor.unwrap_or(under);
    let dist = Vec2::new(to.x - start.x, to.z - start.z).length();
    // The player runs ~8 cm a tick standing, 0.35x of that squatting.
    let speed = if crouch { 2.5 } else { 6.0 };
    let limit = (dist / speed) as i32 * 2 + 120;
    let mut still = 0;
    let mut max_drop = 0.0f32;
    let mut landed = false;
    let input = PdInput { walk_y: 127, ..PdInput::default() };
    for t in 0..limit {
        let p = g.player.pos;
        g.player.theta = theta_towards(p, to);
        let before = g.player.pos;
        g.frame(&input, 4);
        max_drop = max_drop.max(g.player.manground - g.player.ground);
        landed |= g.player.landed.is_some();
        let p = g.player.pos;
        let xz = Vec2::new(to.x - p.x, to.z - p.z).length();
        // By the floor under the cylinder: going up a ramp, `manground` trails it
        // by ~24 cm (the step low-pass).
        let floor = g.player.ground;
        let on_floor = (floor - goal_floor).abs() <= 40.0 || (floor - under).abs() <= 40.0;
        // Mid-fall over the goal is not arriving: wait for the landing.
        let falling = g.player.isfalling || g.player.manground > g.player.ground + 1.0;
        if xz <= 40.0 && !falling {
            return PlayerWalk {
                outcome: if on_floor || g.player.onladder { Outcome::Arrived } else { Outcome::WrongFloor },
                ticks: t,
                end: p,
                max_drop,
                landed,
            };
        }
        if Vec2::new(p.x - before.x, p.z - before.z).length() < 0.05 && !g.player.onladder {
            still += 1;
            if still > 90 {
                return PlayerWalk { outcome: Outcome::Stuck, ticks: t, end: p, max_drop, landed };
            }
        } else {
            still = 0;
        }
    }
    PlayerWalk { outcome: Outcome::Timeout, ticks: limit, end: g.player.pos, max_drop, landed }
}

/// Every link of PD's graph, walked by the player. Print-only.
/// `cargo test --release -p game --lib pd_complex::tests::probe_player_walks_pd_graph -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_player_walks_pd_graph() {
    let (stage, level) = complex();
    let mut g = walker(&level);
    let mut fails = 0;
    let links = pd_links(&stage);
    for &(a, b, one_way) in &links {
        let (pa, pb) = (stage.waypoint_pos(a), stage.waypoint_pos(b));
        let crouch = stage.pads.pads[stage.pads.waypoints[b].padnum].flags.crouch
            || stage.pads.pads[stage.pads.waypoints[a].padnum].flags.crouch;
        let w = walk_player(&mut g, &level, pa, pb, pd_pad_floor(&level, pb).map(|f| f.0), crouch);
        if w.outcome != Outcome::Arrived {
            fails += 1;
            println!("{a:#04x} -> {b:#04x} one_way={one_way} crouch={crouch}: {:?} after {} ticks at {:.0?} (drop {:.0})", w.outcome, w.ticks, w.end, w.max_drop);
        }
    }
    println!("{} links, {fails} failed", links.len());
}

/// The player walks PD's own graph: stairs, ramps, ledge drops, the crawl space
/// (crouched) and the ladder, with the ported `bondwalk.c`. The only links it
/// fails are the ones the simulant spike found bad for bots too, for the same
/// reason: `0x01 -> 0x03` and `0x88 -> 0x8a` climb a sheer 276 cm pit wall
/// (PD's data marks them two-way); `0x0f -> 0x0e` starts at a walkway-edge pad,
/// so a start dropped to the floor under it lands on the floor below.
#[test]
fn the_player_walks_every_link_of_pds_complex_graph_but_three_known_ones() {
    let (stage, level) = complex();
    let mut g = walker(&level);
    let mut failed = Vec::new();
    for (a, b, _) in pd_links(&stage) {
        let (pa, pb) = (stage.waypoint_pos(a), stage.waypoint_pos(b));
        let crouch = stage.pads.pads[stage.pads.waypoints[b].padnum].flags.crouch
            || stage.pads.pads[stage.pads.waypoints[a].padnum].flags.crouch;
        let w = walk_player(&mut g, &level, pa, pb, pd_pad_floor(&level, pb).map(|f| f.0), crouch);
        if w.outcome != Outcome::Arrived {
            failed.push((a, b, w.outcome));
        }
    }
    let known = [(0x01, 0x03), (0x0f, 0x0e), (0x88, 0x8a)];
    let got: Vec<(usize, usize)> = failed.iter().map(|&(a, b, _)| (a, b)).collect();
    assert_eq!(got, known, "{failed:x?}");
}

/// Off a ledge the player falls with PD's gravity, lands on the floor below
/// (not through it, not hovering) and dips on landing.
#[test]
fn walking_off_a_ledge_falls_and_lands_on_the_floor_below() {
    let (stage, level) = complex();
    let mut g = walker(&level);
    // PD's one-way links are its ledge drops.
    let mut drops = 0;
    for (a, b, one_way) in pd_links(&stage) {
        if !one_way {
            continue;
        }
        let (pa, pb) = (stage.waypoint_pos(a), stage.waypoint_pos(b));
        // Where the walk really starts and ends (pad heights lie for some pads).
        let fa = drop_to_ground(&level, chr_adjust_pos_for_spawn(&level, 30.0, pa, 0.0, &[]).unwrap_or(pa)).y;
        let fb = pd_pad_floor(&level, pb).map_or_else(|| drop_to_ground(&level, pb).y, |f| f.0);
        if fa - fb < 100.0 {
            continue;
        }
        let w = walk_player(&mut g, &level, pa, pb, Some(fb), false);
        assert_eq!(w.outcome, Outcome::Arrived, "drop {a:#x} -> {b:#x}: {w:?}");
        assert!(w.max_drop > 69.0, "{a:#x} -> {b:#x} should fall, not step: {w:?}");
        assert!(w.landed, "{a:#x} -> {b:#x}: a {:.0} cm fall should land hard enough to dip", fa - fb);
        assert!((g.player.manground - g.player.ground).abs() < 1.0, "stands on the floor after landing");
        drops += 1;
    }
    assert!(drops >= 3, "only {drops} drops of 1 m+ in PD's graph");
}

/// A spawn pad on Complex with open floor `ahead` cm in front of it, in sight.
fn open_spot(level: &TileLevel, stage: &PdStage, ahead: f32) -> (Vec3, Vec3) {
    for &p in &stage.spawn_pads {
        let pad = &stage.pads.pads[p];
        let a = drop_to_ground(level, pad.pos);
        let d = Vec3::new(pad.look.x, 0.0, pad.look.z).normalize_or_zero();
        let b = drop_to_ground(level, a + d * ahead + Vec3::Y * 60.0);
        if (b.y - a.y).abs() < 1.0 && level.los(a + Vec3::Y * 150.0, b + Vec3::Y * 150.0) && level.los(a + Vec3::Y * 60.0, b + Vec3::Y * 60.0)
        {
            return (a, b);
        }
    }
    panic!("no open spawn with {ahead} cm of floor ahead");
}

/// The player's Falcon kills a simulant standing 4 m ahead: the hits reach
/// `chr_damage`, the bot dies at `maxdamage`, and the kill is the player's.
#[test]
fn the_players_falcon_kills_a_simulant() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    let mut f = Fight::new(cfg).unwrap();
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.bots.brains = false;
    f.bots.place(0, b);
    f.guns.player.place(a, theta_towards(a, b));
    f.guns.bgun.bgun_equip_weapon(WEAPON_FALCON2);
    let me = f.me;
    let mut died_at = None;
    for t in 0..60 * 12 {
        // Tap the trigger (a semi-automatic): 6 ticks down, 6 up.
        let fire = t > 60 && (t / 6) % 2 == 0;
        f.frame(&PdInput { fire, ..PdInput::default() });
        if f.bots.chrs[0].is_dead() {
            died_at = Some(t);
            break;
        }
    }
    assert!(died_at.is_some(), "the simulant survived: damage {:.2}", f.bots.chrs[0].damage);
    assert_eq!(f.bots.chrs[me].kills, 1);
    assert_eq!(f.bots.chrs[0].deaths, 1);
}

/// A simulant hunts down a player who stands still, kills them (health through
/// `chr_damage`'s player branch), and the player respawns at full health.
#[test]
fn a_simulant_kills_the_player_who_respawns() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    cfg.bots[0].difficulty = crate::pd_spike::bot::Difficulty::Hard;
    cfg.bots[0].weapon = Some(crate::pd_spike::weapons::AR34);
    let mut f = Fight::new(cfg).unwrap();
    let me = f.me;
    let mut targeted = false;
    let mut hurt = false;
    let mut died = false;
    for _ in 0..60 * 90 {
        f.frame(&PdInput { fire: died, ..PdInput::default() });
        targeted |= f.bots.chrs[0].target == Some(me);
        hurt |= f.health < 1.0 && f.dead_for.is_none();
        died |= f.dead_for.is_some();
        if died && f.dead_for.is_none() {
            break;
        }
    }
    assert!(targeted, "the simulant never targeted the player");
    assert!(hurt, "the player was never hurt");
    assert!(died, "the player never died");
    assert!(f.dead_for.is_none() && f.spawns >= 2, "no respawn (spawns {})", f.spawns);
    assert_eq!(f.health, 1.0);
    assert_eq!(f.bots.chrs[me].deaths, 1);
    assert_eq!(f.bots.chrs[0].kills, 1);
}

/// The textured BG and the collision tiles are the same building: the same
/// bounding box, and every material's texture exported beside it.
#[test]
fn the_textured_bg_matches_the_collision_tiles() {
    let def = super::bg::load().expect("native/assets/levels/pd_bg/ref (tools/pd-assets/pd_bg.py ref)");
    let (_, level) = complex();
    let (lo, hi) = level.geom.bounds();
    let mut bmin = Vec3::splat(f32::INFINITY);
    let mut bmax = Vec3::splat(f32::NEG_INFINITY);
    let mut tris = 0;
    for b in &def.batches {
        tris += b.indices.len() / 3;
        for v in &b.verts {
            let p = Vec3::new(v[0], v[1], v[2]);
            bmin = bmin.min(p);
            bmax = bmax.max(p);
        }
    }
    assert!(tris > 2000, "{tris} triangles");
    assert!((bmin - lo).abs().max_element() < 1.0 && (bmax - hi).abs().max_element() < 1.0, "BG {bmin} {bmax} vs tiles {lo} {hi}");
    let file = def.file.as_ref().unwrap();
    let dir = def.tex_dir.clone().unwrap();
    for m in &def.materials {
        if let Some(t) = &m.texture {
            let tr = file.textures.get(&t.id.to_string()).unwrap_or_else(|| panic!("texture {:#x} not listed", t.id));
            assert!(dir.join(&tr.file).exists(), "{} missing", tr.file);
        }
    }
}

/// PD's health bar at full health: the green armour strip is lit and the red
/// trauma part is empty; at a sixth of the health the armour is empty and the
/// trauma lit. Drawn into the top of the HUD canvas.
#[test]
fn the_health_bar_fills_green_then_red_and_sits_at_the_top() {
    use crate::pd_guns::font::Canvas;
    let draw = |h: f32| {
        let mut cv = Canvas::new(320, 180);
        super::health::draw_health_bar(h, 1.0, 60.0, &mut cv);
        cv
    };
    let lit = |cv: &Canvas, green: bool| {
        let mut n = 0;
        let mut top = usize::MAX;
        for y in 0..cv.h {
            for x in 0..cv.w {
                let [r, g, _, a] = cv.px[y * cv.w + x];
                if a > 0.05 && if green { g > r * 2.0 && g > 0.1 } else { r > g * 2.0 && r > 0.1 } {
                    n += 1;
                    top = top.min(y);
                }
            }
        }
        (n, top)
    };
    let full = draw(1.0);
    let (g, top) = lit(&full, true);
    assert!(g > 200, "green pixels at full health: {g}");
    assert!(top < 45, "the bar sits at the top of the view: row {top}");
    assert_eq!(lit(&full, false).0, 0, "no trauma at full health");
    let low = draw(1.0 / 6.0);
    assert!(lit(&low, false).0 > 20, "trauma shows at low health");
    assert!(lit(&low, true).0 < lit(&full, true).0 / 4, "armour is empty at low health");
}

/// One traced player walk (`WALK="a,b"` waypoint ids in hex, default 0a,0b).
/// `cargo test --release -p game --lib pd_complex::tests::probe_trace_player_walk -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_trace_player_walk() {
    let (stage, level) = complex();
    let mut g = walker(&level);
    let spec = std::env::var("WALK").unwrap_or_else(|_| "0a,0b".into());
    let ids: Vec<usize> = spec.split(',').map(|x| usize::from_str_radix(x.trim(), 16).unwrap()).collect();
    let (pa, pb) = (stage.waypoint_pos(ids[0]), stage.waypoint_pos(ids[1]));
    println!("from {pa:.0?} to {pb:.0?}, pad floor {:?}, under {:.0}", pd_pad_floor(&level, pb), drop_to_ground(&level, pb).y);
    let spot = chr_adjust_pos_for_spawn(&level, 30.0, pa, 0.0, &[]).unwrap_or(pa);
    let start = drop_to_ground(&level, spot);
    g.player.place(start, theta_towards(start, pb));
    let input = PdInput { walk_y: 127, ..PdInput::default() };
    for t in 0..400 {
        let p = g.player.pos;
        g.player.theta = theta_towards(p, pb);
        g.frame(&input, 4);
        let pl = &g.player;
        let (radius, ymax, _) = pl.player_get_bbox();
        let lad = level.cd_find_ladder(pl.pos, radius * 1.2, ymax - pl.pos.y, pl.manground - pl.pos.y + 1.0);
        if t % 10 == 0 {
            let (_, ymin) = (0, pl.manground + 30.0 - 0.1);
            let up = level.cd_test_volume_simple(pl.pos + Vec3::Y * 2.0, radius, ymax - pl.pos.y, ymin - pl.pos.y, &[]);
            let touching = level.walls_touching(pl.pos + Vec3::Y * 2.0, radius, ymax - pl.pos.y, ymin - pl.pos.y);
            println!(
                "t{t:3} pos {:.0?} man {:.1} ground {:.1} onladder {} updown {:.2} ladder-near {:?} up2 {:?} {:?}",
                pl.pos, pl.manground, pl.ground, pl.onladder, pl.ladderupdown, lad.map(|n| (n * 100.0).round() / 100.0), up, touching
            );
        }
    }
}

/// A 1-bot match against a player who stands still: a per-second timeline.
/// `cargo test --release -p game --lib pd_complex::tests::probe_hunt_the_player -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_hunt_the_player() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    let mut f = Fight::new(cfg).unwrap();
    let me = f.me;
    let mut shots_at_me = 0;
    for t in 0..60 * 60 {
        f.frame(&PdInput::default());
        shots_at_me += f.bots.shots.iter().filter(|s| s.age == 0 && s.hit_chr == Some(me)).count();
        if t % 60 == 0 {
            let b = &f.bots.chrs[0];
            let p = &f.bots.chrs[me];
            println!(
                "t{:3}s bot {:.0?} {:?} target {:?} insight {} dist {:.0} | me {:.0?} eye {:.0} h {:.0} r {:.0} room {:?} | health {:.2} shots-hit {shots_at_me} fired {}",
                t / 60, b.pos, b.actiontype, b.target, b.aibot.targetinsight, b.prop_pos().distance(p.prop_pos()), p.pos, p.player_eye_y, p.height, p.radius, p.floorroom, f.health,
                f.bots.shots.len()
            );
        }
    }
}

/// The player's rounds find PD's body parts: a Falcon round (damage 1.0) at a
/// standing simulant's head costs it 4.0 (`HITPART_HEAD` x4), one at its chest
/// 2.0 (`HITPART_TORSO` x2), and one at its shin 1.0.
///
/// The simulant is unarmed: an armed one holds its gun across its chest, and PD
/// takes the FIRST box in the model's tree order the round passes through
/// (`model_test_for_hit`), where the arms come before the torso — so a chest-high
/// round on an armed bot is an arm hit, x1. Measured, and PD's rule, not a bug.
#[test]
fn a_falcon_round_does_head_torso_and_leg_damage() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    cfg.bots[0].weapon = None;
    let mut f = Fight::new(cfg).unwrap();
    assert!(!f.rigs.is_empty() && !f.hitboxes[f.bots.chrs[0].body].is_empty(), "body rigs + hit boxes must be exported");
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.bots.brains = false;
    f.guns.bgun.bgun_equip_weapon(WEAPON_FALCON2);
    // Let the equip finish and the bot settle into its idle.
    f.bots.place(0, b);
    f.guns.player.place(a, theta_towards(a, b));
    for _ in 0..120 {
        f.frame(&PdInput::default());
    }
    // One aimed round at `height` above the bot's feet, 4 m away: the damage it did.
    let shot_at = |f: &mut Fight, height: f32| -> f32 {
        // A hit shoves the bot back and flinches it: put it back and let it settle.
        f.bots.place(0, b);
        f.bots.chrs[0].aibot.shotspeed = Vec3::ZERO;
        let c = f.bots.chrs[0].clone();
        f.bots.chrs[0].damage = 0.0;
        f.guns.player.place(a, theta_towards(a, b));
        let d = Vec2::new(b.x - a.x, b.z - a.z).length();
        let target = c.pos.y + height;
        let aim = |f: &mut Fight| f.guns.player.verta = ((target - f.guns.player.pos.y) / d).atan().to_degrees();
        for _ in 0..40 {
            aim(f);
            f.frame(&PdInput { aim: true, ..PdInput::default() });
        }
        let before = f.bots.chrs[0].damage;
        for t in 0..8 {
            aim(f);
            f.frame(&PdInput { aim: true, fire: t < 2, ..PdInput::default() });
        }
        f.bots.chrs[0].damage - before
    };
    // Several rounds each: the spread may put one on an arm at the torso's edge.
    let rounds = |f: &mut Fight, h: f32| (0..4).map(|_| shot_at(f, h)).collect::<Vec<f32>>();
    let head = rounds(&mut f, 160.0);
    let chest = rounds(&mut f, 128.0);
    let shin = rounds(&mut f, 45.0);
    let has = |v: &[f32], x: f32| v.iter().any(|d| (d - x).abs() < 1e-3);
    assert!(has(&head, 4.0), "head rounds did {head:?}");
    assert!(has(&chest, 2.0), "chest rounds did {chest:?}");
    assert!(has(&shin, 1.0), "shin rounds did {shin:?}");
    assert!(head.iter().chain(&chest).chain(&shin).all(|d| [0.0, 1.0, 2.0, 4.0].iter().any(|x| (d - x).abs() < 1e-3)), "a round did an odd amount");
}

/// Print a standing simulant's posed hit boxes (world cm) and what a level ray
/// from 4 m in front hits at each height.
/// `cargo test --release -p game --lib pd_complex::tests::probe_hit_boxes -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_hit_boxes() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    cfg.bots[0].weapon = None;
    let mut f = Fight::new(cfg).unwrap();
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.bots.brains = false;
    f.bots.place(0, b);
    f.guns.player.place(a, theta_towards(a, b));
    for _ in 0..120 {
        f.frame(&PdInput::default());
    }
    let t = f.guns.range.targets.iter().find(|t| t.chr == Some(0)).unwrap().clone();
    println!("bot at {:.0?}, coarse box {:.0?}..{:.0?}", f.bots.chrs[0].pos, t.bbox.min, t.bbox.max);
    for (i, p) in t.parts.iter().enumerate() {
        let from = p.to_local.inverse();
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for c in 0..8 {
            let q = Vec3::new(
                if c & 1 == 0 { p.min.x } else { p.max.x },
                if c & 2 == 0 { p.min.y } else { p.max.y },
                if c & 4 == 0 { p.min.z } else { p.max.z },
            );
            let w = from.transform_point3(q);
            lo = lo.min(w);
            hi = hi.max(w);
        }
        println!("  #{i:2} part {:3} y {:5.0}..{:5.0}  x {:6.0}..{:6.0} z {:6.0}..{:6.0} parent {:?}", p.hitpart, lo.y, hi.y, lo.x, hi.x, lo.z, hi.z, p.parent);
    }
    // Pitched from the eye, as the player's rounds are.
    let eye = Vec3::new(a.x, a.y + 159.0, a.z);
    for h in [160.0f32, 128.0, 45.0] {
        let d = (Vec3::new(b.x, b.y + h, b.z) - eye).normalize();
        let hit = crate::pd_guns::range::test_part_boxes(&t.parts, eye, d, 1000.0);
        let at = hit.map(|(t, _)| eye + d * t);
        let whole = f.guns.range.raycast(eye, d, 1000.0);
        println!("  eye ray at {h}: {hit:?} at {at:.0?}; range.raycast {:?}", whole.map(|w| (w.pos, w.kind, w.hitpart)));
    }
    let dir = Vec3::new(b.x - a.x, 0.0, b.z - a.z).normalize();
    for h in (0..190).step_by(10) {
        let o = Vec3::new(a.x, b.y + h as f32, a.z);
        let hit = crate::pd_guns::range::test_part_boxes(&t.parts, o, dir, 1000.0);
        println!("  ray at {h:3} cm: {hit:?}");
    }
}

/// Where aimed Falcon rounds at a height land on a standing simulant.
/// `cargo test --release -p game --lib pd_complex::tests::probe_aimed_rounds -- --ignored --nocapture`
#[test]
#[ignore]
fn probe_aimed_rounds() {
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    cfg.bots[0].weapon = None;
    let mut f = Fight::new(cfg).unwrap();
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.bots.brains = false;
    f.guns.bgun.bgun_equip_weapon(WEAPON_FALCON2);
    f.bots.place(0, b);
    for _ in 0..120 {
        f.frame(&PdInput::default());
    }
    println!("player {a:.0?} bot {b:.0?} bot theta {:.1}°", f.bots.chrs[0].theta().to_degrees());
    for h in [160.0f32, 128.0, 35.0] {
        f.guns.player.place(a, theta_towards(a, b));
        let d = Vec2::new(b.x - a.x, b.z - a.z).length();
        for t in 0..48 {
            f.guns.player.verta = ((b.y + h - f.guns.player.pos.y) / d).atan().to_degrees();
            let before = f.bots.chrs[0].damage;
            f.frame(&PdInput { aim: true, fire: t >= 40 && t < 42, ..PdInput::default() });
            let dd = f.bots.chrs[0].damage - before;
            if t >= 40 {
                println!("h {h}: t{t} eye {:.0?} verta {:.2} hitpos {:.0?} dmg {dd}", f.guns.player.pos, f.guns.player.verta, f.guns.bgun.hands[0].hitpos);
            }
        }
    }
}

/// A Laptop Gun thrown down as a sentry finds a simulant 4 m away by PD's
/// multiplayer round-robin (`nextchrtest`) and hurts it.
#[test]
fn a_deployed_laptop_sentry_shoots_a_simulant() {
    use crate::pd_guns::gset::WEAPON_LAPTOPGUN;
    use crate::pd_guns::props::ObjType;
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    cfg.bots[0].weapon = None;
    let mut f = Fight::new(cfg).unwrap();
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.bots.brains = false;
    f.bots.place(0, b);
    f.guns.player.place(a, theta_towards(a, b));
    f.frame(&PdInput { select: Some((WEAPON_LAPTOPGUN, false)), ..PdInput::default() });
    for _ in 0..150 {
        f.frame(&PdInput::default());
    }
    f.guns.player.verta = -30.0;
    for _ in 0..30 {
        f.frame(&PdInput { use_held: true, ..PdInput::default() });
    }
    for _ in 0..4 {
        f.frame(&PdInput { use_held: true, fire: true, ..PdInput::default() });
    }
    let mut hurt = 0.0f32;
    for _ in 0..600 {
        let before = f.bots.chrs[0].damage;
        f.frame(&PdInput::default());
        hurt += (f.bots.chrs[0].damage - before).max(0.0);
        if f.bots.chrs[0].is_dead() {
            break;
        }
        // Keep it standing where it was (the rounds shove it).
        if f.bots.chrs[0].pos.distance(b) > 100.0 {
            f.bots.place(0, b);
        }
    }
    assert!(f.guns.objs.iter().any(|o| o.ty == ObjType::Autogun), "no sentry deployed");
    assert!(hurt > 0.0 || f.bots.chrs[0].is_dead(), "the sentry never hurt the simulant");
}

/// Footsteps on Complex's metal floors: running simulants make them from their
/// animation's footfall frames (`footstep_check_default`), and the walking player
/// every 150 cm (`bmove_tick`), all from `g_FootstepSounds`' metal row.
#[test]
fn simulants_and_the_player_make_metal_footsteps() {
    use crate::pd_spike::chraction::FOOTSTEP_SOUNDS;
    let metal = &FOOTSTEP_SOUNDS[4 * 8..5 * 8];
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(2);
    cfg.bots[0].weapon = Some(crate::pd_spike::weapons::FALCON2);
    cfg.bots[1].weapon = Some(crate::pd_spike::weapons::FALCON2);
    let mut f = Fight::new(cfg).unwrap();
    let mut bot_steps = 0;
    let mut my_steps = 0;
    for _ in 0..60 * 20 {
        f.frame(&PdInput { walk_y: 127, ..PdInput::default() });
        for s in std::mem::take(&mut f.guns.sounds) {
            if metal.contains(&s.id) {
                if s.pan == 0.0 && s.volume == 1.0 {
                    my_steps += 1;
                } else {
                    bot_steps += 1;
                }
            }
        }
        if f.dead_for.is_some() {
            break;
        }
    }
    assert!(bot_steps > 10, "simulant footsteps: {bot_steps}");
    assert!(my_steps > 3, "player footsteps: {my_steps}");
}

/// The RC-P120's cloak hides the player from a simulant that isn't already
/// tracking them (`bot_is_target_invisible`); uncloaked, it sees them again.
#[test]
fn the_rcp120_cloak_hides_the_player_from_a_simulant() {
    use crate::pd_guns::gset::WEAPON_RCP120;
    let mut cfg = SimConfig::from_env();
    cfg.bots.truncate(1);
    let mut f = Fight::new(cfg).unwrap();
    let me = f.me;
    let (a, b) = {
        let stage = f.bots.stage.clone().unwrap();
        open_spot(&f.bots.level, &stage, 400.0)
    };
    f.guns.player.place(a, theta_towards(a, b));
    f.frame(&PdInput { select: Some((WEAPON_RCP120, false)), ..PdInput::default() });
    for _ in 0..150 {
        f.frame(&PdInput::default());
        f.bots.place(0, b);
    }
    f.guns.rcp120_cloak = true;
    for _ in 0..120 {
        f.frame(&PdInput::default());
        f.bots.place(0, b);
    }
    assert!(f.guns.cloak.cloaked && f.my_chr().cloaked, "cloak on");
    // Forget the player, then look for them for five seconds.
    f.bots.chrs[0].target = None;
    f.bots.chrs[0].aibot.targetcloaktimer60 = 0;
    f.bots.chrs[0].aibot.targetinsight = false;
    f.bots.chrs[0].aibot.chrsinsight[me] = false;
    let mut seen = 0;
    let health = f.health;
    let spawns = f.spawns;
    for _ in 0..300 {
        f.frame(&PdInput::default());
        f.bots.place(0, b);
        seen += f.bots.chrs[0].aibot.chrsinsight[me] as i32;
    }
    assert_eq!(seen, 0, "a cloaked player was seen");
    assert!(f.health == health && f.spawns == spawns, "and not shot");
    f.guns.rcp120_cloak = false;
    let mut seen = 0;
    for _ in 0..300 {
        f.frame(&PdInput::default());
        f.bots.place(0, b);
        seen += f.bots.chrs[0].aibot.chrsinsight[me] as i32;
    }
    assert!(seen > 0, "uncloaked, the simulant should see the player");
}
