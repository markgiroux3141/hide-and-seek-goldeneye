# Design: wall banding — floor-aware bands, a cornice zone, and the shader question

Brainstorm, 2026-08-31. **Ideas A, B and D are built** (§7–§9). Companion to
`DESIGN_TEXTURE_THEMES.md`.

The complaint: a tall continuous wall wears `lower, upper, lower, upper` up its
height, with the repeat position dictated by how many brushes the author happened
to stack rather than by anything visible in the room.

---

## 1. Why it repeats (measured, not guessed)

Walls are split into zone 2 (lower) / zone 3 (upper) at
`origin[1] + WALL_SPLIT_V`, where `origin[1]` is the **owning brush's own
`floor_y`** — `native/crates/engine/src/render/uv_zones.rs:804`, split done in
`emit_wall_split` (`:885`). `WALL_SPLIT_V = 6.0` WT, global (`:32`).

`floor_y` defaults to the brush's own base `y` (`csg_runtime.rs`, `Brush::new`)
and is re-anchored to the new base whenever a `Min`-Y face is pushed or pulled.
It is **the brush's base, not a floor**. Nothing in the pipeline asks whether a
floor exists there.

Rooms are `Op::Subtract` air carved from a solid shell, so two rooms stacked with
`lower.y + lower.h == upper.y` produce **one continuous air column with no floor
between them** — but two wall triangles, because the BSP cuts the shell wall at
that plane, and each fragment is attributed to a different brush with a different
anchor.

`levels/facility_2.json` has **7 such stacked pairs** with large XZ overlap. The
worst is `y=-8 h=8` under `y=0 h=28`: a 36-WT-tall continuous column that
currently bands

```
-8 .. -2   lower      <- correct, there IS a floor at -8
-2 ..  0   upper
 0 ..  6   lower      <- WRONG, no floor at 0
 6 .. 28   upper      <- and 22 WT of one tiling texture, no relief
```

Both halves of the complaint are visible in that one column: a phantom lower
band, and a 22-WT featureless upper band.

---

## 2. Idea A — anchor the band to the air column, not the brush

Replace "split at my base + 6" with "split at the bottom of the air column I
belong to + 6". A wall fragment gets a lower band **iff** its own bottom edge is
a real floor.

The probe is a chain-walk down the subtract brushes:

```
fn column_base(x, z, y) -> f32:
    base = y
    loop:
        // any subtract brush covering (x,z) whose span contains base - eps
        match lowest such brush:
            None    => return base      // solid below: this IS a floor
            Some(b) => base = b.y       // air continues down: keep going
```

`structures.rs:550 floor_y_under` is the closest existing primitive but answers a
different question (nearest surface at-or-below, single step, no chain-walk).
This wants a new, small function next to it.

### Three things that decide whether this works

**Probe per wall FACE, not per brush.** The tempting cheap version — derive
`floor_y` once per brush in `brush_infos()` (`csg_runtime.rs:1424`), which already
derives `origin_xz` from the `vent` flag — regresses stair pits. A pit is a
subtract brush *under the room's interior*; probing at the room's XZ centre would
find it and drag the whole room's wall anchor down 6 WT. Probing at the centre of
each of the four wall faces (offset one eps inward) does not see a central pit,
and does see the room stacked directly below. So the anchor is per-(brush, face),
not per-brush — either widen `BrushInfo` to carry 4 wall anchors, or compute it in
the classifier from the fragment.

**The straddle machinery reads the anchor.** `face_outcome` (`:593`) packs the
anchor into the comparison that `straddle_planes` (`:652`) uses to decide where a
triangle must be cut. Keeping the anchor a per-(brush, face) value means that
logic is unchanged. Making it truly per-position (probed per fragment) breaks the
invariant that the outcome can only change at a candidate's claim edge — the
column base also changes at the XZ edges of the brush *below*, and those planes
would have to be added to the cut set. That is the expensive version. Start
per-face.

**Platforms are not brushes.** `Platform` (`structures.rs:91`) is its own list,
and plane-style platforms are render shells, not solid boxes. A mezzanine built
from platforms is a visible floor the brush-only probe cannot see, so the wall
above it would lose its lower band. Either feed `&[Platform]` into the probe or
accept the limitation — but it must be a decision, not a surprise.

### Known false-positive to accept or handle

A wall whose floor below is present for only part of its length (a mezzanine
edge). Per-face probing gives one answer for the whole face. Correct handling
needs the per-fragment version above. Probably fine to accept at first.

