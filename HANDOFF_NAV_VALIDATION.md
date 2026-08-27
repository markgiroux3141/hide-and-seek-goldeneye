# Nav validation — SHIPPED, both phases

> Rewritten 2026-08-22. **Phase 1 (report + panel + overlay) and phase 2 (the stair-aware
> step limit) are both SHIPPED and green**; awaiting playtest. `slot1` went from 4 walkable
> components with 15% of its floor cut off to **one component, 22,686 cells, zero
> orphans**. Both of the original handoff's open questions are answered with measurements
> rather than argument.
>
> Design doc: `DESIGN_NAV_VALIDATION.md`. Memories: `nav-vs-player-mobility`,
> `perf-diagnostic-tools`, `spawn-points-respawn-pivot`, `ai-navmesh-attempt-reverted`.

## What shipped

**`O` → NAV → Calculate.** An explicit button because it bakes a grid (~0.5 s on a real
level) and then spends ~150–200 ms measuring it — far too slow to run per edit.

| piece | where |
| --- | --- |
| Engine queries | `sim/nav.rs`: `standable_with_components`, `main_component`, `nearest_gap`, `nearest_neighbour_gap`, `pinch_points`, `overclimb_edges`, `cell_size_m`, `max_step_m` |
| Findings + report | `game/src/world/nav_issues.rs` — `NavIssues` / `NavIsland` / `NavOrphan` / `NavLine`, `World::calculate_nav_issues`, `nav_issue_report` |
| Panel | `app.rs` — `PanelTab::Nav`, Calculate + overlay toggle + the coloured line list |
| Overlay | `Renderer::set_nav_overlay_mesh` (its own channel; ~90k verts, uploaded on a revision change, **not** per frame) |
| Headless | `profile_hunt` prints the whole validation block after the three older reports |

What it reports, and why each line is shaped the way it is:

- **Components**, with an explicit clean verdict — "1 walkable component — all N cells of
  floor connect". A panel that says nothing is indistinguishable from a panel that is
  broken (the old open decision 3).
- **Per island: the nearest gap, split into flat + rise.** One 3D distance cannot tell
  "one step too tall" from "a 3.5 m drop", and those have nothing in common as fixes. The
  line names which of the three it is.
- **Orphans** — pads, pickups, turrets, doors that nothing can walk to.
- **The pads `G` will silently drop**, by the runtime's own rule (see below).
- **Pinch points** — corridors under `2·(ENEMY_RADIUS + half a cell)` = 0.73 m, the width
  below which the body clips wall geometry *while walking the grid line*. Derived, not
  tuned. Errors below `2·ENEMY_RADIUS` = 0.48 m, where it does not fit at all.
- **Player-only climbs**, 0.50–0.76 m, flagged as information — except the ones that
  would reconnect a cut-off area, which are errors and listed first.
- **The 3D overlay**: green = the main component, one colour per island, red posts on
  pinches, orange on reconnecting climbs. Independent toggle, so you leave it up, edit,
  and re-Calculate to see whether the island closed.

### Three things the implementation got wrong first, worth not repeating

1. **Measure each island against its nearest component, not against main.** Islands
   *chain*. On `slot1`, comp 1 (1,380 cells) is 3.5 m below main and 0.5 m from a 32-cell
   sliver that is itself 0.5 m from main. Measured against main it reads as an unfixable
   cliff; against its actual neighbour it is two small steps. `nearest_neighbour_gap`
   exists for exactly this, and `NavIsland::gap_to` names the neighbour so the author can
   follow the chain one line down the list.
2. **`NavWorld::is_standable` answers *true* for cells just outside the baked grid** —
   out-of-bounds below the world counts as solid ground — so any neighbour probe that
   then indexes `comp` must bounds-check first or `idx` silently aliases an unrelated
   cell. `comp_at_cell` does. (`label_components` and `find_path` both have the same
   latent aliasing; harmless there, since the aliased cell is in the same component.)
3. **Phrasing is part of the diagnosis.** The first version said "3.50 m below the level —
   one step too tall", which sends the author to fix the wrong thing. The report now picks
   between *one step too tall* / *you jump this, hunters cannot* / *a drop, needs stairs* /
   *a distance, needs a bridge* from the two numbers.

### Island spawn pads: still excluded, and now said out loud

User call, 2026-08-22: keep the exclusion (it is what stopped the 10 fps), and surface it.
The grouping rule is **one function** — `nav_issues::partition_pads` — called by both
`prepare_spawn` and the panel, so "G will IGNORE pad 1 at (12.1, 24.0, -4.6)" is produced
by the same code that does the ignoring. A second copy of that rule would eventually
report confidently on a decision the runtime no longer makes.

## The measurement on `levels/slot1.json`

```
4 walkable components — 15% of the floor is cut off
  main: 19198 cells (85%)
  island comp 4: 2076 cells (9.2%) — 0.50 m above the level          at ( 13.6, 10.0,  -1.9)
  island comp 1: 1380 cells (6.1%) — 0.50 m below island comp 3      at ( -8.9, -9.0, -11.1)
  island comp 3:   32 cells (0.1%) — 0.50 m above island comp 1      at ( -8.9, -8.5, -10.9)
9 authored objects nobody can walk to: 2 spawn pads, 6 pickups, 1 door
1423 cells in corridors under 0.73 m — narrowest 0.25 m (a body is 0.48 m across)
71 player-only climbs 0.50–0.76 m; 12 of them would RECONNECT a cut-off area
calculated in ~190 ms (HUNT, reusing the frozen grid)
```

## ~~Open decision 2~~ ANSWERED: sub-cell stair treads, not real 0.5 m steps

