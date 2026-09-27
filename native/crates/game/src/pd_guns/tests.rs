//! Headless checks for the PD guns spike. They read the exported assets under
//! `native/assets/weapons/pd_fp/` (committed with the spike).

use std::sync::Arc;

use glam::{Mat4, Vec3};

use super::anim::{Anim, AnimCtx};
use super::animdata::AnimBank;
use super::data;
use super::model::{Model, ModelDef};

fn anim_id(w: &data::WeaponsFile, name: &str) -> u16 {
    w.anims
        .iter()
        .find(|(_, m)| m.id == name)
        .map(|(k, _)| k.parse().unwrap())
        .unwrap_or_else(|| panic!("no anim {name}"))
}

#[test]
fn falcon_and_hands_share_joints_0_to_32() {
    let dir = data::assets_dir();
    let gun = data::load_model(&dir, "falcon2").unwrap();
    let hand = data::load_model(&dir, "hand_joaf1").unwrap();
    // Every POSITION node of the hand model must match the gun's joint with the
    // same anim part: same rest offset, same matrix slot (bondgun.c:8394 draws the
    // hand with the gun's matrices).
    let mut checked = 0;
    for hn in hand.nodes.iter().filter(|n| n.kind == "position") {
        let part = hn.animpart.unwrap();
        let gn = gun
            .nodes
            .iter()
            .find(|n| n.kind == "position" && n.animpart == Some(part))
            .unwrap_or_else(|| panic!("gun lacks joint {part}"));
        assert_eq!(hn.mtx.unwrap()[0], gn.mtx.unwrap()[0], "joint {part} slot");
        let d = Vec3::from(hn.pos.unwrap()) - Vec3::from(gn.pos.unwrap());
        assert!(d.length() < 0.3, "joint {part} offset differs by {d:?}");
        checked += 1;
    }
    assert_eq!(checked, 33);
}

#[test]
fn falcon_reload_moves_the_left_hand_in_and_back() {
    let dir = data::assets_dir();
    let w = data::load_weapons(&dir).unwrap();
    let bank = AnimBank::load(&dir.join("anims"), &w.anims).unwrap();
    let def = Arc::new(ModelDef::from_file(data::load_model(&dir, "falcon2").unwrap()));
    let mut m = Model::new(def);
    let reload = anim_id(&w, "ANIM_GUN_FALCON2_RELOAD");
    let mut anim = Anim::default();
    let mut ctx = AnimCtx { bank: &bank, scale: 1.0, chrinfo: None, merging_enabled: true };
    anim.set_animation(&mut ctx, reload, false, 0.0, 1.0, 0.0);

    let wrist = |m: &Model| m.matrices[18].w_axis.truncate(); // left wrist
    let gun = |m: &Model| m.matrices[33].w_axis.truncate();

    m.set_matrices_with_anim(&Mat4::IDENTITY, Some(&anim), &bank, None);
    let (w0, g0) = (wrist(&m), gun(&m));
    let mut closest = f32::MAX;
    for _ in 0..91 {
        anim.tick(&mut ctx, 1, true);
        m.set_matrices_with_anim(&Mat4::IDENTITY, Some(&anim), &bank, None);
        closest = closest.min((wrist(&m) - gun(&m)).length());
    }
    let start_gap = (w0 - g0).length();
    eprintln!("left wrist→gun: start {start_gap:.1}, closest during reload {closest:.1}");
    assert!(closest < start_gap * 0.6, "the left hand never came to the gun");
    assert_eq!(anim.frame as i32, 91, "clamped on the last frame (non-looping)");
}

/// The whole frame loop, headless: equip the Falcon, let it raise, pull the
/// trigger at the first board, and check the chain PD runs — state machine →
/// shot from the camera → hitpos → beam from the muzzle → bullet hole + sparks.
#[test]
fn falcon_fires_down_the_range_and_hits_the_board() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::Sim;

    let mut sim = Sim::new(super::sim::HAND_MODELS[0]).unwrap();
    let idle = PdInput::default();
    let mut raised_at = None;
    for f in 0..240 {
        sim.frame(&idle, 4);
        if raised_at.is_none() && sim.bgun.hands[HAND_RIGHT].visible && sim.bgun.hands[HAND_RIGHT].state == HANDSTATE_IDLE {
            raised_at = Some(f);
        }
    }
    let raised_at = raised_at.expect("the Falcon never became visible + idle");
    eprintln!("falcon visible+idle after {raised_at} frames");
    assert_eq!(sim.bgun.bgun_get_weapon_num(HAND_RIGHT), WEAPON_FALCON2);

    // The gun sits in front of and below the eye, pointing away (camera -z).
    let m = &sim.bgun.hands[HAND_RIGHT].gunmodel.as_ref().unwrap().matrices;
    let root = m[0].w_axis.truncate();
    eprintln!("gun root in camera space: {root:?}, muzzlepos {:?}", sim.bgun.hands[0].muzzlepos);
    assert!(root.z < 0.0, "gun should be in front of the camera: {root:?}");

    let fire = PdInput { fire: true, ..PdInput::default() };
    let mut fired_frame = None;
    for f in 0..30 {
        sim.frame(&fire, 4);
        if fired_frame.is_none() && sim.shots_fired > 0 {
            fired_frame = Some(f);
            let hp = sim.bgun.hands[0].hitpos;
            eprintln!("shot on frame {f}: hitpos {hp:?}, beam age {}", sim.beams[0].age);
        }
    }
    assert!(fired_frame.is_some(), "no shot left the gun");
    assert!(sim.range.targets[0].hits >= 1, "the first board took no hits: {:?}", sim.bgun.hands[0].hitpos);
    assert!(!sim.wallhits.is_empty(), "no bullet hole");
    assert!(sim.sounds.iter().any(|s| s.id == 0x804d), "no Falcon shot sound: {:?}", sim.sounds.iter().map(|s| s.id).collect::<Vec<_>>());
}

/// The spike's shaders pass the validator wgpu runs at `create_shader_module`.
#[test]
fn pd_shaders_validate() {
    for (name, src) in [
        ("pdgun.wgsl", include_str!("pdgun.wgsl")),
        ("pdfx.wgsl", include_str!("pdfx.wgsl")),
        ("pdpost.wgsl", include_str!("pdpost.wgsl")),
        ("n64video.wgsl", include_str!("n64video.wgsl")),
    ] {
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

/// Frame-by-frame trace of the Falcon's first shot (diagnostic; run with --nocapture).
#[test]
#[ignore]
fn trace_first_shot() {
    use super::bgun::*;
    use super::player::PdInput;
    use super::sim::Sim;
    let mut sim = Sim::new(super::sim::HAND_MODELS[0]).unwrap();
    for _ in 0..150 {
        sim.frame(&PdInput::default(), 4);
    }
    let fire = PdInput { fire: true, ..PdInput::default() };
    for f in 0..6 {
        sim.frame(&fire, 4);
        let h = &sim.bgun.hands[HAND_RIGHT];
        let flash_vis: Vec<bool> = h.flash_toggles.iter().map(|&n| h.gunmodel.as_ref().unwrap().visible[n]).collect();
        eprintln!(
            "f{f}: state {} minor {} firing {} flashon {} toggles {:?} shots {} beam age {} dist {:.0} maxdist {:.0}",
            h.state, h.stateminor, h.firing, h.flashon, flash_vis, sim.shots_fired, sim.beams[0].age, sim.beams[0].dist, sim.beams[0].maxdist
        );
    }
}

/// Empty the CMP150 by holding the trigger: while the trigger stays held PD
/// dry-fires (HANDSTATE_ATTACKEMPTY + the click, `bondgun.c:1185`); on release
/// it reloads by itself and the clip comes back full.
#[test]
fn cmp150_empties_and_reloads_itself() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::Sim;
    let mut sim = Sim::new(super::sim::HAND_MODELS[0]).unwrap();
    sim.frame(&PdInput { select: Some((WEAPON_CMP150, false)), ..PdInput::default() }, 4);
    for _ in 0..200 {
        sim.frame(&PdInput::default(), 4);
    }
    assert_eq!(sim.bgun.bgun_get_weapon_num(HAND_RIGHT), WEAPON_CMP150);
    let full = sim.bgun.hands[HAND_RIGHT].loadedammo[0];
    assert!(full > 0, "CMP150 clip empty after equip");
    let fire = PdInput { fire: true, ..PdInput::default() };
    let mut min_loaded = full;
    let mut saw_empty = false;
    for _ in 0..300 {
        sim.frame(&fire, 4);
        min_loaded = min_loaded.min(sim.bgun.hands[HAND_RIGHT].loadedammo[0]);
        saw_empty |= sim.bgun.hands[HAND_RIGHT].state == HANDSTATE_ATTACKEMPTY;
        assert_ne!(sim.bgun.hands[HAND_RIGHT].state, HANDSTATE_RELOAD, "PD doesn't reload with the trigger held");
    }
    assert_eq!(min_loaded, 0, "never emptied the clip");
    assert!(saw_empty, "no dry fire while holding the trigger");
    let mut saw_reload = false;
    for _ in 0..200 {
        sim.frame(&PdInput::default(), 4);
        saw_reload |= sim.bgun.hands[HAND_RIGHT].state == HANDSTATE_RELOAD;
    }
    eprintln!("clip {full}, emptied, dry-fired, reload on release {saw_reload}, shots {}", sim.shots_fired);
    assert!(saw_reload, "never reloaded after release");
    assert_eq!(sim.bgun.hands[HAND_RIGHT].loadedammo[0], full, "clip not refilled");
}

/// Diagnostic: where the muzzle smoke is after a CMP150 burst (run with --nocapture).
#[test]
#[ignore]
fn trace_muzzle_smoke() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::Sim;
    let mut sim = Sim::new(super::sim::HAND_MODELS[0]).unwrap();
    for _ in 0..150 {
        sim.frame(&PdInput::default(), 4);
    }
    let fire = PdInput { fire: true, ..PdInput::default() };
    let idle = PdInput::default();
    for f in 0..90 {
        sim.frame(if f % 6 == 0 && f < 30 { &fire } else { &idle }, 4);
        let h = &sim.bgun.hands[HAND_RIGHT];
        let w2s = sim.bgun.p.world_to_screen;
        let mz = w2s.transform_point3(h.muzzlepos);
        for s in sim.smokes.slots.iter().flatten() {
            if s.ty >= 15 {
                let parts: Vec<String> = s
                    .parts
                    .iter()
                    .filter(|p| p.size > 0.0)
                    .map(|p| {
                        let v = w2s.transform_point3(p.pos);
                        format!("({:.0},{:.0},{:.0}) s{:.1} a{:.0} c{}", v.x, v.y, v.z, p.size, p.alpha, p.count)
                    })
                    .collect();
                eprintln!("f{f} state {} muzzle {mz:.0?} age {} {}", h.state, s.age, parts.join(" "));
            }
        }
    }
    let _ = WEAPON_FALCON2;
}

