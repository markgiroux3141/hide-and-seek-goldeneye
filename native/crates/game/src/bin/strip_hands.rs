//! Reproducible tool: bake `gun_handless.glb` next to every weapon's `gun.glb`.
//!
//! The ripped GoldenEye weapon viewmodels bake in Bond's first-person hand, which
//! floats beside an enemy's own hand when the whole GLB is parented to a hunter's
//! hand bone. This tool reads each `native/assets/weapons/<w>/gun.glb`, removes
//! the hand sub-meshes (see [`game::combat::gun_strip`] for how the hand is
//! identified and stripped), and writes `gun_handless.glb` alongside it — but
//! ONLY for weapons that actually carry a hand (the pistols + detonator). Rifles/
//! shotguns/etc. have no hand mesh, so no variant is written and the enemy loader
//! falls back to `gun.glb`.
//!
//! The source `gun.glb` files are never modified. Re-run any time the assets
//! change:
//!
//! ```text
//! cargo run -p game --bin strip_hands
//! ```

use std::path::Path;

use game::combat::gun_strip;

fn main() {
    let weapons_dir = format!("{}/../../assets/weapons", env!("CARGO_MANIFEST_DIR"));
    let weapons_dir = Path::new(&weapons_dir);

    let mut entries: Vec<_> = std::fs::read_dir(weapons_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", weapons_dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();

    let mut written = 0usize;
    let mut skipped = 0usize;
    for dir in entries {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let gun = dir.join("gun.glb");
        if !gun.exists() {
            continue;
        }
        let bytes = match std::fs::read(&gun) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  {name}: read failed: {e}");
                continue;
            }
        };
        let res = match gun_strip::strip_hand(&bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  {name}: strip failed: {e}");
                continue;
            }
        };
        if res.removed == 0 {
            println!("  {name:16} no hand — skipped (enemy loader falls back to gun.glb)");
            skipped += 1;
            continue;
        }
        let out = dir.join("gun_handless.glb");
        if let Err(e) = std::fs::write(&out, &res.bytes) {
            eprintln!("  {name}: write failed: {e}");
            continue;
        }
        println!(
            "  {name:16} removed {} hand sub-mesh(es) → {}",
            res.removed,
            out.file_name().unwrap().to_string_lossy()
        );
        written += 1;
    }

    println!("\ndone: {written} handless variant(s) written, {skipped} weapon(s) had no hand");
}
