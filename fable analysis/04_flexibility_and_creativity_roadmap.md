# 04 — Flexibility and creativity: what to add

The question was: what could we add to gain more flexibility and creativity in the
editor? This is the answer, ranked. Every item is grounded in what the code can already
do, says what it costs and what it touches, names the trap that will catch a first
attempt, and gives an acceptance bar.

Cost scale: **S** = a day or two for a capable model, one or two files. **M** = a week,
a handful of files, a design doc. **L** = multi-week, touches an invariant, needs a
playtest cycle. "Engine diff" says whether `crates/engine` changes at all.

Two framing facts from `DESIGN_CSG_FLEXIBILITY.md` still hold and organise everything
below. First, the BSP kernel is fully general; every 90 degree assumption lives *above*
it, in `Brush`, the UV classifier, the nav voxeliser and the tools. Second, there are
two channels: Channel A (real CSG, zero engine diff as long as the output is boxes) and
Channel B (hand-emitted zoned quads with their own collider and nav boxes). Items are
tagged with the channel they ride.

---

## Tier 0 — Unblockers: make everything after them cheaper

### 0a. One armed-tool enum and `disarm_all()`   — S, no engine diff

Replace the ten `Option` fields on `World` (`opening_tool`, `place_tool`, `draw_phase`,
`room_phase`, `platform_phase`, `vent_tool`, `ladder_tool`, `prop_tool`, `light_tool`,
`spawn_tool`) with one `armed: Option<ArmedTool>` whose variants carry each tool's phase.
The twelve divergent disarm blocks collapse to one method. The 22 `is_*` predicates
become one match. `radial::Tool` already enumerates most of the variants.

*Trap:* the room tool releases the cursor and swaps the camera; `App::sync_room_cursor`
derives cursor state from `is_room_tool` once per frame. Keep that derivation, point it
at the enum. *Acceptance:* every existing tool test passes; arming any tool from any
other leaves exactly one armed; the scroll ladder in `app.rs` becomes a single match on
the enum.

### 0b. Widen texture zones from 8 to 16   — S, engine diff (small)

`textures.rs::Scheme::zones` is `[Option<ZoneDef>; 8]`, the renderer's material table is
`[Option<BindGroup>; 8]`, and `uv_zones.rs::ZonedBuilder::finish` packs the draw-group
key as `scheme * 8 + zone`. Change all three to 16. Every zone below is blocked on this.

*Trap:* `themes.json` has 394 entries with 8-slot zone arrays; the loader must accept
both lengths. The classifier has an `undefined zone renders as a hole` behaviour that
made the cornice opt-in; keep that. *Acceptance:* the existing zone tests pass, the
theme preview room renders, a theme with a zone 9 defined draws it.

### 0c. Attributed polygons through the CSG kernel   — M, engine diff (mechanical)

Add `face: u32` to `csg::Polygon` and carry it through `split_polygon`, `clip_polygons`,
`Node::build`, `into_all_polygons` and `polygons_to_mesh` (about six touch points, all
copy-the-field). `brush_to_polygons` stamps `(brush_id, face_slot)`. `classify_soup`
then *reads* the owner instead of guessing it.

Payoff is double. It deletes `FaceIndex`, `owner_from_candidates`, `straddle_planes`,
`claim_tol` and `MAX_STRADDLE_FRAGMENTS` (several hundred lines of repair), it makes
PAINT exact rather than "the manual answer that always wins", and it is the
prerequisite for any face that is not one of the six AABB faces (Tier 3).

*Trap:* coplanar polygons never split each other in a BSP, so a floor shared by two
rooms of different themes still needs *one* cut at the room boundary. Keep the straddle
cut for that single case, driven by the now-known owners, and delete the rest.
*Acceptance:* every `uv_zones` test passes unchanged; `facility_2` renders identically
(diff the classified soup by (scheme, zone) histogram before and after); the paint probe
reports the same owner for every triangle it did before.