// ─── deferred work, step 1: smoke + explosions ───────────────────────────────

fn settled_sim(weapon: i32) -> super::sim::Sim {
    use super::player::PdInput;
    let mut sim = super::sim::Sim::new(super::sim::HAND_MODELS[0]).unwrap();
    sim.frame(&PdInput { select: Some((weapon, false)), ..PdInput::default() }, 4);
    for _ in 0..200 {
        sim.frame(&PdInput::default(), 4);
    }
    sim
}

/// Four quick Falcon rounds push `gunsmokepoint` past 0.66; once the hand is
/// idle `smoke_create_for_hand` makes SMOKETYPE_MUZZLE_PISTOL, whose parts rise
/// from the muzzle, and the whole thing frees itself once every part fades.
#[test]
fn falcon_rapid_fire_leaves_muzzle_smoke_that_rises_and_clears() {
    use super::gset::*;
    use super::player::PdInput;
    use super::smoke::SMOKETYPE_MUZZLE_PISTOL;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let fire = PdInput { fire: true, ..PdInput::default() };
    let idle = PdInput::default();
    for f in 0..90 {
        sim.frame(if f % 6 == 0 && f < 30 { &fire } else { &idle }, 4);
    }
    let muzzle = sim.bgun.hands[0].muzzlepos;
    let smoke = sim.smokes.slots.iter().flatten().find(|s| s.ty == SMOKETYPE_MUZZLE_PISTOL).expect("no muzzle smoke");
    let parts: Vec<_> = smoke.parts.iter().filter(|p| p.size > 0.0).collect();
    assert!(parts.len() >= 5, "only {} parts", parts.len());
    for p in &parts {
        assert!((p.pos.x - muzzle.x).abs() < 1.0 && (p.pos.z - muzzle.z).abs() < 1.0, "part off the muzzle column: {:?} vs {muzzle:?}", p.pos);
        assert!(p.pos.y >= muzzle.y - 0.01, "parts only rise");
    }
    assert!(!sim.bgun.hands[0].createsmoke, "createsmoke clears once a smoke is made");
    for _ in 0..900 {
        sim.frame(&idle, 4);
    }
    assert!(!sim.smokes.slots.iter().flatten().any(|s| s.ty == SMOKETYPE_MUZZLE_PISTOL), "muzzle smoke never freed");
}

/// One player, a bullet hole within 4 m: `explosion_create_simple(BULLETHOLE)`
/// — a one-part flame that plays out and frees, and (half the time) a
/// SMOKETYPE_BULLETIMPACT puff. Past 4 m the flame is skipped.
#[test]
fn close_bullet_holes_get_a_flame_and_puff_far_ones_dont() {
    use super::explosions::EXPLOSIONTYPE_BULLETHOLE;
    use super::gset::*;
    use super::player::PdInput;
    use super::smoke::SMOKETYPE_BULLETIMPACT;
    let mut sim = settled_sim(WEAPON_FALCON2);
    sim.player.verta = -40.0; // the floor ~2 m ahead
    let fire = PdInput { fire: true, ..PdInput::default() };
    let idle = PdInput::default();
    let mut saw_flame = false;
    let mut saw_puff = false;
    for f in 0..120 {
        sim.frame(if f % 12 == 0 { &fire } else { &idle }, 4);
        saw_flame |= sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_BULLETHOLE);
        saw_puff |= sim.smokes.slots.iter().flatten().any(|s| s.ty == SMOKETYPE_BULLETIMPACT);
    }
    let hp = sim.bgun.hands[0].hitpos;
    assert!((hp - sim.player.pos).length() < 400.0, "the shots should land close: {hp:?}");
    assert!(saw_flame, "no bullet-hole flame at close range");
    assert!(saw_puff, "no bullet-impact puff in ten close hits");
    for _ in 0..60 {
        sim.frame(&idle, 4);
    }
    assert_eq!(sim.explosions.live(), 0, "bullet-hole flames last 30 + 16 ticks");

    // Now level and far: the first board is ~6.5 m away.
    sim.player.verta = 0.0;
    for f in 0..60 {
        sim.frame(if f % 12 == 0 { &fire } else { &idle }, 4);
        assert!(!sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_BULLETHOLE), "flame beyond 4 m");
    }
}

/// A rocket-sized blast next to a board: object damage on the first frame, the
/// room flash (+rangeh, decaying 2 per 240-tick), the vi shake, SMOKETYPE_LARGE
/// 20 ticks before the end, freed after duration + 16×flarespeed.
#[test]
fn rocket_explosion_hurts_the_board_flashes_the_room_shakes_and_smokes() {
    use super::explosions::EXPLOSIONTYPE_ROCKET;
    use super::gset::*;
    use super::player::PdInput;
    use super::smoke::SMOKETYPE_LARGE;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let board = (sim.range.targets[0].bbox.min + sim.range.targets[0].bbox.max) * 0.5;
    let settled = sim.room.final_brightness();
    assert!(sim.explosion_create_simple(board - Vec3::new(0.0, 0.0, 60.0), EXPLOSIONTYPE_ROCKET));
    assert!(sim.room.br_flash >= 80, "rangeh 80 flash: {}", sim.room.br_flash);
    let idle = PdInput::default();
    sim.frame(&idle, 4);
    assert!(sim.range.targets[0].damage > 1.0, "first-frame object damage: {}", sim.range.targets[0].damage);
    assert!(sim.vi.intensity > 0.0, "no shake");
    assert!(sim.room.final_brightness() > settled);
    let mut smoked_at = None;
    let mut freed_at = None;
    for f in 1..300 {
        sim.frame(&idle, 4);
        if smoked_at.is_none() && sim.smokes.slots.iter().flatten().any(|s| s.ty == SMOKETYPE_LARGE) {
            smoked_at = Some(f);
        }
        if freed_at.is_none() && sim.explosions.live() == 0 {
            freed_at = Some(f);
        }
    }
    eprintln!("smoke at {smoked_at:?}, freed at {freed_at:?}, player damage {}", sim.player_damage);
    let smoked_at = smoked_at.expect("no smoke");
    assert!((68..=72).contains(&smoked_at), "smoke at maxage-20 = 70: {smoked_at}");
    let freed_at = freed_at.expect("never freed");
    assert!((168..=172).contains(&freed_at), "freed at 90 + 16*5 = 170: {freed_at}");
    assert_eq!(sim.room.br_flash, 0, "flash decays back");
    assert_eq!(sim.vi.intensity, 0.0, "shake stops");
}

