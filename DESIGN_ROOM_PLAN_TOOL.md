# Room plan tool

**Status:** BUILT, green (24 new tests, 805 total), release built — awaiting playtest.
Branch `feat/room-plan-tool`.

## The problem

Carving is good at sprawl and bad at planning. Every existing geometry tool is a
*surface* tool: it projects onto a face you are already standing in front of. That is
right for detailing a room you can see, and useless for deciding where the next room
should go. Laying out a base means flying outside the level to judge the arrangement,
memorising a position, flying back in, and carving blind.

## The tool

A drafting view + a footprint you draw in world coordinates.

1. Arm it from the radial (Tools → **Room plan**). It takes the camera and frees the
   cursor, and opens on a top-down orthographic plan of the level.
2. **Click corners.** Every segment is 90°-snapped. Click the first corner to close.
3. **Adjust.** Drag any corner; its two neighbours follow so every edge stays square.
   The scroll wheel slides the whole footprint up and down the Y axis — that is the
   "which storey is this" step, and the plan view's slice follows it.
4. **Click**, then scroll to set the height (down through zero extrudes *below* the
   plane instead of above it). Click again to build.
5. It stays armed on a clean outline, ready for the next room.

The result is real subtract brushes — a sealed room in solid rock, which you connect to
the rest of the level afterwards with the door and hole tools. A corridor is just a thin
footprint drawn between two rooms at the same height.

## Controls

| | |
|---|---|
| LMB | place a corner / grab a corner / advance the phase / build |
| RMB drag | pan the drafting view (ortho) · mouse-look (perspective) |
| Wheel | the current phase's job — zoom, then base height, then room height |
| Ctrl+Wheel | zoom, in every phase |
| Esc | back out one rung (height → base → reopen the outline → one corner at a time) |
| Numpad 7 / 1 / 3 | top / front / right (**+Ctrl** for bottom / back / left) |
| Numpad 5 / 0 | flip to perspective and back / straight to perspective |
| 1–9 | the theme new rooms are built with |

No hotkey, by design — there is no letter left, and this is not a tool to trip into with
a stray keypress.

## Design decisions

**One room at a time, no persistent sketch layer.** The sketch is discarded at commit;
what persists is the geometry. The planning value comes from the *plan view showing the
real level* — every room's footprint is drawn as an outline you place against — not from
keeping editable drawings around. A persistent layer would mean a new authored type in
the level format, undo for it, a sketch↔brush linkage, and the nasty case where
re-extruding a room you have since cut doors into and painted destroys that work.

**The plan view is Y-sliced.** Footprints whose height range straddles the drafting
plane draw bright; the rest draw dim. Without this a multi-storey base is mush from
above, and the "slide the plan up a storey" step has nothing to show for itself.

**Drawing works in top, bottom and perspective; not in the side views.** That is
geometry, not policy: a side view's ray runs *along* the drafting plane, so it genuinely
cannot say where along the depth axis a click meant. `room_plane_hit` returns `None` and
the tool says so.

**The wheel is modal, Ctrl is the escape hatch.** Zoom and the two height steps all want
the wheel. Whichever the phase needs wins; Ctrl+wheel always zooms.

## What it rides on

- **`world::tools::draw`'s 90° machinery, verbatim.** `axis_lock`,
  `segment_self_intersects` and `rect_decompose` are the same problem in world XZ as in
  face UV. Vertices are **integers**, which is what keeps all of it exact and
  epsilon-free — don't switch them to floats.
- **A room is an `Op::Subtract` brush.** The world is implicitly solid, so nothing
  downstream had to learn anything: the fold, the classifier, nav, picking, save/load and
  undo all run untouched.
- **`World::view_proj` is the one seam.** Returning an orthographic matrix there is what
  makes the drafting views cost nothing elsewhere — the renderer, the prop/HUD
  transforms, and crucially `App::mouse_world_ray` (which unprojects the cursor through
  *that* matrix's inverse) all pick it up with no branch of their own.
- **`set_gizmo_mesh`** — REPLACE + depth-Always, i.e. opaque x-ray — is the plan
  overlay's channel. Exactly right when every room is a void inside solid rock.

## Things a future session would otherwise re-derive

1. **A rectilinear corner cannot move alone.** It joins one horizontal and one vertical
   edge; moving it without its neighbours tilts both. So a drag carries the neighbour
   that shares its `x` and the one that shares its `z`. From the author's side that reads
   as "drag this corner" — the two edges follow — which is why it looks like nothing
   special and is the first thing a reimplementation gets wrong.
2. **The incremental self-intersection test is not enough after a drag.**
   `segment_self_intersects` answers "may I add this segment?", which is the right
   question while drawing. A drag moves three corners at once and can push an edge across
   one on the far side of the loop, which that test is never asked about. Hence
   `polygon_is_simple`, an O(n²) whole-polygon check over exact integers. An illegal drag
   is **refused**, not clamped: the footprint stops following the pointer and resumes when
   it comes back, which needs no explanation on screen.
3. **The commit takes the full recluster, not `rebuild_affected_regions`.** Two reasons,
   and the second is the real one. A footprint in open space belongs to no existing
   region, and `assign_brush_to_region` can only place a brush that is already inside
   one — so the tool gives it a region of its own up front. But a footprint drawn *over*
   an existing room has to **merge** with it, and a merge is precisely the case the
   incremental path bails on (`assign_brush_to_region` → `None`). Pushing a fresh region
   and then rebuilding incrementally would leave `brush_to_region` pointing at the merged
   region while the brushes still sat in the new one. `recluster_all` is correct by
   construction, and the memo cache means untouched regions don't re-fold.
4. **Numpad 1–9 were already bound** — `digit_char` aliases them to the number row for
   the crosshair retexture. Giving the numpad to the views needed a row-only
   `row_digit_char`, and cost nothing: retexture needs a crosshair, and an orthographic
   view has none. Every *other* numpad key is swallowed while the tool is armed rather
   than falling through to its row twin.
5. **The cursor is reconciled once per frame, not at each arm site.** The tool is
   disarmed from six places (Esc, a mode switch, arming any other tool, the radial, its
   own key, the cursor release). A cursor left grabbed by a missed one is unrecoverable
   without knowing why, so `App::sync_room_cursor` derives it from `World::is_room_tool`
   instead.
6. **New radial entries go on the *end* of a ring.** "Room plan" belongs next to "Draw"
   by kind, and putting it there moves every tool after it under fingers that already know
   where they are. A fixed layout is the entire value of a radial. (Inserting it at index
   1 also broke `radial::tests::descending_then_flicking_commits_the_child`, which is the
   test doing its job.)
7. **`egui::Area` for the status strip must be `interactable(false)`.** The strip sits at
   the top of the screen where the author is drawing; an ordinary window would swallow the
   clicks that place corners.
8. **One wall-texture anchor per room, not one per brush** — the same trap `draw` hit.
   `uv_zones::face_owner` reads the owning brush's `floor_y` as the wall-UV origin, so a
   decomposed L-shaped room left on the per-brush default bands against itself along an
   internal boundary the author never drew.

## Not built

- Sketches are not saved. See "one room at a time" above.
- The four side views are for *looking*, not editing — you cannot drag the base height
  in a front view, only scroll it.
- No snapping to existing room edges. The grid is the only snap.
- A committed room is edited with the ordinary tools, not by re-opening its plan.