### 0d. Land `REFACTOR_APP.md` step 1 and 2   — M, no engine diff

Already planned and correctly diagnosed. Do it before any substantial panel work. Not
repeated here.

---

## Tier 1 — Authoring throughput: the biggest creative win is speed

These need **no engine change**. `Brush` is `Copy + Serialize`; a room is
`find_room_brushes` (already used by retexture, bounded by frames); a region re-folds
from any brush list. Everything here is tool work on data that already exists.

### 1a. Rooms and brushes as selectable objects: delete, move, duplicate, mirror   — M

Add a `Selection::Room(Vec<brush_id>)` and `Selection::Group(group_id)` alongside the
face selection. Verbs: **Delete** (finally wire `remove_brush_from_region`, which is
dead code waiting for exactly this), **Move** by whole WT along an axis (offset every
member; the fold and recluster handle merges), **Duplicate** with an offset (fresh ids,
same `group`), **Mirror** about an axis through the selection's bbox centre
(`x' = 2c - x - w`; swap `face_tex` slots Min/Max on that axis; stairs mirror by
flipping `side` and `direction` as appropriate). Room selection picks up the room's
stairs (`void_ids` membership), doors, lights and props inside the bbox.

Ortho view helps enormously here (1c), but the crosshair version works: aim at a room,
press the verb from the radial's Selection ring.

*Traps:* (1) a room's frames are shared with its neighbour, so "delete room" must decide
whether the doorway frame goes; the honest rule is that a frame belongs to whichever
room the flood-fill reached it from and is deleted with it, leaving a wall the neighbour
can re-cut. (2) The commit must take `Vec<RegionMesh>` (rule of the road). (3) After a
move, `floor_y` on the moved brushes must shift by the same Y offset or the wall bands
jump. *Acceptance:* delete, move, duplicate, mirror each round-trip through save/load
with identical folded geometry; undo restores exactly; a mirrored room's door opens the
same way.

### 1b. A stamp (prefab) library   — M, no engine diff

A stamp is a level-file fragment: brushes + stairs + entities relative to an anchor, in
JSON, with a display name, a thumbnail and a theme-remap table. Store shipped stamps in
`native/assets/stamps/` and user stamps beside `user_themes.json`. Verbs: **Save
selection as stamp** (from 1a), **Place stamp** (ghost follows the cursor in ortho or the
crosshair face, rotates in 90 degree steps via the mirror + axis-swap from 1a, snaps to
WT, click commits), **Retheme on place**.

Ship a starter set that answers `DESIGN.md`'s pacing risk directly: a stairwell tower
(the square-spiral stair from `DESIGN_IDEAS.md`), a corridor elbow and tee, a guard
room with a door, a two-storey atrium with a landing, a vent run, a pit trap. Each is
just a saved selection from a hand-built level.

*Trap:* brush ids and group ids must be re-allocated on place (`next_brush_id`), and
entity `AuthoredId`s likewise; the ECS `AuthoredId -> Entity` map exists for this.
*Acceptance:* place, undo, place again yields identical geometry; a stamp saved from a
level and placed into an empty level bakes to a single walkable nav component.

### 1c. The plan editor: ortho drafting for more than one tool   — M, no engine diff

`World::view_proj` is the seam the room tool proved. Extend the ortho mode so that,
while it is active, the *existing* tools accept a plane hit instead of a crosshair hit:
click a shared wall segment between two room outlines to cut a **door** there; drag a
room outline's edge to **push/pull** that wall; drag a rectangle to **select** brushes
(1a); a **corridor** verb that takes a polyline and emits a run of subtract boxes of
width w at the current base height. Retexture by clicking an outline.