---

## 3. Idea B — a cornice band flush to the ceiling

A third band, always `CORNICE_H` WT tall measured **down from the ceiling**, is
what would break up that 22-WT upper wall. It is a small change in the same
function and a large change in the theme contract.

**Zone 4 is the last free slot.** Zones are 0..7, the packing key is
`scheme * 8 + zone` (`uv_zones.rs`, `ZonedBuilder::finish`) and the material table
is `[Option<BindGroup>; 8]`. Zone 4 is currently reserved-and-unused — there is a
test asserting exactly that (`textures.rs:819`). Spending it here means the band
stack is closed to further growth unless the key widens.

**It needs a second anchor.** Today `origin[1]` is the floor and V is
`worldY - floor_y`. A ceiling-flush band must measure from the top, or the trim
floats at a different height in every room. Same chain-walk, run upward, to get
`column_top`; then emit the zone-4 fragment with
`origin[1] = column_top - CORNICE_H` so its V still starts at 0 at the band's
bottom edge. `emit_tri` already takes `origin` per triangle, so this costs nothing
structurally.

**Make it opt-in per theme.** 392 themes were extracted from the GoldenEye levels
and none of them define zone 4. Rule: if `scheme.zones[4].is_none()`, do not
split — emit the fragment as zone 3. Existing levels then render identically and a
theme opts in by defining one texture. The `zones[4].is_none()` test becomes a
test that the *extracted* themes leave it undefined.

---

## 4. Idea C — bands from a combined texture + UVs instead of mesh splits

The instinct in the question is right: **plain per-vertex UVs cannot do this.**
A base band that does not tile, a middle that tiles N times, and a top band that
does not tile is a piecewise V mapping, and per-vertex UVs are affine across a
triangle. Without intermediate geometry there is no place to put the breakpoints.

But the breakpoints do not have to live in the vertices. They can live in the
**fragment shader**, which knows the world position:

```wgsl
// per fragment; h = worldY - column_base, H = column_top - column_base
if      (h < BASE_H)     { v = remap(h, 0.0, BASE_H, 0.0, base_end); }
else if (h > H - TOP_H)  { v = remap(h, H - TOP_H, H, top_start, 1.0); }
else                     { v = mid_start + fract((h - BASE_H) * mid_rep)
                                         * (mid_end - mid_start); }
```

The only new per-vertex data is `column_base` and `column_top` — two floats, or
one `vec2` in an extra vertex attribute. `shader_textured.wgsl` already carries
`world_pos` through to the fragment stage for the point lights, so the plumbing
exists.

### What it actually buys (measured on facility_2, not estimated)

A full bake of `levels/facility_2.json` (3 schemes, 17 stair runs):

```
total tris                 20,146
  zone 0 floor              5,242
  zone 1 ceiling            1,388
  zone 2 lower wall         4,781
  zone 3 upper wall         7,707     <- walls are 62% of the mesh
  zone 5/6 frames           1,028
draw groups                    16    (6 of them wall)
wall tris entering split   10,142
  actually 3-way split      1,068  -> +2,136 tris = 10.6% of the mesh
same bake, split disabled  18,046 tris, 14 draw groups
```

So the geometric win is **~10% of triangles and 16 -> 14 draw groups.** Real, but
modest — most wall triangles already sit wholly in one band and are emitted
untouched. Anyone selling C on triangle count is overselling it.

The wins that actually matter are different:

**Band height becomes a uniform, not a bake input.** Today changing
`WALL_SPLIT_V` — or idea D's per-theme height — cuts the mesh differently and
needs a full region re-bake. In the shader it is a float in the material uniform:
the TEXTURES panel could drag a band height live with no rebake at all. Given how
much of this repo's pain is authoring-time rebake cost, that is the strongest
argument on the list.

**It retires the "zone 4 is the last free slot" constraint.** With one strip per
scheme, the wall bands share a single bind group and stop being entries in the
`scheme * 8 + zone` key. Band count is then limited by the strip, not by the
8-zone table — so a skirting *and* a wainscot *and* a cornice becomes possible
rather than impossible.

**Boundaries go pixel-exact**, and A and B stop being separate machinery: the
shader is anchored to the column extents by construction, so there is exactly one
base band and one cornice per column regardless of how many brushes tile the wall.

### What C does NOT simplify

The classifier has four separate cutting mechanisms. C deletes **one**:

| mechanism | what it does | under C |
| --- | --- | --- |
| face-map attribution | triangle -> owning brush -> scheme + anchor | **stays** |
| straddle cutting (`straddle_planes`) | cuts where the *owner* changes | **stays** — different schemes are still different draw groups, so the mesh must still be cut |
| frame splits -> zones 5/6 | door/hole tunnel surfaces (1,028 tris here) | **stays** — an XZ region test, not a vertical band |
| `emit_wall_split` | the vertical lower/upper cut | **deleted** |

**And the column probe from idea A is still required.** C does not avoid it — the
extents have to be computed at bake time to be written into the vertices. What C
avoids is the *awkward* half of A: the per-face-versus-per-fragment dilemma and
the straddle-cut-plane interaction (§2), because a per-triangle value needs no
agreement with the cut set.

One free break: `ZonedBuilder::finish` already emits **un-indexed** geometry, 3
vertices per triangle. So a per-triangle attribute costs nothing and needs no
de-indexing, and the extents can be flat per triangle rather than interpolated —
which is what you want, since interpolating `column_base` across the mezzanine-edge
case of §2 would draw a *sloped* band.

### Two ways to feed it, one of which is blocked

**C1 — atlas strip per scheme.** Blit the band textures into one vertical strip at
load. Works with the current renderer shape (one bind group per draw). Costs: mip
bleeding across band seams (drop mips on wall strips, or clamp per band with
`textureSampleGrad`); per-band aspect compensation, since the strip forces a
common width and each zone has its own `repeat` (`material.params.x`) — pass a
repeat per band instead of one; and 392 strips built at load, which is cheap
because the sources are tiny.

**C2 — texture_2d_array, no atlas.** Strictly nicer in principle: no seams, each
band tiles natively, layer indices per scheme in a uniform. **Measured blocker:**
the 1024 shipped textures have more than 20 distinct sizes — 484 are 32×32 and
127 are 64×64, but the tail includes 40×33, 128×47, 64×17 and 64×1. A texture
array requires one size for all layers, so this needs rescaling everything to a
common size, which distorts the non-power-of-2 sources and changes the
`repeat`-based tiling look. Not worth it.

So the shader path means C1.

### Blast radius

A is local: `uv_zones.rs` plus a probe in `structures.rs`. C touches the vertex
format, the wall pipeline and shader, texture loading (strip builder), the theme
schema, and the TEXTURES panel's real-CSG preview room. That difference, not the
triangle count, is the thing to weigh.

---

## 5. Idea D — the cheapest win, independent of everything above

`WALL_SPLIT_V` is one global constant, 6.0 WT, for all 392 themes. Making it a
per-theme field is one number in `themes.json`, one field in `Scheme`, and no
geometry work at all — and it is already the kind of thing the TEXTURES panel
edits live. A tiled-bathroom theme wants a 2-WT skirting; a wood-panelled theme
wants a 6-WT wainscot. Today they are forced to agree.

Worth doing first regardless of which of A/B/C lands, because it is hours not days
and it makes the existing bands *look* deliberate.

---

## 6. Where this wants to end up

A, B and D are three special cases of one idea: a **declarative band stack** per
theme, resolved against the air column.

```json
"bands": [
  { "zone": 2, "from": "floor",   "h": 6.0 },
  { "zone": 4, "from": "ceiling", "h": 1.5 },
  { "zone": 3, "fill": true }
]
```

- Idea A is "resolve `from: floor` against the column base, not the brush base".
- Idea B is one more entry with `from: ceiling`.
- Idea D is `h` being authored instead of constant.
- Idea C is *where the stack is evaluated* — per fragment in the shader instead of
  per triangle in the classifier.

That framing also caps the ambition honestly: the 8-zone key means at most a few
bands ever, so this is a band stack, not a general trim system.

## Suggested order

The probe is the shared part, so it comes first either way.

1. **The column probe** — `column_base` / `column_top` as a pure, tested function
   next to `floor_y_under`. Needed by A *and* by C. Decide the platform question
   here (§2).
2. Then fork:
   - **Cheap path (A):** feed it into the existing `emit_wall_split`. Fixes the
     reported bug in one localized change, mesh still split. ~20 lines that C
     would later delete — so not wasted work, just not the destination.
   - **Real path (C1):** extents into a per-triangle vertex attribute + the
     fragment remap. Deletes `emit_wall_split`, and gets **B** (cornice) and **D**
     (per-theme band height, live and rebake-free) essentially free, plus lifts
     the 8-zone band cap.
