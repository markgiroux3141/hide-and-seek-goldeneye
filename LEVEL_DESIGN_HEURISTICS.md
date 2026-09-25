# Level Design Heuristics (for the auto level-gen)

A running log of what makes a *good* multiplayer hide-and-seek level, captured as
we iterate on the headless level generator (`native/crates/game/src/levelgen/`).
The end goal is to distill this into a **Claude skill**: a set of heuristics an
agent follows to author coherent, fun levels, verified by the headless report
(floorplans + reachability + flow metrics).

Units: **WT** = world tile = 0.25 m. So 4 WT = 1 m, a "2 m ceiling" = 8 WT.

## Rules at a glance

The log below is append-only history; this table is the current state of every rule
in it. **Enforced** rules are checked by the levelgen report (`levelgen/analyze.rs`):
a *check* can fail the verdict, a *lint* (the report's "design rules" block) can only
warn. **Advice** rules are not machine-checkable yet. **Superseded** rules turned out
to be wrong — the entry that overturned them says why. When you add a rule, add its
row here; when the report starts enforcing one, name the check.

| Rule | From | Status |
| --- | --- | --- |
| Ceilings ≥ 12 WT; never 8 | 07-25 arena walk | Enforced — lint `ceilings` |
| Mix room sizes; no uniform grid | 07-25 arena walk | Enforced — lint `variety` |
| One or two hero rooms, 40–60 WT across, 24–30 WT tall | 07-25 slot-1 study | Enforced — lint `hero room` |
| Platforms ≥ 4 WT deep | 07-25 arena walk | Enforced — lint `decks` |
| Platforms lead somewhere (or are a perch with a view) | 07-25 arena walk | Enforced — lint `decks` |
| Stairs in large rooms, never small ones | 07-25 varied | Enforced — lint `stair space` |
| A texture scheme per room | 07-25 slot-1 study | Enforced — lint `textures` |
| Every room reachable; no accidental islands | 07-25 | Enforced — checks `walkable`, `reachable` |
| Loops, not spokes; few dead-ends (terminal vaults are fine) | 07-25 | Enforced — check `loops` (warns only at 0 loops) |
| ≥ 1 perch with a real overlook | 07-25 | Enforced — check `perches` |
| Headroom ≥ 8 WT on every walkable cell | 07-25 slot-1 study | Enforced — check `headroom` |
| A wall between rooms meant to be separate (≥ 1 WT) | 09-24 | Enforced — check `merged rooms` |
| Pillars ≥ 3 WT from walls, stairs and doors (or flush) | 09-25 compound | Enforced — check `pinches` |
| Passages overlap both rooms by ≥ 2 WT | 07-25 varied | Built in — `door`, `corridor` |
| Anything solid added after the carves | 07-25 varied | Built in — the builder defers pillars |
| A floor hole covers its whole stair, with headroom | 07-26 | Built in — `stair_through_floor` |
| A stair must lead somewhere at its far level | 07-25 sprawl | Built in — `stair_between` |
| Split levels (pits, mezzanines, catwalks) read as handcrafted | 07-25 slot-1 study | Advice |
| Cover pillars to break long sightlines | 07-25 | Advice |
| Route a perch's stair along a wall so it keeps its view | 07-25 varied | Advice |
| Sprawl as chains branching off each other, not a hub | 07-25 2nd walk | Advice |
| A second route up/down each vertical, for flanking | 07-25 | Built in — the generator's `second_route` (most levels); advice for hand designs |
| Pillars ≥ 4 WT (1 m) from flights, holes, decks and each other | 09-25 generator | Built in — generator `PILLAR_GAP`; enforced — check `pinches` |
| Free-standing stairs down are player-only | 07-25 / 07-26 | **Superseded 09-24** — a bug (`find_floor_y_at`), fixed |
| Cantilever perches; don't hug the wall | 07-25 | **Superseded 09-24** — a perch-metric artefact |

---

## Player feedback log (chronological — the source of truth)

**2026-07-25 — first walk of the `arena` (3×3 grid) level:**
- Everything connects up — good.
- ✋ Rooms are **too small and all the same size**. Want **variety**: some
  smaller, some larger, some **long & skinny**, etc.
- ✋ **Ceilings are too low.** A room ceiling should "probably never be this low"
  (the arena used 8 WT = 2 m). Rooms want to feel roomy.
- ✋ **Stairs feel cramped** — a symptom of the small rooms. Give stairs space.
- ✋ The **overlook platform looks half as wide as it should be and goes nowhere**
  (it was a 2-WT-deep sliver against a wall). Platforms must be walkable-wide and
  must **lead somewhere**.
- 💡 **Platform → door → second floor**: put a landing platform at the top of a
  stair, and a **doorway at the top that leads onto a proper second floor**. The
  platform doubles as a stair landing + an overlook, not a dead-end sliver.
- 💡 General: **more variety, get creative.**

---

## Heuristics so far (derived from the above)

### Rooms & scale
- **Ceiling height:** minimum ~12 WT (3 m); big feature rooms 16–22 WT (4–5.5 m).
  Never 8 WT. A tall central atrium (20+ WT) that spans two floors reads great.
- **Room-size variety:** mix footprints deliberately in one level —
  - small (~8×8) closets / camp spots,
  - medium (~14×16) fight rooms,
  - large (~24×24) arenas / warehouses,
  - long-skinny (~6×28) galleries / sightline corridors.
  Avoid a uniform grid of identical boxes.
- **Aspect variety** matters as much as size — a long skinny room plays totally
  differently (sniper lane) than a square one (brawl).

### Verticality & circulation
- **Give stairs room to breathe:** a stair needs ~rise-in-WT of horizontal run
  (1:1 slope for nav) *plus* clearance around it; don't wedge it into a small
  room. Put the main stair in a large space (atrium/hall).
- **Platforms must lead somewhere.** A good platform is a **stair landing that
  connects to a second-floor room via a doorway**, and doubles as an overlook.
  A slab that only overlooks and dead-ends feels pointless.
- **Platform width:** walkable and generous — ≥4 WT deep (≥1 m), usually more.
  A 2-WT sliver looks broken.
- **Real second floor:** carve upper rooms (floor at the upper Y) and connect the
  stair/landing into them with a doorway, so the upstairs is a place you go, not
  just a perch.

### Flow (multiplayer)
- **Loops, not trees:** every room wants ≥2 connections so there are multiple
  routes; minimize dead-ends (a deliberate sniper nest is the exception).
- **Overlooks:** at least one perch with real sightlines down into a room below
  (the "snipe from above" fantasy). Verify with the LOS metric.
- **Cover:** pillars / short walls to break long sightlines and make camp corners.

### Verification (what the report must show green)
- All rooms reachable from spawn; upper floors reachable.
- Independent loops > 0; dead-ends ≈ 0 (perches excepted).
- ≥1 perch with a decent sightline count into a lower room.
- No accidental sealed voids or unreachable levels.

---

## Build-loop learnings (from generating the `varied` level)

- **A perch is blinded by its own stair.** If the stair rises through the space
  the landing overlooks, LOS is blocked. Route the stair *along a wall* to one
  side, so the landing keeps a clear line over the room. (First `varied` build:
  landing saw 0 atrium cells; moving the stair to the east wall gave it a real
  sightline over the warehouse approach instead.)
- **Pillar tops read as "unreachable" standable cells.** A solid pillar (Op::Add)
  up to y=8 makes a standable cell on top that no stair reaches — the report
  flags that floor level as "not reachable," which is *correct* (you can't climb
  it). Don't panic at a lone unreachable level if it's a pillar top.
- **Spokes make dead-ends; add perimeter links.** A hub (atrium) with one door to
  each room = every room degree 1. Add room↔room links around the perimeter to
  turn spokes into loops. Target: loops ≥ 3, dead-ends ≤ 2 for a mid-size map.
- **Stairs eat floor.** A stair's footprint (+ the low-clearance zone under its
  top) is non-standable floor. Put stairs in a large room so that lost strip
  doesn't matter; never in a small one.