*Trap:* a side-view ray runs along the drafting plane; the room tool already returns
`None` there and says so. Keep that. Slicing by Y (bright footprints straddle the plane,
dim ones don't) is what makes multi-storey plans legible; every plan-mode tool must
respect the slice. *Acceptance:* a four-room base with doors can be authored entirely in
plan view and is one walkable component in the NAV tab.

### 1d. Numeric readout, face grid, measurement   — S, no engine diff

Print the ghost's WT dimensions next to it (the HUD font drops unatlased glyphs; digits
and `x` are present). Draw a 1 WT grid on the tinted surface while drawing (the surface
tint channel exists). A measure verb: two clicks, a distance in WT and metres.

### 1e. Section draw: a side-view outline extruded along a wall   — M, no engine diff

The draw tool takes a rectilinear outline on a face and extrudes it perpendicular to
the face. Add a mode where the outline is drawn on a **wall** and extruded *along* the
wall's length instead (or equivalently, drawn in a side ortho view and extruded along
the room's depth). Every result is still AABBs after decomposition. This gives stepped
and vaulted-looking ceilings, clerestories, split-level floors, raised galleries,
alcove runs and sunken channels, all from the machinery that exists
(`axis_lock`, `rect_decompose`, `segment_self_intersects`).

*Trap:* decomposed rects must share one `floor_y`, exactly as the draw tool learned.
*Acceptance:* the existing draw tests pass; a U-shaped section extruded along a
corridor produces a single region with no internal wall
(`adjacent_decomposed_brushes_leave_no_internal_wall` pattern).

---

## Tier 2 — Visual richness inside the axis-aligned world

Mostly Channel B, mostly cheap, and each one changes how *every existing level* reads.
The renderer is the constraint here more than the CSG.

### 2a. Bevels (45 degree floor/wall and wall/ceiling runners)   — M, engine diff, needs 0b

Fully specified in `DESIGN_EDITOR_FLEXIBILITY.md`. Derive the fillet strips from the
folded soup's concave right-angle edges (handles doorways, pits and multi-brush rooms
automatically), emit as zoned quads into the collider and render mesh, never into nav.
Per-room toggle rides `find_room_brushes`. The one stale claim in that doc is that zone
4 is free; it is not. Do 0b first and give bevels zone 8.

### 2b. Trim, baseboard and dado bands   — S/M, engine diff, needs 0b

Cheaper than bevels and arguably sells "designed room" harder. The wall classifier
already splits at `WALL_SPLIT_V` and carries a cornice band measured down from the
ceiling; a skirting band measured up from the anchor and a dado at a theme-set height
are the same `emit_wall_split` idea with two more boundaries. Zones 9 and 10.

### 2c. Arches and vaulted doorways   — M, engine diff (Channel B)

A decorative reveal inside a rectangular opening: a half-cylinder of zoned quads at the
top of a door or hole frame, closing the rectangle down to an arch. Collider gets the
curve; nav is unaffected because the frame void is still a rectangle. Author it as a
flag on the frame brush (`arch: bool`), so it rides save/undo/recluster for free.

*Trap:* the arch lowers headroom at the edges of the opening while nav still believes
the full rectangle is clear, so a hunter can clip its head through the arch shell.
Bound the arch depth to one WT below the frame top and accept it, as GoldenEye did.

### 2d. Sky-open rooms and fog   — M, engine diff

A `sky: bool` on a subtract brush: the classifier drops its ceiling triangles (the same
exact-plane strip `strip_shell_skin` already does for the outer skin) and the renderer
draws a skybox or sky gradient behind, plus one directional "sun" light added to
`shade()`, plus a distance fog uniform (fog currently does not exist in any shader and
is a five-line addition). Nav is unaffected. This is the cheapest route to GoldenEye's
Surface, Dam and Runway feel and to courtyards in a base.

*Trap:* the wall band probe walks the air column upward looking for a ceiling; a
sky room has none. Give the probe a cap. *Acceptance:* a sky room's walls band from
their floor; a rain-free "outdoor" level renders with the sun casting shadows into
adjacent enclosed rooms.

### 2e. Glass   — L, engine diff