/// The spike's full `g_ExplosionTypes` and the game's `PD_EXPLOSIONS`
/// (`combat::pd_weapons`) are two transcriptions of the same table; they must
/// agree on every field both carry.
#[test]
fn explosion_table_agrees_with_the_games_port() {
    use super::explosions::EXPLOSION_TYPES;
    use crate::combat::pd_weapons::PD_EXPLOSIONS;
    for (i, (a, b)) in EXPLOSION_TYPES.iter().zip(PD_EXPLOSIONS.iter()).enumerate() {
        assert_eq!(b.index as usize, i);
        let close = |x: f32, y: f32| (x - y).abs() <= 1e-3 * x.abs().max(1.0);
        assert!(close(a.blastradius / 100.0, b.blast_radius_m), "{} blast", b.name);
        assert!(close(a.damageradius / 100.0, b.damage_radius_m), "{} damage radius", b.name);
        assert!(close(a.innersize / 100.0, b.inner_size_m), "{} innersize", b.name);
        assert!(close(a.duration as f32 / 60.0, b.duration_s), "{} duration", b.name);
        assert_eq!(a.propagationrate, b.propagation_rate, "{} propagation", b.name);
        assert!(close(a.damage, b.damage), "{} damage", b.name);
    }
}

/// Diagnostic: follow a thrown grenade (run with --nocapture).
#[test]
#[ignore]
fn trace_grenade_throw() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_GRENADE);
    let fire = PdInput { fire: true, ..PdInput::default() };
    let idle = PdInput::default();
    for f in 0..400 {
        sim.frame(if f < 3 { &fire } else { &idle }, 4);
        let h = &sim.bgun.hands[HAND_RIGHT];
        if f < 60 || f % 10 == 0 {
            let objs: Vec<String> = sim
                .objs
                .iter()
                .map(|o| {
                    let p = o.proj.as_ref();
                    format!(
                        "#{} w{:#x} t{} pos({:.0},{:.0},{:.0}) {} speed {:?} bounces {}",
                        o.id,
                        o.weaponnum,
                        o.timer240,
                        o.pos.x,
                        o.pos.y,
                        o.pos.z,
                        p.map_or("rest".to_string(), |p| format!("flags {:#x}", p.flags)),
                        p.map(|p| (p.speed * 10.0).round() / 10.0),
                        p.map_or(0, |p| p.bouncecount)
                    )
                })
                .collect();
            eprintln!("f{f} state {}/{} anim {} firing {} exps {} | {}", h.state, h.stateminor, h.anim.animnum, h.firing, sim.explosions.live(), objs.join(" ; "));
        }
    }
}

/// Diagnostic: a proximity mine thrown at the floor (run with --nocapture).
#[test]
#[ignore]
fn trace_mine_floor() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_PROXIMITYMINE);
    sim.player.verta = -45.0;
    let fire = PdInput { fire: true, ..PdInput::default() };
    let idle = PdInput::default();
    for f in 0..120 {
        sim.frame(if f < 3 { &fire } else { &idle }, 4);
        for o in &sim.objs {
            if let Some(p) = &o.proj {
                eprintln!("f{f} pos {:.1?} speed {:.2?} flags {:#x} bounces {} ymin {:.1}", o.pos, p.speed, p.flags, p.bouncecount, o.bbox.rotated_y_min(&o.realrot));
            } else {
                eprintln!("f{f} pos {:.1?} attached {} rest", o.pos, o.attached);
            }
        }
    }
}

/// Diagnostic: where a cooked grenade's explosion is (run with --nocapture).
#[test]
#[ignore]
fn trace_cook() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_GRENADE);
    let fire = PdInput { fire: true, ..PdInput::default() };
    for f in 0..262 {
        sim.frame(&fire, 4);
        if f >= 238 {
            let w2s = sim.bgun.p.world_to_screen;
            for o in &sim.objs {
                eprintln!("f{f} obj pos {:.1?} cam {:.1?} t{}", o.pos, w2s.transform_point3(o.pos), o.timer240);
            }
            for e in sim.explosions.slots.iter().flatten() {
                let parts: Vec<String> = e.parts.iter().filter(|p| p.frame > 0).map(|p| format!("fr{} cam{:.0?} s{:.0}", p.frame, w2s.transform_point3(p.pos), p.size)).collect();
                eprintln!("f{f} exp age {} pos {:.1?} cam {:.1?} campos {:.1?} | {}", e.age, e.pos, w2s.transform_point3(e.pos), sim.campos(), parts.join(" "));
            }
        }
    }
    let h = &sim.bgun.hands[0];
    eprintln!("muzzle {:.1?} state {}/{}", h.muzzlepos, h.state, h.stateminor);
}

// ─── deferred work, step 2: thrown projectiles ───────────────────────────────

fn throw_once(sim: &mut super::sim::Sim) {
    use super::player::PdInput;
    let fire = PdInput { fire: true, ..PdInput::default() };
    for _ in 0..3 {
        sim.frame(&fire, 4);
    }
}

fn run(sim: &mut super::sim::Sim, n: usize) {
    for _ in 0..n {
        sim.frame(&super::player::PdInput::default(), 4);
    }
}

/// `bgun_create_thrown_projectile` → an airborne sticky grenade with the fuse
/// less the wind-up (primetimer), bouncing off the first board, a forced first
/// hop, `projectile_settle`, then `prop_explode` at 0 — a rocket-sized blast
/// that hurts the board it rests by.
#[test]
fn grenade_is_thrown_bounces_settles_and_explodes_on_its_fuse() {
    use super::explosions::EXPLOSIONTYPE_ROCKET;
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_GRENADE);
    throw_once(&mut sim);
    let mut made = None;
    let mut max_bounces = 0;
    let mut settled = false;
    let mut exploded = None;
    for f in 0..400 {
        run(&mut sim, 1);
        if let Some(o) = sim.objs.first() {
            if made.is_none() {
                made = Some((f, o.timer240));
            }
            if let Some(p) = &o.proj {
                max_bounces = max_bounces.max(p.bouncecount);
            } else {
                settled = true;
            }
        }
        if exploded.is_none() && sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_ROCKET) {
            exploded = Some(f);
        }
    }
    let (made_at, fuse) = made.expect("no grenade thrown");
    assert!(fuse > 0 && fuse < 960, "4 s fuse less the wind-up: {fuse}");
    assert!(max_bounces >= 1, "it should bounce");
    assert!(settled, "it should come to rest");
    let exploded = exploded.expect("never exploded");
    let expect = made_at as i32 + fuse / 4 + 1;
    assert!((exploded as i32 - expect).abs() <= 2, "blast at {exploded}, fuse said ~{expect}");
    assert!(sim.objs.is_empty(), "the grenade is gone");
    assert!(sim.range.targets[0].damage > 0.0, "the board beside it takes the blast");
    assert!(sim.wallhits.iter().any(|w| w.texnum == super::fx::WALLHITTEX_SCORCH), "no scorch on the floor");
}

/// Holding the trigger past the 4 s fuse: the grenade leaves the hand already
/// at 0 (HANDSTATEMINOR_ATTACK_THROW_GRENADEWAIT), blows up on the player, and
/// the hand waits activatetime + 240 ticks before it is usable again.
#[test]
fn cooked_grenade_blows_up_in_the_hand() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_GRENADE);
    let fire = PdInput { fire: true, ..PdInput::default() };
    let mut saw_wait = false;
    for _ in 0..300 {
        sim.frame(&fire, 4);
        saw_wait |= sim.bgun.hands[HAND_RIGHT].stateminor == HANDSTATEMINOR_ATTACK_THROW_GRENADEWAIT;
    }
    assert!(saw_wait, "never entered GRENADEWAIT");
    assert!(sim.player_damage > 1.0, "the blast should hit the player: {}", sim.player_damage);
    let mut idle_at = None;
    for f in 0..600 {
        sim.frame(&PdInput::default(), 4);
        if idle_at.is_none() && sim.bgun.hands[HAND_RIGHT].state == HANDSTATE_IDLE {
            idle_at = Some(f);
        }
    }
    assert!(idle_at.is_some(), "the hand never recovered");
}

