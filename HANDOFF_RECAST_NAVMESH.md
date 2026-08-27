# Handoff — replacing the voxel grid with a proper Recast navmesh

> Written 2026-08-27, immediately after commit `3c5f5fd` ("Hunters that reliably come and
> find you"). **Nothing here is built.** The baseline is green, pushed, and is the thing to
> revert to if this fails.
>
> This is the *second* attempt. The first (2026-07-30) was built and reverted the same day.
> Read "Why it failed last time" before writing any code — the failure was not the idea.
>
> Companion memories: `ai-navmesh-attempt-reverted`, `nav-vs-player-mobility`,
> `hunter-movement-chokepoint`, `perf-diagnostic-tools`, `ai-testbed`.

## The one thing that is different this time

**There is now an acceptance harness that runs on the real level.** The last attempt died
because its tests were toy single-region rooms that passed while the shipping level broke,
and there was no way to see the difference except by playing. That gap is closed:

```
cargo run --release --bin probe_hunt -- 1 --secs 60
```

drives a real hunter with the real mover over `levels/slot1.json`, every spawn-pad pair.
**Capture the baseline before touching anything**, and treat it as the go/no-go.

### Baseline to beat (measured 2026-08-27, commit `3c5f5fd`)

| measurement | command | baseline |
| --- | --- | --- |
| pad-to-pad sweep | `probe_hunt 1 --secs 60` | **90/90 arrived**, 0 failed |
| cross-floor chase, full AI | `probe_hunt 1 --ai --from 12.71,-3.25,0.33 --to 26.7,18.0,-0.2 --secs 60` | **came and found the player in 21.1 s**, closing to 5.5 m |
| connectivity | printed by either of the above | **1 walkable component, 22686 cells, 0 orphans** |
| corridors under 0.73 m | same | **0** |
| bake cost | `profile_hunt 1 20` | G (bake + spawn) **~550–630 ms** |
| sim cost | `profile_hunt 1 20` | **~0.5 ms/step**, 0 A\* failures |
| tests | `cargo test` | **501 game + 91 engine + 3 csg**, 2 ignored |

**If the navmesh cannot match all of those, it has not earned the swap.** Say so plainly
and revert — that is a legitimate outcome of this session, not a failure to deliver.

## Why it failed last time (2026-07-30)

It was **not** Recast. It was a hand-rolled polygon mesh greedy-merged from the voxel grid,
with poly-A\* and a string-pull. On a real multi-region base it returned **valid-but-wrong,
truncated paths that dead-ended at region boundaries**, and the grid-A\* fallback only
caught an outright `None` — never a bad non-`None` path — so the broken paths were used.
Hunters stayed in the spawn room, refused hallways, and hung at the tops of staircases.

Three rules follow directly:

1. **Use the actual Recast pipeline, not a bespoke merge of our voxels.** The value is in
   the specific stages (below), not in "a mesh".
2. **Any fallback must validate the returned path, not just its `Some`-ness.** A path that
   does not end near the requested goal is a failed path.
3. **Green unit tests prove nothing here.** The lab passed last time. `probe_hunt` on
   slot1 is the test that matters.

## Why it is worth doing anyway

Four of the fixes in commit `3c5f5fd` are compensating for the voxel grid's structure.
Recast removes the *class* of problem rather than the instance:

| our patch | what Recast does instead |
| --- | --- |
| `STAIR_STEP` — stair-tagged 2-cell step, because runs steeper than 45° get treads shallower than a cell and the walkable strip skips one | Rasterizes triangles into **spans** and merges them; `walkableClimb` is a first-class parameter. Sub-cell treads simply do not arise. |
| `pinch_points` reporting corridors a body cannot fit down, and a movement-time wall-clearance nudge that fights the mover | **Erodes the walkable surface by the agent radius** at bake. A path on the mesh is guaranteed to fit, so the nudge and the report both become unnecessary. |
| `chase_aim_point` validating targets because `find_path` silently snaps an off-mesh goal up to 6 m and can land on the caller's own cell | `dtNavMeshQuery::findNearestPoly` takes **explicit search extents** and reports failure. The silent-relocation class disappears. |
| Off-mesh traversal (jump/drop/vault) has no representation at all | **Off-mesh links** are first-class. |

It also gives `walkableSlopeAngle` as a real parameter instead of our implicit ~45°, which
is where the whole stair saga started.

## What Recast will *not* fix — do not conflate these

- **Two `AiState::Chase` arms** (legacy FSM ~`enemy.rs:1690`, utility layer ~`:2300`). The
  utility one is what runs. This is duplicated behaviour and it has already cost one fix
  that went into the wrong arm.
- **`stuck_hold`** — the anti-grind settle silently zeroes the requested velocity, so a
  frozen hunter looks identical to a calm one on every other diagnostic.
- **Doors.** Ours ride a live overlay on the frozen grid so they need no re-bake. Detour's
  equivalent is a poly flag / dynamic obstacle. Plan the mapping deliberately; do not
  leave it to the end.
- The mover itself (`try_step`, `snap_to_floor`, `ground_path_clear`) — see
  `hunter-movement-chokepoint`. Every displacement goes through `try_step`; keep it that
  way.

## Suggested shape

**Decide the dependency first, and say the trade-off out loud.** The project is pure Rust
today (wgpu, rapier3d, hecs) and builds with plain `cargo build` on Windows. Options:

- **C++ Recast via bindings** — the battle-tested original; adds a C++ toolchain to the
  build. This is a real cost for a Windows solo project; check `cargo build` still works
  from clean before going further.
- **A Rust implementation** (e.g. `oxidized_navigation`'s core, `landmass`, or similar) —
  keeps the toolchain, but verify it implements the *full* pipeline including **radius
  erosion** and **region partitioning**, not just poly pathfinding over a supplied mesh.
  A crate that only does A\* on a mesh you build yourself is the 2026-07-30 attempt again.

Then, staged, each stage measurable:

1. **Bake only.** Feed the existing collision geometry (region trimesh + structure solids +
   prop boxes — see `World::structure_solid_boxes` / `prop_solid_boxes` /
   `stair_run_solid_boxes`) into the pipeline. Ship no behaviour change. Report polys,
   bake time, and — the key one — whether the walkable surface covers the same places the
   grid's 22,686 standable cells do. A visual diff through the existing NAV overlay
   channel (`World::nav_overlay_mesh` → `Renderer::set_nav_overlay_mesh`) is the fastest
   way to see a hole.
2. **Queries behind a flag**, default OFF: `find_path`, `nearest_standable`,
   `floor_height_at`, `component_at`, `los_clear`. Keep the grid alive as the A/B, exactly
   as `AI=pd|ours` and `PD_EXPLOSIONS=0` are done in this codebase.
3. **Flip the flag on and run the harness.** Sweep, chase, connectivity, bake cost, sim
   cost. Compare against the table above.
4. Only once that passes: delete `STAIR_STEP`, the pinch report, and the wall-clearance
   nudge, and re-run the harness to prove they were compensations rather than load-bearing.
5. Off-mesh links (the actual payoff) — a separate session.

## Traps, all learned the hard way

- **`prop_solid_boxes` and `register_doors_with_nav` need loaded GLB mesh bounds, which
  headless binaries do not have.** So `probe_hunt` bakes with no props and no doors. Any
  claim about doors or prop-blocked pockets must be made in-game (F10), not headless. This
  bit me twice.
- **A hunter's body is not a point.** `ENEMY_RADIUS` is 0.24 m; the erosion radius must
  match it or the mesh will promise clearances the body does not have.
- **The player and the hunters use different mobility models** and always will — the
  player is a rapier capsule that jumps 0.76 m. `nav_issues` reports player-only climbs for
  exactly this reason. Recast does not reconcile them; it just makes the hunter's side
  honest.
- **Bake cost lands on the `G` keypress.** 550 ms is already a felt hitch; do not make it
  worse without saying so.
- **The `AI=pd` knowledge policy makes hunters omniscient about the player's position.**
  A pathing regression will therefore show as hunters walking into walls rather than as
  hunters losing you — do not read "they still chase" as "pathing is fine".
- Keep `World::nav_issue_report()` working. It is the level author's tool and it is how a
  broken bake gets noticed by someone who is not debugging.

## The honest bar

The grid A\* that exists today works: one component, every pad reachable, hunters that
climb 21 m and find the player in 21 s, 0.5 ms a step. Recast is the right *architecture*
and it will pay for itself in off-mesh links and in the bugs it makes impossible — but it
has to clear a bar that is no longer low. Measure first, flag it, A/B it, and be willing to
report "the baseline is still better" if that is what the numbers say.
