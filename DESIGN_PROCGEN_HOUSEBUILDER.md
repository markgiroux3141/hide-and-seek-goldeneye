# Procedural floor-plan generation — HouseBuilder port notes

Status: **idea parked** (not built). Captured 2026-07-25 after reviewing an old
Unity project so we can pick it up later.

## Source

`D:\Old Projects Transfer\Backup Code\HouseBuilder` — a Unity/C# automated level
generator (written ~2017). Files of interest:

- `HouseBuilder.cs` — the whole algorithm (~800 lines).
- `Room.cs`, `Door.cs`, `Wall.cs`, `Collision.cs` — plain data.
- `Helpers.cs` — AABB collision (`CheckRoomCollision`) + `ContractEdge`.
- `ProceduralMesh.cs`, `Bracing.cs` — mesh + cosmetic trim. **Not relevant** to us.

## What it does

A recursive room-branching floor-plan generator:

1. Start with one main room at center.
2. Each room spawns up to 4 children (top/bottom/left/right), each a random size,
   slid to a random offset along the shared edge (`GetRandRoomPos`).
3. `AdjustRooms` — AABB collision detection that **contracts edges** to resolve
   overlaps (`Helpers.ContractEdge`).
4. Prune passes: `RemoveSmallRooms` / `RemoveFloatingRooms` /
   `RemoveOverAdjustedRooms` drop rooms contracted below viable size or orphaned.
5. Doors placed at random valid positions along each parent→child shared edge.
6. Walls built as leftover geometry (`GetWallParams` / `GetWallRemainder`).
7. Per-room random `WallHeight` → vertical variety; taller child rooms get an
   extra wall strip above the parent ceiling line.

## Why it's a good fit for our Rust levelgen

Our `native/crates/game/src/levelgen/` already has everything downstream of a
layout: `builder.rs` (carve-air intent API), `analyze.rs` (nav bake +
connectivity/dead-end report), `serialize.rs` (playable `slotN.json`), and the
build→bake→report→slot loop. There is currently **no generator** — every design
in `designs.rs` is hand-authored. This algorithm is exactly a generator that
emits the primitives the builder already exposes.

## The key insight — subtractive deletes the hard half

Unity's HouseBuilder is **additive** (build walls around rooms). Our engine is
**subtractive** (carve air out of solid; walls = leftover solid). So a port keeps
only the *layout* half and throws the *wall-construction* half away entirely:

| Unity piece | Port verdict |
|---|---|
| Recursive 4-way branching + random size/slide | **Take** — the whole value |
| AABB collision + edge contraction | **Take, but loosen** — overlapping carves just merge into one air space (not a crash); only need enough spacing that a wall survives between unrelated rooms |
| Prune passes (small/floating/over-adjusted) | **Take** — keeps the graph clean |
| Random door placement along shared edge | **Take** → emit as `builder::passage()` |
| Per-room wall-height variation | **Take** → the `height` param |
| Wall construction / `GetWallRemainder` / remainder math | **Drop** — subtractive makes it free |
| `ProceduralMesh`, `Bracing` | **Drop** — engine meshes CSG; bracing is cosmetic |

Net: ~150–200 lines of Rust (branch + collision + prune + door emission), not an
800-line translation. The nastiest ~300 Unity lines evaporate.

## What the algorithm is missing for hide-and-seek

1. **Loops.** Parent→child doors form a spanning tree — connected but almost no
   loops. Our `LEVEL_DESIGN_HEURISTICS.md` (and the `varied` design) deliberately
   add perimeter loops to kill dead-ends. Needs a post-pass adding passages
   between adjacent non-parent-child rooms.
2. **Deliberate variety.** Pure random branching gives blobby, uniform rooms. Our
   best hand-authored levels have a tall atrium, a skinny gallery, etc. Best used
   as a **scaffold generator**, not a finished-level generator.

## Recommended shape if we pick this up

- New `native/crates/game/src/levelgen/generate.rs`: `generate(seed) -> BuiltLevel`
  emitting `LevelBuilder` calls (room/passage/height), seeded RNG for repeatability.
- Wire as `LEVELGEN_DESIGN=generated` in `mod.rs`.
- Add a loop-adding post-pass (adjacent-room extra passages).
- Lean on the existing `analyze.rs`: generate N seeds, auto-reject seeds whose
  report shows poor connectivity / too many dead-ends, then hand-tune the winner.
- Tuning knobs to expose: branch depth, min/max room size, ceiling-height range,
  loop density, seed.

## Related

- `LEVEL_DESIGN_HEURISTICS.md` — the quality bar any generated level must clear.
- `native/crates/game/src/levelgen/` — the harness the generator plugs into.