/// A timed mine thrown at the left wall sticks to it standing on the wall
/// normal (`obj_stick_default`), goes off on its 4 s timer, and scorches the
/// wall it was on (the attached branch of `prop_explode`).
#[test]
fn timed_mine_sticks_to_the_wall_and_scorches_it() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_TIMEDMINE);
    sim.player.theta = 90.0; // facing -x, the left wall 6 m away
    throw_once(&mut sim);
    let mut stuck = None;
    for f in 0..300 {
        run(&mut sim, 1);
        if stuck.is_none() {
            if let Some(o) = sim.objs.first() {
                if o.attached {
                    stuck = Some((f, o.pos, o.realrot.y_axis.normalize()));
                }
            }
        }
    }
    let (_, pos, up) = stuck.expect("the mine never stuck");
    assert!(pos.x < -560.0, "stuck on the left wall: {pos:?}");
    assert!(up.x > 0.95, "stood on the wall normal (+x): {up:?}");
    assert!(sim.objs.is_empty(), "the timer should have fired");
    let scorch = sim.wallhits.iter().find(|w| w.texnum == super::fx::WALLHITTEX_SCORCH).expect("no scorch");
    assert!(scorch.corners.iter().all(|c| (c.x + 600.0).abs() < 1.0), "the scorch lies on the wall: {:?}", scorch.corners);
}

/// Remote mine: stuck, waits forever; B + fire presses the detonator
/// (HANDATTACKTYPE_DETONATE → `g_PlayersDetonatingMines`), and it goes up.
#[test]
fn remote_mine_waits_for_the_detonator() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_REMOTEMINE);
    sim.player.theta = 90.0;
    throw_once(&mut sim);
    run(&mut sim, 400);
    assert_eq!(sim.objs.len(), 1, "a remote mine does not go off by itself");
    assert!(sim.objs[0].attached);
    let hold = PdInput { use_held: true, ..PdInput::default() };
    for _ in 0..30 {
        sim.frame(&hold, 4);
    }
    let mut clicked = false;
    for _ in 0..4 {
        sim.frame(&PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
        clicked |= sim.sounds.iter().any(|s| s.id == 0x80ab);
    }
    for _ in 0..4 {
        sim.frame(&hold, 4);
    }
    assert!(sim.objs.is_empty(), "the detonator should have set it off");
    assert!(sim.explosions.live() > 0);
    assert!(clicked, "no detonator click");
}

/// Proximity mine: arms after 4 s, then the player within 2.5 m sets it off —
/// their own included, as in PD.
#[test]
fn proximity_mine_arms_then_takes_the_player_who_walks_up() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_PROXIMITYMINE);
    sim.player.verta = -45.0;
    throw_once(&mut sim);
    run(&mut sim, 300);
    let o = sim.objs.first().expect("no mine");
    assert_eq!(o.timer240, 1, "armed after 4 s");
    let at = o.pos;
    assert!((at - sim.player.pos).length() > 250.0, "landed out of reach: {at:?}");
    sim.player.pos.x = at.x;
    sim.player.pos.z = at.z - 150.0;
    run(&mut sim, 3);
    assert!(sim.objs.is_empty(), "it should go off");
    assert!(sim.player_damage > 0.0);
}

/// The throwing knife (secondary) flies true and sticks in the first board,
/// which counts the hit (PD scores it in the firing range).
#[test]
fn thrown_knife_sticks_in_the_board() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_COMBATKNIFE);
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    run(&mut sim, 60);
    assert_eq!(sim.bgun.hands[0].weaponfunc, FUNC_SECONDARY, "B held should switch to throw");
    sim.player.verta = -3.0;
    throw_once(&mut sim);
    run(&mut sim, 60);
    let knife = sim.objs.iter().find(|o| o.weaponnum == WEAPON_COMBATKNIFE).expect("no knife");
    assert_eq!(knife.embedded_board, Some(0), "stuck in the first board: {:?}", knife.pos);
    assert!(sim.range.targets[0].hits >= 1);
}

/// N-Bomb: impact detonation → `nbomb_create_storm`; the dome reaches 500 cm at
/// 80 ticks, darkens the room, fades from 310, and is gone after 370.
#[test]
fn nbomb_storm_grows_darkens_and_fades() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_NBOMB);
    throw_once(&mut sim);
    let mut born = None;
    for f in 0..200 {
        run(&mut sim, 1);
        if born.is_none() && sim.nbombs.active() {
            born = Some(f);
        }
    }
    assert!(born.is_some(), "no storm");
    // A fresh storm for exact ages.
    let pos = sim.player.pos + Vec3::new(0.0, 0.0, 400.0);
    let mut s2 = settled_sim(WEAPON_FALCON2);
    let mut out = super::nbomb::NbombOut::default();
    s2.nbombs.create_storm(pos, &mut out);
    run(&mut s2, 80);
    let n = s2.nbombs.bombs.iter().find(|n| n.age240 >= 0).unwrap();
    assert!((n.radius - 500.0).abs() < 30.0, "radius at 80 ticks: {}", n.radius);
    assert!(s2.room.br_flash < -100, "the storm darkens the room: {}", s2.room.br_flash);
    run(&mut s2, 300);
    assert!(!s2.nbombs.active(), "gone after 370 ticks");
}

// ─── deferred work, step 3: fired projectiles ────────────────────────────────

fn secondary(sim: &mut super::sim::Sim) {
    use super::player::PdInput;
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    run(sim, 80);
}

fn fire_once(sim: &mut super::sim::Sim) {
    use super::player::PdInput;
    sim.frame(&PdInput { fire: true, ..PdInput::default() }, 4);
}

/// The launcher shows its rocket (`bgun_create_held_rocket`, drawn at the
/// muzzle); firing turns that same object into a powered projectile with a
/// smoke trail that blows up on what it hits.
#[test]
fn rocket_launcher_holds_its_rocket_fires_it_and_it_blows_on_impact() {
    use super::explosions::EXPLOSIONTYPE_ROCKET;
    use super::gset::*;
    use super::props::*;
    let mut sim = settled_sim(WEAPON_ROCKETLAUNCHER);
    let held = sim.bgun.hands[0].rocket.expect("no rocket in the launcher");
    let o = sim.objs.iter().find(|o| o.id == held).unwrap();
    assert!(o.heldrocket && o.throwthrough);
    assert!((o.pos - sim.bgun.hands[0].muzzlepos).length() < 0.01, "it sits at the muzzle");
    assert_eq!(sim.held_rockets().len(), 1);
    fire_once(&mut sim);
    run(&mut sim, 2);
    let o = sim.objs.iter().find(|o| o.id == held).expect("the held rocket becomes the projectile");
    assert!(!o.heldrocket);
    let p = o.proj.as_ref().expect("flying");
    assert!(p.flags & PROJECTILEFLAG_POWERED != 0, "no gravity");
    assert!((o.scale - PROP_MODEL_SCALE * 2.1).abs() < 1e-4, "scaled ×2.1 in flight");
    let mut trail = false;
    let mut boom = false;
    for _ in 0..200 {
        run(&mut sim, 1);
        trail |= sim.smokes.slots.iter().flatten().any(|s| s.ty == super::smoke::SMOKETYPE_ROCKETTAIL);
        boom |= sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_ROCKET);
    }
    assert!(trail, "no rocket trail");
    assert!(boom, "no blast on impact");
    assert!(!sim.objs.iter().any(|o| o.id == held), "the rocket is gone");
    assert!(sim.bgun.hands[0].rocket.is_some(), "the next rocket is loaded");
}

/// Devastator: the grenade round arcs and goes off when it lands.
#[test]
fn devastator_round_goes_off_when_it_lands() {
    use super::explosions::EXPLOSIONTYPE_ROCKET;
    use super::gset::*;
    use super::props::WEAPON_GRENADEROUND;
    let mut sim = settled_sim(WEAPON_DEVASTATOR);
    sim.player.verta = -10.0;
    fire_once(&mut sim);
    run(&mut sim, 2);
    assert!(sim.objs.iter().any(|o| o.weaponnum == WEAPON_GRENADEROUND && o.proj.is_some()), "no round in flight");
    let mut boom = None;
    for f in 0..200 {
        run(&mut sim, 1);
        if boom.is_none() && sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_ROCKET) {
            boom = Some(f);
        }
    }
    let f = boom.expect("the round never went off");
    assert!(f < 60, "it should go off on landing, not on its 20 s timer: {f}");
}