- **Passages: overlap both rooms by ≥2 WT.** Generous connecting boxes bake into
  robust nav; a passage that only just touches a wall can fail to connect.
- **Vertical connection that works:** stair (1:1 slope, rise≈run) → landing
  platform (≥8 WT deep) at the upper floor's Y → doorway carved through the wall
  at that Y into the upper room. The landing doubles as the overlook.

### Known limitations / next iterations
- **Second floor is single-access** (one stair up). Add a second route upstairs
  (a drop-down hole, a back stair to another room) so the upper floor isn't a
  dead-end and supports flanking.
- **No windows / interior openings** yet for cross-room sightlines mid-wall.
- Room *shapes* are all rectangular; L-shapes / diagonals would add character
  (would need multiple carves per room).

## 2026-07-25 (2nd walk) — feedback on the `varied` level

- ✋ **Pillars didn't reach the ceiling** (stubs). Cause: pillars are additive CSG,
  so a room *subtract* carved later ate their tops, and I gave them a fixed height
  instead of floor→ceiling. **Fix applied:** the builder now defers pillars to the
  very end (appended after all subtracts, so nothing carves them) and `pillar_in`
  spans the room's full floor→ceiling. Also: keep pillars clear of stairs.
- ✋ **Still reads as "central room + a few rooms."** Want **sprawl** — the human
  process: make a room, carve a door, run a hallway to another room, then go back
  to the main room and start *another* sequential chain elsewhere.
