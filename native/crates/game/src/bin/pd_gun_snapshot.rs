//! Headless PD gun snapshots (offscreen GPU, no window) — see `game::pd_guns::snapshot`.
//! `cargo run --release --bin pd_gun_snapshot -- <outdir> [weapon short names...]`

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().cloned().unwrap_or_else(|| "pd_gun_snapshots".into());
    game::pd_guns::snapshot::run(std::path::Path::new(&out), &args[1.min(args.len())..]);
}