The old handoff left this open, correctly: raising `MAX_STEP` to 2 reconnects the whole
level, but only *if* those 0.5 m gaps are a sampling artifact rather than steps the author
built. They are an artifact, and the chain is now complete:

1. Every reconnecting climb sits on **stair run 10 or 13**.
2. `resolve_run` (`geometry/structures.rs:295`) does `steps = round(rise / step_height)`
   and `step_run = total_run / steps`. Runs 10 and 13 are **steeper than 45°** — rise/run
   `18/16` and `14/13` — so with 1 WT risers their treads come out **0.889 and 0.929 WT
   deep: shallower than one nav cell.**
3. Cell-centre sampling therefore misses some treads entirely. The walkable strip skips a
   tread and the vertical gap between consecutive standable cells becomes 2 cells.
4. **Exactly 2 of the level's 17 stair runs have sub-cell treads, and they are the two
   that produce all three islands.** The other 15 (treads 1.14–2.30 WT) are clean.

| run | rise (WT) | run (WT) | steps | tread depth |
| --- | --- | --- | --- | --- |
| 10 | 18 | 16 | 18 | **0.889** |
| 13 | 14 | 13 | 14 | **0.929** |
| the other 15 | — | — | — | 1.14 – 2.30 |

So `MAX_STEP = 2` would hide a 2-in-17 authoring/sampling artifact by making *every*
hunter out-climb the player *everywhere*. It is one constant and it is tempting; it is
also the least honest option on the table now that the cause is known.

## Phase 2 SHIPPED — a stair-aware step limit

User call, 2026-08-22: teach nav about stairs rather than move the author's geometry.

`nav::STAIR_STEP = 2` cells (0.5 m) applies **only where both ends of a move are stair
cells**; everywhere else `MAX_STEP` stays 1. `bake` now takes a third argument, the stair
volumes (each region's own `StairDesc` treads are found and tagged automatically; the
caller passes the free-standing stair-run boxes, hence
`World::stair_run_solid_boxes()`), and tags their cells — widened one cell in XZ and
`AGENT_HEIGHT_CELLS` upward, because the cells that matter are the standable ones *on* the
flight, not the ones inside the solid.

Two things this deliberately is **not**: a global `MAX_STEP = 2` (a hunter beside a
staircase gains nothing — both ends must be stair cells), and a change to what is solid
(LOS, clearance and collision queries are untouched).

The refactor that came with it matters as much: **`NavWorld::can_step` is now the single
definition of nav adjacency**, called by `label_components`, `find_path`, `free_run` and
`overclimb_edges`. Those were separate copies of the same rule, and the O(1) unreachable
refusal in `find_path` is only sound while labelling and search agree exactly — the stair
step would have given them a third thing to drift on. `can_step` also bounds-checks (see
trap 2 above) and, for a 2-cell climb, checks *both* intervening cells of the agent's own
column rather than only the top one.

### Result on `levels/slot1.json`

```
before:  4 components, 15% of the floor cut off, 9 orphaned objects, 2 pads dropped at G
after:   1 walkable component — all 22686 cells connect
         0 orphaned objects, all 10 pads in the pool
         0 A* failures over 10 s of sim (the failures were the 10 fps)
         bake 553 ms (unchanged), validation 51 ms, sim 0.47 ms/step
```

Same outcome the handoff measured for a global `MAX_STEP = 2`, reached without giving every
hunter half a metre of climb everywhere. The island-pad exclusion in `prepare_spawn` is now
dead code *on this level* and stays as a guard.

### A correction: the pinch count was almost entirely false

An earlier version of this document reported "1,423 cells in corridors under 0.73 m,
narrowest 0.25 m" and argued that it pulled against loosening the grid. It did not, because
it was not real. `free_run` measured the free span at a **fixed height**, and a stair tread
is one cell deep with the next tread one cell up — so every step of every flight read as a
0.25 m corridor. With the span following the walkable surface (`can_step`), the count on
`slot1` is **0**. There is no corridor-width problem on this level, and the "tight
hallways" half of the original report was the same severed-staircase bug all along.

### Still open

- **`profile_hunt` bakes without props.** `prop_solid_boxes` needs registered GLB bounds,
  which the headless binary has none of, so its nav is missing every crate. In-game the
  same level reported 9 components where the headless run said 4 — the extra five were
  9–20 cell pockets **walled off by placed props**. The panel is the authority; the
  headless report under-states.
- Volume-sampling the cell (instead of its centre) is still the general fix for thin
  floors and misaligned geometry. Nothing currently demands it.
- Off-mesh links on the existing grid, if jumps ever need to be authorable
  (`ai-navmesh-attempt-reverted`).

## Traps that still apply

- **Do not make the grid more permissive than the hunter's body.** See above.
- **"I can get there" does not prove a hunter can.** The player jumps 0.76 m and hunters
  cannot jump at all, so some islands may be legitimately unwalkable. Over-climb edges are
  reported as *player-only* rather than as errors for that reason.
- **An open room cannot test any of this.** Connectivity and line of sight both need
  interior geometry; `nav_issues`'s own fixtures stack `Platform`s to make a severed ledge,
  and `partitioned_room` in the spawn_point tests is the LOS pattern.
- **`prepare_spawn` receives `nav` as a parameter and `self.nav` is `None` there** — the
  caller has taken the field.
- **Component labelling deliberately ignores doors.** A door can only *remove*
  connectivity, so a cross-component refusal in `find_path` is always sound. If phase 2
  makes doors affect connectivity, that O(1) refusal stops being sound — and it is what
  stopped a 10 fps playtest.
- **The nav overlay is the one coloured channel not rebuilt per frame.** ~90k vertices;
  `World::nav_overlay_rev()` gates the upload. A toggle that forgets to bump it shows a
  stale overlay (there is a test).
