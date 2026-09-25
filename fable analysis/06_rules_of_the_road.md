# 06 — Rules of the road (for the model that picks this up)

The repo already encodes most of these in comments and memory notes. This is the
distilled list, so a smaller model does not have to rediscover them.

## Invariants that must hold after any editor change

1. **A save/load round trip must not change the folded geometry.** Regions fold in
   ascending brush-id order; `rebuild_from_flat` restores that order after a recluster.
   If you add a brush field, decide explicitly whether `region_hash` includes it
   (`regions.rs`) — geometry and texture-affecting fields yes, bookkeeping like `group` no.
2. **Anything that adds a brush must take the whole `Vec<RegionMesh>`.** A new brush can
   bridge regions and trigger a recluster returning one mesh per region. Use
   `with_undo_many`. Only edits that pass an already-mapped brush id and change no
   geometry may narrow to one mesh.
3. **One box set drives render, collider and nav** for anything Channel-B. If you emit a
   shell, emit its nav boxes (see `StairDesc::solid_boxes`, `stair_run_boxes`) or a
   documented overlay (`NavRamp`, `NavVent`). There is no automatic check; write a test.
4. **Undo goes through a snapshot.** New tool commits either call `record_undo` /
   `with_undo` inside `World` (preferred) or the app wraps them. Do not add a third way.
5. **Themes are persisted by name; brushes are AABBs in WT; the claim tolerance is slop,
   never a cut boundary.** Cutting at the tolerance haloed every vent mouth once.
6. **Crouch shrinks height, not radius. Vents are one cell narrower than the agent by a
   `const` assert.** Leave both alone.
7. **New radial entries go on the end of a ring.** A fixed layout is the whole value.
8. **Don't bind an editor key to Tab** (egui swallows it) and assume no letter is free.
   New tools: radial + panel.
9. **Every new tool must join the mutual-exclusion set** — today that means editing all
   twelve disarm blocks, which is why roadmap 0a comes first.
10. **The nav grid is the runtime. Do not attempt a navmesh again** without reading
    `HANDOFF_RECAST_NAVMESH.md` and the memory note; two attempts failed the same way.

## The don't-do list

- Don't put 45° segments in the draw or room tools. Diagonals are a separate, deliberate
  project (roadmap 3a), not a draw-tool feature.
- Don't teach `Brush` about polygons to get concave shapes; decompose to rectangles.
- Don't retry group push/pull across a coplanar surface without the frame exclusion; it
  was built and reverted because it widened doorframes.
- Don't drive the game window yourself to playtest; it hijacks the user's machine. Hand
  off with a specific brief. Headless: `world/tests.rs`, `ai_testbed`, `levelgen`, `probe_hunt`.
- Don't add a 19th env var. Add to `PlayConfig` and register a `PlayPin`.
- Don't compare textures by filename; compare by content hash.
- Don't "fix" the cold first build; 90–137 s once is expected. Incremental release is ~1.4 s.
- Don't build with `--profile release-dist` for handoff; `cargo build --release -p game`.

## How to start a task here

1. Read the module header of every file you will touch. They are design documents.
2. Read the memory index (`MEMORY.md`) entry for the feature area, then the design doc
   it points at, then its "what shipped / what was wrong" section.
3. Find the closest existing tool and copy its shape: `opening.rs` for a rect-on-face
   tool, `platform.rs` for a phase machine with an Esc ladder, `draw.rs`/`room.rs` for
   integer-grid outlines, `prop.rs` for an ECS entity with a gizmo.
4. Write the acceptance test first in `world/tests.rs` or the tool's own `#[cfg(test)]`,
   driving `World`'s public API with no GPU. The existing tests show the pattern.
5. Run the two rebake benchmarks if you touched `regions.rs`, `csg_runtime.rs` or
   `uv_zones.rs`.
6. Build release and hand off with a brief that says exactly what to try and what
   "wrong" would look like.
7. Update the design doc's status line and, if the build taught you something the doc
   had wrong, add it under a heading that says so.

## Acceptance bars used in this repo (reuse them)

- Editor geometry: the fold test asserts triangle presence/absence on specific planes,
  not just "non-empty" (`adjacent_decomposed_brushes_leave_no_internal_wall`).
- Nav: `probe_hunt` sweep success rate against the captured baseline; the NAV tab
  reports one walkable component.
- Texturing: a guard test that a decomposed shape shares one band anchor
  (`a_wall_shape_spanning_heights_still_shares_one_anchor`).
- AI: `ai_testbed` scenarios that assert defect *classes* via the jank monitor.
- Level files: an unknown field survives a load/save; a v1 file loads.
- Shaders: the `naga` compile test.

## Where a smaller model is most likely to break something

- `app.rs`: no tests, one borrow forces a snapshot/apply sandwich; a change to a tab
  arm can silently drop a deferred write. Prefer to land `REFACTOR_APP.md` step 1 first
  if you must do substantial UI work.
- `world/mod.rs`'s `World` struct: adding a field is easy; forgetting it in `snapshot()`
  or the level file is invisible until undo or load loses it (this exact bug happened
  with level name/ambient/hotkeys).
- `uv_zones.rs`: a tolerance change can be right on one level and haloed on another.
  Test on `facility_2` (76 brushes, vents, multiple themes), not a fresh room.
- Anything that adds a `Brush` field: decide `region_hash`, serde default, snapshot,
  `BrushInfo`, and the surface probe readout together.
