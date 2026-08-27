//! Headless hunter navigation probe — `cargo run --release --bin probe_hunt -- <slot> [opts]`
//!
//! Answers the question a connectivity report cannot: **can a hunter actually walk from
//! A to B in this level, and if not, which gate refused the step it died on.**
//!
//! ```text
//!   probe_hunt 1                          # sweep every spawn pad against every other
//!   probe_hunt 1 --from 12,2,-4 --to -8,-9,-11
//!   probe_hunt 1 --secs 45 --log runs/probe.log
//! ```
//!
//! Every probe's telemetry goes to the log file; the console gets one verdict line each
//! plus a summary, so a sweep is readable at a glance and diagnosable in the file.
//!
//! The point of the sweep is that it finds failures you have *not* noticed. A single
//! `--from/--to` only ever confirms one you already saw.

use std::io::Write as _;

use game::world::World;
use glam::Vec3;

fn parse_point(s: &str) -> Option<Vec3> {
    let p: Vec<f32> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    (p.len() == 3).then(|| Vec3::new(p[0], p[1], p[2]))
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let slot: u8 = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let secs: f32 = flag("--secs").and_then(|s| s.parse().ok()).unwrap_or(30.0);
    let log_path = flag("--log").unwrap_or_else(|| "probe_hunt.log".into());
    let from = flag("--from").and_then(|s| parse_point(&s));
    let to = flag("--to").and_then(|s| parse_point(&s));
    // --ai runs the FULL AI (target selection, belief, combat) instead of commanding the
    // hunter to a point. It answers a different question: not "can it walk there" but
    // "will it come and find you" — which on a long route is not the same claim.
    let ai = args.iter().any(|a| a == "--ai");
    // A/B kill-switches, so a suspect subsystem can be ruled in or out in one run
    // instead of by argument.
    let no_nudge = args.iter().any(|a| a == "--no-wall-nudge");
    let no_avoid = args.iter().any(|a| a == "--no-avoidance");

    let mut world = World::new();
    match world.load_slot(slot) {
        Ok(m) => println!(
            "loaded slot{slot} — {} region mesh(es), {} spawn pad(s)",
            m.len(),
            world.spawn_pad_count()
        ),
        Err(e) => {
            eprintln!("could not load slot{slot}: {e}");
            std::process::exit(1);
        }
    }
    // The probe walks a level with nobody else in it: a wave would fight, crowd and
    // shove the instrument being measured.
    world.set_spawn_enemies(false);
    if no_nudge {
        world.set_wall_clearance(false);
        println!("(wall-clearance nudge OFF)");
    }
    if no_avoid {
        world.set_local_avoidance(false);
        println!("(local avoidance OFF)");
    }
    world.toggle_mode(); // BUILD → HUNT: bake nav, bring doors/props/pickups live
    println!("{}", world.nav_issue_report());

    if ai {
        let (Some(a), Some(b)) = (from, to) else {
            eprintln!("--ai needs --from and --to");
            std::process::exit(1);
        };
        // The full-AI chase needs a wave, so the suppression above is lifted for it.
        world.set_spawn_enemies(true);
        let r = world.probe_chase(a, b, secs);
        println!("{}", r.verdict());
        if let Ok(mut f) = std::fs::File::create(&log_path) {
            let _ = writeln!(f, "{}", r.report());
            println!("full telemetry: {log_path}");
        }
        std::process::exit(if r.arrived { 0 } else { 2 });
    }

    let results = match (from, to) {
        (Some(a), Some(b)) => vec![world.probe_walk(a, b, secs)],
        _ => {
            let pts = world.probe_points();
            if pts.len() < 2 {
                eprintln!(
                    "slot{slot} has {} spawn pad(s) — a sweep needs at least 2, or pass \
                     --from/--to",
                    pts.len()
                );
                std::process::exit(1);
            }
            println!(
                "sweeping {} pad(s) — {} probe(s), up to {secs:.0}s each\n",
                pts.len(),
                pts.len() * (pts.len() - 1)
            );
            world.probe_sweep(&pts, secs)
        }
    };

    let mut log = std::fs::File::create(&log_path).ok();
    let mut failed = 0usize;
    let mut timed_out = 0usize;
    for r in &results {
        let ok = r.arrived;
        // A run that was still closing when the clock ran out is a budget problem, not a
        // defect. Reporting it as a failure is how a sweep teaches you to ignore it.
        let defect = r.is_defect();
        if defect {
            failed += 1;
        } else if !ok {
            timed_out += 1;
        }
        let tag = if ok {
            "  ok  "
        } else if defect {
            "FAILED"
        } else {
            " slow "
        };
        println!("{tag} {}", r.verdict());
        if let Some(f) = log.as_mut() {
            // Only failures get their full telemetry: an arrival is not a mystery, and a
            // log padded with successful walks is a log nobody reads.
            let _ = if ok {
                writeln!(f, "{}\n", r.verdict())
            } else {
                writeln!(f, "{}\n", r.report())
            };
        }
    }

    println!(
        "\n{} probe(s): {} arrived, {timed_out} ran out of time still walking, {failed} FAILED",
        results.len(),
        results.len() - failed - timed_out,
    );
    if failed > 0 {
        println!("full telemetry for the failures: {log_path}");
        // Non-zero exit so a sweep can gate a commit.
        std::process::exit(2);
    }
}