- 💡 **Use the CSG stair tool** (up/down arrows on a selected wall) — never used
  it. Plus **partial wall selection** for hallways that change direction / go up
  or down a flight.

### Applied in the `sprawl` design
- **Sprawl pattern:** author as *chains*, not spokes — foyer→hub→atrium running
  east (~110 WT across), then branch new chains north (nest), west (west_wing),
  south (cellar). A couple of loops (parallel halls; an L-corridor closing a SW
  loop). Feels rambling, not radial.
- **CSG stair authored in the builder** (`csg_stair`): replicates the editor's
  confirm — two subtract voids (stairwell + 1-WT destination corridor) + a
  `StairDesc` whose treads bake into nav. Cut into a wall, climbs a flight to a
  mezzanine at a higher floor; carve a room at the destination level so it *leads
  somewhere*. Verified walkable (mezzanine reachable, floors 0–6 continuous).
- **Direction-changing hallway** = two carved boxes meeting at a right angle (the
  west_wing→cellar L-corridor). Partial-wall stairs (up/down mid-wall) are the
  `csg_stair` primitive.

### New heuristics
- **Additive-before-subtractive gets eaten.** Anything solid you *add* (pillars,
  columns) must come after every carve, or a later subtract removes it. Build
  order: carve rooms/halls/stairs first, add solids last.
- **CSG stair vs free-standing stair:** CSG stair = a stairwell *cut into a wall*
  that connects two levels through the wall (great for "hallway goes up a flight").
  Free-standing stair/platform = a slab+steps sitting *in* a room's air (great for
  balconies/overlooks). Use CSG for level-to-level circulation, free-standing for
  perches.
- **A stair must lead somewhere:** always carve a room/hallway at the stair's
  destination level, flush with its exit, or the climb dead-ends into a wall.

## 2026-07-25 (3rd walk) — the `sprawl` level: "excellent"

Player: "excellent... Let's go for something much larger, use everything we've
learned and create a full level." → built the `facility` design.

### The `facility` level (the "everything" build)
- **13 rooms, 3 floors** (basement y=−8, ground y=0, upper y=8–10), ~118×94 WT.
- Four sprawling wings off a huge tall atrium (40 WT ceiling), varied sizes
  (huge atrium, large armory/east_hall, medium mess/barracks, small
  closets/bunks, a long-skinny gallery).
- **Every primitive in one map:** CSG stair *up* a flight (→ upper_east), CSG
  stair *down* a flight (→ basement), a free-standing grand stair + mezzanine
  **perch**, full-height pillars, multiple loops, camp nooks.
- All 13 reachable; every floor level y−8…y10 continuous; 3 loops.

### New learnings
- **The degree metric undercounts `void` corridors.** A hallway carved with
  `void()` (no recorded edge) still connects nav for real, so a room can show
  "dead-end" in the graph yet be genuinely looped in play. Reachability is the
  truth; treat the degree/dead-end count as *intended-topology* shorthand, and
  prefer `passage(a,b,…)` / `link(a,b)` over `void()` when you want the edge
  counted.
- *(⚠ SUPERSEDED 2026-09-24: a perch-metric artefact — see "the report was measuring the wrong things".)* **A high central perch overlooks corridors better than the floor below it.**
  The mezzanine saw only ~14/255 atrium-floor cells (its own slab + pillars block
  straight-down LOS) but strongly covered the barracks/mess/bunk approaches
  *through the hallways* at eye height. Perches are corridor-watchers; for a true
  floor overlook, cantilever the platform out over the room and keep cover away
  from the sightline.
