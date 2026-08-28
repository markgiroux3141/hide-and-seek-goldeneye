# Recast navmesh, attempt 2 — results

> Written 2026-08-27, answering `HANDOFF_RECAST_NAVMESH.md`. **The navmesh did not pass
> the acceptance harness. The grid baseline stands.** The work is parked behind
> `NAV=recast`, which defaults **off**; with the flag unset the shipping path is
> unchanged and re-verified at 90/90.
>
> Read this before attempting a third time. The dependency question is settled and the
> failure is *not* where the last two attempts' failures were.

## The go/no-go, measured

Baseline re-measured on this machine first, and it reproduced the handoff's table exactly.

| measurement | baseline | `NAV=recast` | verdict |
| --- | --- | --- | --- |
| `probe_hunt 1 --secs 60` | **90/90 arrived**, 0 failed | 46 arrived, 7 slow, **37 FAILED** | ✗ |
| `probe_hunt 1 --ai` cross-floor chase | found in **21.1 s**, closing 5.5 m | found in 29.0 s, closing 6.2 m | ✗ |
| `G` (bake + spawn) | ~600 ms | ~1000 ms (grid 490 + mesh 434) | ✗ |
| sim | 2.02 ms/step | 2.04 ms/step | = |
| `cargo test` | 501 game + 91 engine + 3 csg | 501 + **99** + 3, all green | = |

(The handoff's table says ~0.5 ms/step; this machine measures 2.02 ms/step on the
*baseline*, so the sim comparison above is like-for-like and the difference from the
handoff is the machine, not a regression.)

## What actually went wrong — and where it is *not*

The first attempt (2026-07-30) died on a hand-rolled polygon merge. That is fixed: this
used the real Recast stages. It died somewhere new.

**On `slot1`, Recast bakes 1271 polygons in 180 disconnected islands** (biggest = 180
polys, 14%), where the grid finds **one** component of 22,686 cells.

That is the whole story. Everything downstream follows from it: 86% of path requests came
back "goal walled off", the grid fallback carried them (466 of 466 accepted — the fallback
works), and the sweep still failed because of the paths the mesh *did* serve.

What was ruled out, by measurement:

- **Not the parameters.** Swept cell size, cell height, region min/merge area,
  simplification error (including 0), max edge length, walkable climb (to 2 m), walkable
  height, slope (to 89°), and erosion radius (**including 0**). Biggest island stayed
  11–26%; edge adjacency sat at exactly 45% throughout.
- **Not the crate.** `rerecast` — an entirely independent Rust Recast port — produces
  **1271 polys, the same 180-poly island, the same 45% adjacency** where `landmark`
  produces 1269. Two independent implementations agreeing to within two polygons means
  Recast is doing what Recast does.
- **Not the pipeline wiring.** A synthetic 36-room control level through the same code
  bakes **98% connected** (1450 polys, one island). Scale is not it either: a 10×10-room
  control at 4210 polys is also 98%.
- **Not erosion, and not the agent size.** Radius 0 still gives 14%.
- **Not vertex duplication** (0 duplicated vertices) and **not winding** (the 1254-up /
  1240-down split is just floors and ceilings; double-siding made it worse).

**So it is slot1's own triangle soup that fragments, and the root cause is not yet
identified.** That is the single question a third attempt should open with. The islands
are coherent rooms and corridors that never touch — the connections between them are
missing, not mangled.

## Follow-up: is it our CSG-plus-boxes level style?

Asked directly, and measured rather than reasoned about. **No.**

- **Baking from the CSG regions ONLY gives the identical result** to the full mix — the
  same 7-island spawn-pad split (606 polys vs 1269). Subtraction brushes mixed with
  platform/stair boxes is *not* what breaks it.
- **Recast does not care about dirty soup.** It voxelizes, so T-junctions, coplanar
  fragments, overlapping solids and duplicate faces are all normal input for it.
- **A clean hand-built level works perfectly through the identical code path**: the
  synthetic control bakes 98% in one island. So yes — carefully authored geometry is a
  much better candidate, and that is measured, not assumed.

### The real mismatch: two different definitions of "the world"

- The **grid** asks a *volumetric* question — `region.solid_at(x, y, z)`, is this point
  inside brush material.
- **Recast** asks a *surface* question — where are the triangles.
- The triangle soup we can hand it is a **derived artifact** built for rendering and
  physics. It is not a watertight description of the playable volume.

