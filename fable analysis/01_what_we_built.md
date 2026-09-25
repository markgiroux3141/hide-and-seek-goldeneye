# 01 — What we built

An inventory for someone arriving cold. Numbers measured at `c634982`.

## Shape of the repo

```
src/                    the original three.js prototype (~15k LOC JS) — kept as the
                        "behavioural oracle" for the port; has terrain + caves + light bake
                        the port dropped. Undocumented as legacy; index.html still offers
                        a terrain mode the shipping engine doesn't have.
native/
  crates/csg/           386-line BSP CSG kernel, vendored from CSG.js. Fully general;
                        union + subtract only (no intersect). No attributes on polygons.
  crates/engine/        ~21k LOC, domain-agnostic: geometry (csg_runtime, structures),
                        render (renderer 4.2k, uv_zones 2.1k, textures), sim (physics via
                        rapier, nav 2.3k voxel grid, ORCA avoidance, ragdoll), skeletal
                        (clips, layer stack, IK), assets (glTF, OBJ, textured models), audio.
  crates/game/          ~73k LOC: app.rs (7.1k, the winit/egui shell), world/ (the
                        authored scene + every editor tool, ~25k), enemy.rs (4.4k),
                        pdsim/ (Perfect Dark bot model), combat/, ecs/ (hecs scaffold),
                        levelgen/ (headless intent-level authoring), radial/, hud/.
  assets/               1024 textures + 394 themes, 44 GE + 6 PD bodies, 36 PD clips,
                        27 weapons, 59 props, 13 MB audio.  ~43 MB tracked.
  levels/               gitignored; local authoring output (JSON, format v4).
tools/                  Python: PD ROM asset decoders (model/anim/tex → GLB), GE Setup
                        Editor reverse-engineering probes, theme extraction, turret split.
reference/              gitignored ~2.2 GB: three decomp clones + extracted ROM assets.
*.md at root            34 design/handoff docs. 13 are referenced by no other doc.
.claude/skills/build-level   the one skill: drives levelgen headlessly.
```

Dependency direction is real and one-way: `csg → engine → game`. The engine has no
notion of an entity, a tool, or a hunter. That boundary has held for seven weeks
under heavy churn, which is worth noting.

## The three tracks the history shows

Commits per ISO week: 19, 29, 12, (gap), 2, 35, 30, 17. Three overlapping efforts:

1. **Engine port + editor (Jul 14 → Jul 26, then Aug 17 → Sep 1).** Day-one port of
   the JS editor to wgpu + rapier + a grid nav. The incremental region rebake landed
   `ec3d721`. The last two weeks were the most productive editor stretch of the project:
   radial menu, block stairs, crouch, vents as nav portals, ladders, PLAY tab, face paint
   with the straddle fix, plane platforms and ramp stairs, coplanar patch editing, the
   wall-banding air-column probe, custom theme save, the room plan tool with ortho
   drafting views, and a new-level start screen.
2. **Enemy AI + animation (Jul 20 → Jul 31, ~30 commits).** Procedural layer stack,
   barrel-axis aiming, foregrip IK, RVO/ORCA, head look-at, foot IK + stride warp,
   ragdolls, a utility-scored decision layer over the FSM, and a headless AI lab with a
   jank monitor that names defect classes (stall, thrash, walk-in-place).
3. **Perfect Dark extraction + simulant port (Aug 12 → Aug 18).** ROM → GLB pipeline,
   PD bodies on the shared 15-bone rig, PD's difficulty/personality/zeroing/distance-band
   model, PD's explosion table, PD's 33 guns (then parked: GE guns + PD bodies ship).

Two attempts at a polygon navmesh (hand-rolled, then real Recast) were built, failed
the acceptance harness, and reverted. Both are documented with root causes. The grid
is the nav runtime and that is settled.

## The editor's data model in five sentences

The world is implicitly solid; a room is an `Op::Subtract` box carved out of a derived
shell (`csg_runtime.rs`, `Region::update_shell`). A `Brush` is an axis-aligned box in
integer-ish world tiles (1 WT = 0.25 m) with an op, a theme (persisted by name), a
per-face override array of six slots, and flags for door/frame/vent, plus a `group` id
and a `floor_y` texture anchor. Touching brushes cluster into regions; each region folds
through the BSP in ascending brush-id order into one triangle soup that is both the
Rapier trimesh collider and, after the classifier guesses each triangle's owner and
zone, the render mesh. Undo is a whole-level snapshot of authored data restored through
the same path as load. Everything that is not a box (stairs, platforms, ramps, railings,
ladders) is "Channel B": hand-emitted zoned quads with a separate simplified collider
and a separate set of nav boxes.

## The tool roster

Face tools (crosshair-driven, modal): push/pull with coplanar patch scope, sub-face
carve/extrude, door, hole, pillar, brace, CSG stairs up/down, vent duct, ladder.
Freeform: the 90°-snapped draw tool (`Q`, on a face) and the room plan tool (radial
only, orthographic top-down drafting). Structures: platform, block stairs, connect, with
a move/scale gizmo. Entities via the O panel: props, point lights, spawn pads, pickups,
turrets, with a translate/rotate gizmo. Panels: OBJECTS, TOOLS, PLAY, LIGHTING, SPAWNS,
TEXTURES (browse + custom theme editor with a real CSG preview room), PAINT, NAV
(reachability validation), LEVELS (named catalog). A middle-mouse radial menu mirrors
every verb through the `EditorAction` enum, which is the one dispatch seam.

## What the game is today

BUILD is unconstrained editing. `G` bakes nav once and enters HUNT, which is a Perfect
Dark multiplayer round: authored spawn pads, PD's shortlist spawn rule, 2 s respawn on
both sides, first to 10 kills, up to 16 hunters with a 0..10 difficulty dial, guns and
ammo looted from the floor. There is no build budget, no timer, no wave counter, no
patch phase, no CSG destruction, and the six enemy archetypes in `DESIGN.md` do not
exist. See [05](05_beyond_the_editor.md) for what that means.