- **Terminal rooms are fine at scale.** On a big level, closets/bunks/basement as
  single-access destinations read as realistic, not broken — don't force every
  room into a loop.

### Next iterations (queued)
- Upper-floor **catwalks/loops** (bridge upper_east ↔ mezzanine) so the second
  floor isn't a set of separate single-access rooms.
- A **second route** up/down each vertical (a back stair) for flanking.
- Room **shape variety** (L-shaped rooms = 2+ carves) and interior windows.

## 2026-07-25 — studied the player's handcrafted `slot1.json`

The single best calibration input so far. What their level does that mine didn't:
- **Scale.** Their hero hall is **59×57×27 WT** (~15×14 m, ~7 m ceiling); another
  40×36×28. Mine topped out ~34×30. Rooms should be *much* bigger — push the
  shell way out. One or two huge hero rooms per level.
- **Split-level floors within one room** — a sunken pit (−31) inside a −23 hall, a
  raised −24 shelf; a room half at y0 half at y−8. Multiple floor heights in one
  connected space (served by stairs). I only made flat floors.
- **Per-room texture schemes** (they used 0/1/2). Visual identity per room. I
  defaulted everything to 0 → monotonous.
- **Long thin platforms as catwalks/bridges** (59×4, 4×53) spanning big halls, not
  just small perches.
- **Windows / floor-holes** (frame carves) for cross-room and cross-*floor*
  sightlines.
- **Head-bump bug:** their corridors/stairs are 7 WT tall; going down a stair the
  top has only ~7 WT clearance and the camera clips. Corridors/stairwells want
  **>= 8 WT** of headroom.

### Applied in the `showcase` design + new builder/analyzer features
- `set_scheme(n)` — "pick a texture, then build"; carves/pillars inherit it.
  Valid room schemes 0..=8 (9 is the platform style). Showcase uses 5 schemes.
- `window(x, sill, z, w, h, d)` — a **frame** opening at an explicit vertical band;
  above the floor it's a see/shoot-through window (nav won't route it), giving a
  sniper sightline between rooms.
- **Layered verticality in one room:** a big mezzanine balcony + a catwalk bridge
  (long thin platform) over the hero hall, reached by a grand stair. The perch now
  sees 102/396 hall-floor cells (vs the facility mezzanine's 14) — cantilevering a
  wide deck over the room *does* give a real floor overlook.
- **Headroom lint** (analyzer): flags any walkable cell with < 8 WT clearance —
  would have caught the slot-1 head-bump. Set CSG down-stair `ceil` >= floor + 8
  and it reads clean.

### Still to steal from their level (next)
- **Split-level pits/shelves** inside big rooms (a `pit`/`dais` builder helper +
  a stair down into it).
- **Floor-holes / skylights** between stacked rooms (window with shallow `d` on a
  horizontal plane).
- Even bigger — a 50–60 WT hero hall like their 59×57.

## 2026-07-25 — bigger + split-level (`grand` design)

Pushed the two queued items: go bigger, and add sunken pits.
- **Hero hall 56×48×30 WT** (14×12 m, 7.5 m ceiling) — closer to the player's
  59×57. Three real levels inside one room: **pit (−4) / main (0) / mezzanine +
  catwalk (+14)**, the catwalk bridging *over* the pit.
- `pit(x,z,w,d, room_floor, depth)` helper — sinks a floor section (a split-level
  within one room).

### Hard-won stair learnings (cost several iterations)
> **⚠ SUPERSEDED 2026-09-24** — this was a bug, not a nav limit; see the 2026-09-24 entry below.
- **Free-standing ground-to-ground `stair_ground` does NOT bake walkable nav** in
  my tests — the pit floor came back 0/246 cells reachable no matter the depth or
  grounding. Every stair that *works* in my levels is either a `csg_stair` or a
  `stair_to_platform`. **Use a CSG down-stair to enter a pit** (cut it into the
  pit rim before opening the pit) — reliable, since it's the same primitive the
  basement uses. (Free-standing ground stairs need a separate debug pass.)
- **A stair-run's lowest tread lands one step ABOVE its ground anchor** (off-by-one
  to remember if we ever fix `stair_ground`).
