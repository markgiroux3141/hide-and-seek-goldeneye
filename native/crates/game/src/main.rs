//! BUILD & HIDE launcher — a thin binary over the `game` library crate.

fn main() {
    // Headless level-generation harness: build + analyze + write a slot, no
    // window. Opt in with LEVELGEN=1 (see `game::levelgen`).
    if std::env::var("LEVELGEN").is_ok() {
        game::levelgen::run();
        return;
    }
    game::run();
}
