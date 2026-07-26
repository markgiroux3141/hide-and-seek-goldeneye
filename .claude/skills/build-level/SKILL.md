---
name: build-level
description: Author a playable level for the native BUILD & HIDE (GoldenEye-style hide-and-seek) game using the headless levelgen harness. Use when the user asks to build, design, generate, extend, or iterate on a level/map for the Rust game in native/, or mentions rooms/halls/stairs/pits/the level generator. Drives the build → headless report → iterate → ship-to-slot loop.
---

# Build a level for BUILD & HIDE

You author levels **as code** with the `levelgen` builder, then a **headless
harness** bakes the nav grid and prints an LLM-readable report (ASCII floorplans
+ reachability + flow/headroom metrics). You iterate against that report until
it's clean, then write a playable `levels/slotN.json` and rebuild the release
binary so the user can walk it.

**Read [LEVEL_DESIGN_HEURISTICS.md](../../../LEVEL_DESIGN_HEURISTICS.md) first** —
it's the running log of playtest feedback and hard-won gotchas. This skill is the
operating manual; that file is the accumulated taste. Append new lessons there.

Units: **WT** = world tile = 0.25 m. `4 WT = 1 m`. A "3 m ceiling" = 12 WT.
Coordinates are **min-corner**; Y is up. The world is **subtractive CSG**: it
starts solid and you *carve air*. Rooms are carved boxes; walls are leftover
solid.

## Where things live
- Builder API: `native/crates/game/src/levelgen/builder.rs`
- Designs (author here): `native/crates/game/src/levelgen/designs.rs` — one `fn`
  per level, returning `b.finish()`.
- Register a new design in `native/crates/game/src/levelgen/mod.rs` (the `match`
  + the default).
- Analyzer/report: `native/crates/game/src/levelgen/analyze.rs`
- Serializer (writes the slot) + `verify_loads`: `serialize.rs` / `mod.rs`
- Nav (why descents fail): `native/crates/engine/src/sim/nav.rs`

## The loop (do this every time)
1. **Plan** the level as a room graph (spaces + how they connect + verticality),
   then write/edit a `fn` in `designs.rs` using the builder API below. Register it
   in `mod.rs` and make it the default design.
2. **Build + report** (from `native/`, debug is fine for iterating):
   ```
   LEVELGEN=1 LEVELGEN_DESIGN=<name> LEVELGEN_SLOT=7 cargo run -p game
   ```
   Grep the sections you care about (`=> all`, `y=<n> `, `OK — every`,
   `density:`, `SNIPER`, `### floor y=`). The floorplans are `step=2` on big
   levels, which **hides 1-WT-wide features** (thin stairs/pillars) — don't
   diagnose those from the plan; use the reachability + headroom numbers.
