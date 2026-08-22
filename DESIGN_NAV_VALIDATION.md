# Nav validation — a BUILD tool that shouts about unwalkable geometry

> Brainstorm, 2026-08-21, off the back of the 10 fps playtest. Nothing here is built.
> The measurements are real; the design is a menu of options, not a decision.
>
> Prompted by the right observation: *"I can reach all the spawn points when I run through
> the level, but the enemies stop moving on the radar in steep stairs and tight hallways —
> the nav mesh they use might have different accessibility than I do."*

## The problem, measured

On the shipping level (`levels/slot1.json`):

```
4 walkable component(s), 22686 standable cells:
  comp 2: 19198 cells (84.6%)   ← every spawn pad
  comp 4:  2076 cells (9.2%)    ← nothing
  comp 1:  1380 cells (6.1%)    ← nothing
  comp 3:    32 cells (0.1%)
```

**15% of the walkable floor is unreachable from the other 85%**, and the two big islands
are 1,380 and 2,076 cells — whole areas, not quantization slivers. A hunter that spawns or
wanders into one is out of the round. Worse, until this week, anything *wanting* to reach
one burned a full-region A\* every 0.4 s (that was the 10 fps).

The current mitigation — dropping island spawn pads from the pool — treats the symptom, and
does it by silently discarding pads the author placed on purpose. It is a floor, not a fix.

## Why the two mobility models disagree

This is the heart of it, and it is not a bug so much as two independently-correct models
that were never reconciled. The player moves by rapier collide-and-slide; hunters move by
grid A\*. Their capabilities:

| | player | hunter (nav grid) |
| --- | --- | --- |
| step up | 0.25 m (`autostep.max_height`) | 0.25 m (`MAX_STEP` = 1 cell) |
| step down | 0.25 m snap, **then falls any distance** | 0.25 m — a bigger drop is a **wall** |
| jump | **0.76 m apex** (`JUMP_VELOCITY` 5.5, `GRAVITY` 20) | none |
| slope | **50°** (`max_slope_climb_angle`) | ~45° equivalent, and only if every intermediate cell is standable |
| body | capsule r=0.25 m, h=1.5 m, slides along walls | 0.25 m cell centres, 4-connected, plus a wall-clearance nudge of ~0.24 m |
| geometry test | true collision against the mesh | is the **cell centre** inside a brush |

So "I can walk there" genuinely does not imply "a hunter can walk there". Four independent
ways to build a one-way route without noticing:

1. **A drop.** Anything over 0.25 m: you walk off it, hunters see a cliff. Two-way for you
   if it is under 0.76 m, because you can jump back up.
2. **A jump-up.** Between 0.25 m and 0.76 m you hop it; hunters have no jump at all.
3. **A steep stair or ramp.** Over ~45° per tread the grid cannot climb it. And on a smooth
   ramp the cells whose *centre* falls inside the slope read as solid, so the standable strip
   can thin or vanish entirely — the tool has to be able to say which.
4. **A thin floor.** A cell is standable only if the cell *below its centre* is solid. A
   platform thinner than 0.25 m, or one whose surface sits between cell centres, can produce
   **no solid cell at all** — an invisible hole in the walkable floor over solid-looking
   geometry. Prime suspect for the reported stairs.

And one that is not about connectivity at all, which the report must treat separately:

5. **A tight hallway.** The grid is 4-connected over 0.25 m cells, so a corridor two cells
   (0.5 m) wide is a legal path — but the hunter body is ~0.5 m across and the wall-clearance
   nudge tries to push it 0.24 m off each wall. It cannot satisfy both, so it stalls
   *mid-route* rather than never starting. That is the other half of "they stop moving": not
   "no path" but "path it cannot physically walk".

## What the tool should report

Two classes, and they want different presentation because they have different fixes.

### A. Connectivity — "nobody can get there"

- **Component summary.** Count, cell counts, % of floor, which is the main one. One line is
  already enough to know you have a problem (`nav_component_report()` prints this today).
- **The 3D overlay.** The main component in green, each island in its own colour, drawn over
  the level in BUILD. This is the feature that actually fixes levels, because the shape of
  the disconnect is instantly legible and no list of coordinates ever is.