An `Op::Add` brush flagged `glass`: its faces go to a new alpha-blend pipeline drawn
after the opaque pass, sorted back to front per region group; its collider lives in a
separate rapier group so hitscan passes through it (or shatters it: remove the brush,
re-fold). Facility's control-room windows and Frigate's bridge are the reference.

### 2f. Water volumes   — M/L, engine diff

A subtract brush flagged `water`: the top face renders as a translucent scrolling plane
(needs 2e's blend pipeline), the volume slows the player and applies a nav cost to
hunters (`DOOR_COST` is the precedent for an overlay cost). Frigate, Dam.

### 2g. Baked vertex-colour ambient occlusion   — M, engine diff

`TexVertex.color` exists and is written white; `shader_textured.wgsl` never reads it.
The original JS editor had `lightBaker.js` and `ambientOcclusion.js`. Bake AO per vertex
by casting a handful of rays into the region's own trimesh through rapier at bake time,
subdividing long edges first so corners darken smoothly. Instantly grounds every room.
Optional grime: darken toward the floor band.

*Trap:* the fold emits unwelded vertices per polygon; AO must be sampled per position
and shared, or seams appear at every polygon edge. Hash by rounded position.

### 2h. Decals   — S/M, no CSG change

An ECS entity: a quad with an alpha-tested texture from the library, placed on a face
with the prop gizmo, drawn with a small depth bias. Signage, grates, posters, warning
stripes, blood. The 1016-texture library already contains the signage; this is the
missing verb for it.

### 2i. Per-face UV controls in PAINT   — S, engine diff (small)

`FaceTex` gains `uv: Option<FaceUv { offset: [f32;2], rot90: u8, scale: f32 }>`. The
classifier applies it after projection. Rotation in 90 degree steps only, to stay exact.
Hash it in `region_hash`. Lets an author align a panel texture to a specific wall
without editing the theme.

### 2j. Light presets, flicker and auto-placement   — S, no engine diff

Preset colours and ranges in the LIGHTING tab (sodium, fluorescent, red alarm), a
flicker curve on `PointLight`, and an "auto-light this room" verb that drops one light
at the room's ceiling centroid. `DESIGN_IDEAS.md` calls auto-lighting a foundation for
darkness-based misdirection, not polish.

---

## Tier 3 — Breaking the 90 degrees, deliberately

The docs' analysis is right: fixed increments only, never arbitrary, and only if the
box grid is genuinely limiting design. Here is the narrowest version that still pays.

### 3a. The Add-only 45 degree wedge   — L, engine diff, needs 0c

A `Shape::Wedge { axis, corner }` on `Brush`: an AABB cut by one 45 degree plane through
two opposite edges, so a triangular prism. Restrict it to `Op::Add` placed inside a
room. Because it only adds solid inside an existing void, it never has to be a room
shell and never meets the region shell logic.

What it needs, and why each is small once 0c exists:
- **Fold:** the prism is convex; `brush_to_polygons` emits five polygons instead of six.
  The kernel does not care.
- **Owner and UV:** with attributed polygons the sloped face carries its own id; give it
  a seventh `face_slot` and project UVs in the face's own frame (u along the horizontal
  in-plane axis, v along the slope, scaled by sqrt 2 so texels stay square). This is
  exactly what the stair emitters already do by hand.
- **Nav:** `contains()` is box AND half-space. `solid_at` is a point test.
- **Picking:** `pick_face_hit` tests five planes instead of six.
- **Serde:** `#[serde(default)]` shape = box.