- **A descent stair must run inside the low area's footprint**, or its upper steps
  bury in the solid main floor.

### Analyzer upgrades this round
- **Headroom lint** — flags walkable cells with < 8 WT clearance (would catch the
  slot-1 head-bump). CSG down-stair `ceil` >= floor + 8 reads clean.
- **Per-room texture schemes** (`set_scheme`) + a **`window`** primitive (raised
  frame opening = see/shoot-through sightline).
- **Robust verticality** — samples up to 16 cells per floor level and reports
  reachable/total, instead of one possibly-stray cell (this is what finally proved
  the pit was disconnected).

### Still open
- ~~Debug `stair_ground` (ground-to-ground) so open pits can use a free-standing
  stair instead of a CSG cut.~~ Fixed 2026-09-24.
- *(⚠ SUPERSEDED 2026-09-24: a perch-metric artefact — see "the report was measuring the wrong things".)* Perch-over-own-room: a big edge-hugging mezzanine overlooks *adjacent rooms*
  well but not the floor beneath it; cantilever a narrower deck out over the room
  for a true floor overlook.

## 2026-07-26 — pit descent: free-standing vs CSG (resolved)

Playtest of the `grand` pit exposed a real split between **player physics** and
**enemy grid-nav**:
- **CSG down-stair** into an open pit: enemy-nav reaches the pit floor (16/16), BUT
  a Down stair always renders a "far-end fill" wall (`ceil-sc`..`ceil`) to close
  its stairwell — in an OPEN pit that wall floats/juts as a stray brown chunk.
  Lowering `ceil` only sinks it into the pit as a back-riser; can't remove it.
- **Free-standing stair** (blue platform stair) into the pit: renders clean, and
  the **player walks down/up fine** (physics collider + autostep). But **enemy
  grid-nav can reach the stair treads yet NOT the carved pit floor** (0/16),
  across ~6 configs (ground/platform anchor, integer 1:1, flush lowest tread,
  grounded on/off). The grid nav won't make the last hop from a free-standing
  tread onto the adjacent carved-floor cell.
- **Decision:** shipped the free-standing stair — it's what reads clean, the
  player traverses it, and for hide-and-seek a pit the seekers can't fully search
  is a legit hiding spot. The pit floor being off the nav grid is accepted.

> **⚠ SUPERSEDED 2026-09-24** — this was a bug, not a nav limit; see the 2026-09-24 entry below.

Follow-ups:
- **Open bug to fix:** free-standing descending-stair → carved-floor nav hop. Fix
  it and open pits get clean stairs AND full enemy access. Likely in
  `sim/nav.rs` standability/step-check or `structures::stair_run_boxes`.
- **Analyzer:** room-reachability now samples a 3×3 grid over the footprint and
  prefers a spawn-reachable point, so a room whose center sits in a pit/hole isn't
  falsely flagged "UNREACHABLE".

## 2026-07-26 — extending `grand`: upper wing great, undercroft hole botched

- ✅ **Upper wing works and looks great:** mezzanine (y14) → **door** in the north
  wall → **loft** → **CSG up-stair** → **attic** (y22). Ascending + wall-cut stairs
  are reliable and clean. Reuse this pattern freely.
- ✋ **The "hole in the floor + platform stair down" was broken** (undercroft
  unreachable AND ugly). Root causes, all fixable:
  1. **Hole too small** — a floor hole must be *wide enough to walk through and to
     fit the whole stair*, not a little slot. Size it to the stair footprint + a
     landing.
  2. **Hole edge not connected to the stair** — the top of the stair must meet the
     hole's rim at the upper floor level so you can step onto it. There was a gap.
  3. **No headroom to descend** — the slab you cut through + the room below must
     leave ≥ 8 WT of clearance over every tread, or you bump going down.
  4. Plus the standing limitation: **free-standing stairs down don't bake into
     enemy nav** (player-only). For an enemy-searchable lower room, a wall-cut CSG
     down-stair is currently the only nav-clean option.

### Rule for a floor-hole + downstair (until the nav hop is fixed)
> **⚠ SUPERSEDED 2026-09-24** — the last bullet's "accept player-only" was a bug; the geometry rules still hold; see the 2026-09-24 entry below.
- Make the hole ≥ the stair width + 2 WT on each side, and long enough for the
  full run.