- **Per-island: the nearest gap.** For each island, the closest cell-pair between it and the
  main component, with the **horizontal gap and the height difference**. That single number
  is the whole diagnosis: `island 2 is 0.35 m too high at (12.4, 23.8, -4.2)` says "raise
  `MAX_STEP` or add a step" and `island 3 is 4.2 m away across a pit` says "you meant that,
  build a bridge". Also gives the author somewhere to fly to.
- **Orphaned entities.** Spawn pads, weapon/ammo pickups, doors, turrets that are not in the
  main component. A gun nobody can fetch is a level bug the pickups feature made possible.

### B. Traversability — "they can path it but not walk it"

- **Pinch points.** Runs of walkable cells whose free width is under the hunter's clearance
  diameter. Mark them; they are where hunters grind.
- **Over-climb edges.** Adjacent floor-height deltas in the band the player can do and the
  hunter cannot (0.25 m to ~0.8 m). Label them as what they are: *one-way for the player*.
  This is also the honest answer to "but I can get there" — sometimes the level is wrong and
  sometimes the *hunter* is, and this list is where that argument gets settled.
- **Door clearance.** A door whose swing needs more standoff than the corridor has.

## Where it lives

- A **NAV tab in the `O` panel**, with an explicit **Calculate** button. The bake is ~520 ms
  on this level, far too slow to run on every edit — and that cost is exactly why this is a
  button and not a live gizmo.
- The overlay is a toggle, independent of Calculate, so you can leave it on while you edit
  and re-Calculate to see the effect.
- Plus a one-line summary at `G` in the log (partly there now: component count, ignored pads,
  the too-few-pads warning). The panel is for fixing; the log line is for noticing.

## The other half: should the nav grid change?

Reporting is phase 1 because it is safe and it tells us which of these is actually worth
doing. Options, roughly by cost:

- **Raise `MAX_STEP` to 2 cells (0.5 m).** One constant — and **measured: it reconnects the
  whole level.** 4 components → 1, 22,686 cells, all 10 pads (3 cells changes nothing
  further, so every island is behind exactly one 0.5 m climb). Risks: hunters "climbing"
  0.5 m without an animation reads as gliding, and 0.5 m is *twice* the player's autostep,
  so hunters would out-climb the player. Whether that is a cheat or a fix depends on whether
  the 0.5 m gaps are real steps or a **quantized ramp** (see the handoff) — on a slope,
  consecutive standable cells can sit 2 cells apart because the cell centre between them
  falls inside the ramp and reads as solid, in which case the geometry is continuously
  walkable and the grid is simply wrong.
- **Let descent be free.** Nav already treats up and down symmetrically; allowing any drop
  matches the player (who falls) and makes one-way routes one-way for hunters too. Needs the
  fall to be *animated* or it reads as a glide, and it makes paths asymmetric — A\* would
  need directional costs.
- **Sample the cell volume, not the centre.** Fixes thin floors and misaligned geometry at a
  real bake-time cost. The most "correct" fix and the least visible one.
- **Off-mesh links on the existing grid** — the conclusion the reverted navmesh attempt
  already reached (see the `ai-navmesh-attempt-reverted` memory): author or auto-detect jump
  links between islands rather than rebuilding the nav representation. Most work, best
  ceiling.
- **Widen the corridor rule** instead of nudging bodies off walls: mark a cell walkable only
  if the hunter's radius fits, which turns pinch points into non-paths (honest) but shrinks
  the walkable set (and may sever routes that currently work).

## Deliberately out of scope

- Rebuilding nav as a real navmesh. Tried, reverted, and the reason is recorded: the grid
  A\* fallback only catches `None`, not a *bad* path, so the regression surfaced only on the
  real multi-region level.
- Auto-fixing geometry. The tool says where and why; the author decides.

## First thing to settle in the implementation context

Whether island spawn pads stay excluded. Right now they are, which is what stops the 10 fps
— but the author placed them and can walk to them, so the exclusion is silently overruling
a deliberate choice. If `MAX_STEP` or the thin-floor fix reconnects those areas, the
exclusion becomes dead code and should go. If they stay disconnected, it should probably
stay *and* say so in the panel rather than only in the log.
