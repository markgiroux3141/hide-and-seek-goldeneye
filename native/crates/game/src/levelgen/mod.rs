//! Headless procedural level generation + analysis harness.
//!
//! Runs entirely without a window: an author (the LLM) writes a level with the
//! [`builder`] intent API, and this module puts it through **the game's own
//! pipeline** — load into a real [`World`], save with the real level writer, load the
//! file back, bake nav with the same function `G` and the NAV tab use — then prints an
//! LLM-friendly text [`analyze`]sis plus the NAV tab's own report. The loop is: build →
//! report → fix → repeat, then open the level in-game to confirm.
//!
//! **Why it goes through `World` rather than around it.** Until 2026-09 this module
//! hand-mirrored the nav bake and the file format, and both had drifted — the bake had
//! no props or ramp planes, the file was written as v2 while the game was on v4 — so
//! the report described a level slightly different from the one the author then opened.
//! Owning no copy of either is what keeps it honest.
//!
//! Entry: set `LEVELGEN=1` (optionally `LEVELGEN_DESIGN=name`, `LEVELGEN_SLOT=N`) and
//! launch the binary; `main` calls [`run`] instead of opening the window.

pub mod analyze;
pub mod builder;
pub mod designs;

#[cfg(test)]
mod tests;

use std::path::Path;

use builder::BuiltLevel;

use crate::world::{persist, World};

/// Every registered design, by the name `LEVELGEN_DESIGN` takes. The golden tests walk
/// this same table, so a design cannot be runnable without also being tested.
pub const DESIGNS: &[(&str, fn() -> BuiltLevel)] = &[
    ("smoke", designs::smoke),
    ("arena", designs::arena),
    ("varied", designs::varied),
    ("sprawl", designs::sprawl),
    ("facility", designs::facility),
    ("linear", designs::linear),
    ("showcase", designs::showcase),
    ("grand", designs::grand),
    ("compound", designs::compound),
    ("pd_lab", designs::pd_lab),
];

/// Look a design up by name.
pub fn design(name: &str) -> Option<BuiltLevel> {
    DESIGNS.iter().find(|(n, _)| *n == name).map(|(_, f)| f())
}

/// The display name a generated level is saved under — also how the harness recognises
/// a file it wrote itself (and so may overwrite) from one an author made.
pub fn generated_name(design: &str) -> String {
    format!("levelgen {design}")
}

