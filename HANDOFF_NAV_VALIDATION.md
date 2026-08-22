# Handoff — a BUILD tool that shouts about unwalkable geometry

> Written 2026-08-21 at the end of the pickups/spawn playtest session. **Nothing in the
> tool is built.** The diagnosis is measured, not guessed; the design brainstorm is
> `DESIGN_NAV_VALIDATION.md` and this is the working script for building it.
>
> Companion memories: `nav-vs-player-mobility`, `perf-diagnostic-tools`,
> `spawn-points-respawn-pivot`, `ai-navmesh-attempt-reverted`.

## The report

> "I can reach all the spawn points when I run through the level and there are no spawn
> points on rooftops. I do notice however that there are certain parts of the map with
> steep stairs, tight hallways where the enemies stop moving on the radar which means they
> might not have a route to me. This tells me the nav mesh they are using might have
> different accessibility than I do."

That reading is correct, and it is now measured.

## What is confirmed

`levels/slot1.json`, via `World::nav_component_report()` (`world/hunt.rs:1033`):

```
4 walkable component(s), 22686 standable cells:
  comp 2: 19198 cells (84.6%)   ← every spawn pad
  comp 4:  2076 cells (9.2%)    ← nothing
  comp 1:  1380 cells (6.1%)    ← nothing
  comp 3:    32 cells (0.1%)
```

**15% of the walkable floor is unreachable from the rest**, in two chunks large enough to
be rooms. The author can walk to all of it. So the grid is severing real routes, and the
job is to find out where and why.

### The mobility asymmetry, from the source

Two independently-correct models nobody reconciled. Player = rapier collide-and-slide
(`engine/src/sim/physics.rs:120-126`, `game/src/character.rs:20-25`); hunters = grid A\*
(`engine/src/sim/nav.rs:18-20`).

| | player | hunter |
| --- | --- | --- |
| step up | 0.25 m `autostep.max_height` | 0.25 m (`MAX_STEP` = 1 cell, `nav.rs:20`) |
| step down | 0.25 m snap, **then falls any distance** | 0.25 m — a bigger drop is a wall |
| jump | **0.76 m apex** (`JUMP_VELOCITY` 5.5 / `GRAVITY` 20) | none |
| slope | **50°** `max_slope_climb_angle` | ~45°, and every intermediate cell must be standable |
| body | capsule r 0.25 m, h 1.5 m, slides on walls | 0.25 m cells, 4-connected, + a ~0.24 m wall-clearance nudge |
| geometry | true mesh collision | **is the cell centre inside a brush** (`nav.rs` bake loop, ~`:750`) |

Four ways to make a one-way route by accident — a drop over 0.25 m, a jump-up in
0.25–0.76 m, a stair/ramp over ~45°, and a **floor thinner than 0.25 m** (`is_standable`,
`nav.rs:386`, needs the cell *below the centre* solid → an invisible hole over
solid-looking geometry; the prime suspect for "steep stairs").

**And one that is not connectivity at all.** A 2-cell (0.5 m) corridor is a legal
4-connected path, but the hunter body is ~0.5 m wide and `wall_clearance_offset`
(`nav.rs:572`) pushes it 0.24 m off each wall. It cannot satisfy both, so it stalls
**mid-route**. That is the "tight hallways" half of the report and it needs its own
detector — "no path" and "a path it cannot walk" are different bugs with different fixes.

## What is already built — do not rebuild it

- `NavWorld::label_components` (`nav.rs:321`) — flood-fills standable cells into connected
  components at bake, using A\*'s own adjacency but **ignoring doors** (a door can only
  *remove* connectivity, so a cross-component refusal is always sound).
- `component_at` (`:379`), `component_sizes` (`:364`) — the queries the panel needs.
- The **O(1) unreachable refusal** in `find_path` (`nav.rs:627`). Keep it: it is what stopped
  a 10 fps playtest (four hunters each running a doomed full-region A\* every 0.4 s).
- `nav::path_stats()` / `path_counts()` / `reset_path_stats()` — A\* calls / failures / cells
  expanded. **The failure count is the signal**; a failed search expands the whole region.
- `World::nav_component_report()` (`hunt.rs:1033`), `spawn_reachability_report()` (`:1065`),
  `hunter_report()` (`:1100`) — text versions of what the panel should draw.
- `cargo run --release --bin profile_hunt -- 1 20` — loads a real slot, enters HUNT, prints
  the step profile plus all three reports. This is the iteration loop; use it before the game.
- Island spawn pads are **excluded** from the pool at `prepare_spawn` (`hunt.rs:930`), with a
  per-pad warning. See the open decision below.

## Suggested order

**Phase 1 — report, no behaviour change.** Safe, and it tells us which phase-2 fix is worth
doing.

