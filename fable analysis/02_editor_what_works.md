# 02 — The editor: what works

These are the decisions that are carrying the editor, in rough order of how much they
carry. A model extending the editor should treat them as load-bearing walls, not as
conventions to be tidied.

## 1. "The world is solid, you carve air"

One primitive (an AABB brush), one implicit shell, two ops. Every room, corridor,
doorway, pit, vent and stairwell void is the same `Op::Subtract`; every pillar, brace
and extrusion is the same `Op::Add`. Because of this, the room plan tool, the draw tool,
the levelgen builder and the hole tool all needed **zero engine change** to land: they
are new ways of *choosing* boxes, not new geometry. `DESIGN_EDITOR_FLEXIBILITY.md` calls
this "a better selection primitive" and it is exactly right. It also means the level
file is tiny, hand-editable and stable across engine rewrites (`world/persist.rs` never
stores a mesh).

## 2. One fold feeds render, collision and picking

`Region::evaluate_both` runs the BSP once; the soup is the Rapier trimesh and, after
classification, the render mesh. A ray hit reports a triangle index the PAINT tab's
surface probe reads back to explain *why* a triangle looks the way it does, using
exactly the classifier the bake used (`Region::brush_infos` is public for this reason).
There is no separate collision authoring and no way for render and physics to drift.
Channel-B structures follow a stated rule — one WT box set drives render, collider and
nav — and the code honours it (`structures.rs`).

## 3. Incremental rebake with a memo cache, and the fold order is pinned

`regions.rs` ports the JS `rebuildAffectedRegions`: only the touched region re-folds,
a 128-entry cache keyed on a hash of authored data makes undo/redo/load nearly free, and
the hash comments name the stale-bake bug each hashed field prevents (cornice depth,
face overrides, platform decks). The recluster path was found to reorder brushes (DFS
order is not authoring order; a Subtract after an Add carves it) and now sorts by
ascending id, with the invariant written down: *a save/load round trip must not change
the folded geometry.*

## 4. Undo is in-memory save/load

`history.rs` snapshots the authored POD rather than recording inverse commands. One
snapshot type covers a dozen tools, and restore goes through the same rebuild path as
load so it cannot drift from it. The room tool adds its own small sketch history for
in-progress corners, correctly reasoning that the level snapshot should not contain
un-committed sketch state. `with_undo` only records a checkpoint when the edit actually
changed something, so a refused pull never leaves a dead step.

## 5. Themes by name, textures by content hash, overrides by face not triangle

Level files store theme *names*, so the 394-theme registry can be reordered freely.
Per-face overrides key on `(brush, axis, side)` because triangles have no identity
across a fold. Both are the kind of decision that is invisible when right and
catastrophic when wrong, and the code comments explain the failure each avoids.

## 6. Exact integer grids where exactness matters

Draw-tool and room-tool vertices are `(i32, i32)` in WT. Self-intersection tests,
rectilinear decomposition and corner-drag validity are therefore epsilon-free. Grid
alignment also makes the "rasterise then greedy-merge" decomposition exact, which is
why concave L/U/T footprints work with no special casing.

## 7. The classifier's repair work is excellent, given its constraint

Triangles come out of the fold unattributed. `uv_zones.rs` recovers the owner by
dominant axis + face-plane coordinate + centroid containment, with a two-tier
tolerance, smallest-volume tie-break, an indexed candidate search, and a straddle
splitter that cuts a triangle spanning two rooms rather than arguing over it. The wall
band anchor is probed per fragment from the actual air column beneath the wall, so a
pit wall bands from the pit floor and a platform deck counts as a floor. This is a lot
of correct code solving a problem the data model created (see [03](03_editor_what_doesnt.md) §B3
and roadmap 0c). It works.

## 8. The tool-design discipline

Every tool: arm → ghost preview → adjust by scroll → confirm by click → Esc backs out
one rung. Constraints are refused, not clamped (a pillar that does not fit says so;
an illegal corner drag stops following the pointer). The `EditorAction` enum is the
single dispatch seam for keys and radial. The room plan tool swapped the camera for an
orthographic one through the one seam (`World::view_proj`), so the renderer, HUD
transforms and the mouse ray all picked it up with no branch. Vents are hunter-proof by
a `const` assert that the bore is one cell narrower than the agent, so no test can
regress it.

## 9. The headless authoring loop

`levelgen/` is an intent API (`room`, `passage`, `pit`, `pillar`, `csg_stair`,
`platform`, `link`) that bakes nav the way the hunt does, prints an LLM-readable report
(floorplans + reachability + loops + perches + headroom), writes a level file and then
**verifies it by loading it through the real `World::load_slot`**. The `build-level`
skill drives it and appends lessons to `LEVEL_DESIGN_HEURISTICS.md`. This is the seed of
every "generative" idea in the roadmap.

## 10. The diagnostics

`nav_issues.rs` separates connectivity from traversability because they have opposite
fixes. `nav_probe.rs` drives a real hunter with the real mover and reports which gate
refused a step. The NAV tab shows it in BUILD. `probe_hunt` and `profile_hunt` bins,
the F10 telemetry dump, and the jank monitor are the instruments that caught the 15%
unreachable-slot bug and the Recast failure. Most hobby editors never get these.

## 11. The prose

Nearly every non-obvious constant explains the bug it fixed and the alternative it
rejected. `patch.rs` opens with a section titled "Related prior art, deliberately NOT
shared (yet)". `room.rs` admits a shipped-wrong constant in bold. Design docs carry
status lines and "what the build learned this document had wrong" sections. For a
smaller model this is the most valuable asset in the repo: the reasons survive.
