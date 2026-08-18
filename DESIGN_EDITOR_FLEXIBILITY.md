# Design — Extending the Level Editor

> Companion to [DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md), [DESIGN.md](DESIGN.md)
> and [DESIGN_IDEAS.md](DESIGN_IDEAS.md).
> Captures the 2026-08-17 discovery pass on two concrete editor features:
> **(1) a freeform 90°-snapped draw tool that extrudes or insets into a face**, and
> **(2) beveled rooms** — the 45° runner where floor meets wall, as in GoldenEye's Bunker.
> Grounded in a full read of the CSG → classify → collider pipeline. Nothing here is
> committed; this is a spec-shaped thinking document meant to be picked up cold.

---

## TL;DR / recommendation

Both ideas are cheap **because neither one breaks axis-alignment**. That is the whole story.
[DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md) found that the one real technical wall
in this engine is the dominant-axis UV classifier, and that angled *walls* are what smash into
it. Both features here stay clear of that wall — the draw tool emits only axis-aligned solids,
and the bevel is decorative geometry with hand-authored UVs that never reaches the classifier.

Build order:

1. **Bevels first** — contained to one file, no invariant touched, uses a texture-zone slot
   that is already sitting free, and visibly transforms *every existing level* on landing.
2. **Drag-select a rect on a face** — small, useful on its own, and it is the exact
   crosshair→face-plane projection the draw tool needs. Stepping stone.
3. **Freeform draw** — the bigger win, but the cost is UX iteration, not engine work.

