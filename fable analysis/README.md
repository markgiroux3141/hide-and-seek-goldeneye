# Fable analysis — BUILD & HIDE, read at commit `c634982` (2026-09-02)

An outside read of the native Rust game in `native/`, with the level editor as the
main subject. Written by Claude Fable 5.1 after a full pass over the editor core
(`world/`, `world/tools/*`, `csg_runtime.rs`, `uv_zones.rs`, `regions.rs`,
`history.rs`) and four delegated audits (editor tools, engine geometry/render/nav,
gameplay/AI/app, docs/lineage/assets). Nothing in the repo was changed. These
documents are meant to be picked up later by a smaller model, so every claim that
matters carries a file pointer and every proposal carries a cost, a blast radius
and an acceptance bar.

## The short verdict

**This is not AI slop. It is a serious, unusually well-reasoned indie engine and
level editor, with a game attached that is not yet the game its design doc
describes.** The long form is in [05_beyond_the_editor.md](05_beyond_the_editor.md).

The evidence in one paragraph: ~95k lines of Rust across three crates, 844 tests
that mostly assert behaviour (a raycast-to-collider round trip, a hunter that walks
to the gun rather than the player, a body that plays the other game's clips), a
genuine BSP CSG kernel with region clustering and memoised incremental rebake, a
wgpu renderer with omnidirectional shadow cubes, a skeletal layer stack with foot
IK and head look-at, ORCA avoidance, a voxel nav with a live door overlay, a port of
Perfect Dark's simulant model cited to decomp line numbers, an asset pipeline that
decodes N64 model/animation formats from a ROM with 686/686 models parsing clean,
and a documentation culture that records what was tried, what was reverted and what
the plan got wrong. That last habit is the single strongest signal of merit, and it
is the opposite of what slop looks like.

The honest caveats, also in one paragraph: the hunt is a Perfect Dark deathmatch,
not the build/hunt/patch loop of `DESIGN.md`; the build phase has no budget, timer or
wave escalation; the one sealed-box enforcement that was built (breakable doors) is
switched off; the app layer is a 7,133-line file with zero tests and a 2,870-line UI
function; 366k lines were inserted in seven weeks by one person driving an AI, so
nobody has read all of it; and the repo ships ~43 MB of ripped Rare art with no
licence note anywhere.

## The documents

| File | What it is for |
|---|---|
| [01_what_we_built.md](01_what_we_built.md) | Inventory and architecture map. Read first if you are new to the repo. |
| [02_editor_what_works.md](02_editor_what_works.md) | The editor's real strengths and the design decisions that earned them. |
| [03_editor_what_doesnt.md](03_editor_what_doesnt.md) | Friction, gaps, latent defects and structural debt, ranked by how soon they bite. |
| [04_flexibility_and_creativity_roadmap.md](04_flexibility_and_creativity_roadmap.md) | **The main deliverable.** Ranked proposals for more expressive building, each grounded in the code, with cost, files, traps and acceptance bar. |
| [05_beyond_the_editor.md](05_beyond_the_editor.md) | The engine, the AI, the game-vs-vision gap, and the full merit assessment. |
| [06_rules_of_the_road.md](06_rules_of_the_road.md) | For the model that picks this up: the invariants that must hold, the don't-do list, and how to start a task here. |

## Suggested pick-up order

If the goal is "more flexibility and creativity" with the least risk, the order that
makes each step cheaper than the last is:

1. The armed-tool enum and `disarm_all()` (roadmap item 0a).
2. Room/brush as a selectable object: delete, move, duplicate, mirror (1a).
3. A stamp library built on that selection (1b).
4. Widen the texture-zone packing from 8 slots to 16 (0b).
5. Bevels and trim bands, now that zones exist for them (2a, 2b).
6. The plan editor and the section (side-view) draw (1c, 1e).
7. Sky-open rooms and fog (2d).
8. Attributed polygons through the CSG kernel (0c).
9. The Add-only 45° wedge brush (3a), which 0c makes tractable.

Everything else in the roadmap is independent and can be picked by taste.
