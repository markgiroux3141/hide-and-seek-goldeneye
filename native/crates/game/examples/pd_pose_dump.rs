//! Dump what the **engine** computes for a Perfect Dark character, so it can be
//! rendered and looked at outside the game.
//!
//! `tools/pd-assets/pd_preview.py` verifies the exported GLBs by re-implementing
//! the glTF skinning math in Python. That catches an exporter bug, but it cannot
//! catch a disagreement between the exporter and *this* engine — both could be
//! self-consistently wrong about the same asset. This closes that gap: it loads a
//! PD body and clip through the real `gltf_skin::load` / `clip::load` /
//! `Skeleton::skinning_matrices` path, skins the vertices on the CPU exactly as
//! the vertex shader does, and writes the resulting positions out. Feeding those
//! back to `pd_preview.py --positions` draws engine-computed geometry with the
//! Python renderer's camera — so a mismatch shows up as a visibly broken figure
//! rather than as a number nobody reads.
//!
//! ```sh
//! cargo run --release --example pd_pose_dump -- pd_a51guard pd-running 6 out/
//! python tools/pd-assets/pd_preview.py \
//!     native/assets/enemies/pd/characters/pd_a51guard.glb engine.png \
//!     --positions out/pd_a51guard_pd-running.f32 --frames 6
//! ```
//!
//! Output is a flat little-endian `f32` file: `frames x vertices x [x, y, z]`, in
//! the GLB's own units (the caller applies `CHAR_SCALE` if it wants metres).

use std::io::Write;

use engine::skeletal::{clip, gltf_skin};
use glam::Vec3;

fn main() {
    let mut args = std::env::args().skip(1);
    let body = args.next().unwrap_or_else(|| "pd_a51guard".into());
    let clip_name = args.next().unwrap_or_else(|| "pd-idle".into());
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);
    let outdir = args.next().unwrap_or_else(|| ".".into());

    let root = env!("CARGO_MANIFEST_DIR");
    let body_path = format!("{root}/../../assets/enemies/pd/characters/{body}.glb");
    let clip_path = format!("{root}/../../assets/enemies/pd/animations/{clip_name}.glb");

    let model = gltf_skin::load(&body_path).expect("load PD body");
    let anim = clip::load(&clip_path, &model.skeleton).expect("load PD clip");
    println!(
        "{body}: {} verts, {} joints ({}) | {clip_name}: {:.2}s, {} channels",
        model.vertices.len(),
        model.skeleton.joint_count(),
        model.skeleton.names.join(","),
        anim.duration,
        anim.bound_channels(),
    );

    std::fs::create_dir_all(&outdir).expect("create outdir");
    let out = format!("{outdir}/{body}_{clip_name}.f32");
    let mut fh = std::io::BufWriter::new(std::fs::File::create(&out).expect("create dump"));

    for f in 0..frames {
        let t = anim.duration * f as f32 / frames as f32;
        let joints = anim.skinning_matrices(t, &model.skeleton);
        // Linear blend skinning — the CPU mirror of the character shader.
        for v in &model.vertices {
            let src = Vec3::from(v.pos);
            let mut p = Vec3::ZERO;
            for k in 0..4 {
                let w = v.weights[k];
                if w != 0.0 {
                    p += w * joints[v.joints[k] as usize].transform_point3(src);
                }
            }
            for c in [p.x, p.y, p.z] {
                fh.write_all(&c.to_le_bytes()).expect("write");
            }
        }
    }
    println!("wrote {frames} frame(s) -> {out}");
}
