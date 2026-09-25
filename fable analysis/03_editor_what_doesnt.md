# 03 — The editor: what doesn't work, or will stop working

Grouped as **A. capability gaps** (things an author cannot do), **B. structural debt**
(things that make every new tool cost more), **C. latent defects** (patterns that have
already bitten and will again), and **D. everything around the editor**. Each item says
how soon it matters.

## A. Capability gaps

**A1. You cannot delete, move, copy or select a brush or a room.** *(bites now)*
`SelectionOp::Delete` only reaches `platform.rs::delete_selected`, which early-returns
unless a platform is selected. `regions.rs::remove_brush_from_region` is `#[allow(dead_code)]`
"for the forthcoming delete-brush tool". The only way to remove a room is undo. Nothing
moves a room, duplicates it or mirrors it. `Brush::group` is stamped by the draw and
room tools and read by nothing. For an editor whose game thesis is "build a large,
complex base fast", this is the biggest hole, and it is pure tool work (roadmap 1a).

**A2. The selection primitive is one brush face.** *(bites now)* Patch scope (key `0`)
widens it to the coplanar faces of one room, derived never stored. There is no
multi-face, multi-brush or volume selection, no rubber-band, no "select room". The
2026-08-18 attempt at group push/pull was reverted because the doorframe joined the
group; the fix (bound by `find_room_brushes`, exclude `frame`) is known and was judged
not worth the special case.

**A3. Nothing is authored off-axis except props.** *(by design; reconsider deliberately)*
No slope you can select or texture (ramps are derived shells, and `paint.rs` refuses
them). No 45 degrees. No arch. The docs' verdict, fixed increments if ever and never
arbitrary, is correct. Roadmap 3a gives the narrowest useful version.

**A4. Ortho drafting exists for exactly one tool.** *(bites soon)* The room plan tool
proved the seam is cheap. Doors, corridors, retexture and push/pull still need the
first-person crosshair, so laying out a base is plan tool, fly inside, cut doors, fly
out. Roadmap 1c.

**A5. No numeric feedback.** WT counts are eyeballed; there is no dimension readout on
the ghost, no grid on faces, no measurement. Small, unglamorous, constant friction.

**A6. Texture control is theme-level.** UV scale and offset live on the theme; a face
override can swap theme and force a zone but cannot shift, rotate or scale UVs on that
face alone. Decals and signage placement do not exist as a concept. The library's 1016
textures include signage the author has no way to place as a sign.

**A7. Every room is a lit box with no sky.** The renderer has no skybox, no fog, no
transparency for world geometry, and ignores the vertex-colour attribute on level
meshes. Courtyards, windows and water are all off the table until the render layer
grows one or two passes (roadmap 2d to 2g).

**A8. Nav is invisible until you press Calculate.** Nav bakes once at `G`; the NAV tab
is manual because a full bake is about 0.5 s. A room that strands hunters is silent
while you build it. Reasonable today; a per-region dirty flag with a background bake
would fix it.

## B. Structural debt

**B1. The armed-tool state is about ten loose `Option` fields on `World`** (`opening_tool`,
`place_tool`, `draw_phase`, `room_phase`, `platform_phase`, `vent_tool`, `ladder_tool`,
`prop_tool`, `light_tool`, `spawn_tool`). Twelve call sites re-implement "disarm the
others" by naming each other's fields, and the sets **differ**: `placement.rs` clears
`prop_tool` but not light, spawn, ladder or vent; `draw.rs` clears none of those five.
Adding a tool means editing twelve blocks correctly. Twenty-two `is_*` predicates exist
because the app needs to ask which one is armed. This is the highest-value refactor in
the editor and the first roadmap item (0a).

**B2. Undo wrapping is split between `World` and `app.rs`.** Some tools record their own
checkpoint (gizmo, ladder, door, light, prop, spawn pad); for others the *app* wraps the
call in `with_undo` (opening, place, vent, draw, room, stairs, retexture, paint). So
`confirm_opening`, `confirm_stairs`, `vent_click`, `confirm_draw` and `confirm_room` are
undo-less from any caller but `app.rs`: headless levelgen, tests, a future UI. The
invariant lives in a call-site convention no type enforces.

**B3. Polygons carry no attributes through the fold.** `csg::Polygon` is vertices plus a
plane. Everything in `uv_zones.rs` that guesses an owner (`FaceIndex`,
`owner_from_candidates`, `straddle_planes`, `claim_tol`, `MAX_STRADDLE_FRAGMENTS = 256`)
is several hundred lines of correct, clever repair that exists because one `u32` is
missing from a struct. It also means any non-AABB face has *no* owner, which is the real
blocker on angled geometry, more than the UV projection itself. Roadmap 0c.