/// Devastator wall hugger: sticks to the wall, holds for 2 s, drops, and goes
/// off on the floor.
#[test]
fn wall_hugger_sticks_drops_and_blows() {
    use super::gset::*;
    use super::props::WEAPON_GRENADEROUND;
    let mut sim = settled_sim(WEAPON_DEVASTATOR);
    secondary(&mut sim);
    assert_eq!(sim.bgun.hands[0].weaponfunc, FUNC_SECONDARY);
    sim.player.theta = 270.0; // facing +x, the right wall 6 m away
    fire_once(&mut sim);
    let mut stuck_at = None;
    let mut dropped_at = None;
    let mut blew_at = None;
    for f in 0..400 {
        run(&mut sim, 1);
        let o = sim.objs.iter().find(|o| o.weaponnum == WEAPON_GRENADEROUND);
        match o {
            Some(o) if o.attached && stuck_at.is_none() => stuck_at = Some(f),
            Some(o) if stuck_at.is_some() && !o.attached && dropped_at.is_none() => dropped_at = Some(f),
            None if stuck_at.is_some() && blew_at.is_none() => blew_at = Some(f),
            _ => {}
        }
    }
    let s = stuck_at.expect("never stuck");
    let d = dropped_at.expect("never dropped");
    let b = blew_at.expect("never blew");
    assert!((110..=130).contains(&(d - s)), "held ~480 quarter-ticks: {}", d - s);
    assert!(b > d, "it blows after it drops");
}

/// The crossbow bolt sticks in the first board and quivers (timer240 13 → 1).
#[test]
fn crossbow_bolt_sticks_and_quivers() {
    use super::gset::*;
    use super::props::WEAPON_BOLT;
    let mut sim = settled_sim(WEAPON_CROSSBOW);
    fire_once(&mut sim);
    let mut quiver = Vec::new();
    for _ in 0..60 {
        run(&mut sim, 1);
        if let Some(o) = sim.objs.iter().find(|o| o.weaponnum == WEAPON_BOLT && o.attached) {
            quiver.push(o.timer240);
        }
    }
    let bolt = sim.objs.iter().find(|o| o.weaponnum == WEAPON_BOLT).expect("no bolt");
    assert!(bolt.attached, "stuck");
    assert!(sim.range.targets[0].hits >= 1, "the board counts it");
    assert!(quiver.first().copied().unwrap_or(0) >= 12 && *quiver.last().unwrap() == 1, "quiver ran 13 → 1: {quiver:?}");
}

/// Slayer fly-by-wire: the camera rides the rocket, the stick turns it, fire
/// blows it, and the view comes back through one frame of static.
#[test]
fn slayer_fly_by_wire_rides_steers_and_blows() {
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::VisionMode;
    let mut sim = settled_sim(WEAPON_SLAYER);
    secondary(&mut sim);
    fire_once(&mut sim);
    run(&mut sim, 30);
    assert_eq!(sim.visionmode, VisionMode::SlayerRocket);
    let id = sim.slayer.expect("no rocket to ride").rocket;
    let rocket = sim.objs.iter().find(|o| o.id == id).unwrap();
    let cam = sim.bgun.p.projection.w_axis.truncate();
    assert!((cam - rocket.pos).length() < 20.0, "the camera is on the rocket: {cam:?} vs {:?}", rocket.pos);
    let heading0 = rocket.proj.as_ref().unwrap().speed.normalize();
    for _ in 0..40 {
        sim.frame(&PdInput { walk_x: 127, ..PdInput::default() }, 4);
    }
    let rocket = sim.objs.iter().find(|o| o.id == id).unwrap();
    let heading1 = rocket.proj.as_ref().unwrap().speed.normalize();
    assert!(heading0.dot(heading1) < 0.99, "the stick should turn it: {heading0:?} → {heading1:?}");
    assert!(sim.player.pos == sim.prev_player_pos, "Jo stands still while riding");
    fire_once(&mut sim);
    run(&mut sim, 1);
    assert!(sim.static_alpha > 0.99, "the signal is lost: static");
    run(&mut sim, 1);
    assert_eq!(sim.visionmode, VisionMode::Normal);
    assert!(sim.explosions.live() > 0);
}

/// Phoenix secondary: FUNCFLAG_EXPLOSIVESHELLS — the round that stops in the
/// board blows up there (EXPLOSIONTYPE_PHOENIX).
#[test]
fn phoenix_explosive_shells_blow_on_the_board() {
    use super::explosions::EXPLOSIONTYPE_PHOENIX;
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_PHOENIX);
    secondary(&mut sim);
    assert_eq!(sim.bgun.hands[0].weaponfunc, FUNC_SECONDARY);
    fire_once(&mut sim);
    run(&mut sim, 20);
    assert!(sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_PHOENIX), "no Phoenix blast");
}

/// SuperDragon secondary: its grenade round (gunfunc FUNC_2) goes off as
/// EXPLOSIONTYPE_SDGRENADE.
#[test]
fn superdragon_grenade_uses_the_sdgrenade_blast() {
    use super::explosions::EXPLOSIONTYPE_SDGRENADE;
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_SUPERDRAGON);
    secondary(&mut sim);
    sim.player.verta = -12.0;
    fire_once(&mut sim);
    let mut boom = false;
    for _ in 0..200 {
        run(&mut sim, 1);
        boom |= sim.explosions.slots.iter().flatten().any(|e| e.ty == EXPLOSIONTYPE_SDGRENADE);
    }
    assert!(boom, "no SDGRENADE blast");
}

/// Diagnostic: deploy the Laptop Gun as a sentry (run with --nocapture).
#[test]
#[ignore]
fn trace_laptop_deploy() {
    use super::bgun::*;
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_LAPTOPGUN);
    sim.player.verta = -30.0;
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    for f in 0..500 {
        let inp = if f < 4 { PdInput { use_held: true, fire: true, ..PdInput::default() } } else { PdInput::default() };
        sim.frame(&inp, 4);
        let h = &sim.bgun.hands[HAND_RIGHT];
        if f < 80 && f % 4 == 0 || f % 40 == 0 {
            let objs: Vec<String> = sim
                .objs
                .iter()
                .map(|o| {
                    let a = o.autogun.as_ref();
                    format!(
                        "{:?} pos({:.0},{:.0},{:.0}) {} yrot {:.2} xrot {:.2} target {:?} firing {} ammo {}",
                        o.ty,
                        o.pos.x,
                        o.pos.y,
                        o.pos.z,
                        if o.proj.is_some() { "fly" } else if o.attached { "stuck" } else { "rest" },
                        a.map_or(0.0, |a| a.yrot),
                        a.map_or(0.0, |a| a.xrot),
                        a.and_then(|a| a.target),
                        a.map_or(false, |a| a.firing),
                        a.map_or(0, |a| a.ammoquantity)
                    )
                })
                .collect();
            let hits: u32 = sim.range.targets.iter().map(|t| t.hits).sum();
            eprintln!("f{f} w {} state {}/{} throwing {} hits {hits} | {}", sim.bgun.bgun_get_weapon_num(HAND_RIGHT), h.state, h.stateminor, sim.bgun.ctrl.throwing, objs.join(" ; "));
        }
    }
}

// ─── deferred work, step 4: the Laptop sentry ────────────────────────────────