3. **D** standalone is still worth doing first if C is deferred — one JSON field,
   hours of work, makes the existing bands look deliberate.

If C1 is the intended destination, going A-then-C means writing the band logic
twice; going straight to C1 after step 1 means the reported bug stays visible for
longer. That is the actual trade, and it is a scheduling call, not a technical one.

## Open questions

- Should the column probe see platforms and structures, or only brushes?
- Does a cornice belong on tunnel/doorframe zones (5/6) too, or walls only?
- Is the mezzanine-edge false positive (§2) acceptable, or does it force the
  per-fragment anchor and the extra straddle cut planes?

---

## 7. Built (2026-08-31): the air-column probe, per fragment

`cargo test --workspace` = 769 green. Release binary built.

### What landed

**`ColumnInputs`** (`structures.rs`) — the region's brushes as authored plus the
level's platform slabs, resolved once at construction:

- `solid_at(x, y, z)`: the same **ordered CSG replay** as `Region::solid_at` —
  start solid, every brush containing the point flips it — plus platform slabs.
  The shell is deliberately not consulted; it only decides points outside the
  level, which are solid either way.
- `base_at_with(x, z, y, scratch)`: the floor at the bottom of the connected air
  column through `(x, z)` containing height `y`. Walks down boundary to boundary,
  sampling the **midpoint of each interval** — exact rather than a guess, because
  solidity cannot change between two consecutive boundaries.

**Wired per fragment.** `classify_fragment` probes from each wall triangle's own
bottom edge, at its centroid stepped `PROBE_INSET` (0.25 WT) into the cavity along
the face normal. `BrushInfo` keeps its original scalar `floor_y`, now only a
**pin**: where an author has moved it off the brush base the probe stands down.

### Why per fragment and not per face — the correction that matters

The first cut of this anchored per `(brush, face)` with an all-or-nothing sample
across the face, on the reasoning in §2. **A playtest killed it in one edit.** A
partial-face pull with `-` calls `create_sub_face_brush(Op::Add, …)`, which builds
a solid ledge protruding into the room — a floor partway up a wall. The wall above
that ledge and the wall beside it are *the same brush face*, so no per-face value
can give the two answers the author can plainly see are needed.

Two defects, not one:

1. **`Op::Add` was invisible to the probe.** The per-face version walked only
   subtractive cavities and went straight through the ledge, returning the room
   floor. Solidity is an ordered replay; treating "not in a subtract" as solid is
   not the same thing.
2. **Per-face granularity cannot express it**, even with (1) fixed.

Per fragment also turns out to be *better* on the case §2 used to justify
all-or-nothing. A shaft carved against part of a wall: the wall inside the shaft
bands from the shaft floor and the wall beside it from the room floor — both true
at once. The per-face rule had to pick one and gave up whenever they disagreed.
§2's worry was really about probing a *fixed sample point* per face; probing from
each triangle's own bottom edge sidesteps it.

**Why it is safe against the straddle machinery.** `face_outcome` still compares
the *authored* `floor_y`, so cutting behaviour is unchanged. That leaves the
question of a triangle spanning a ledge edge, and the fold already answers it: a
ledge's faces are not coplanar with the wall, so the BSP splits the wall there. The
end-to-end ledge test confirms the split exists rather than assuming it.

### Measured

- **Cost: 0.9 ms on a 27.6 ms bake** for facility_2 (28.48 vs 27.58 ms, 20 runs,
  release) — ~3%. Pre-resolving platform solid boxes at construction and reusing
  one scratch buffer are what keep it there; a grounded platform's box costs a
  floor lookup over every brush, which was never affordable per query.
- **Effect on the shipped levels** (per-face version, still indicative of where
  columns run): 16 of 296 wall faces in facility_2, 6 of 84 in slot7, 1 of 20 in
  egyptian_level, 0 in aztec/slot8/slot4.
- The reported stacked case, facility_2 brush 1 (`y=0 h=28` over `y=-8 h=8`): only
  the wall with open air below it re-anchors, and its split plane falls below the
  wall's own bottom, so it draws entirely upper — no phantom skirting.

### Things that were wrong on the way

- **Platform awareness barely moves the classification.** 16 faces vs 21 without,
  but zone triangle counts were byte-identical — those faces' split planes land
  outside their wall's y-range either way. The "6 of 7 seams have a platform"
  figure in §2 predicted far more than it delivered. It still shifts their UV
  anchor, and it is nearly free, so it stays.