- Put the stair's top tread flush with the hole rim at the upper floor.
- Give ≥ 8 WT vertical clearance the whole way down.
- Accept enemy-nav-unreachable (player-only) OR use a CSG down-stair cut into a
  real wall of the lower room so the closing-wall hides.

## 2026-09-24 — "stairs down don't bake for enemies" was a bug

The rule above (07-25/07-26: free-standing stairs down are player-only, "confirmed
across ~6 configs", blamed on a nav "last hop") was wrong. **Cause:** a grounded
platform-style stair-run floors its steps with `structures::find_floor_y_at`, which
looks for a floor *strictly below* the foot and **falls back to 0.0**. For any
flight whose foot is below y=0, that 0.0 is *above* every step, every step came out
with negative height, and `stair_run_boxes` returned **nothing** — no nav boxes at
all. The player walked it anyway because their collider is the ramp quad, which
never reads the floor. The six configs all shared the one input that mattered.

- **Fixed** in `structures::resolve_run` (clamp the floor to the flight's foot; runs
  that baked before bake identically). Regression test:
  `a_platform_stair_down_into_a_pit_bakes_walkable_nav`.
- `grand`'s pit is now enemy-reachable. The **undercroft is still cut off — that
  one is real geometry**: the stair runs on past the floor hole under the armory
  slab with ~0.5 m of headroom (the 07-26 "botched hole" diagnosis, items 1–3).
- It had also silently broken a hand-built level: `aztec_level` run 3 (platform at
  y=0 down to y=−20) was hunter-proof; it now joins two islands into one.

**Lesson for this log:** a limitation found by black-box trial and error is a
hypothesis, not a rule. Before writing one down, read the code that produces the
geometry (here, five lines of `stair_run_boxes`) or probe it directly.

## 2026-09-24 — the report was measuring the wrong things (analyzer rewrite)

The levelgen report was rebuilt (summary first, JSON, a derived room graph). Three of
its old numbers had been steering the rules above:

- **Perches.** The old check sighted from 0.4 m above the *centre* of the deck — a
  metre under the player's eye, from the one spot its own slab hides the floor below
  best. That is why a wide mezzanine "saw only 14 atrium cells" and why the log
  concluded perches must be cantilevered and must not hug the wall. From the edge at
  eye height, `grand`'s wall-hugging 56×12 mezzanine sees **69%** of the hall and
  `facility`'s sees 83% of the atrium. **Superseded:** "a high central perch
  overlooks corridors better than the floor below it" and "cantilever the platform
  out over the room" (07-25) — hugging the wall is fine; what matters is an edge.
- **Loops.** Counted from declared edges, and then (briefly) from a room-to-room graph
  that collapsed parallel halls into one edge. Counted now on the walkable graph with
  corridors as their own nodes, `sprawl` has the 2 loops it was designed with.
- **Floorless decks look "reachable".** `smoke`'s perch sits 5 WT up in an 8 WT room
  — 3 WT of headroom, so not one standable cell — and the old check passed it by
  snapping to a stair tread nearby. It is now its own finding ("no standable floor").

New check, first catch: **merged rooms**. `grand`'s north_loft is carved flush against
the hall (its air starts at z=0 where the hall's ends), so there is no wall between
them — the 6-wide "door off the deck" the design declared is actually the whole wall.
**Rule:** keep ≥ 1 WT of solid between rooms that should be separate; connect them
with a `passage`.

## 2026-09-25 — first walks of generated levels (gen-10, gen-125, gen-28, gen-212)

- ✅ Single-floor and three-floor generated levels play: "a good start". The balcony →
  door → upper floor → flight back down loop, and the basement's two ways in, work for
  the player and for hunters.
- ✋ **Textures need work.** The generator gives each room an independent random scheme
  (0..8), with no theming by role, wing or floor. Not a priority yet. When it is, the
  obvious next rule is *per-wing or per-floor theming* rather than per-room noise —
  and a lint to go with it.

## Toward a Claude skill
Eventually package the above as a `level-design` skill: the checklist + the WT
conventions + "author with the builder, run the report, fix until green" loop,
so any agent can produce and self-verify a level. Keep appending player feedback
here; that log is the raw material.