/// Laptop Gun, B + fire: FUNCFLAG_DISCARDWEAPON takes it out of the inventory
/// and lowers it; the lowered gun is thrown as the secondary (`laptop_deploy`),
/// lands, and — PD's firing-range autogun — picks the nearest board facing it
/// and shoots it every other tick until its 200 rounds are gone.
#[test]
fn laptop_deploys_as_a_sentry_and_shoots_the_boards() {
    use super::gset::*;
    use super::player::PdInput;
    use super::props::ObjType;
    let mut sim = settled_sim(WEAPON_LAPTOPGUN);
    sim.player.verta = -30.0;
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    for _ in 0..4 {
        sim.frame(&PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
    }
    run(&mut sim, 200);
    assert!(!sim.bgun.p.inventory.iter().any(|(w, _)| *w == WEAPON_LAPTOPGUN), "the laptop left the inventory");
    assert_ne!(sim.bgun.bgun_get_weapon_num(0), WEAPON_LAPTOPGUN, "switched away");
    let sentry = sim.objs.iter().find(|o| o.ty == ObjType::Autogun).expect("no sentry");
    let a = sentry.autogun.as_ref().unwrap();
    assert!(a.target.is_some() || a.seentarget, "it found a board");
    let hits0: u32 = sim.range.targets.iter().map(|t| t.hits).sum();
    assert!(hits0 > 0, "it hit a board");
    run(&mut sim, 600);
    let a = sim.objs.iter().find(|o| o.ty == ObjType::Autogun).unwrap().autogun.as_ref().unwrap();
    assert_eq!(a.ammoquantity, 0, "all 200 rounds spent");
    let hits1: u32 = sim.range.targets.iter().map(|t| t.hits).sum();
    assert!(hits1 > 40, "most rounds should score: {hits1}");
    // A second deploy blows up the first (one per player).
    sim.restock();
    sim.frame(&PdInput { select: Some((WEAPON_LAPTOPGUN, false)), ..PdInput::default() }, 4);
    run(&mut sim, 200);
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    for _ in 0..4 {
        sim.frame(&PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
    }
    let mut blew = false;
    for _ in 0..200 {
        run(&mut sim, 1);
        blew |= sim.explosions.slots.iter().flatten().any(|e| e.ty == super::explosions::EXPLOSIONTYPE_LAPTOP);
    }
    assert!(blew, "the old sentry should blow up");
    assert_eq!(sim.objs.iter().filter(|o| o.ty == ObjType::Autogun).count(), 1);
}

// ─── deferred work, step 5: the Farsight ─────────────────────────────────────

/// Aiming the Farsight turns on x-ray (`bondgun.c:8007`): erasertime counts
/// from 0, the zoom blur starts at 249/255 and settles at 99/255 after 200
/// ticks (`lv.c:1462`), the eraser sits 5 m ahead and zooming pushes it out
/// (`bg.c:5253`); letting go turns it off.
#[test]
fn farsight_aim_turns_on_xray_with_the_smear_and_the_eraser_follows_the_zoom() {
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::VisionMode;
    let mut sim = settled_sim(WEAPON_FARSIGHT);
    assert_eq!(sim.visionmode, VisionMode::Normal);
    let aim = PdInput { aim: true, ..PdInput::default() };
    sim.frame(&aim, 4);
    assert_eq!(sim.visionmode, VisionMode::Xray);
    assert_eq!(sim.erasertime, 0);
    let (a, sx, sy) = sim.xray_zoom_blur().unwrap();
    assert!((a - 249.0 / 255.0).abs() < 1e-6 && sx == 1.05 && sy == 1.05);
    // -500 / c_lodscalez: a 16:9 view is 180 PD lines, so even at 60° the
    // lod scale is 120/90 and the eraser sits 3.75 m out (PD's widescreen too).
    let ahead = sim.eraser.pos - sim.campos();
    let want = 500.0 / sim.bgun.p.c_lodscalez;
    assert!((ahead.length() - want).abs() < 1.0 && ahead.z > 0.0, "{want} ahead: {ahead:?}");
    for _ in 0..60 {
        sim.frame(&aim, 4);
    }
    assert_eq!(sim.erasertime, 240);
    assert!((sim.xray_zoom_blur().unwrap().0 - 99.0 / 255.0).abs() < 1e-6);
    // Manual zoom in (C-up held): the fov narrows, the eraser goes further.
    let zoom = PdInput { aim: true, zoom_in: true, ..PdInput::default() };
    // Two seconds of C-up: 60° / 1.0125^120 ≈ 13.5° (the Farsight zooms at
    // half rate), which puts the eraser ~17 m down the hall. (Fully zoomed to
    // 2° it would sit ~12 km out, past every wall.)
    for _ in 0..120 {
        sim.frame(&zoom, 4);
    }
    let fov = sim.bgun.p.gunzoomfovs[1];
    assert!((fov - 60.0 / 1.0125f32.powi(120)).abs() < 0.1, "zoomed: {fov}");
    let far = (sim.eraser.pos - sim.campos()).length();
    assert!(far > 1200.0 && sim.eraser.pos.z < 3300.0, "the eraser rides the zoom: {far}");
    // The x-ray picture: BG only near the eraser, the gun gone.
    assert!(sim.gun_fx().is_empty());
    let bg = sim.world_fx().into_iter().find(|b| b.kind == super::fx::FxKind::XrayBg).expect("x-ray BG");
    assert!(!bg.verts.is_empty());
    assert!(bg.verts.iter().all(|v| (v.pos - sim.eraser.pos).length() < 400.0 + super::xray::XRAY_TESS * 1.5));
    sim.frame(&PdInput::default(), 4);
    assert_eq!(sim.visionmode, VisionMode::Normal);
    assert!(sim.xray_zoom_blur().is_none());
}

/// The Farsight shoots through walls (`prop.c:688`, `:724`): with a wall
/// between Jo and the first board, a Falcon round stops at the wall but the
/// Farsight's scores, sparking the wall outside x-ray and not in it.
#[test]
fn farsight_rounds_go_through_the_wall_to_the_board() {
    use super::gset::*;
    use super::player::PdInput;
    use super::range::Aabb;
    use glam::Vec3;
    let wall = Aabb::new(Vec3::new(-150.0, 0.0, 150.0), Vec3::new(150.0, 400.0, 170.0));
    let mut falcon = settled_sim(WEAPON_FALCON2);
    falcon.range.solids.push(wall);
    fire_once(&mut falcon);
    run(&mut falcon, 30);
    assert_eq!(falcon.range.targets[0].hits, 0, "the wall stops a Falcon round");

    for xray in [false, true] {
        let mut sim = settled_sim(WEAPON_FARSIGHT);
        sim.range.solids.push(wall);
        let aim = PdInput { aim: xray, ..PdInput::default() };
        for _ in 0..20 {
            sim.frame(&aim, 4);
        }
        assert_eq!(sim.visionmode == super::sim::VisionMode::Xray, xray);
        let sparks0 = sim.sparks.live();
        sim.frame(&PdInput { fire: true, ..aim }, 4);
        let mut sparked = false;
        for _ in 0..60 {
            sim.frame(&aim, 4);
            sparked |= sim.sparks.live() > sparks0;
        }
        assert!(sim.range.targets[0].hits >= 1, "x-ray {xray}: through the wall to the board");
        assert_eq!(sparked, !xray, "x-ray {xray}: the wall sparks only outside x-ray");
    }
}


// ─── deferred work, step 6: boost + cloak ────────────────────────────────────

/// Combat Boost (`bgun_apply_boost` → `bgun_add_boost`, `lv.c:1478`,
/// `lv.c:2061`): a pill buys 10 s; the wipe (zoom blur + white fade) peaks at
/// step 15 where `speedpillon` flips; while on, a 20 Hz frame is capped to 4
/// quarter-ticks (one third speed) and the heartbeat loops; when the time runs
/// out it wipes back off with Jo's groan and full speed returns.
#[test]
fn combat_boost_slows_time_with_a_wipe_and_wears_off() {
    use super::gset::*;
    use super::player::PdInput;
    use super::sim::MISCSFX_LOOP_BASE;
    let mut sim = settled_sim(WEAPON_COMBATBOOST);
    sim.sounds.clear();
    let loaded = sim.bgun.hands[0].loadedammo[0];
    sim.frame(&PdInput { fire: true, ..PdInput::default() }, 12);
    let mut peak = 0.0f32;
    for _ in 0..40 {
        sim.frame(&PdInput::default(), 12);
        peak = peak.max(sim.boost_fx.map_or(0.0, |f| f.2));
    }
    assert!(sim.speedpill.on && sim.speedpill.change == 30, "{:?}", sim.speedpill);
    assert_eq!(sim.bgun.hands[0].loadedammo[0], loaded - 1, "one pill used");
    assert!((peak - 15.0 * 0.006_666_667).abs() < 1e-4, "white fade peaks at 0.1: {peak}");
    assert_eq!(sim.lv().lvupdate240, 4, "20 Hz frames run at a third speed");
    assert!(sim.sounds.iter().any(|r| r.id == 0x05c9), "Jo: boost activate");
    assert!(sim.sounds.iter().any(|r| r.id == 0x05c8 && r.loop_hand == Some(MISCSFX_LOOP_BASE)), "heartbeat loop");
    let t0 = sim.speedpill.time;
    sim.frame(&PdInput::default(), 12);
    assert_eq!(t0 - sim.speedpill.time, 1, "the boost clock runs in game ticks");
    // Run it out.
    sim.speedpill.time = 10;
    for _ in 0..60 {
        sim.frame(&PdInput::default(), 12);
    }
    assert!(!sim.speedpill.want && !sim.speedpill.on && sim.speedpill.change == 0, "{:?}", sim.speedpill);
    assert!(sim.sounds.iter().any(|r| r.id == 0x02ad), "Jo groans as it wears off");
    assert!(sim.stop_loops.contains(&MISCSFX_LOOP_BASE), "heartbeat stops");
    assert_eq!(sim.lv().lvupdate240, 12, "full speed again");
}

/// RC-P120 cloak (hold B + fire, `prop.c:1367`): Jo fades out over ~1 s
/// (`chr_update_cloak`) and the gun with him (`chr_get_cloak_alpha` → env
/// alpha); the cloak eats 0.4 rounds a tick once fully in (`bondgun.c:8055`);
/// firing drops it for 2 s (`chr_uncloak_temporarily`), then it comes back;
/// switching guns turns it off.
#[test]
fn rcp120_cloak_fades_eats_ammo_breaks_on_firing_and_comes_back() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_RCP120);
    sim.sounds.clear();
    for _ in 0..30 {
        sim.frame(&PdInput { use_held: true, ..PdInput::default() }, 4);
    }
    for _ in 0..4 {
        sim.frame(&PdInput { use_held: true, fire: true, ..PdInput::default() }, 4);
    }
    assert!(sim.rcp120_cloak && sim.cloak.cloaked);
    assert!(sim.sounds.iter().any(|r| r.id == 0x005b), "cloak on");
    run(&mut sim, 70);
    assert!(sim.cloak.fadefinished && sim.cloak.alpha() <= 20, "{:?} alpha {}", sim.cloak, sim.cloak.alpha());
    let a0 = sim.bgun.hands[0].loadedammo[0];
    run(&mut sim, 50);
    let used = a0 - sim.bgun.hands[0].loadedammo[0];
    assert!((19..=21).contains(&used), "0.4 rounds a tick: {used}");
    // Shoot: the cloak drops, and returns 120 ticks later.
    run(&mut sim, 2);
    sim.frame(&PdInput { fire: true, ..PdInput::default() }, 4);
    run(&mut sim, 2);
    assert!(!sim.cloak.cloaked && sim.cloak.pause > 100, "{:?}", sim.cloak);
    assert!(sim.sounds.iter().any(|r| r.id == 0x005c), "cloak off");
    assert!(sim.rcp120_cloak, "the device stays on");
    run(&mut sim, 125);
    assert!(sim.cloak.cloaked, "back on after the pause");
    // Another gun: off.
    sim.frame(&PdInput { select: Some((WEAPON_FALCON2, false)), ..PdInput::default() }, 4);
    run(&mut sim, 120);
    assert!(!sim.rcp120_cloak && !sim.cloak.cloaked);
    assert_eq!(sim.cloak.alpha(), 255);
}