- **The shell's outer skin bands at a meaningless height, and always has.** The
  first end-to-end test failed on triangles at `x/z = -1` — the outside of the
  shell, which lies on no brush face and takes the no-owner default of world zero.
  Any band test must be scoped to cavity wall planes, judged **per triangle**: a
  shell-skin triangle can still have one corner sitting at x = 0.

### Known follow-ups

- **A platform edit does not re-band walls until the region next re-bakes.**
  `rebuild_structures` rebuilds only the structures mesh, and platform
  add/delete/move goes through it. Self-healing on the next region edit or reload,
  and `region_hash` hashes platforms so no stale bake is *served* — but a deck
  dropped in during authoring will not move a band immediately. Needs the
  multi-`RegionMesh` return the other tools already want. `Op::Add` ledges are
  region data and so are unaffected.
- Stair treads are not solid to the probe, so a stairwell joining two storeys reads
  as one tall wall. Believed right; unconfirmed by eye.
- A cornice band (§3) still wants `column_top` — the same walk run upward.

---

## 8. The wedge (2026-08-31, second playtest): cutting at column steps

Reported: the fresh band above a pulled ledge appeared correctly, but the wall
beside it had **one triangle** wearing the lower band as a diagonal wedge.

### Why per-fragment anchoring alone was not enough

§7 claimed the fold already cuts a wall at a ledge boundary, so a per-triangle
anchor is sufficient. **That claim was wrong**, and it was wrong for the very
reason `straddle_planes` exists: a ledge pulled from a wall is *coplanar with that
wall*, and coplanar polygons do not split each other in a BSP.

So a single wall triangle can span a step in the column floor. Anchoring per
fragment picks one answer for the whole triangle, and the other half of it draws
the wrong band — along whatever diagonal the fold happened to triangulate with,
which is exactly the reported wedge.

Two families of cut are needed, for different reasons
(`ColumnInputs::floor_edges_near_wall` → `uv_zones::column_planes`):

* **Horizontal**, at a ledge's side edges: the wall beside a ledge and the wall
  above it want different bands.
* **Vertical**, at a ledge's top and bottom: a triangle spanning a ledge's top has
  its own bottom edge *below* the ledge, so it probes the room floor and never sees
  the ledge at all. Cutting there gives the upper part a bottom edge that finds it.

Only the horizontal family was obvious. Adding it alone fixed aztec's extra-band
count (65 → 0) and barely moved facility_2's missing bands (681 → 674); the
vertical family is what took those to zero.

### The invariant, and why the first test missed the bug

`a_walls_bands_agree_with_its_air_column_everywhere` states it directly: **for every
point inside a wall triangle, the band it was drawn in must agree with the air
column at that point.** Two things had to be right for the test to mean anything:

* **Sample interior barycentric points, never vertices or edges.** A first attempt
  probed at vertices and reported violations in every configuration — all false.
  Vertices sit exactly on brush boundaries, where the probe's inclusivity is
  ambiguous and every answer is a coin toss.
* **Exclude brushes with an authored `floor_y`.** They pin their bands by hand and
  the classifier honours that. Before excluding them, the three pinned brushes
  across the shipped levels accounted for *every* remaining flag (facility_2 #55
  `y=-40 floor_y=-35`, slot7 #18 `y=14 floor_y=22` — the reported errors matched
  their pin offsets exactly, 4.9 and 7.9 WT).

The §7 ledge test passed while the bug was live because it sampled triangles into
`over_ledge` / `beside_ledge` buckets by centroid, and the offending triangle's
centroid fell in the wrong bucket where its top *was* the expected height. **A
hand-picked sampling window hid a defect that an invariant found immediately.**

The simple synthetic room + ledge also did not reproduce it — the fold does cut
there. Three of the six *authored* levels did (facility_2 122 violations, aztec 65,
slot7 91), so the synthetic cases now include adjoining rooms sharing a coplanar
wall, which is what defeats the fold's own cutting.

### Cost

facility_2, release: bake **27.58 ms** (no probe) → 28.48 (probe, no cuts) →
**30.15 ms**, and 20,146 → 21,528 triangles. So the whole feature costs ~9% of a
bake and ~7% of the wall mesh. The bake is memoized per region, so this is paid
per edit, not per frame.

### Test coverage now