What it buys: cut corners in rooms (diagonal sightlines, the one gameplay argument for
angles), buttresses, chamfered pillars, and, lying on its side, a **real ramp** the
player and hunters both understand (the `NavRamp` overlay already exists for the
stair-ramp shell). Subtractive wedges (chamfering a room's own corner) are the second
step and need the shell logic; do not start there.

*Trap:* the `DESIGN_CSG_FLEXIBILITY.md` warning about dominant-axis ties at exactly 45
degrees is real for the *classifier*, which is why this item is gated on 0c: with owners
known, `Axis::dominant` is never consulted for a wedge face. *Acceptance:* a wedge in a
room folds with no internal faces; its slope face textures without stretch (measure the
UV derivative); a hunter walks up a wedge ramp in `ai_testbed`; save/load round-trips.

### 3b. What not to do

Arbitrary yaw on brushes (five times the work of 3a for ten percent more expressiveness,
per the docs, and I agree). Curved CSG (do curves as Channel B shells and rotatable
props: a cylindrical column prop, a pipe run, an arched doorway shell). Terrain, unless
Game B from `DESIGN_IDEAS.md` is actually chosen; the JS prototype's terrain and cave
code is the reference if it ever is.

---

## Tier 4 — Procedural and generative creativity

The headless builder and the skill make this the most under-exploited direction in the
repo.

### 4a. A JSON or text form of the intent API   — M, no engine diff

Today each `levelgen` design is a Rust function, so authoring a level means recompiling
the game. Give `LevelBuilder` a JSON front end (`{"room": {...}}, {"passage": ...}`) so
the `build-level` skill, or any script, writes a level without touching Rust, and add
**Import intent file** to the LEVELS tab. This is also the sharing format that
`DESIGN_IDEAS.md` notes unlocks agent-authored bases and asymmetric multiplayer.

### 4b. Port the HouseBuilder procgen   — M, no engine diff

`DESIGN_PROCGEN_HOUSEBUILDER.md` estimates 150 to 200 lines of Rust for a recursive
branching floorplan generator that emits into the builder API that exists. A "Generate
base" button with a seed, size and style in the LEVELS tab, followed by normal editing,
turns the editor into a "generate, then sculpt" tool. Verify every output with the
existing analyzer before writing it.

### 4c. Rule-based decoration   — S/M

Auto-props (crates in corners of storerooms, a table in a guard room), auto-lights (2j),
auto-trim by theme group. Each is a pass over `find_room_brushes` output.

### 4d. Symmetry mode   — S/M

Mirror-edit live about a chosen plane for multiplayer arenas: every committed brush set
is duplicated through the mirror from 1a. Trivial once 1a exists.

### 4e. Export and sharing   — S/M

Level plus user themes plus stamps as one zip; a GLB export of the folded level (the JS
prototype had `GLBExporter.js`) for showing a base off outside the game.

---

## Tier 5 — Editor features the *game* needs to make building creative in play

These are gameplay, but each is a volume or prop the editor has to expose, and without
them a flexible editor is only flexible in BUILD. Short, with pointers, because they
belong in a game-loop plan rather than here.

- **One-way drops and void hazards**: a subtract brush flagged `void` with a kill or fall
  trigger; nav treats it as impassable for hunters. Herding tool.
- **Trigger volumes**: a box entity with an action (open/lock a door, sound an alarm,
  spawn a wave). The ECS `Interactable` and `Door` components are the seam.
- **Breakable wall panels**: revive `world::Door` and `breach_tick` (dead since
  2026-07-16) as a placeable panel over a hole, with HP and a breach noise ping.
- **Noise-maker and decoy props**: a prop with a timer or remote trigger that calls
  `alert_enemies_to_noise`, which exists.
- **Dark zones**: lights already have ranges; a per-room "no auto light" and a
  destructible light entity give the darkness mechanic from `DESIGN.md`.

---

## If only three things get built

1. **1a + 1b** (select, move, duplicate, mirror, stamps). Authoring speed is the creative
   multiplier this game's thesis depends on, and it is zero engine risk.
2. **2d** (sky rooms + fog + a sun). One flag and two small shader additions change what
   kinds of places can exist.
3. **0c then 3a** (attributed polygons, then the wedge). The one architectural change that
   both shrinks the codebase and opens the only angle worth having.
