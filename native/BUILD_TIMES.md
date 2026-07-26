# Speeding up native build times

Long builds are normal for a `wgpu` + `rapier3d` game, but our setup was making
them worse than necessary. Levers below are ordered roughly by impact. None of
this changes game code.

> **Status (2026-07-26): items 1 and 2 implemented.** See "What was done" at the
> bottom. Items 3–4 remain optional follow-ups.

## 1. Stop iterating on the fat-LTO release build (biggest lever)

`Cargo.toml` currently has:

```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
```

These are *runtime-speed* settings that trade away compile time hard:

- `codegen-units = 1` forces the whole crate through the optimizer as one unit
  — no codegen parallelism.
- `lto = true` (fat LTO) re-optimizes across the **entire** dependency graph at
  link time. With wgpu + rapier + gltf in the tree, the link step alone can be
  tens of seconds to minutes.

We currently hand off / test via the **release** binary (to avoid stale
debug-exe confusion), so we pay this cost every iteration.

**Fix — introduce a middle profile** optimized enough to *play* but cheap to
rebuild, and keep full `lto`/`codegen-units = 1` only for real ship builds.
Options:

- Bump `profile.dev` to `opt-level = 1` (or `2`) — playable debug builds.
- Optimize **dependencies** heavily (they compile once and cache) but leave
  **our own crates** cheap, so incremental edits stay fast:

  ```toml
  # Optimize deps hard, our crates stay cheap → fast incremental edits.
  [profile.dev.package."*"]
  opt-level = 3
  ```

- Or a dedicated `[profile.release-fast]` for day-to-day testing.

This dep-heavy / own-crate-cheap pattern is the gamedev sweet spot.

## 2. Faster linker (Windows-specific, easy win)

Default MSVC linker is slow and we're link-heavy (big dep graph; LTO worsens
it). Switch to `lld` (ships as `rust-lld` with the toolchain) or newer `wild`.
It's a `.cargo/config.toml` one-liner, no code changes, and stacks with
everything else. Typically cuts link time substantially.

## 3. Cache the cold-build cost

Heavy crates (wgpu, rapier3d, gltf, image, kira, winit) make a *clean* build
brutal, but they only recompile on `cargo clean`, a version bump, or a feature
change.

- **`sccache`** — caches compiled artifacts across cleans/branch switches.
  Helps most since we switch branches (e.g. `spike-procedural-anim`).
- **Avoid `cargo clean`** — let incremental compilation do its job. Day-to-day
  pain should be incremental rebuilds of `game`/`engine`, not cold builds.

## 4. Smaller knobs

- **Feature audit**: `image` is already trimmed (`default-features = false`,
  `bmp/png/jpeg`) — good. Re-check whether we need all of gltf's
  `import + utils` and all of kira's default features.
- **Crate split**: the `engine`/`game` split already helps incremental builds
  (editing `game` doesn't rebuild `engine`). Keep frequently-churned code in the
  leaf `game` crate to maximize this.
- **Toolchain**: parallel front-end and the Cranelift codegen backend (fast
  debug codegen) are both real options for dev builds now.

## Recommended first move

If we touch only one thing: **set up a fast dev/test profile (optimized deps,
cheap own-crate builds) + switch to `lld`.** Those two together are likely the
difference between "go make coffee" and "a few seconds," without touching game
code or the ship build.

## Before committing to changes

Run `cargo build --timings` to see where time actually goes, so we tune based on
data rather than guesses.

---

## What was done (2026-07-26)

### Item 1 — profiles (`Cargo.toml`)

Redefined `release` as the fast day-to-day test/handoff build and added a
separate max-perf ship profile, plus optimized deps in debug:

- `[profile.release]` — `lto = false`, `codegen-units = 16`, `incremental = true`
  (kept `opt-level = 3`). This is what `cargo build --release` now produces, so
  the existing handoff/level-ship workflow gets faster with no command change.
- `[profile.release-dist]` — `inherits = "release"` but `lto = true`,
  `codegen-units = 1`. Use `--profile release-dist` only for a real shippable
  artifact.
- `[profile.dev.package."*"]` — `opt-level = 3` so dependencies are optimized
  once and cached; our own crates stay at opt-level 0 for cheap incremental
  rebuilds.

### Item 2 — faster linker (`native/.cargo/config.toml`, new file)

Switched Windows MSVC linking to the bundled `rust-lld`:

```toml
[target.x86_64-pc-windows-msvc]
linker = "rust-lld"
rustflags = ["-Clinker-flavor=lld-link"]
```

No install needed — `rust-lld` resolves from the toolchain sysroot on stable
1.92. (The unstable `-Clinker-features=+lld` flag is *not* usable on stable, so
we point `linker` at `rust-lld` directly with the `lld-link` flavor.) To revert,
comment out that block.

### One-time cost

The first build after these changes recompiles everything (new linker flags +
new dep opt-levels invalidate the cache). Incremental builds after that are the
ones that benefit.

### Not yet done

Items 3 (`sccache`) and 4 (feature audit, Cranelift/parallel front-end) remain
optional follow-ups if builds are still slower than desired.