The one line **not** to cross: no 45° segments in the draw tool. See
[The line not to cross](#the-line-not-to-cross).

---

## The two channels (read this before anything else)

Every piece of geometry in the editor arrives through one of two paths, and which path a
feature lands on decides its cost by roughly an order of magnitude.

**Channel A — real CSG.**
[`evaluate()`](native/crates/engine/src/geometry/csg_runtime.rs#L854) folds `shell ± brushes`
through a genuine BSP kernel into one polygon soup. That soup becomes *both* the Rapier trimesh
collider and — after [`classify_soup`](native/crates/engine/src/render/uv_zones.rs#L248)
recovers owner brush, texture zone and planar UVs from the dominant normal axis — the render
mesh. A `Brush` is an AABB in world tiles (WT) with **no rotation field**
([`Brush`](native/crates/engine/src/geometry/csg_runtime.rs#L129)).

**Channel B — hand-authored geometry.**
Stairs are the template. A `StairDesc` descriptor emits explicit quads with explicit zones and
explicit UVs via [`append_zoned`](native/crates/engine/src/geometry/csg_runtime.rs#L505),
appended straight into the region mesh — and a *separate, simplified* surface into the collider
via [`append_ramp_collision`](native/crates/engine/src/geometry/csg_runtime.rs#L455), which is
why the player sees discrete steps but walks a smooth ramp. Channel B bypasses the classifier
entirely and touches no invariant.

> **Idea 1 is Channel A. Idea 2 is Channel B.** Both land well.

---

## Idea 1 — Freeform 90°-snapped draw → extrude / inset

**Concept:** on a flat face, click out an arbitrary polyline whose segments snap to the WT grid
and lock to 90°; close the loop; then extrude the enclosed shape out of the face or inset it
into the face. Touching a wall connects automatically. Gives raised floor sections, sunken
areas, alcoves, and non-rectangular protrusions.

### Verdict

Very doable — **but only if the drawn shape is decomposed into rectangles at draw time.**

### The sharp constraint

[`polygons_to_mesh`](native/crates/csg/src/csg.rs#L358) fan-triangulates, which is valid only
for **convex** polygons. An L- or U-shaped drawn footprint is concave. Push it through as a
single N-gon prism and you get both garbage triangles and an unreliable BSP split. So:

| Path | What it costs |
|---|---|
| **Add `Shape::ExtrudedPoly` to `Brush`** | Triangulation, `contains()` for nav ([`solid_at`](native/crates/engine/src/geometry/csg_runtime.rs#L1155)), the AABB early-reject, `face_owner` in the classifier (AABB-based), `pick_face_hit` (matches hits against brush AABB face planes, [pick.rs:110-133](native/crates/game/src/world/pick.rs#L110-L133)), and serde. The whole stack. |
| **Decompose to rectangles in the tool** ✅ | Nothing downstream changes *at all*. |

Take decomposition. Because every segment is 90°-snapped and WT-grid-aligned, rectilinear
partition is **exact** — a sweep over the vertical edges, no approximation. The tool emits N
ordinary `Brush`es; fold, classifier, nav, picking, undo, region clustering, save format and
incremental rebake all work untouched.

### What comes for free

- **Extrude vs. inset is `Op::Add` vs `Op::Subtract`.**
  [`create_sub_face_brush`](native/crates/game/src/world/editing.rs#L167) already does exactly
  this for the scroll-sized sub-rect. This feature is not a new concept — it is an upgrade to
  the *selection primitive*, from "scroll-sized rectangle" to "drawn polygon". The whole
  push/pull machinery in [editing.rs](native/crates/game/src/world/editing.rs) is reusable.
- **"Touches a wall → auto-connects" is already the behaviour.**
  [`brushes_overlap_or_touch`](native/crates/engine/src/geometry/csg_runtime.rs#L938) clusters
  touching brushes into one region, and resolving coincident faces is precisely what the BSP
  kernel does. Nothing to build.
- **Texturing is a non-issue.** Every face of an axis-aligned extrusion is axis-aligned, so
  the dominant-axis classifier works unchanged. This is the crucial difference from the angled-
  wall problem — big expressive win, zero contact with the one thing that actually breaks.
- **Collision and nav are free.** The collider *is* the fold; nav replays brush membership via
  `solid_at`, and rect-decomposed brushes are just brushes.

### Two things to get right

1. **Set `floor_y` on inset brushes.** A lowered floor must anchor its own wall texture or the
   whole room's walls shift. Precedent + guard test:
   [uv_zones.rs:614](native/crates/engine/src/render/uv_zones.rs#L614).
2. **Decide the group id up front.** After decomposition the user's one drawn shape is 5
   brushes, so re-selecting "that raised section" as a unit needs a
   `#[serde(default)] group: u32` on `Brush`. It is additive and invisible to the fold — but
   retrofitting it later is a save migration. Decide before shipping, not after.

### The actual cost: UX

As [DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md) predicted, the engine is the easy
part. The tool needs:

- Crosshair → face-plane `(u, v)` projection (the face's two orthogonal axes come from
  [`Axis::orthogonals`](native/crates/engine/src/geometry/csg_runtime.rs#L87)).
- Click to drop a WT-snapped vertex; each segment axis-locks to whichever of `u`/`v` the
  crosshair moved further along. That *is* the 90° snap.
- Close-the-loop detection (click the first vertex) + self-intersection rejection.
- Depth sizing (scroll, mirroring [`adjust_opening_size`](native/crates/game/src/world/tools/opening.rs#L82))
  and a confirm that picks extrude or inset.
- Esc back-out ladder per vertex — follow the phase machine in
  [platform.rs](native/crates/game/src/world/tools/platform.rs#L66).

**The ghost is cheap.** `set_highlight` takes a single `CpuMesh` with culling disabled, so the
in-progress polyline is just thin quads laid on the face plane, built the same way as
[`face_quad_mesh`](native/crates/game/src/world/geom.rs#L11). No new render pipeline.

### Open question

Nav has no gravity for enemies (they move on grid-nav only — see the *Enemy nav vs physics*
note). A raised section taller than one WT is a wall to a hunter but a step to the player.
That is a *design* decision, not a bug, but the tool should probably surface the step height so
the author knows when they have created a player-only shortcut.

---

## Idea 2 — Beveled rooms (Bunker-style 45° runners)

**Concept:** a superficial 45° chamfer strip where floor meets wall and where wall meets
ceiling. No change to the room's actual volume — it just kills the hard 90° crease that makes
every room read as a box.

### Verdict

**The cheapest high-impact idea on the table.** Channel B, and derive the strips from the
*folded soup*, not from brush AABBs.

A true CSG chamfer needs a rotated or prism brush and is not worth it. Superficial is correct,
and it is exactly how stairs already work.

### Where the strips come from — the decision that matters

The obvious approach is to walk each room brush's 12 AABB edges. It has one nasty failure mode:
a strip runs straight **across every doorway**, and across the open boundary between two
subtract brushes of the same room, leaving visible ridges through openings. Fixing that means
span-culling against frame AABBs forever, for every case you later invent.

**Better: a post-pass over the CSG output.** After the fold, hash triangle edges by rounded
endpoints, find shared edges where the two adjacent triangles' normals are perpendicular and
the corner is concave, and emit a 45° fillet quad there. This handles doorways, pits, L-shaped
rooms and multi-brush rooms **automatically**, because it operates on the real surface rather
than on authoring intent. One adjacency pass, slotting into
[`evaluate_both`](native/crates/engine/src/geometry/csg_runtime.rs#L1100) right where stairs
already append.

### Two facts to know before speccing

- **Zone 4 is free — and it is the last one.**
  [`ZonedBuilder::finish`](native/crates/engine/src/render/uv_zones.rs#L206) packs the draw-group
  key as `scheme * 8 + zone`, so zones must stay `< 8`. Currently 0,1,2,3,5,6,7 are used. A
  bevel zone fits slot 4 with no packing change. **If you later want floor-bevel and
  ceiling-bevel to carry *different* textures, you are out of slots and that multiplier has to
  change.** Decide which you want now.
- **Nav does not need to know.** The nav grid bakes from brush membership, so it never sees the
  fillet — which is correct, since a chamfer blocks nothing. It goes into the collider (as the
  stair ramp does), the player slides over it, hunters are unaffected.

### The fiddly bit

Three-surface corners, where two fillets meet, leave a small triangular notch. Either emit a
gusset triangle or accept it — at 0.25–0.5 WT it is invisible, and GoldenEye didn't solve it
either.

### Authoring

Per-room toggle is the natural granularity, and the flood-fill already exists:
[`find_room_brushes`](native/crates/game/src/world/editing.rs#L306) walks connected subtract
brushes and stops at door/hole frames — the same mechanism the number-key retexture uses. A
`bevel: bool` (or a bevel size in WT) alongside `scheme` on `Brush` follows an established path
end to end.

---

## Other directions, ranked by payoff-per-risk

1. **Copy/paste + mirror of brush groups.** `Brush` is `Copy` + serde; cloning a set with an
   offset is pure tool work, zero engine change. Probably the single biggest authoring-
   throughput win available, and it is what makes the large bases in
   [DESIGN_BASE_SCALE.md](DESIGN_BASE_SCALE.md) actually tractable to author.
2. **Drag-select a rect on a face** instead of scroll-sizing it. Small on its own, but it is
   the same crosshair→face-plane projection the draw tool needs. Build it first.
3. **Arches / vaulted ceilings.** Identical pattern to bevels (descriptor + explicit UVs,
   Channel B). Very GoldenEye — Archives, Statue. Cheap once the bevel machinery exists.
4. **Trim / baseboard bands.** Cheaper than bevels (a thin band at a fixed height above
   `floor_y`), and arguably sells "designed room" harder than the chamfer does.
5. **Numeric readout + grid overlay while drawing.** Unglamorous; right now WT counts are
   eyeballed.

---

## The line not to cross

**Do not let the draw tool place 45° segments.**

The moment a drawn polygon contains a diagonal, rectilinear decomposition dies and you are into
real prism brushes *plus* the oriented-UV problem — the exact "can of worms" tier that
[DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md) flagged. 90°-only is precisely what keeps
this a tool-layer feature with a zero-line diff in the engine.

If diagonals are ever genuinely wanted, that is the *fixed-increment diagonal walls* item in
the other doc, and it should be taken on deliberately as its own project — not smuggled in as a
draw-tool feature.

---

## Grounded file map (for picking this up cold)

| What | Where |
|---|---|
| BSP kernel (fully general; fan-triangulation is the convexity constraint) | [csg.rs](native/crates/csg/src/csg.rs) — `polygons_to_mesh:358` |
| Brush model (AABB, no rotation) + the fold | [csg_runtime.rs](native/crates/engine/src/geometry/csg_runtime.rs) — `Brush:129`, `evaluate:854`, `evaluate_both:1100` |
| Channel-B precedent (stairs: zoned emit + separate collider surface) | `csg_runtime.rs` — `append_zoned:505`, `append_ramp_collision:455` |
| UV / zone classifier + the `scheme * 8 + zone` packing | [uv_zones.rs](native/crates/engine/src/render/uv_zones.rs) — `classify_soup:248`, `finish:206` |
| Push/pull + sub-face carve (what extrude/inset reuses) | [editing.rs](native/crates/game/src/world/editing.rs) — `create_sub_face_brush:167` |
| Room flood-fill (per-room bevel toggle rides this) | `editing.rs` — `find_room_brushes:306` |
| Face picking against brush AABB planes | [pick.rs](native/crates/game/src/world/pick.rs) — `pick_face_hit_from:89` |
| Rect-on-face tool to copy the shape of | [opening.rs](native/crates/game/src/world/tools/opening.rs) |
| Phase-machine / Esc-ladder tool to copy the shape of | [platform.rs](native/crates/game/src/world/tools/platform.rs) |
| Ghost preview quads (culling off, one `CpuMesh`) | [geom.rs](native/crates/game/src/world/geom.rs) — `face_quad_mesh:11` |
| Save format + `#[serde(default)]` migration convention | [persist.rs](native/crates/game/src/world/persist.rs) |