1. `nav_issues()` on `World`, returning structured findings rather than a string: component
   summary; per-island **nearest gap** to the main component (closest cell pair, with the
   horizontal distance *and* the height delta); orphaned entities (pads, pickups, doors,
   turrets not in the main component); pinch points; over-climb edges (adjacent floor deltas
   in 0.25–0.8 m, i.e. player-only). The nearest-gap number is the whole diagnosis — "0.35 m
   too high at (12.4, 23.8, -4.2)" says fix the step limit, "4.2 m across a pit" says build
   a bridge.
2. A **NAV tab** in the `O` panel. Add a `PanelTab::Nav` variant (`app.rs:56`, and to
   `ALL`/`title` at `:64`/`:71`), then a body arm beside `PanelTab::Spawns =>`
   (`app.rs:1313`). It needs an explicit **Calculate** button: the bake is ~520 ms on this
   level, far too slow to run per edit.
3. The **3D overlay** — main component green, each island its own colour. This is the part
   that actually fixes levels; a list of coordinates never does. Follow the existing
   coloured-mesh channel rather than inventing one: `World::spawn_marker_mesh`
   (`tools/spawn_point.rs:168`) → `Renderer::set_marker_mesh`
   (`render/renderer.rs:2884`). A second channel of the same shape is the cheap path.
   `NavWorld::all_standable()` (`nav.rs:512`) already hands back the cells.

**Phase 2 — make the grid agree with the player.** Options, cheapest first, all listed in
the design doc with their risks: raise `MAX_STEP` to 2 cells; let descent be free; sample the
cell *volume* instead of its centre; off-mesh links (where the reverted navmesh attempt
already concluded we should go). Measure with `profile_hunt` before and after — component
count and the orphan list are the acceptance criteria.

## Traps

- **Do not "fix" this by making the grid more permissive than the hunter's body.** The two
  reported symptoms pull in opposite directions: connectivity wants a looser grid, tight
  hallways want a stricter one. A single knob will trade one bug for the other.
- **"I can get there" does not prove a hunter can.** The player jumps 0.76 m and hunters
  cannot jump at all, so some islands may be legitimately unwalkable. The tool has to
  distinguish, not assume — that is why over-climb edges are reported as *player-only*
  rather than as errors.
- **An open room cannot test any of this.** Line-of-sight and connectivity both need interior
  geometry; see `partitioned_room` in the spawn_point tests for the pattern.
- **`prepare_spawn` receives `nav` as a parameter and `self.nav` is `None` there** — the
  caller has taken the field. The first version of the island filter read `self.nav` and
  silently did nothing.
- **The bake is already ~520 ms.** Anything phase 2 adds to it (volume sampling especially)
  lands on the `G` keypress, which is a real hitch the user will feel.
- Component labelling deliberately ignores doors. If phase 2 makes doors affect
  connectivity, the O(1) refusal in `find_path` stops being sound.

## Open decisions for the fresh context

1. **Do island spawn pads stay excluded?** They are today (commit `9132839`) and that is what
   stopped the 10 fps — but the author placed them and can walk to them, so the exclusion
   silently overrules a deliberate choice. If phase 2 reconnects those areas it becomes dead
   code. Options: keep it, downgrade to a loud warning that still spawns there, or keep it
   only for components under some size. **Ask before choosing.**
2. ~~**Is `MAX_STEP` = 1 cell the actual bug?**~~ **Measured before this handoff was filed —
   yes.** One constant, `nav.rs:20`:

   ```
   MAX_STEP = 1 (0.25 m):  4 components — 19198 / 2076 / 1380 / 32 cells, 8 of 10 pads usable
   MAX_STEP = 2 (0.50 m):  1 component  — 22686 cells (100%), all 10 pads
   MAX_STEP = 3 (0.75 m):  1 component  — no further change
   ```

   Every island is behind a **single 0.5 m climb**. Two readings, and the phase-1
   nearest-gap report is what distinguishes them:

   - **Real 0.5 m steps.** Then the *player* cannot walk up them either (autostep is
     0.25 m) — they are jumping without thinking about it — and raising `MAX_STEP` makes
     hunters *more* mobile than the player. Defensible, but state it out loud.
   - **A quantized ramp**, which is likelier for "steep stairs". On a slope, consecutive
     standable cells can sit 2 cells apart vertically because the cell centre *between*
     them falls inside the ramp and reads as solid. The geometry is continuously walkable
     at the player's 50° and the grid has stepped it into 0.5 m jumps. Here raising
     `MAX_STEP` is not a cheat, it is a **correction for a sampling artifact** — and
     volume sampling would fix it more honestly at bake cost.

   So: do not just raise the constant and declare victory. Get the nearest-gap readout,
   look at what is actually there, and decide which of those two it is — the answer changes
   whether the shipped fix is `MAX_STEP`, volume sampling, or both.
3. **What is the tool's verdict on a level with no issues?** A panel that says nothing is
   indistinguishable from a panel that is broken. It should say "1 component, 0 orphans" out
   loud.