/// Headless entry point. Reads `LEVELGEN_DESIGN` (default `grand`) and, optionally,
/// `LEVELGEN_SLOT`. Exits non-zero on any failure so a scripted caller notices.
pub fn run() {
    let name = std::env::var("LEVELGEN_DESIGN").unwrap_or_else(|_| "grand".to_string());
    let Some(built) = design(&name) else {
        let names: Vec<&str> = DESIGNS.iter().map(|(n, _)| *n).collect();
        eprintln!("unknown LEVELGEN_DESIGN='{name}' — known designs: {}", names.join(", "));
        std::process::exit(2);
    };
    // Where it goes: a named level in the LEVELS tab by default, or a numbered quick
    // slot (F-key / `LOAD_SLOT`) when asked for one explicitly.
    let slot: Option<u8> = std::env::var("LEVELGEN_SLOT").ok().and_then(|s| s.trim().parse().ok());
    let path = match slot {
        Some(n) => persist::slot_path(n),
        None => persist::path_for_name(&generated_name(&name)).expect("design names slug"),
    };

    // `LEVELGEN_REPORT=json` prints the report as JSON (one document on stdout) for a
    // scripted caller to assert on; the default is the text report.
    let json = std::env::var("LEVELGEN_REPORT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    if !json {
        println!("=== levelgen: design='{name}' -> {} ===
", path.display());
    }
    match generate(&name, &built, &path) {
        Ok(out) if json => println!("{}", serde_json::to_string_pretty(&out.json).unwrap_or_default()),
        Ok(out) => println!("{}", out.text),
        Err(e) => {
            eprintln!("[!] levelgen failed: {e}");
            std::process::exit(1);
        }
    }
}

/// A finished run: the text report and the same content as JSON.
pub struct Generated {
    pub text: String,
    pub json: serde_json::Value,
}

/// Build → save → reload → analyze. The level analyzed is the one **read back from
/// disk**, i.e. exactly what an author opens.
pub fn generate(name: &str, built: &BuiltLevel, path: &Path) -> Result<Generated, String> {
    refuse_foreign(path, name)?;

    let mut world = headless_world();
    world.load_built_level(built).map_err(|e| format!("load into World: {e}"))?;
    world.set_level_name(&generated_name(name));
    world.save_level(path).map_err(|e| format!("save {}: {e}", path.display()))?;

    let mut loaded = headless_world();
    loaded
        .load_level(path)
        .map_err(|e| format!("reload {}: {e}", path.display()))?;
    let roundtrip = roundtrip_check(built, &loaded);
    let roundtrip_ok = roundtrip.starts_with("round trip: OK");

    let nav = loaded
        .bake_level_nav()
        .ok_or("nav bake produced nothing — the level has no walkable volume")?;
    loaded.calculate_nav_issues();
    let issues = loaded.nav_issues().ok_or("the NAV pass produced no findings")?;
    let analysis = analyze::Analysis::new(name, &nav, &loaded, built, issues);

    let mut text = analysis.report();
    text.push_str(&format!("
{roundtrip}
wrote playable level to {}
", path.display()));

    let mut json = serde_json::to_value(analysis.data()).map_err(|e| e.to_string())?;
    if let Some(obj) = json.as_object_mut() {
        obj.insert("roundtrip_ok".into(), roundtrip_ok.into());
        obj.insert("roundtrip".into(), roundtrip.into());
        obj.insert("file".into(), path.display().to_string().into());
    }
    Ok(Generated { text, json })
}

/// A `World` for headless use, with prop bounds registered the way the app registers
/// them at startup — without which placed props would silently vanish from the bake.
pub fn headless_world() -> World {
    let mut w = World::new();
    w.register_catalog_prop_bounds();
    w
}

/// Refuse to overwrite a level the harness did not write. Quick slots are exempt: the
/// caller named that slot explicitly, and overwriting it is what the flag has always
/// meant.
fn refuse_foreign(path: &Path, design: &str) -> Result<(), String> {
    if !path.exists() || is_slot(path) {
        return Ok(());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let existing = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .unwrap_or_default();
    if existing == generated_name(design) {
        Ok(())
    } else {
        Err(format!(
            "{} already exists and was not written by the harness (its name is {existing:?}) \
             — refusing to overwrite it",
            path.display()
        ))
    }
}

fn is_slot(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("slot"))
        .is_some_and(|n| n.parse::<u8>().is_ok())
}

/// Did the level survive save → load intact? Compared as JSON so the check needs no
/// `PartialEq` on the engine types, and brushes in id order because a load re-partitions
/// them into regions (which is also where the fold order comes back from).
fn roundtrip_check(built: &BuiltLevel, loaded: &World) -> String {
    fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
    }
    let mut brushes: Vec<_> = loaded.regions().iter().flat_map(|r| r.brushes.iter().copied()).collect();
    brushes.sort_by_key(|b| b.id);
    let mut stairs: Vec<_> = loaded.regions().iter().flat_map(|r| r.stairs.iter().copied()).collect();
    stairs.sort_by_key(|s| s.void_ids[0]);
    let mut want_stairs = built.stairs.clone();
    want_stairs.sort_by_key(|s| s.void_ids[0]);

    let mut bad = Vec::new();
    if json(&brushes) != json(&built.brushes) {
        bad.push("brushes");
    }
    if json(&stairs) != json(&want_stairs) {
        bad.push("CSG stairs");
    }
    if json(&loaded.platforms()) != json(&built.platforms) {
        bad.push("platforms");
    }
    if json(&loaded.stair_runs()) != json(&built.stair_runs) {
        bad.push("stair-runs");
    }
    if loaded.spawn_marker().distance(built.spawn) > 1e-4 {
        bad.push("spawn");
    }
    if bad.is_empty() {
        format!(
            "round trip: OK — saved and reloaded through the game's own level format \
             ({} brushes, {} stairs, {} platforms, {} stair-runs, {} regions after load)",
            brushes.len(),
            stairs.len(),
            built.platforms.len(),
            built.stair_runs.len(),
            loaded.regions().len()
        )
    } else {
        format!("[!] round trip: MISMATCH in {} after save → load", bad.join(", "))
    }
}

