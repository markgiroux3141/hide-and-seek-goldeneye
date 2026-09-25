---
name: build-level
description: Author a playable level for the native BUILD & HIDE (GoldenEye-style hide-and-seek) game using the headless levelgen harness. Use when the user asks to build, design, generate, extend, or iterate on a level/map for the Rust game in native/, or mentions rooms/halls/stairs/pits/the level generator. Drives the build → headless report → iterate → ship loop.
---

# Build a level for BUILD & HIDE

You author levels **as code** with the `levelgen` builder, then a **headless
harness** puts the level through the game's own pipeline — load into a real
`World`, save with the real level writer, reload the file, bake nav with the same
function `G` uses — and prints an LLM-readable report (ASCII floorplans +
reachability + flow/headroom metrics + the NAV tab's own findings). You iterate
against that report until it's clean; the file it writes is already playable.

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
- Register a new design in **three** places, all enforced by a test: the `DESIGNS`
  table in `levelgen/mod.rs`, the `golden!(…)` list in `levelgen/tests.rs`, and its
  expected component sizes in `EXPECTED` there.
- Analyzer/report: `levelgen/analyze.rs`; the NAV findings are
  `world/nav_issues.rs` (the same code as O → NAV → Calculate).
- Nav: `native/crates/engine/src/sim/nav.rs`; stair/platform solids:
  `engine/src/geometry/structures.rs`.

## The loop (do this every time)
1. **Plan** the level as a room graph (spaces + how they connect + verticality),
   then write/edit a `fn` in `designs.rs` using the builder API below and register
   it (above).
2. **Build + report** (from `native/`):
   ```
   LEVELGEN=1 LEVELGEN_DESIGN=<name> cargo run --release -p game
   ```
   Writes `levels/levelgen_<name>.json` (listed in the LEVELS tab as
   "levelgen <name>"). It will overwrite its own earlier output but **refuses** to
   overwrite a level an author saved. `LEVELGEN_SLOT=N` writes `levels/slotN.json`
   instead (F-key / `LOAD_SLOT`). Exit code is non-zero on failure.
   The report opens with a `VERDICT` and one `[PASS|WARN|FAIL]` line per check —
   read that first; everything below it is the detail behind a line. For scripting,
   `LEVELGEN_REPORT=json` prints the same report as one JSON document (`verdict`,
   `checks`, `rooms`, `declared`, `merged`, `perches`, …) — assert on that rather
   than grepping prose.
3. **Iterate** until the verdict has no `FAIL`, and every `WARN` is one you mean
   (a pillar-top island, a deliberate terminal vault). `round trip: OK` must hold.
4. **Test**: `cargo test -p game levelgen` — the golden test pins every design's
   walkable components cell for cell. If you changed a design on purpose, update
   its `EXPECTED` row in the same change and say why in its comment.
5. **Ship**: `cargo build --release -p game`, rerun step 2 with the release exe
   (`./target/release/build-and-hide.exe`), then give the user the launch line:
   ```powershell
   cd "d:\Claude Code Projects\Hide and Seek Level Builder\native"; $env:LOAD_LEVEL="levelgen <name>"; .\target\release\build-and-hide.exe
eleaseuild-and-hide.exe
   ```
   (Click to grab the mouse; WASD+mouse to fly; `G` = on-foot HUNT, `I` = invincible.)
   **The game window locks the exe** — if a release build finishes in <1s or says
   "Access is denied," the user's game is open; ask them to close it.

Headless diagnostics take any level too — a slot number, a level name, or a path —
and now bake **with** placed props, exactly as in-game:
`./target/release/profile_hunt.exe "facility 2" 1` (nav findings + step timing) and
`./target/release/probe_hunt.exe "facility 2"` (drives real hunters between pads).

## Builder API cheat-sheet (`LevelBuilder`)
All positions min-corner WT. `let mut b = LevelBuilder::new();` … `b.finish()`.
`designs::compound` is a whole level written this way — read it first.

### Relational layer — prefer this
These take **rooms** and work out the boxes themselves (which wall two rooms share,
where the opening centres, how far it overlaps each room, how many steps a stair
needs). A call that can't be built records a **builder problem** — the report's
`builder` check fails and says what to change — rather than carving something wrong.
Compass: **North = −z** (up in the floorplans), East = +x.
- `room_beside(name, of, Dir, wall, w, d, floor, height) -> RoomId` — carve a room
  across a `wall`-thick wall on `of`'s side, centred. `room_beside_at(…, along, …)`
  sets the offset from `of`'s min corner instead. A wall of ≥ 1 WT keeps them apart.
- `door(a, b, width)` / `door_at(a, b, t, width, height)` — open the shared wall
  (at fraction `t` along it), overlapping both rooms by 2 WT; records the edge. Any
  wall thickness, so it is also a straight corridor. Floors must match (within 1 WT)
  — except a door *off a mezzanine*: if the lower room's ceiling clears the upper
  floor by the door's height, it opens at the upper floor.
- `corridor(a, b, width)` — straight if they face each other, else an L.
- `window_between(a, b, t, sill, width, height)` — see/shoot-through, not walkable.
  Keep `t` off the door's (`door` uses 0.5).
- `stair_between(a, b, width)` — rooms on different floors across a shared wall; the
  wall must be ≥ steps + 1 WT (it tells you how much to move them if not).
- `stair_through_floor(upper, lower, x, z, Dir, width)` — rooms stacked one over the
  other: a free-standing flight from `(x, z)` on the upper floor descending toward
  `Dir`, with the floor hole cut over the whole flight so every tread has headroom.
- `spawn_pad(x, y, z, yaw_deg)`, `weapon(name, x, y, z)`, `ammo(name, x, y, z)` — the
  match. Players and hunters start unarmed, so put weapons down. A misspelt weapon is a
  builder problem. (Doors as props are not in the builder yet.)

### Coordinate layer
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
- **Cover:** thin full-height pillars (`pillar_in`) to break sightlines — **≥ 3 WT
  from any wall, stair or door**, or flush against it: a 2 WT gap is a 0.5 m slot a
  hunter's body cannot pass (the `pinches` check flags it).
- **Perch:** a deck overlooks whatever its **edge** can see at eye height — a wide
  mezzanine along a wall works (the old "cantilever it, don't hug the wall" rule was
  mostly an artefact of a perch check that sighted from the deck's centre, 1 m below
  eye height). Verify with PERCHES.
- **Additive-after-subtractive:** anything solid you add (pillars) must come after
  carves — the builder already defers pillars; keep this in mind for custom Adds.

## Verticality
- **Up and down both work** for player AND enemy nav: `csg_stair` (wall-cut, either
  direction), `stair_to_platform`, and `stair_ground` (free-standing, either
  direction — including down into a pit or a room below y=0).
  - *History:* until 2026-09-24 free-standing stairs **down** baked no enemy nav,
    and this section called that a law. It was a bug — `find_floor_y_at`'s 0.0
    default culled every step of any flight below y=0 (fixed in
    `structures::resolve_run`, regression test
    `a_platform_stair_down_into_a_pit_bakes_walkable_nav`). If a descent is
    unreachable now, it is the geometry: read the NAV findings, don't route around it.
- **CSG down-stair** renders a closing "fill" wall (`ceil-sc`..`ceil`) that **floats
  in any open space** (pit / stacked room). Use a free-standing stair into open
  spaces; use CSG-down where it's cut into a **real wall** so the fill hides in solid.
- **A stair-run's lowest tread lands one step ABOVE its ground anchor** — anchor
  one lower to land flush.
- **Floor hole + downstair:** use `stair_through_floor` — it sizes the hole to the
  flight. By hand, the hole must **cover the whole stair footprint**, the stair top
  must meet the hole rim, and every tread needs ≥ 8 WT of headroom; a flight that runs
  on under the slab past the hole is what cut `grand`'s undercroft off until 2026-09.
- **Headroom everywhere ≥ 8 WT.** Corridors/stairwells at 7 WT cause head-bump.
  The analyzer's HEADROOM lint flags anything under 8 — keep it green.

## Reading the report
- **Summary** — `VERDICT` plus one line per check: `walkable` (components; an island
  over 16 cells fails), `reachable` (cut off vs *no standable floor*, which means
  under 6 WT of headroom or buried), `declared links` (every `passage`/`link` you
  declared is walkable), `merged rooms` (two carved rooms share air with no wall
  and you never declared them connected — usually a missing wall), `loops` (counted
  on the real walkable graph, corridors as nodes, so parallel halls count),
  `perches`, `headroom`, `floors`.
- **NAV** — verbatim what O → NAV → Calculate shows in-game: islands with the gap to
  the nearest neighbour, orphaned objects, player-only climbs.
- **ROOMS** — per room: cells, reachable, links in the *derived* graph; then the
  declared connections (`!!` = not walkable), connections that exist but were never
  declared, and merged pairs.
- **FLOORS** — one plan per real floor (a level with ≥ 16 flat cells); treads and
  steps are drawn on the floor they rise from. `.` floor · `/` stairs & steps · `!`
  cut off from the main area · `#` wall · `S` spawn · letters = rooms. Wide plans
  downsample but keep the most important glyph per block, so thin stairs and islands
  stay visible.
- **PERCHES** — per deck, the share of each lower room visible from somewhere on its
  edge at eye height. **HEADROOM** — cramped cells, clustered. **CAMP CORNERS** —
  flat corner cells clear of stairs.

When done, **append any new playtest feedback / lessons to
LEVEL_DESIGN_HEURISTICS.md** so the next session inherits them.