// ─── deferred work, step 7: the HUD ──────────────────────────────────────────

/// The ROM fonts decode (`text_load_font`): the numeric '8' is 3×5 with its
/// halo border, drawn by `text_render_v1` as a green core in a dark halo.
#[test]
fn hud_fonts_decode_and_draw_a_haloed_digit() {
    use super::font::Canvas;
    use super::hud::HudFonts;
    let f = HudFonts::load().expect("fonts");
    let eight = f.numeric.chars[(b'8' - 0x21) as usize];
    assert_eq!((eight.width, eight.height), (3, 5));
    let mut cv = Canvas::new(32, 16);
    f.numeric.render_v1(&mut cv, 4, 4, "8", 0x00ff00a0, 0x000000a0);
    let lit: Vec<_> = cv.px.iter().filter(|p| p[3] > 0.0).collect();
    assert!(lit.iter().any(|p| p[1] > 0.5 && p[0] < 0.1), "green core");
    assert!(lit.iter().any(|p| p[1] < 0.05 && p[3] > 0.3), "dark halo");
    // text_measure: HandelGothic XS "Falcon 2" has a width and the XS line height.
    let (h, w) = f.handelgothicxs.measure("Falcon 2\n");
    assert!(w > 20 && h > 0, "{w}×{h}");
}

/// The magazine gauge's `abmag` follows the clip: settled full, a shot starts
/// a fade (change −1) that settles one unit lower; the 800-round reserve uses
/// the merged bar (36 px).
#[test]
fn hud_gauges_follow_the_clip_and_the_reserve() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let hs = &sim.hud_state;
    assert_eq!(hs.abmag[0].ref_, 8, "full clip");
    assert_eq!(hs.ctrl_abmag.ref_, 36, "full reserve bar");
    fire_once(&mut sim);
    run(&mut sim, 3);
    assert_eq!(sim.hud_state.abmag[0].change, -1, "the spent round fading");
    run(&mut sim, 60);
    assert_eq!((sim.hud_state.abmag[0].ref_, sim.hud_state.abmag[0].change), (7, 0));
    assert!(sim.hud.as_ref().unwrap().px.iter().any(|p| p[3] > 0.0), "something drawn");
}

/// The function square fades red → yellow on the secondary (`fnfader`), and
/// the function name follows.
#[test]
fn hud_function_square_goes_yellow_on_the_secondary() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_CMP150);
    assert_eq!(sim.hud_state.fnfader, 0);
    secondary(&mut sim);
    assert_eq!(sim.hud_state.fnfader, 255);
    let cv = sim.hud.as_ref().unwrap();
    let (x, y) = (cv.w - 9 - 24 - 8, cv.h - 13 - 5);
    let p = cv.px[y * cv.w + x];
    assert!(p[0] > 0.2 && p[1] > 0.2 && p[2] < 0.01, "yellow square: {p:?}");
    let func = sim.gset.func(WEAPON_CMP150, FUNC_SECONDARY).unwrap();
    assert_eq!(sim.hud_state.curfnstr.as_deref(), Some(func.name.as_str()));
}


// ─── playtest fixes: sounds and the remote mine's hands ──────────────────────

/// Every explosion sound the range's blasts use is in the sound pack (a pack
/// re-export once dropped the SFXMAP entries and every blast went silent),
/// except 0x80a5, which is an MP3 speech file in PD.
#[test]
fn every_explosion_sound_is_in_the_pack() {
    let path = format!("{}/../../assets/audio/pd/sfx/sfx_manifest.json", env!("CARGO_MANIFEST_DIR"));
    let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let have: std::collections::HashSet<u16> = m
        .as_object()
        .unwrap()
        .keys()
        .filter_map(|k| match k.strip_prefix("SFXMAP_") {
            Some(r) => u16::from_str_radix(&r[..4], 16).ok(),
            None => u16::from_str_radix(k, 16).ok(),
        })
        .collect();
    for (i, t) in super::explosions::EXPLOSION_TYPES.iter().enumerate() {
        if t.sound != 0 && t.sound != 0x80a5 {
            assert!(have.contains(&t.sound), "explosion type {i}: sound {:#06x} missing", t.sound);
        }
    }
    for id in [0x80a9u16, 0x80aa, 0x8079, 0x810c, 0x05c8, 0x005b] {
        assert!(have.contains(&id), "{id:#06x} missing");
    }
}

/// A thrown grenade is heard leaving the hand (SFXMAP_80A9_THROW) and going
/// off 5 m away at full volume: explosions take their audio config's
/// distances (25 m full for 0x809F), not the 4 m default.
#[test]
fn grenade_throw_and_blast_are_audible() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_GRENADE);
    sim.sounds.clear();
    throw_once(&mut sim);
    run(&mut sim, 300);
    let throw = sim.sounds.iter().find(|r| r.id == 0x80a9).expect("no throw sound");
    assert!(throw.volume > 0.99);
    let boom = sim.sounds.iter().find(|r| r.id == 0x809f).expect("no blast sound");
    assert!(boom.volume > 0.99, "blast volume {}", boom.volume);
    // PD's curve: full to dist1, sqrt fade to 1000/32767 at dist2, then linear.
    use super::sim::ps_calculate_volume_from_distance as vol;
    assert_eq!(vol(2000.0, [2500.0, 4900.0, 5000.0]), 1.0);
    assert!((vol(4899.0, [2500.0, 4900.0, 5000.0]) - 1000.0 / 32767.0).abs() < 0.01);
    assert_eq!(vol(5000.0, [2500.0, 4900.0, 5000.0]), 0.0);
}

/// A mine landing plays the mine-landing sound (SFXMAP_80AA), not a gunshot
/// ricochet (`bgun_play_bg_hit_sound`, `bondgun.c:8781`).
#[test]
fn a_mine_landing_sounds_like_a_mine_not_a_ricochet() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_TIMEDMINE);
    sim.player.theta = 90.0;
    sim.sounds.clear();
    throw_once(&mut sim);
    run(&mut sim, 60);
    assert!(sim.objs.first().is_some_and(|o| o.attached), "stuck");
    assert!(sim.sounds.iter().any(|r| r.id == 0x80aa), "mine landing sound");
    assert!(!sim.sounds.iter().any(|r| (0x13..=0x2a).contains(&r.id)), "no ricochet");
}