- `a_walls_bands_agree_with_its_air_column_everywhere` — the invariant, on five
  synthetic configurations. Fails without `column_planes`.
- `bands_agree_on_the_shipped_levels` — `#[ignore]`d (depends on `levels/*.json`,
  authored data rather than fixtures). Run it after touching the classifier:
  `cargo test --release -p engine --lib bands_agree_on_the_shipped_levels -- --ignored --nocapture`
  Both exclude stair geometry: `append_zoned` emits treads and risers with explicit
  zones rather than through the classifier, so the column rule does not apply to
  them.

---

## 9. Built (2026-08-31): the cornice (idea B), and idea D with it

`cargo test --workspace` = 773 green. Release binary built.

### Zone 4 is the fifth surface at the fourth index

Worth stating plainly, because the two numbers differ. Measured across every theme
on disk:

```
zone 0 (floor)        394 themes
zone 1 (ceiling)      394
zone 2 (lower wall)   394
zone 3 (upper wall)   394
zone 4                  0   <- free
zone 5 (stair/frame)  394
zone 6 (frame floor)  391
zone 7 (brace)          4
```

389 of the 394 define exactly `[0,1,2,3,5,6]`. So the cornice is the **fifth room
surface** a theme can define, while occupying the **fourth index** — the table has 8
slots and went from 7 used to 8. Nothing was displaced: index 4 was a vestigial
flat-colour "legacy tunnel" slot the JS original had and this port never emitted, which
the old code documented on `ZoneDef` and asserted with `zones[4].is_none()`.

`the_shipped_library_defines_no_cornice` now pins both halves — that no library theme
defines one (so no existing level restyles until an author opts in), and that overall
occupancy is still `[0,1,2,3,5,6,7]`, which is the tripwire for the `scheme * 8 + zone`
packing key needing to widen before a *further* band.

### The finding that decided the design

`renderer.rs` draws a zone group as:

```rust
let Some(bg) = zones[g.zone as usize].as_ref() else { continue };
```

**An undefined zone's draw group is silently skipped.** So emitting the cornice for
a theme that does not define zone 4 would not fall back to the upper wall — it would
punch a *hole* against every ceiling in the level.

That makes opt-in mandatory rather than merely tidy, and it answers the question of
whether the 392 extracted themes need editing: **no.** Undefined zone 4 means "no
cornice", the classifier does not split, and those themes classify
triangle-for-triangle as before (pinned by
`a_theme_without_a_cornice_classifies_exactly_as_before`). Duplicating the upper-wall
texture into every theme would have cost an extra draw group and extra triangles per
scheme for zero visual difference.

### What landed

- `textures::CORNICE_ZONE = 4` and `DEFAULT_CORNICE_V = 1.5` WT.
- `ZoneDef.height` / `ZoneJson.height` — **per-theme depth**, which is idea D
  arriving for the cornice: the right depth is a look, not a constant (a picture rail
  and a deep frieze are both cornices). Zones are already a sparse string-keyed map in
  `themes.json` / `user_themes.json`, so persistence needed no schema change.
- `ColumnInputs::top_at_with` — `base_at_with` run upward. The band is measured *down
  from the air column's ceiling*, so trim sits flush in a 6-WT corridor and a 28-WT
  atrium alike.
- `column_edges_near_wall` (was `floor_edges_near_wall`) lost its "below the reference
  height" prune: that was right while only the floor mattered, but a ceiling-measured
  band steps at geometry *above* too.
- `emit_wall_split` now cuts through `split_tris` — the same algorithm it used to
  inline, generalised over the axis — so the no-cornice path is byte-identical while
  three bands cost no extra case analysis. The cornice carries **its own UV origin**
  (`origin[1] = ceiling - depth`) so V starts at 0 at the band's foot; sharing the
  floor anchor would slide the trim by however tall the room is.
- `CORNICE_MIN_UPPER = 1.0` WT: a room barely taller than its own trim gets none.
- Theme editor: zone 4 listed as **"Top wall"**, with a depth slider
  (`CORNICE_RANGE` 0.5–6.0 WT) shown only for that zone.

### The cornice is the first theme property that changes geometry

Every other live edit in the texture editor is pushed to the renderer's material
table under unchanged geometry — which is why `ensure_theme_preview_room` uploaded
the preview once per session and said so in a comment. A band boundary is not a
material, so:

- `textures::CORNICE` is an `RwLock` table seeded from the registry, because the
  registry itself is a `OnceLock` and a depth has to be writable while authoring.
