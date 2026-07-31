# Design — More Flexible CSG Building

> Companion to [DESIGN.md](DESIGN.md) and [DESIGN_IDEAS.md](DESIGN_IDEAS.md).
> Captures the 2026-07-31 brainstorm on the question:
> **how far could we push the CSG level-builder toward more flexible building
> (arbitrary angles, slopes, curves, etc.) — and what's actually worth it?**
> Thinking document, not a spec. Nothing here is committed. Grounded in a full
> read of the current CSG pipeline (see "Where the invariants live" below).

---

## TL;DR / recommendation

The fear was that flexibility is a can of worms because the current state was hard-won.
The map says the opposite of the naive intuition:

- The **CSG math already survives arbitrary angles** — the BSP kernel is fully general;
  the axis-locking lives *above* it (data model, UV classifier, tools).
- **Collision is free** for any geometry — the collider *is* the CSG fold (a Rapier trimesh).
- **Nav and rebake degrade gracefully** — they don't break on off-axis geometry.
- The **one real technical wall is texturing** (dominant-axis planar UV projection).
- The **real cost is the editor UX**, not the engine — that's where the hard-won
  iteration lives, and arbitrary angles mean re-earning all of it.

Spend the risk budget in this order:

1. **Slopes/ramps as a first-class primitive** — bounded, we're already halfway there.
2. **Props/prefabs for organic flavor** — near-zero risk, big perceived-flexibility win.
3. **Fixed-increment diagonal walls (45°/30/60)** — *only if* playtesting says the box-grid
   genuinely limits level design; and only the finite-angle version.
4. **Truly arbitrary angles — don't**, unless the level-builder-as-toy *is* the product.

---

## Where the invariants live (grounded map)

The world is **axis-aligned box "brushes" on an integer world-tile (WT) grid**, folded by a
**genuine BSP boolean-CSG kernel** into per-region triangle soups that serve simultaneously
as the render mesh (after UV/zone classification) and the Rapier trimesh collider. Nav is a
separate voxel grid baked from brush membership. Everything above the kernel is hard-wired
to X/Y/Z.

| Concern | Where | Reacts to arbitrary angles how? |
|---|---|---|
| BSP CSG kernel | `crates/csg/src/csg.rs` (`csg_subtract:328`, `csg_union:343`) | **Fully general** — survives untouched |
| Brush data model | `crates/engine/src/geometry/csg_runtime.rs` (`Brush:128`) | **Blocks** — AABB min-corner+dims, *no rotation field* |
| Fold (near/far, early-reject) | `csg_runtime.rs:804-844` | Degrades — world AABB over-selects, still correct |
| Collision | `crates/engine/src/sim/physics.rs` (`set_region_collider:310`) | **Free** — collider is the fold trimesh |
| Nav grid | `crates/engine/src/sim/nav.rs` (`bake:553`, samples `solid_at`) | Degrades — angled wall voxelizes to a chunky staircase |
| UV / zone classifier | `crates/engine/src/render/uv_zones.rs` (`classify_soup:248`, `vertex_uv:104`) | **Breaks** — dominant-axis planar projection; a 45° wall has no dominant axis |
| Authoring tools (snap/pick/adjacency) | `world/tools/*`, `world/editing.rs` (`brushes_touching:330`) | **Breaks** — axis+side selection, whole-WT snap, exact face-coincidence |
| Incremental rebake | `crates/game/src/world/regions.rs` (`rebuild_affected_regions:160`) | Degrades — a little less locality |
| Props (already rotatable!) | `world/tools/prop.rs` (`Transform.rot: Quat`, `:131`) | Already support full `Quat` rotation |

**Bottom line:** the generality is in the kernel; the 90° assumptions are in the brush data,
the fold's AABB helpers, the dominant-axis UV/zone classifier, the nav voxelizer, and every
authoring tool. Angled walls would need: orientation on `Brush`, oriented UV frames replacing
dominant-axis projection, and a from-scratch rotation-authoring UX. The CSG math would largely
survive.

---

## The cliff that matters: fixed angles vs. truly arbitrary

The single most important fork.

- **Fixed increments (45° / 30 / 45 / 60)** keep the world *discrete*. A finite orientation
  set lets you pre-solve UV frames per orientation, keep snapping meaningful, keep adjacency
  reasoning, add a "diagonal" zone to the classifier. You get most of the visual/gameplay
  payoff (diagonal sightlines, cut corners, non-boxy rooms) while staying in a world you can
  still reason about.
- **Truly arbitrary yaw** forces the general case of *everything* — UV frames, snapping,
  corner-joining, adjacency — with no discrete structure to lean on.

The jump between these is enormous and it's **mostly UX, not math**. If angled walls ever
happen, do fixed increments. Arbitrary is ~5× the work for maybe ~10% more expressiveness.

---

## Ideas, tiered by *actual* grounded cost

**Cheap + high-value (respect the grid, small blast radius):**
- Variable / partial-height walls, variable floor heights — mostly tool UX; no invariant broken.
- Sub-grid snapping (half-WT) — pure tool-layer; data is already `f32`.
- **Ramps/slopes** — already half-built: stairs emit a smooth ramp collider
  (`csg_runtime.rs:455`) and there's a `floor_y` concept. A sloped-floor primitive is a
  bounded, known special case with hand-authored UVs (like stairs/platforms already do),
  *not* the general angle problem.
- **Lean on props/prefabs for organic detail** — props already carry full `Quat`. Keep the
  CSG shell orthogonal + structural; get "doesn't look like a grid" flavor from rotated props
  and prefab clusters. Highest ROI, ~zero risk to the CSG core.

**Medium (breaks axis-alignment, but bounded because discrete):**
- **Fixed-increment diagonal walls** — real work in the classifier + a diagonal zone +
  orientation-aware snapping, but tractable because the orientation set is finite.
  Collision/nav come free.

**Can of worms (arbitrary geometry):**
- **Truly arbitrary-angle walls** — general oriented UV frames + from-scratch rotation editor
  UX. Math is fine; the *feel* is a rewrite of the authoring layer.
- **Curved / arc walls** — the above plus tessellation control.
- **Arbitrary subtractive shapes** — closer than the rest, since the `Subtract` op + general
  kernel already exist; "carve a non-box void" is reachable if the void is still built from
  axis-aligned pieces.

---

## What's actually worth it for *this* game

Hide-and-seek lives on **line-of-sight and navigation**, not architectural expressiveness.
The gameplay value of angles is diagonal sightlines, cut corners for peeking/hiding, and rooms
that don't all read as identical boxes — and **fixed 45° diagonals deliver ~all of that.**
Truly arbitrary angles add variety a hunted player will never consciously register.

## The clarifying question (unresolved)

Which pressure is actually driving the desire for flexibility?

- **The look** (levels feel too boxy) → props/prefabs + slopes solve it cheaply.
- **The gameplay** (want sightlines/cover the grid can't give) → fixed-increment diagonals.
- **The authoring** (want the editor to feel less constrained) → the *only* motivation that
  justifies touching the scary arbitrary-angle path, because there the expressiveness of the
  tool *is* the product.
