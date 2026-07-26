# How the native build works

Authoritative reference for building the native Rust game. If build behavior
surprises you, read this first. Rationale + the optimization history live in
[BUILD_TIMES.md](BUILD_TIMES.md); this doc is just "how it works now."

## TL;DR

- `cargo build --release -p game` — the day-to-day **test / handoff** build.
  Optimized (`opt-level = 3`) but **fast** (no fat LTO). This is the exe the user
  plays and the one the level-ship step regenerates slots with.
- `cargo run -p game` — debug build for the **levelgen harness** and quick iteration.
- `cargo build --profile release-dist -p game` — **only** for a true shippable
  artifact (fat LTO, maximum runtime perf, slow to compile). You almost never need this.

## Profiles (`Cargo.toml`)

| Profile | `opt-level` | `lto` | `codegen-units` | `incremental` | Use |
|---|---|---|---|---|---|
| `dev` (default) | 0 (our crates) | off | many | yes | levelgen harness, quick iteration |
| `dev.package."*"` | **3** | — | — | — | deps optimized once + cached, so debug runs smoothly |
| `release` | 3 | **off** | 16 | **yes** | **test / handoff / level-ship** |
| `release-dist` | 3 | **on (fat)** | 1 | off | true distributable only |

Key point: **`release` deliberately does NOT use fat LTO.** LTO was the main
build-time killer — it forced a full relink of the whole binary on every edit.
Dropping it makes incremental release rebuilds ~1.4s instead of a full relink,
at the cost of a few percent runtime perf that doesn't matter for iterating.
Fat LTO now lives only in `release-dist`.

## Linker (`native/.cargo/config.toml`)

Windows MSVC builds link with the LLVM linker (`rust-lld`) bundled in the Rust
toolchain, not the default `link.exe`:

```toml
[target.x86_64-pc-windows-msvc]
linker = "rust-lld"
rustflags = ["-Clinker-flavor=lld-link"]
```

- **No install needed** — `rust-lld` resolves from the toolchain sysroot on
  stable Rust 1.92.
- We can't use the cleaner `-Clinker-features=+lld` — it's unstable and rejected
  on stable, so we point `linker` at `rust-lld` with the `lld-link` flavor.
- To fall back to the default MSVC linker, comment out that block.

## Measured build times (2026-07-26, stable 1.92)

| Build | Cold (one-time) | Incremental edit |
|---|---|---|
| `--release` | ~90s | **~1.4s** |
| debug | ~137s | **~2.3s** |

## Expect a one-time slow build

The **first** build on a machine, or the first after `cargo clean` / a
toolchain or dependency version bump, recompiles everything (~90–137s) because
the profile + linker settings and the `opt-level = 3` dependency builds must be
produced once. This is normal — do **not** "fix" it by reverting the config.
Every incremental build after that is fast.

## If a release build finishes in <1s or says "Access is denied"

The running game window locks `target/release/build-and-hide.exe`. Ask the user
to close the game, then rebuild. (Already noted in the build-level skill.)