3. **Iterate** until: every room reachable; `HEADROOM` says OK; loops > 0 and few
   dead-ends (terminal closets/vaults are fine); at least one working perch; no
   accidental unreachable levels (a lone unreachable floor level is usually a
   **pillar top** — that's correct).
4. **Ship**: build release + regenerate the slot, then verify it loads:
   ```
   cargo build --release -p game
   LEVELGEN=1 LEVELGEN_DESIGN=<name> LEVELGEN_SLOT=7 ./target/release/build-and-hide.exe
   ```
   Confirm `wrote playable level` + `verify: slot 7 loads in-engine OK`. Default
   to **slot 7** (F-keys load 1–8; `LOAD_SLOT=7` boots straight in).
   **The game window locks the exe** — if a release build finishes in <1s or says
   "Access is denied," the user's game is open; ask them to close it.
5. Give the user the launch line and ask for a walk-through:
   ```powershell
   cd "d:\Claude Code Projects\Hide and Seek Level Builder\native"; $env:LOAD_SLOT=7; .\target\release\build-and-hide.exe
   ```
   (Click to grab the mouse; WASD+mouse to fly; `G` = on-foot HUNT, `I` = invincible.)

## Builder API cheat-sheet (`LevelBuilder`)
All positions min-corner WT. `let mut b = LevelBuilder::new();` … `b.finish()`.
- `set_scheme(n: 0..=8)` — texture for subsequent carves/pillars. **Vary per room/
  wing** (9 is reserved for platforms). Set before each room.
- `room(name, x, z, w, d, floor, height) -> RoomId` — carve a room (air box).
- `passage(a, b, x, z, w, d, floor, height)` — carve a connecting doorway/corridor
  **and record the edge a↔b**. Overlap BOTH rooms by ≥2 WT or it won't connect.
- `void(x, z, w, d, floor, height)` — carve air with no recorded edge (L-corridor
  legs, shafts). Pair with `link()` for the logical edge.
- `window(x, sill, z, w, height, d)` — a **thin frame opening** (sightline). Keep
  it ~4 WT deep (just through the wall), sill above the floor, and **inset from
  the perpendicular walls** or textures glitch.
- `pit(x, z, w, d, room_floor, depth)` — sink a floor section (split-level).
  **Needs a descent** — see verticality caveats.
- `pillar_in(room, x, z, size)` — full floor→ceiling cover column. Deferred to the
  end (carve-proof). Keep clear of stairs/platforms.
- `csg_stair(axis, side, face_pos, u0, u1, floor, ceil, dir, steps)` — stair cut
  into a wall. `axis` = wall normal (X or Z); `[u0,u1)` = width along the wall;
  `floor`/`ceil` = the source room's vertical extent; `dir` = `StairDir::Up|Down`.
- `platform(name, x, z, sx, sz, top, railings) -> PlatId` — free-standing slab
  (balcony/catwalk/landing). Also adds a room label → grab it with `last_room()`.
- `stair_to_platform(from:(x,y,z), plat, edge, offset, width, railings)` — free-
  standing stair, ground → platform edge.
- `stair_ground(from, to, width, railings)` / `stair_platform_to_platform(...)`.
- `link(a, b)` — record a logical connection with no geometry (stairs, holes).
- `spawn_wt(x, y, z)` — ingress point (WT; converted to meters).

## Hard rules (the short version — full log in the heuristics file)
- **Go big.** One or two hero rooms 40–60 WT wide with 24–30 WT ceilings. Never an
  8-WT ceiling. Mix sizes: small closets, medium fight rooms, large halls, long-
  skinny galleries. Uniform boxes read as bad.
- **Loops, not spokes.** Every room wants ≥2 connections; add perimeter room↔room
  links so there are multiple routes. Terminal vaults/closets may be dead-ends.
- **Split-levels read as handcrafted:** sunken pits, raised catwalks/mezzanines,
  balconies. Layer three heights in one hero room when you can.
- **Textures per room** via `set_scheme` — visual identity, not all-white.
- **Cover:** thin full-height pillars (`pillar_in`) to break sightlines.
- **Perch:** cantilever a wide deck out over a room (don't hug the wall) so it
  actually overlooks the floor; verify with the SNIPER metric.
- **Additive-after-subtractive:** anything solid you add (pillars) must come after
  carves — the builder already defers pillars; keep this in mind for custom Adds.

## Verticality — the critical gotcha
- **UP is easy and clean:** `csg_stair(... Up ...)` (wall-cut) or
  `stair_to_platform` (ascending) both bake walkable for player AND enemy nav.
  Prefer these. (mezzanine→door→loft→up-stair→attic is a proven pattern.)
- **DOWN is the hard problem:**
  - **Free-standing stairs down** render clean and the **player walks them**, but
    **enemy grid-nav can't reach the lower floor** (it reaches the treads, not the
    carved floor below). Confirmed across every config. Result: a player-only area
    (fine as a hiding spot; the report flags it "not reachable by ENEMY grid-nav").
  - **CSG down-stair** IS enemy-walkable, but a Down stair renders a closing "fill"
    wall (`ceil-sc`..`ceil`) that **floats in any open space** (pit / stacked
    room). Keep `ceil` low (~1) to sink it, or only use CSG-down where it's cut
    into a **real wall** so the fill hides in solid.
  - **A stair-run's lowest tread lands one step ABOVE its ground anchor** — anchor
    one lower to land flush.
- **Floor hole + downstair** (if you do it): the hole must be **wide enough to
  walk through and fit the whole stair**, the **stair top must meet the hole rim**
  at the upper floor, and there must be **≥8 WT headroom** the whole way down.
- **Headroom everywhere ≥ 8 WT.** Corridors/stairwells at 7 WT cause head-bump.
  The analyzer's HEADROOM lint flags anything under 8 — keep it green.
- **Open backlog bug:** the free-standing-descending-stair → carved-floor nav hop
  in `sim/nav.rs`. Fixing it makes open pits/holes work for enemies too.

## Reading the report
`overview` (bounds/counts) · `FLOORPLANS` (per-floor ASCII: `.`floor `#`solid
` `air/void `S`spawn, letters=rooms) · `CONNECTIVITY` (reachability + edges +
loops/dead-ends) · `VERTICALITY` (each floor level reachable? samples many cells)
· `SNIPER PERCHES` (LOS from platforms into lower rooms) · `HEADROOM` (clearance
lint) · `CAMP NOOKS` (alcoves). Green = all rooms reachable, HEADROOM OK,
loops>0, ≥1 perch.

When done, **append any new playtest feedback / lessons to
LEVEL_DESIGN_HEURISTICS.md** so the next session inherits them.
