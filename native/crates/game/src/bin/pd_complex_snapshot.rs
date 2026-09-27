//! Headless snapshots of the Complex match (offscreen GPU, no window) — see `game::pd_complex::snapshot`.
//! `cargo run --release --bin pd_complex_snapshot -- <outdir>`

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "pd_complex_snapshots".into());
    game::pd_complex::snapshot::run(std::path::Path::new(&out));
}
