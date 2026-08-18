# Design — Extending the Level Editor

> Companion to [DESIGN_CSG_FLEXIBILITY.md](DESIGN_CSG_FLEXIBILITY.md), [DESIGN.md](DESIGN.md)
> and [DESIGN_IDEAS.md](DESIGN_IDEAS.md).
> Captures the 2026-08-17 discovery pass on two concrete editor features:
> **(1) a freeform 90°-snapped draw tool that extrudes or insets into a face**, and
> **(2) beveled rooms** — the 45° runner where floor meets wall, as in GoldenEye's Bunker.
> Grounded in a full read of the CSG → classify → collider pipeline.
>
> **Status (2026-08-18): Idea 1 is BUILT** — `world::tools::draw`, key `Q` in BUILD.
> See [What shipped](#what-shipped-idea-1) for the four things the build learned that
> this document had wrong or missing. Idea 2 (bevels) and everything under
> [Other directions](#other-directions-ranked-by-payoff-per-risk) are still unbuilt
> spec.

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

### What shipped (Idea 1)

`Q` in BUILD arms the tool. A cool translucent **tint** washes the surface you're about
to draw on. Click a room surface to drop the first corner, click out corners (each segment
axis-locks to whichever in-plane axis the crosshair moved further along — that *is* the 90°
snap), click the first corner to close. Scroll then sets a **signed** depth: up protrudes
out of the face (`Op::Add`), down sinks into it (`Op::Subtract`), through 0. A click builds.
Esc walks back one rung at a time (depth step → outline → one corner per press); the tool
stays armed after a commit.

**Before the first corner, scroll cycles candidate surfaces.** On an edge two faces meet
and on a corner three do, and `Axis::dominant(hit.normal)` resolves that from whichever
normal the physics engine happened to report — arbitrary, and invisible to the author until
the first segment ran down the wrong plane. `candidate_faces` enumerates every face the hit
point lies on across all three axes and both sides, ordered so index 0 *is* the old
dominant-normal pick (the cycle is purely additive). The tint is what makes the choice
legible, so the two mechanisms only work as a pair.

The tint needed a new renderer channel: `set_surface_tint`, a second fragment entry point
(`fs_tint`) on the highlight pipeline's layout, drawn just before the highlight so the
outline reads on top. Neither existing coloured channel fits — `gizmo` is `BlendState::REPLACE`
with `depth_compare: Always`, i.e. opaque and x-ray, and the highlight's warm yellow is
hardcoded in the shader with no uniform to vary it.

The build order in this document's TL;DR was skipped — drag-select-a-rect was proposed
as a stepping stone toward the crosshair→face-plane projection, but the draw tool
subsumes it, so it was built directly. Everything else the analysis predicted held:
decomposition to rectangles is a zero-line engine diff, extrude/inset really is just
`Op::Add` vs `Op::Subtract` on the existing face anchor, and texturing was a non-issue.

Four things the analysis above did **not** have right:

1. **`floor_y` has to be *uniform* across the decomposed brushes, not merely "set".**
   The note in [Two things to get right](#two-things-to-get-right) says to set `floor_y`
   on inset brushes. That's necessary but not sufficient.
   [`face_owner`](native/crates/engine/src/render/uv_zones.rs#L360) attributes each
   triangle to the *smallest-volume* brush whose face plane it lies on and reads that
   brush's `floor_y`. Left at the per-brush default (`Brush::new` sets `floor_y = y`), an
   L drawn on a **wall** spans two heights, so its rects get different anchors and
   texture-shift against each other — a visible seam along an internal decomposition
   boundary the author never drew. The whole shape now shares one anchor (its lowest
   point). Guarded by `a_wall_shape_spanning_heights_still_shares_one_anchor`.

2. **The commit must return `Vec<RegionMesh>`, not `Option<RegionMesh>`.**
   Every pre-existing tool ends with `rebuild_affected_regions(&ids).into_iter().next()`
   — taking the first mesh and dropping the rest. That's survivable for a tool adding one
   or two brushes inside a region, but this tool adds N at once across a footprint drawn
   up against walls, so it routinely bridges regions and trips the full-recluster path,
   which returns a mesh per region. Dropping the extras leaves stale geometry rendering.
   Hence `World::with_undo_many` alongside `with_undo`.

3. **Coincident internal faces between adjacent decomposed brushes fold away cleanly.**
   This was the main risk to the whole approach — two extruded rects share a full
   internal face, and a leftover wall there would be a slab standing inside the author's
   shape. The kernel handles it (coplanar-opposed polygons route to `coplanar_back` in
   [`split_polygon`](native/crates/csg/src/csg.rs#L77) and get clipped), and it's now
   pinned by `adjacent_decomposed_brushes_leave_no_internal_wall`, which also asserts the
   L's *outward* step face is present so it can't pass vacuously.

4. **`region_hash` enumerates brush fields by hand**, so a new field is a decision, not
   a default. `group` is deliberately **excluded** — it carries no CSG meaning, and
   hashing it would invalidate a memoized bake on a pure regroup.

5. **A drawable surface is not a brush's face** — found in the first playtest, and the
   most important of the five. Every existing face tool (`pick_face_hit`, the hole tool,
   `create_sub_face_brush`) works on *one brush's* face rect, and that's fine for them
   because they place one rectangle centred on the crosshair. It is not fine for a tool
   you drag across a room: enlarge a room by pushing a wall out, or extend it by carving
   an adjoining area, and the floor becomes two or more subtract brushes forming one
   continuous plane with an invisible seam across it. Clamped to the picked brush, the
   outline stopped dead at that seam.

   The fix is a **coplanar face group** (`draw::coplanar_face_group`): flood-fill from
   the picked brush across subtracts whose face on the *same side* of the same axis lies
   on the same plane and whose in-plane rect overlaps or touches a member already in.
   Contiguity is required, so a different room's floor at the same height stays out; and
   matching on `side` matters, because the ceiling of the room below shares the plane
   with this floor but is a different surface. Frames are deliberately *not* excluded
   (unlike `find_room_brushes`) — a doorway threshold really is part of the floor.

   The group's union bounding box is what corners clamp to, which gives free rein across
   the whole surface. But for an L-shaped group that box contains a corner of solid rock,
   so what actually gets built is masked cell-by-cell to the real member rects
   (`rect_decompose_where` + `DrawFace::covers_cell`). That trims a shape drawn over the
   missing corner instead of carving into — or straight through — the solid, and it
   avoids inventing a new refusal path in the middle of the author's drawing.

   **Any future tool that drags across a surface hits this same wall.** Bevels (Idea 2)
   already dodge it by deriving from the folded soup rather than from brush AABBs, which
   is the same insight from the other direction.

6. **The fold order was not preserved across a recluster — a pre-existing engine bug.**
   Found in the second playtest: a shape extruded across a widened room's seam rendered
   correctly, then lost everything past the seam on save/load.

   `evaluate` folds a region's brushes in **slice order**, and that order is load-bearing
   — a `Subtract` after an `Add` carves the added geometry away, which is what makes
   "punch a hole through a pillar" expressible at all. At build time a region's order is
   push order, i.e. ascending brush id, i.e. authoring order. But
   [`cluster_brush_indices`](native/crates/engine/src/geometry/csg_runtime.rs#L975) is a
   **stack-based DFS** — it pushes every touching neighbour then pops the *last* — so
   every recluster (load, undo, redo, or a cross-region merge) rebuilt regions in an order
   that was neither authored nor stable. The drawn `Op::Add` landed *before* the second
   room brush, which then carved away everything inside its own volume.

   Fixed by restoring ascending-id order in `rebuild_from_flat`: ids are allocated
   monotonically at creation, so ascending id *is* authoring order, and it is exactly what
   the incremental path produces. The invariant to hold onto: **a save/load round trip
   must not change the folded geometry.**

   This was never draw-tool-specific — a pillar or brace placed in a room that was later
   extended would lose part of itself the same way. It went unnoticed because every other
   tool places its additive geometry inside a *single* subtract brush.

Decisions taken at build time: `group: u32` is on `Brush` now (`#[serde(default)]`, so
old files load and it needs no migration later) but carries no re-select UX yet — a
group takes the id of its first brush, which is unique by construction and so needs no
allocator threaded through snapshot/save/load. Vertices are held as **integers** in
face-UV WT, which is what lets the self-intersection test and the decomposition be
epsilon-free. Drawing is restricted to `Op::Subtract` room faces, matching the
pillar/brace tools, since "out of the face" is only defined relative to a room interior.
The decomposition is rasterize-then-greedy-merge, not a sweep-line partition: grid
alignment makes rasterizing exact, and concave corners need no handling at all.

The [open question](#open-question) about step height was answered by surfacing it: a
floor extrude taller than 1 WT logs that hunters climb at most `nav::MAX_STEP` = 1 WT, so
the author is told when they've made a player-only shortcut.

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

## Tried and rejected — group push/pull across a coplanar surface

**Do not re-propose this.** Built 2026-08-18 and reverted the same day (user call).

The idea was the obvious symmetry: the draw tool learned that a *surface* is not a brush's
face, so make full-face push/pull move every coplanar contiguous face too — which would
also have made a drawn shape raise/lower as one unit for free, since its rectangles' top
faces are coplanar. It worked, it was atomic (refusing a pull no member could absorb rather
than tearing the face into a notch), and the surface tint showed the affected extent up
front. All green.

**It still has to go, because of the doorframe.** Cut a door, push the protoroom out into a
new room, then select one of *that room's side walls* — the wall perpendicular to the door.
The frame carve's side face lies on the same plane, on the same side, with the same op, and
touches the room in-plane at the wall thickness. So it joins the group, and pushing the
room's wall widens the doorframe along with it.

Worth noting the near-miss in reasoning: frames *are* correctly excluded for a wall
**parallel** to the door (there the room's `Max` face and the frame's `Min` face share a
plane but face opposite ways, so the side-match rejects them). Checking only that case makes
the whole thing look safe. It is the perpendicular wall that breaks it.

That could be patched — exclude `frame` brushes, bound the group by
[`find_room_brushes`](native/crates/game/src/world/editing.rs) — but each patch is another
special case guarding the most-used operation in the editor, and the payoff is small: if an
extrude is wrong, undo and redraw it. The per-brush selection limitation is a real
limitation and it is the right one to live with.

The reverted work is recoverable from the git stash on `feat/editor-draw-tool` if the
calculus ever changes. `Brush::group` remains stamped-but-unread, which costs nothing and
keeps a delete-as-a-unit option open without any live code path.

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