/// The remote mine's gunviscmds put the mine in the right hand and the
/// detonator in the left (`gunviscmds_remotemine`, GUNVISOP_SETVISIBILITY).
#[test]
fn remote_mine_hands_hold_the_mine_and_the_detonator() {
    use super::gset::*;
    const MODELPART_REMOTEMINE_MINE: i32 = 40;
    const MODELPART_REMOTEMINE_DETONATOR: i32 = 41;
    let sim = settled_sim(WEAPON_REMOTEMINE);
    let cmds = &sim.gset.weapon(WEAPON_REMOTEMINE).unwrap().gunviscmds;
    assert!(cmds.iter().all(|c| c.op == 3), "SETVISIBILITY decoded");
    let vis = |h: usize, part: i32| sim.bgun.hands[h].gunmodel.as_ref().unwrap().part_visible(part);
    assert!(vis(0, MODELPART_REMOTEMINE_MINE) && !vis(0, MODELPART_REMOTEMINE_DETONATOR), "right: the mine");
    assert!(vis(1, MODELPART_REMOTEMINE_DETONATOR) && !vis(1, MODELPART_REMOTEMINE_MINE), "left: the detonator");
}


/// Switching away from the launcher frees its loaded rocket for good
/// (`bgun_free_weapon`, `bondgun.c:5262`), so it isn't drawn on the next gun.
#[test]
fn switching_off_the_launcher_leaves_no_rocket_on_the_next_gun() {
    use super::gset::*;
    use super::player::PdInput;
    let mut sim = settled_sim(WEAPON_ROCKETLAUNCHER);
    assert_eq!(sim.held_rockets().len(), 1);
    sim.frame(&PdInput { select: Some((WEAPON_GRENADE, false)), ..PdInput::default() }, 4);
    run(&mut sim, 200);
    assert_eq!(sim.bgun.ctrl.weaponnum, WEAPON_GRENADE);
    assert!(sim.held_rockets().is_empty() && sim.objs.is_empty() && sim.bgun.hands[0].rocket.is_none());
}

// ─── the N64 pad: PD's control style 1.1 ─────────────────────────────────────

fn pad(f: impl FnOnce(&mut super::player::PdInput)) -> super::player::PdInput {
    let mut i = super::player::PdInput { pad: true, ..Default::default() };
    f(&mut i);
    i
}

/// Stick up walks forward, stick right turns the way the mouse does, C-up/down
/// look up/down, C-left strafes the way A does (bondmove.c:1166).
#[test]
fn pad_stick_walks_and_turns_and_c_buttons_strafe_and_look() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let p0 = sim.player.pos;
    for _ in 0..60 {
        sim.frame(&pad(|i| i.look_y = 80), 4);
    }
    let d = sim.player.pos - p0;
    // (A few cm of sideways drift is the walk's head bob.)
    assert!(d.z > 50.0 && d.x.abs() < 10.0, "forward (+z at theta 0): {d:?}");

    let t0 = sim.player.theta;
    for _ in 0..20 {
        sim.frame(&pad(|i| i.look_x = 80), 4);
    }
    let mut keyboard = settled_sim(WEAPON_FALCON2);
    let kt0 = keyboard.player.theta;
    for _ in 0..20 {
        keyboard.frame(&super::player::PdInput { mouse_dx: 20.0, ..Default::default() }, 4);
    }
    let turned = |a: f32, b: f32| ((b - a + 540.0) % 360.0) - 180.0;
    assert!(turned(t0, sim.player.theta).abs() > 5.0, "the stick turns");
    assert_eq!(turned(t0, sim.player.theta).signum(), turned(kt0, keyboard.player.theta).signum(), "right is right");

    let v0 = sim.player.verta;
    for _ in 0..20 {
        sim.frame(&pad(|i| i.c_up = true), 4);
    }
    assert!(sim.player.verta > v0 + 5.0, "C-up looks up: {v0} → {}", sim.player.verta);
    for _ in 0..40 {
        sim.frame(&pad(|i| i.c_down = true), 4);
    }
    assert!(sim.player.verta < v0, "C-down looks down");

    let mut a = settled_sim(WEAPON_FALCON2);
    let mut b = settled_sim(WEAPON_FALCON2);
    let (pa, pb) = (a.player.pos, b.player.pos);
    for _ in 0..40 {
        a.frame(&pad(|i| i.c_left = true), 4);
        b.frame(&super::player::PdInput { walk_x: -127, ..Default::default() }, 4);
    }
    let (da, db) = (a.player.pos - pa, b.player.pos - pb);
    assert!(da.length() > 30.0 && da.normalize().dot(db.normalize()) > 0.9, "C-left strafes like A: {da:?} vs {db:?}");
}

/// Aimed (R): the stick moves the crosshair (up is up), C-down crouches, a
/// short R tap stands you back up (AIMCONTROL_HOLD), and on the sniper rifle
/// C-up zooms instead of crouching.
#[test]
fn pad_aiming_moves_the_crosshair_crouches_and_zooms() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let c = [sim.bgun.p.screen_width * 0.5, sim.bgun.p.screen_height * 0.5];
    for _ in 0..20 {
        sim.frame(
            &pad(|i| {
                i.aim = true;
                i.look_x = 50;
                i.look_y = 50;
            }),
            4,
        );
    }
    let cp = sim.bgun.p.crosspos;
    assert!(cp[0] > c[0] + 10.0 && cp[1] < c[1] - 5.0, "crosshair right and up: {cp:?} from {c:?}");

    let stand = sim.bgun.p.crouchpos;
    sim.frame(&pad(|i| i.aim = true), 4);
    sim.frame(
        &pad(|i| {
            i.aim = true;
            i.c_down = true;
        }),
        4,
    );
    for _ in 0..30 {
        sim.frame(&pad(|i| i.aim = true), 4);
    }
    assert_eq!(sim.bgun.p.crouchpos, stand - 1, "C-down while aiming crouches");
    for _ in 0..5 {
        sim.frame(&pad(|_| {}), 4);
    }
    for _ in 0..3 {
        sim.frame(&pad(|i| i.aim = true), 4);
    }
    sim.frame(&pad(|_| {}), 4);
    assert_eq!(sim.bgun.p.crouchpos, stand, "an R tap uncrouches");

    let mut sniper = settled_sim(WEAPON_SNIPERRIFLE);
    let f0 = sniper.bgun.p.gunzoomfovs[0];
    for _ in 0..30 {
        sniper.frame(
            &pad(|i| {
                i.aim = true;
                i.c_up = true;
            }),
            4,
        );
    }
    assert!(sniper.bgun.p.gunzoomfovs[0] < f0 * 0.8, "C-up zooms the sniper");
    assert_eq!(sniper.bgun.p.crouchpos, stand, "and doesn't crouch");
}

/// A tap = next gun, A + Z = previous gun (Z doesn't fire while A is held);
/// B tap = reload, B hold = the secondary function.
#[test]
fn pad_a_cycles_guns_and_b_taps_reload_or_holds_the_function() {
    use super::gset::*;
    let mut sim = settled_sim(WEAPON_FALCON2);
    let held = |sim: &super::sim::Sim| (sim.bgun.ctrl.weaponnum, sim.bgun.hands[1].inuse);
    let w0 = held(&sim);
    for _ in 0..3 {
        sim.frame(&pad(|i| i.a_held = true), 4);
    }
    run(&mut sim, 200);
    // The next inventory entry after one Falcon is two (bgun_cycle_forward).
    assert_eq!(held(&sim), (w0.0, true), "A tap: next entry, the dual Falcons");
    let shots = sim.shots_fired;
    for _ in 0..3 {
        sim.frame(&pad(|i| i.a_held = true), 4);
    }
    for _ in 0..3 {
        sim.frame(
            &pad(|i| {
                i.a_held = true;
                i.fire = true;
            }),
            4,
        );
    }
    sim.frame(&pad(|_| {}), 4);
    run(&mut sim, 200);
    assert_eq!(held(&sim), w0, "A + Z: back to one Falcon");
    assert_eq!(sim.shots_fired, shots, "Z with A held doesn't fire");

    for _ in 0..3 {
        sim.frame(&pad(|i| i.fire = true), 4);
        run(&mut sim, 10);
    }
    let clip = sim.bgun.hands[0].clipsizes[0];
    assert!(sim.bgun.hands[0].loadedammo[0] < clip);
    sim.frame(&pad(|i| i.use_held = true), 4);
    sim.frame(&pad(|_| {}), 4);
    run(&mut sim, 150);
    assert_eq!(sim.bgun.hands[0].loadedammo[0], clip, "B tap reloaded");

    let mut cmp = settled_sim(WEAPON_CMP150);
    for _ in 0..30 {
        cmp.frame(&pad(|i| i.use_held = true), 4);
    }
    run(&mut cmp, 60);
    assert_eq!(cmp.bgun.hands[0].weaponfunc, FUNC_SECONDARY, "B hold: secondary");
    assert_eq!(cmp.bgun.hands[0].loadedammo[0], cmp.bgun.hands[0].clipsizes[0], "a hold is not a reload tap");
}

