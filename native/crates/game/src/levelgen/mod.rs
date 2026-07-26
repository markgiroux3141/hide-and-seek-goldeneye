//! Headless procedural level generation + analysis harness.
//!
//! Runs entirely without a window: an author (the LLM) writes a level with the
//! [`builder`] intent API, this module bakes the nav grid the same way the hunt
//! does, prints an LLM-friendly text [`analyze`]sis, and writes a **playable**
//! `levels/slotN.json` the real game can load. The loop is: build → bake →
//! read the report → fix → repeat, then open the slot in-game to confirm.
//!
//! Entry: set `LEVELGEN=1` (optionally `LEVELGEN_SLOT=N`, `LEVELGEN_DESIGN=name`)
//! and launch the binary; `main` calls [`run`] instead of opening the window.

pub mod analyze;
pub mod builder;
mod designs;
mod serialize;

use engine::geometry::csg_runtime::Region;
use engine::geometry::structures;
use engine::sim::nav;

use builder::BuiltLevel;

/// Headless entry point. Reads `LEVELGEN_DESIGN` (default `arena`) and
/// `LEVELGEN_SLOT` (default `9`), builds + analyzes + writes the slot.
pub fn run() {
    let design = std::env::var("LEVELGEN_DESIGN").unwrap_or_else(|_| "grand".to_string());
    // Default to slot 7 so it's loadable in-game with F7 (F-keys map to 1–8).
    let slot: u8 = std::env::var("LEVELGEN_SLOT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);

    let built = match design.as_str() {
        "smoke" => designs::smoke(),
        "arena" => designs::arena(),
        "varied" => designs::varied(),
        "sprawl" => designs::sprawl(),
        "facility" => designs::facility(),
        "linear" => designs::linear(),
        "showcase" => designs::showcase(),
        "grand" => designs::grand(),
        other => {
            eprintln!("unknown LEVELGEN_DESIGN='{other}', using 'facility'");
            designs::facility()
        }
    };

    println!("=== levelgen: design='{design}' -> slot {slot} ===\n");
    analyze_and_print(&built);

    match serialize::write_slot(&built, slot) {
        Ok(path) => {
            println!("\nwrote playable level to {}", path.display());
            verify_loads(slot);
        }
        Err(e) => eprintln!("\nfailed to write slot {slot}: {e}"),
    }
}

/// Load the just-written slot back through the **real** game persist path
/// (`World::load_slot`) headlessly, proving the file is actually playable and
/// re-bakes cleanly — not just that our serializer emitted plausible JSON.
fn verify_loads(slot: u8) {
    let mut world = crate::world::World::new();
    match world.load_slot(slot) {
        Ok(meshes) => println!(
            "verify: slot {slot} loads in-engine OK ({} region mesh/clear ops rebuilt)",
            meshes.len()
        ),
        Err(e) => eprintln!("verify: slot {slot} FAILED to load in-engine: {e}"),
    }
}

/// Bake the nav grid from a built level and print the full text report.
/// Bypasses `World` entirely — constructs the region + structure solids from the
/// engine's public API, exactly mirroring `World::structure_solid_boxes` +
/// `nav::bake`.
pub fn analyze_and_print(built: &BuiltLevel) {
    // One region holding every carve/add brush (the builder is single-region).
    let mut region = Region::new(0);
    region.brushes = built.brushes.clone();
    region.stairs = built.stairs.clone(); // CSG stair treads → nav solids
    region.refresh_shell();
    let brushes = region.brushes.clone();
    let mut regions = vec![region];

    // Structure solids: platform slabs + stair-run step blocks (the nav extras).
    let mut solids: Vec<[f32; 6]> = Vec::new();
    for p in &built.platforms {
        if let Some(b) = p.solid_box(&brushes) {
            solids.push(b);
        }
    }
    for r in &built.stair_runs {
        let fp = r
            .from_platform
            .and_then(|id| built.platforms.iter().find(|p| p.id == id));
        let tp = r
            .to_platform
            .and_then(|id| built.platforms.iter().find(|p| p.id == id));
        solids.extend(structures::stair_run_boxes(r, fp, tp, &brushes));
    }

    match nav::bake(&mut regions, &solids) {
        Some(navw) => {
            let a = analyze::Analysis::new(&navw, &regions, built);
            print!("{}", a.report());
        }
        None => println!("[!] nav bake produced nothing — the level has no walkable volume."),
    }
}