The clearest evidence: **Recast finds 4381 m² of walkable surface where the grid finds
1418 m²** — and they disagree in *both* directions. The mesh picks up roofs, wall tops and
the tops of solids that the grid excludes (the single biggest "island", 2926 m², is the
building's roof), while missing places the grid counts. The two systems are not modelling
the same world, so a pathfinder swap alone was never going to line up.

**What is still unknown:** the specific geometric feature that severs the pad rooms. Ruled
out by measurement: every Recast parameter (against the pad-island metric, not just poly
counts), the crate, and the geometry mix.

### Two of my own metrics were misleading — do not quote them

- **"180 islands / 14%" was polygon count**, and a big open room can be one polygon. By
  *area* the biggest island is 67%, and 12 islands hold 90% of the walkable area. The
  honest gameplay metric is the pad one: **10 pads on 7 islands**, which no parameter moves.
- **"9.2% of cells have no floor beneath them"** used a 0.35 m tolerance, and the grid
  reports cell positions up to ~0.5 m above the actual surface in places. That number is
  inflated and should not be treated as a hole count.

## The second finding: the mover and the mesh are two world models

Independently of the fragmentation, mesh paths get **"refused by `ground`"**. Every hunter
displacement goes through `Enemy::try_step`, which validates each step against the *grid*.
A path that is valid on the mesh is not automatically walkable to that gate.

The first guess — that the funnel's corner-to-corner legs are too sparse for a gate
written for adjacent-cell waypoints — was right in principle and insufficient in practice:
resampling to 0.25 m is **kept** (it is correct regardless) but only moved the sweep from
39 failures to 37.

**Swapping the pathfinder therefore means swapping `try_step`'s validation too.** The
handoff's staged plan treated the mover as untouched; it cannot be. See
`hunter-movement-chokepoint`.

## What is in the tree

All of it is behind `NAV=recast` (`grid` is the default; `NAV` unset takes a code path
identical to before — `find_path` calls `find_path_grid` directly when no mesh is baked).

- `crates/engine/src/sim/navmesh.rs` — the real Recast pipeline via `landmark`, queried
  through `waymark`, plus **our own funnel** and the goal validation. 8 unit tests.
- `NavWorld::find_path` dispatches to the mesh and falls back to the grid **with the
  returned path validated against the goal** — the handoff's rule 2. `find_path_grid` is
  the old function, unchanged.
- Diagnostics, all env-gated and all of which earned their keep:
  `NAV_DEBUG=1` (coverage at three radii, island flood, poly counts),
  `NAV_SLICE=<y>` (top-down grid-vs-mesh map),
  `NAV_WHERE=1` (first point where a grid route leaves the mesh),
  `NAV_DUMP=<path>` / `NAV_DUMP_CELLS=<path>` (the bake input as OBJ — this is what made
  parameter sweeps take seconds instead of a game rebuild each).

Two dependencies were added (`landmark`, `waymark`, plus a second `glam` at 0.30 for their
boundary). **If this is not going to be picked up again soon, reverting the commit is
reasonable** — the findings are in this file and in the `recast-navmesh-crates` memory.

## Two `waymark` 0.1 bugs, if anyone uses it

Neither causes the fragmentation, but both are silent and both cost real time:

1. `NavMesh::build_from_recast` walks the polygon array at stride `nvp` when Recast's
   layout is `nvp * 2` (vertices *then* neighbours), so polygons progressively read
   neighbour data as vertex indices. Use `create_params_from_polymesh` +
   `add_tile_from_params`. 
2. `NavMeshQuery::find_straight_path`'s funnel is wrong — on a 1260-path sweep **25% of
   paths ran straight through solid walls**, with no truncation and no error.
   `crates/engine/src/sim/navmesh.rs::string_pull` replaces it.

And the trap that cost the most: **Detour's `triarea2` is `(c-a) × (b-a)`, the opposite
sign to the usual 2D cross product.** Get it backwards and the funnel still returns a
path — just one that is *longer* than the corridor it was pulled from (a correct funnel is
always shorter) and that clips walls. Length-vs-corridor is the cheap check for it.

## The oracle worth keeping

Sample every path leg at 0.25 m and call `find_nearest_poly` with tight extents; any miss
means the path left the mesh. It caught the `find_straight_path` bug on first contact.
Portal-midpoint paths (verified clean on all 1260) are the safe fallback if a funnel is
ever suspect. This is the check the 2026-07-30 attempt lacked, and it works.
