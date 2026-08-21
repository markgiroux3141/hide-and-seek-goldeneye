//! Dump the turret **as the engine poses it**, so the rig can be looked at and
//! measured without launching the game.
//!
//! `tools/sentry/sentry_preview.py` renders a rig written in Python; `crate::turret`
//! is a second, independent transcription of the same numbers into glam, and the two
//! could disagree in ways every unit test still passes — a transposed multiply or a
//! flipped rotation sign reads as "the barrel points backwards", not as an assertion
//! failure. This writes what the *engine* computed, in two forms:
//!
//! * a posed Wavefront OBJ, referencing the original MTL so it renders textured
//!   through the same previewer as the Python rig (the silhouettes must match);
//! * a per-part AABB + centroid table on stdout, which `sentry_preview.py --verify`
//!   diffs against its own numbers so the agreement is measured, not eyeballed.
//!
//! Usage:
//!     cargo run --release -p game --example turret_pose_dump -- <out.obj> [yaw pitch spin]
//!
//! Angles in degrees. The output path should sit **beside the source asset** so the
//! `mtllib`/texture references resolve.

use game::turret;
use glam::Vec3;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args
        .first()
        .cloned()
        .unwrap_or_else(|| "turret_posed.obj".to_string());
    let deg = |i: usize| -> f32 {
        args.get(i)
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0)
            .to_radians()
    };
    let (yaw, pitch, spin) = (deg(1), deg(2), deg(3));

    let src = format!(
        "{}/../../assets/props/sentry_gun/sentry_gun.obj",
        env!("CARGO_MANIFEST_DIR")
    );
    let parts = engine::assets::obj_model::load_obj_components(&src).expect("sentry gun loads");
    assert_eq!(parts.len(), turret::PARTS.len(), "rig/asset piece-count mismatch");

    // The loader normalises GoldenEye units to metres; write the OBJ back out in the
    // source's own units so the posed file is directly comparable with the original.
    const TO_GE: f32 = 1000.0;

    let mut obj = String::from("# posed by crates/game/examples/turret_pose_dump.rs\n");
    obj.push_str("mtllib sentry_gun.mtl\n");
    let mut vbase = 1usize;
    let mut vtbase = 1usize;
    let mut body = String::new();

    println!(
        "# turret pose  yaw={:.1}  pitch={:.1}  spin={:.1}  (degrees)",
        yaw.to_degrees(),
        pitch.to_degrees(),
        spin.to_degrees()
    );
    println!("# part            min(x,y,z)                 max(x,y,z)                 centroid");

    for (i, (part, model)) in turret::PARTS.iter().zip(&parts).enumerate() {
        let m = turret::part_matrix(part, yaw, pitch, spin);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut sum = Vec3::ZERO;
        for v in &model.vertices {
            let p = m.transform_point3(Vec3::from_array(v.pos)) * TO_GE;
            min = min.min(p);
            max = max.max(p);
            sum += p;
            body.push_str(&format!("v {:.4} {:.4} {:.4}\n", p.x, p.y, p.z));
            body.push_str(&format!("vt {:.6} {:.6} 0.0\n", v.uv[0], 1.0 - v.uv[1]));
        }
        let c = sum / model.vertices.len().max(1) as f32;
        println!(
            "{i} {:<16} {:9.3} {:9.3} {:9.3}   {:9.3} {:9.3} {:9.3}   {:9.3} {:9.3} {:9.3}",
            part.key, min.x, min.y, min.z, max.x, max.y, max.z, c.x, c.y, c.z
        );

        // Faces, per primitive so each keeps its material. Primitive order matches the
        // loader's material buckets; names are recovered by index into the MTL, which
        // the previewer only needs for the texture, so `m<N>` naming is enough.
        for (pi, prim) in model.primitives.iter().enumerate() {
            body.push_str(&format!("usemtl {}\n", material_name(i, pi)));
            let idx = &model.indices
                [prim.index_start as usize..(prim.index_start + prim.index_count) as usize];
            for tri in idx.chunks_exact(3) {
                body.push_str(&format!(
                    "f {a}/{a} {b}/{b} {c}/{c}\n",
                    a = vbase + tri[0] as usize,
                    b = vbase + tri[1] as usize,
                    c = vbase + tri[2] as usize,
                ));
            }
        }
        vbase += model.vertices.len();
        vtbase += model.vertices.len();
    }
    let _ = vtbase;
    obj.push_str(&body);
    std::fs::write(&out, obj).expect("write posed obj");
    eprintln!("wrote {out}");
}

/// The MTL material a piece's primitive draws with.
///
/// The split loader keeps material *order* per piece but not the source names, and the
/// previewer only reads the material to find its texture — so this reconstructs the
/// mapping from the known layout of `sentry_gun.mtl` rather than plumbing names
/// through the loader for a debug dump's benefit.
fn material_name(part: usize, prim: usize) -> String {
    // (piece → the MTL materials its faces use, in first-seen order)
    const BY_PART: [&[&str]; 6] = [
        &["m16", "m17", "m18"],
        &["m5", "m6ClampSClampT", "m7ClampSClampT", "m8", "m9", "m10"],
        &[
            "m12ClampT",
            "m13CullBoth",
            "m14CullBothClampT",
            "m15CullBoth",
            "m19CullBothEnvMappingTexScaleS0.062501TexScaleT0.031250",
        ],
        &["m0", "m1", "m2"],
        &["m3", "m4"],
        &["m11"],
    ];
    BY_PART
        .get(part)
        .and_then(|ms| ms.get(prim))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "m0".to_string())
}