- `push_theme_to` compares the depth, and only on a real change clears
  `theme_preview_uploaded` and re-folds the level (`World::initial_meshes`, which the
  memo cache makes cheap for untouched regions). Guarded on a difference because a
  full re-bake per slider drag would be unusable.
- `region_hash` folds in `cornice_of(b.scheme)` — the third entry in that hash for
  exactly this reason, after `face_tex` and platforms: something outside the brushes
  changes how they classify, so without it the region hands back its pre-cornice bake.

### How a theme opts in

In the TEXTURES panel: pick **Top wall**, choose a texture, drag **depth (WT)**. Or by
hand in `assets/user_themes.json`:

```json
"4": { "texture": "tempImgEd05BC", "repeat": 0.2, "height": 1.5 }
```

### The browse list needed a fifth slot too

The TEXTURES and PAINT theme lists preview each theme as a row of swatches, and that
row was a hardcoded `[Option<TextureHandle>; 4]` over zones 0–3. So the cornice had an
*input* (the editor's zone list) but no *preview*: a theme with one looked identical to
a theme without.

Now `PREVIEW_ZONES = 5`. Two details:

* Stair/frame (5), doorframe floor (6) and brace (7) stay out. They are fittings, not
  room surfaces, and were never previewed — the row is the wall stack, not the theme.
* The 394 themes with no cornice show an **empty** fifth slot, which is exactly right:
  that is what "no cornice" looks like, and it is a slot an author can go and fill. The
  render loop already drew undefined zones as an outlined box, so this cost one array
  length.
* The TEXTURES row dropped from 30 px to 24 px swatches so five occupy the width four
  did — the keep/cut buttons share that row and the panel is a fixed 206 wide. Both
  rows now name the zone on hover, since an unlabelled empty box is otherwise cryptic.

### Still open

- The 392 extracted themes define no cornice, so nothing changes until a theme opts
  in. Whether any of the *shipped* library should get one is a content decision.
- Idea D for the **lower** band (`WALL_SPLIT_V` is still one global 6.0) is untouched.
  The cornice now proves the per-theme-height plumbing works.
- Idea C (shader V-remap) is unchanged as a destination, and would retire the 8-zone
  cap that made zone 4 the last available band.

---

## 10. Built (2026-08-31): editing a theme in place

`cargo test --workspace` = 778 green.

The editor could only ever *branch*: every save went to
`first_free_custom_slot()`, so "open Archives 1, fix the upper wall, save" left
Archives 1 untouched and a near-copy beside it — and there are only
`CUSTOM_SLOTS = 24`.

The write side already took an explicit slot (`save_custom_preset(slot, …)`); what
was missing was anything that remembered where a draft came from.

- `ThemeDraft.origin` — set by `seed_from`, deliberately **not** cleared by editing.
  After a save it points at the slot just written, so a second save goes to the same
  place rather than spawning a copy.
- `overwrite_target()` — the origin, filtered to `SchemeKind::Custom`. A library
  theme returns `None`: those 394 are the reference the custom slots are derived
  *from*, and overwriting one would restyle every level using it with no way back
  short of reinstalling `themes.json`.
- `save_over_origin()` **refuses** a library origin rather than redirecting to a free
  slot. A save that lands somewhere other than where the button said is worse than an
  error message.
- Two buttons now: `Save to "<label>"` (only when the origin is editable, and only
  while the draft is dirty, so a stray click cannot overwrite an unchanged source) and
  `Save as new preset`.
- The `✎` in the theme list now reads **"edit in place"** on a custom slot and
  **"edit a copy"** on a library theme, so the entry point says which you are getting.
- Both save paths share `App::after_theme_saved`, which pushes the slot's materials
  live and — via `push_theme_to` — re-folds the level if the cornice depth moved.

### Testing note

Only the non-I/O paths are covered. `save_custom_preset` writes the real
`assets/user_themes.json`, and a test that edits the author's own theme file to prove
a button works has done more harm than the bug it guards. So the tests pin origin
tracking, the library refusal (which returns before any write), and that a refused
save mutates nothing — and the suite still leaves `user_themes.json` untouched, which
was checked rather than assumed.

---

## 11. Playtest of the cornice: it drew as a hole, exactly as predicted

`cargo test --workspace` = 779 green.

Reported: with a texture picked for **Top wall**, the band showed "the default brown
upper wall texture from facility" instead. It was not showing a texture at all — it
was the **hole** §9 warned about, with the shell's outer skin visible through it
wearing the no-owner default theme.

### Guarding the classifier was not enough

`build_materials` builds `materials[scheme][zone]` once at startup, and it built a
bind group **only for zones that had a texture then**:

```rust
let Some(zdef) = zone else { continue };
let Some(name) = zdef.texture else { continue };
```

No theme on disk defines a cornice, so `materials[scheme][4]` was `None` for all 419
schemes. Two consequences, both silent:

* `set_material_texture` refuses a slot with no existing bind group, so the picked
  texture never arrived.
* `render_*` skips a draw group whose bind group is missing — the hole.

So §9's rule ("emit the band only for a theme that defines it") protected the
*classifier* while the *renderer* still could not receive the zone. **Every slot now
gets a bind group**, filled with a stand-in (the theme's upper-wall texture) for zones
it leaves undefined. Safe because an unfilled slot is never drawn: the classifier
emits the cornice only for a theme that defines it, and no other zone is optional.

The decision moved into `textures::material_texture_for`, a pure function, so the
invariant is testable without a GPU: `every_zone_slot_resolves_to_a_texture` asserts
all 8 slots of all 419 schemes resolve, and that a defined zone always binds its own
texture rather than the stand-in.

This is the same class of trap as the pre-allocated custom slots — a table built once
from what happens to exist at startup, then asked to accept something new.

### Also from the same playtest

**Default depth 1.5 → 3.0 WT.** At a quarter of `WALL_SPLIT_V` it read as a hairline
against a room's full height rather than as trim. Now half, so it starts visible and an
author dials *down*; per-theme via `ZoneDef::height` either way.

**The preview room now follows whichever theme is on show.** It was rendered only in
edit mode and always tagged with the scratch scheme, so leaving the editor dropped you
back to the level's own textures — reported as "go back to the library and it shows the
default facility textures". `theme_preview_uploaded: bool` became
`theme_preview_scheme: Option<usize>` and the subject is resolved per frame:
the scratch theme while editing, the **armed** theme while browsing (which is what a
click on a row already selects). A 419-theme review list you cannot see was the real
cost of that.

---

## 12. Built (2026-08-31): "platforms count as floors" is a level setting, off by default

`cargo test --workspace` = 781 green.

A room is CSG; a platform is not. Treating a deck's top as a floor is right for a
mezzanine built as a deck and wrong for a catwalk you would rather read as furniture
inside one tall room — so it is a choice, and the safe default is **off**: a level's
walls band from its *carved* shape alone, and dropping a platform in never restyles the
wall behind it.

### The implementation is the absence of one

`evaluate_both` and `region_hash` already take a platform slice, so "off" is simply an
**empty** one:

```rust
pub(crate) fn band_platforms(&self) -> &[Platform] {
    if self.platforms_are_floors { &self.platforms } else { &[] }
}
```

Nothing in the engine learns that a toggle exists, and the memo cache invalidates on
the change for free because `region_hash` covers what it was handed. `rebuild_region`
and the PAINT probe both go through it, so the probe's explanation cannot describe a
band the bake did not draw.

### Where it lives, and why it is persisted

**TOOLS tab**, under its existing PLATFORMS section — that tab is already "what the
platform and stair tools do".

But unlike the rest of that tab it is **persisted in the level file**.
`BuildStyle` decides what the *next* thing built gets, so a session preference is
right for it; this changes how *existing* geometry is classified, so a level has to
carry its own answer rather than inherit whatever the last session left switched on.
`#[serde(default)]` = off, which is both a new level's default and what every file
written before the field says — so no existing level's walls move.

Toggling re-folds every region (a band boundary is geometry, not a material) and bumps
the revision so the LEVELS tab shows unsaved work. Untouched regions hit the memo
cache, which is what makes a whole-level re-fold affordable on a checkbox.

### A test caught the cornice being used

`the_shipped_library_defines_no_cornice` started failing: overall zone occupancy had
become `[0,1,2,3,4,5,6,7]`. Not a regression — the author had saved a cornice into
`custom_03` ("complex custom": `tempImgEd00A6`, height 3.0), which is the feature
working and proved the `height` field round-trips. The assertion was scoped wrong, not
the code: it now checks the **library's** occupancy, since a user theme filling zone 4
is exactly what is supposed to happen. The tripwire it still keeps is that all 8 slots
of the `scheme * 8 + zone` key are claimed, so a sixth room surface needs that key
widened first.