**B4. Eight texture zones, all taken, packed as `scheme * 8 + zone`.**
`DESIGN_EDITOR_FLEXIBILITY.md` says zone 4 is free; it is not. It is the cornice now
(`textures.rs::CORNICE_ZONE`, "the last free slot"), and zone 7 is double-booked for
brace and railing. Bevels, trim bands, arches and any second band variant have nowhere
to go until the key and the `[Option<ZoneDef>; 8]` table widen (roadmap 0b). *Stale
doc: fix the flexibility doc's claim when you touch this.*

**B5. `World` is a roughly 180-field god struct** mixing editor state, combat, AI toggles,
audio and economy in `world/mod.rs` (3.8k lines), and `app.rs` is 7.1k lines with a
2,870-line `build_egui_frame` forced into a snapshot, closure, apply sandwich by one
avoidable borrow. `REFACTOR_APP.md` diagnoses it correctly and its plan is sound. No
test covers any of `app.rs`, so it is also where a smaller model is most likely to break
something invisibly.

**B6. Return-type schism.** Confirms return `Option<RegionMesh>`, `Vec<RegionMesh>` or
`bool` depending on the tool. The ten-line explanation of why it must be a `Vec` is
copy-pasted verbatim into four files. The scroll wheel is a ten-branch, order-sensitive
`else if` ladder in `app.rs` over nine differently shaped `adjust_*` signatures.

**B7. Two coplanar-group algorithms** (`patch.rs::patch_ids` and
`draw.rs::coplanar_face_group`) with three documented deliberate differences. Honest,
but still two.

## C. Latent defects and the patterns behind them

**C1. `Vec<RegionMesh>` narrowing.** Any tool that adds a brush can bridge regions and
trigger a recluster that returns a mesh per region; `.into_iter().next()` drops the rest
and leaves stale geometry on screen. Fixed in the draw, room, opening, stairs, placement
and vent paths; still present, safely and with comments, in `editing.rs::set_scheme_along`
and `paint.rs`. Any *new* brush-adding tool will meet it. The region-merge mesh-loss bug
the room tool exposed ("a doorway between two rooms cut nothing") was the same family.

**C2. Owner guessing can be wrong, and PAINT is the manual fix.** `FaceTex` exists "for
two jobs that turn out to be the same job", one of which is repair. Straddle splitting is
capped at 256 fragments per triangle; past that it stops cutting. A dense level with many
small adjoining rooms of different themes is where to look for the next visible
mis-texture.

**C3. Nav cannot see sub-cell geometry.** Two of seventeen stair runs in `slot1` had
treads shallower than one 0.25 m cell and severed 15% of the level; fixed with a
stair-local step limit, not a general one. Every Channel-B feature must be mirrored as
nav boxes by hand or hunters walk through it. The ramp overlay and vent portals are the
existing precedents; there is no automatic check that a render shell has a nav twin.

**C4. The fold is O(brushes x soup) per region and a connected base is one region.**
The near/far partition keeps the BSP local, but each brush still scans the whole soup.
There are two benchmarks in `world/tests.rs` (`bench_rebake_slot1` and
`bench_scaling_connected_region`); run them before believing a slowdown theory. On a
very large single-region base this will bend before anything else in the editor does.

**C5. Keys are exhausted.** The room tool is radial-only because "there is no letter
left"; egui swallows `Tab` unconditionally; the numpad had to be split from the number
row. New tools should assume radial plus panel only.

## D. Around the editor

- **Feature-flag sprawl**: 18 `std::env::var` sites plus about 20 runtime boolean
  toggles on `World`. `PlayConfig` plus `PlayPins` is the right migration and is under
  way; finish it rather than adding a 19th variable.
- **Two `Door` types**: the live ECS door and the dead breakable-panel `world::Door`
  (disabled 2026-07-16, still cleared unconditionally in `prepare_spawn`). Either revive
  it for the game loop or delete it.
- **Docs rot**: `DESIGN.md` still describes patch phase, economy and destruction as the
  core loop with no note of the pivot; 13 of 34 docs are orphaned (including everything
  from the last editor week); there is no repo-level index (`START_HERE.md` indexes only
  the reverse-engineering track) and no `CLAUDE.md`; `index.html` advertises a terrain
  mode; seven `probe_*.log` files, `hunter_telemetry.log` and a 1.2 MB `anim_log.txt`
  sit in the tree.
- **Tests**: `world/tests.rs` is 5,692 lines; `mod tmp_chain` is a debugging probe left
  checked in; `platform.rs` (1,167 lines), `stairs.rs`, `gizmo.rs`, `light.rs` and
  `prop_gizmo.rs` have zero local tests; `levelgen/` has none at all.
- **Licensing**: 1024 Rare textures, 50 rigs, PD clips, GE and PD weapon models and
  13 MB of GE audio are committed with no LICENSE, NOTICE or fair-use note. The only
  stated position covers the *code* in `reference/`. Fine for a private project; the
  single largest unexamined risk if it is ever shown publicly.
