//! Headless step profiler — `cargo run --release --bin profile_hunt -- <slot> [seconds]`.
//!
//! Loads a real authored level from its quick-slot, enters HUNT, and times the fixed
//! step. Exists because "the frame rate is terrible after G" is a claim about a per-step
//! cost, and the only honest way to find one is to measure the step rather than reason
//! about it.
//!
//! The nav counters it prints are the ones that matter for a big level: a *failing* A\*
//! (no path from here to there) explores the whole reachable grid before giving up, so a
//! handful of hunters walking toward somewhere unreachable can cost more than everything
//! else in the frame put together.

use std::time::Instant;

use game::world::World;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args = std::env::args().skip(1);
    let slot: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let secs: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5.0);

    let mut world = World::new();
    let t0 = Instant::now();
    match world.load_slot(slot) {
        Ok(meshes) => println!(
            "loaded slot{slot} in {:.0} ms — {} region mesh(es), {} spawn pad(s)",
            t0.elapsed().as_secs_f32() * 1000.0,
            meshes.len(),
            world.spawn_pad_count(),
        ),
        Err(e) => {
            eprintln!("could not load slot{slot}: {e}");
            std::process::exit(1);
        }
    }
    world.set_wave_size(6);

    engine::sim::nav::reset_path_stats();
    let t = Instant::now();
    world.toggle_mode(); // BUILD → HUNT: bake nav + physics, spawn the wave
    println!(
        "G (bake + spawn) took {:.0} ms",
        t.elapsed().as_secs_f32() * 1000.0
    );
    println!("nav: {}", engine::sim::nav::path_stats());

    let dt = 1.0 / 60.0;
    let input = Default::default();
    let steps = (secs / dt) as usize;
    engine::sim::nav::reset_path_stats();
    let mut total = 0.0f64;
    let mut worst = (0.0f32, 0usize);
    let mut hist = [0usize; 6]; // <2, <5, <10, <20, <50, >=50 ms
    // Per-phase totals (ms), so a hot frame can be attributed rather than guessed at.
    let (mut t_step, mut t_anim, mut t_inst, mut t_draws) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..steps {
        let t = Instant::now();
        world.fixed_step(dt, &input);
        world.enemy_combat_step(dt);
        t_step += t.elapsed().as_secs_f64() * 1000.0;
        // Everything else the real frame does on the CPU before it draws anything.
        let t2 = Instant::now();
        world.advance_animation(dt);
        t_anim += t2.elapsed().as_secs_f64() * 1000.0;
        let t3 = Instant::now();
        let n = world.character_instances().len();
        std::hint::black_box(n);
        t_inst += t3.elapsed().as_secs_f64() * 1000.0;
        let t4 = Instant::now();
        std::hint::black_box(world.prop_draws(1.6).len());
        std::hint::black_box(world.enemy_weapon_draws(1.6).len());
        std::hint::black_box(world.spawn_marker_mesh().is_some());
        std::hint::black_box(world.hud_mesh(1.6).is_some());
        t_draws += t4.elapsed().as_secs_f64() * 1000.0;
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        total += ms as f64;
        if ms > worst.0 {
            worst = (ms, i);
        }
        hist[match ms {
            m if m < 2.0 => 0,
            m if m < 5.0 => 1,
            m if m < 10.0 => 2,
            m if m < 20.0 => 3,
            m if m < 50.0 => 4,
            _ => 5,
        }] += 1;
    }
    let mean = total / steps as f64;
    println!(
        "\n{steps} steps ({secs} s of sim): mean {mean:.2} ms/step, worst {:.1} ms (step {})",
        worst.0, worst.1
    );
    println!(
        "  <2ms {} | <5ms {} | <10ms {} | <20ms {} | <50ms {} | 50ms+ {}",
        hist[0], hist[1], hist[2], hist[3], hist[4], hist[5]
    );
    // A 60 Hz fixed step has 16.7 ms; anything near that is the whole frame budget gone
    // before a single triangle is drawn.
    println!(
        "  → the sim alone caps the frame rate at {:.0} fps (budget at 60 Hz is 16.7 ms)",
        1000.0 / mean.max(0.001)
    );
    let per = |v: f64| v / steps as f64;
    println!(
        "  phases: fixed_step {:.2} | advance_animation {:.2} | character_instances {:.2} | \
         draw lists {:.2} ms",
        per(t_step),
        per(t_anim),
        per(t_inst),
        per(t_draws)
    );
    println!("nav: {}", engine::sim::nav::path_stats());
    println!();
    println!("{}", world.hunter_report());
    println!("{}", world.spawn_reachability_report());
    println!("{}", world.nav_component_report());
    // The full validation pass — the same findings the BUILD NAV tab shows, which is
    // where the *diagnosis* lives: component sizes say a level is broken, the nearest-gap
    // and orphan lines say where and why.
    println!("── nav validation ──");
    println!("{}", world.nav_issue_report());
}
