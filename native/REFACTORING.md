# Native Engine — Refactoring Opportunities

A review of the `native/` Rust workspace for modularity, genericity, scalability,
and maintainability. Findings are ordered by impact. Nothing here is a bug report —
the code works and is unusually well-documented; this is about structure that will
hurt as the tool roster and feature set grow.

## Snapshot

```
crates/csg/        pure BSP CSG core (vendored)          ~470 LOC   clean
crates/game/       entry point                           7 LOC      trivial
crates/engine/     domain-agnostic runtime               ~9,200 LOC
  world.rs         3,661   ◄── monolith (39% of engine)
  structures.rs    1,074
  csg_runtime.rs     975
  renderer.rs        764
  nav.rs             569
  uv_zones.rs        498
  textures.rs        326
  physics.rs         258
  mesh.rs            154
  camera / character / enemy / gltf_load / input   < 110 each   fine
```

The crate split (`csg` → `engine` → `game`) is excellent: the one-way dependency is
documented and real, the CSG core is cleanly vendored, and the small modules
(`camera`, `character`, `input`, `enemy`, `physics`, `mesh`) are each cohesive and
right-sized. **The problem is concentrated almost entirely in `world.rs`**, with
secondary opportunities in `renderer.rs` and some cross-module helper duplication.

---

## 1. `world.rs` is a 3,661-line god object — split it

`World` is simultaneously:

- the **scene aggregate** (camera, physics, regions, mode);
- the **simulation driver** (`fixed_step`, `look`, `toggle_mode`, HUNT wiring);
- the **HUNT runtime** (doors, breaching, enemy/nav);
- **six independent authoring tools**, each with its own state, key handlers,
  preview, confirm, and cancel: face push/pull + sub-face, opening (door/hole),
  placement (pillar/brace), platform + stair-run, the platform gizmo, and the
  arrow-key stair tool;
- a **geometry/mesh helper library** (`make_wall_brush`, `boxes_mesh`,
  `push_colored_box`, `make_stair_void`, `face_quad_mesh`);
- a **math helper library** (`ray_aabb`, `in_box_eps`, `axis_index/normal/val`,
  `others`, `flip`, `same_face`);
- and **~900 lines of tests**.

The `World` struct has **~35 fields**, of which roughly 20 are tool-local scratch
state (`connect_from`, `connect_edge`, `connect_slide_wt`, `simple_from`,
`gizmo_drag`, `hole_w/h`, `pillar_size`, `brace_width/depth`, `sel_size_u/v`,
`sel_bounds`, `active`, `pending_stair`, …). The constructor initializes all of them
in one 40-line block; most are meaningless unless one specific tool is armed.

### Suggested module layout

Convert `world.rs` into a `world/` directory. Each tool becomes a submodule that
either (a) holds its own state struct and an `impl World` block, or better (b) owns
its state entirely (see §2).

```
world/
  mod.rs         World struct + Default/new, Mode, RegionMesh, is_build/player_pos
  lifecycle.rs   look, fixed_step, view_proj, toggle_mode, floor_under
  pick.rs        Selection, FaceInfo, pick_face, pick_face_hit, pick_structure_hit
  editing.rs     push, pull, sub-face (ActiveOp/SubOp), create_sub_face_brush,
                 rebuild_region, selection preview + highlight meshes
  hunt.rs        Door, build_doors, breach_tick, door_mesh, enemy_mesh
  tools/
    opening.rs   OpeningKind/OpeningPlacement + arm/confirm/cancel/preview/size
    placement.rs PlaceKind + pillar/brace resolve/confirm/preview
    platform.rs  PlatformPhase/ConnectTarget/StructureHit + the state machine
    gizmo.rs     GizmoHandle/GizmoDrag + parts/pick/start/drag/mesh
    stairs.rs    PendingStair + push_stairs/confirm/cancel/preview
  geom.rs        make_wall_brush, make_stair_void, boxes_mesh, push_colored_box,
                 ray_aabb, in_box_eps  (all currently free fns at file bottom)
tests moved to  crates/engine/tests/authoring.rs (integration) or per-module #[cfg(test)]
```

Rust lets you spread `impl World { … }` across files, so this split needs no API
change and no field-visibility churn to land as a first step. It immediately makes
each concern independently readable and reviewable.

---

## 2. There is a `Tool` abstraction screaming to be extracted

Every authoring tool re-implements the **same lifecycle** by hand:

| Concern            | opening        | placement      | platform         | stairs          |
|--------------------|----------------|----------------|------------------|-----------------|
| arm / toggle       | `arm_opening`  | `arm_place`    | `platform_tool_key` | `push_stairs` (implicit) |
| "is armed?" query  | `is_opening_arming` | `is_placing` | `is_platform_tool` | `has_pending_stair` |
| cancel             | `cancel_opening` | `cancel_place` | `cancel_platform_tool` | `cancel_stairs` |
| per-frame preview  | `update_opening_preview` | `update_place_preview` | `update_platform_preview` | `stair_preview_mesh` |
| scroll sizing      | `adjust_opening_size` | `adjust_place_size` | `adjust_platform_size` / `adjust_connect_slide` | — |
| click confirm      | `confirm_opening` | `confirm_place` | `platform_click` | `confirm_stairs` |

This duplication leaks into **three places** and grows every time a tool is added:

1. **`world.rs` mutual exclusion** — each `arm_*` manually disarms the others:
   ```rust
   self.opening_tool = None;
   self.opening_preview = None;
   self.place_tool = None;
   self.clear_platform_state();
   self.selected = None;
   ```
   This block (in various orderings) is copy-pasted into `arm_opening`, `arm_place`,
   and `platform_tool_key`. Forgetting one line is a latent bug.

2. **`app.rs` click dispatch** ([app.rs:153-167](crates/engine/src/app.rs#L153-L167)):
   ```rust
   let rm = if opening { w.confirm_opening() }
            else if placing { w.confirm_place() }
            else if platform { w.platform_click() }
            else { w.select_at_crosshair(); None };
   ```

3. **`app.rs` scroll dispatch** and **`app.rs` preview dispatch** — two more parallel
   `if is_connect_sliding … else if is_platform_placing … else if is_placing …` ladders
   ([app.rs:198-208](crates/engine/src/app.rs#L198-L208),
   [app.rs:277-289](crates/engine/src/app.rs#L277-L289)).

### Recommendation

Model the armed tool as a single enum so exclusivity is a **type invariant** (you
cannot represent two armed tools), and give it a common surface:

```rust
enum ActiveTool {
    None,
    Opening(OpeningTool),   // owns kind, hole_w/h, preview
    Placement(PlaceTool),   // owns kind, sizes
    Platform(PlatformTool), // owns the whole phase machine + gizmo + connect scratch
    Stairs(StairTool),      // owns PendingStair
}

trait Tool {
    fn preview(&mut self, ctx: &PickCtx) -> Option<CpuMesh>;
    fn scroll(&mut self, du: f32, dv: f32);
    fn confirm(&mut self, world: &mut EditTarget) -> Option<RegionMesh>;
    fn cancel(&mut self);
}
```

`World` then holds one `active_tool: ActiveTool` instead of ~20 flat fields, arming
is just `self.active_tool = ActiveTool::Opening(..)` (old tool dropped automatically),
and `app.rs` collapses its three ladders into `world.active_tool_preview()` /
`.active_tool_confirm()` / `.active_tool_scroll()`. **Adding a tool becomes: implement
`Tool`, add one enum variant, bind one key** — instead of editing four call sites.

(The sub-face push/pull on a *selected face* is arguably not a modal tool and can stay
on `World` proper; the six armed tools are the ones that fit this pattern.)

---

## 3. `app.rs` — collapse the `Option<World>` dance and the key ladder

Two smaller structural issues:

- **`Option` juggling.** After `resumed`, `window`/`renderer`/`world` are always
  `Some`, yet every handler repeats
  `self.world.as_ref().map(|w| w.is_placing()).unwrap_or(false)` and
  `self.world.as_mut().and_then(…)`. The idiomatic winit fix is a two-state enum:
  ```rust
  enum App { Uninitialized, Running(RunState) }
  struct RunState { window: Arc<Window>, renderer: Renderer, world: World, input: InputState, … }
  ```
  Handlers match once on `Running(state)` and then use plain fields — removing dozens
  of `.as_mut()` unwraps and the `unwrap_or(false)` defaults.

- **`on_key_pressed` is a 150-line if-ladder** of `if code == KeyCode::X { … return; }`.
  Once tools implement a common surface (§2), most branches become a small
  key → action lookup, leaving only genuinely special keys (Esc ordering, `G`)
  as explicit cases.

---

## 4. `renderer.rs` — five overlay pipelines are near-identical boilerplate

`Renderer` declares **8 pipelines** and, for the five overlay meshes (highlight,
stair-ghost, entity, door, gizmo), the pattern is copy-pasted:

- a `<name>_pipeline: wgpu::RenderPipeline` field,
- a `<name>_mesh: Option<GpuMesh>` field,
- a `set_<name>_mesh(&mut self, mesh: Option<&CpuMesh>)` method that does the identical
  "upload or clear" dance ([renderer.rs:523-566](crates/engine/src/renderer.rs#L523-L566)),
- a near-identical draw block in `render`.

These overlays differ only in a few pipeline knobs (depth test on/off, blend, cull,
vertex layout, shader). Introduce a small descriptor + collection:

```rust
struct Overlay { pipeline: wgpu::RenderPipeline, mesh: Option<GpuMesh> }
// built from a table of (shader, layout, depth_test, blend) specs
overlays: HashMap<OverlayKind, Overlay>,
fn set_overlay(&mut self, kind: OverlayKind, mesh: Option<&CpuMesh>) { … } // one impl
```

That deletes five setters and five draw blocks in favor of one each, and adding a new
overlay effect becomes a one-line table entry. The six WGSL files
(`shader`, `shader_textured`, `shader_crosshair`, `shader_door`, `shader_entity`,
`shader_highlight`) also share a lot of vertex/camera boilerplate that could be
factored, though that's lower priority than the Rust side.

---

## 5. Cross-module helper duplication — pull into a shared `geom`/`Axis`

Small, exact-duplicate helpers are scattered across modules:

- **`axis_index`** exists in both [world.rs:2719](crates/engine/src/world.rs#L2719)
  and [csg_runtime.rs:593](crates/engine/src/csg_runtime.rs#L593). The `Axis`-indexing
  helpers (`axis_index`, `axis_normal`, `axis_val`, `others`) are naturally **methods
  on `Axis`** (`csg_runtime::Axis`) and should live there once:
  ```rust
  impl Axis { fn index(self)->usize; fn normal(self)->[f32;3]; fn orthogonals(self)->(Axis,Axis); }
  fn Vec3::component(self, axis: Axis) -> f32   // or an extension trait
  ```

- **The "dominant axis from a surface normal" block** is duplicated verbatim in
  `pick_face_hit` ([world.rs:1039-1046](crates/engine/src/world.rs#L1039-L1046)) and
  `pick_structure_hit` ([world.rs:2153-2159](crates/engine/src/world.rs#L2153-L2159)).
  Extract `Axis::dominant(normal: Vec3) -> Axis`.

- **`push_quad_double`, `tri_normal`/`quad_normal`** appear in both
  `structures.rs` and `csg_runtime.rs`. **AABB containment** logic recurs as
  `in_box_eps` (world.rs), `in_box` (nav.rs), and `Brush::contains` (csg_runtime.rs).
  A tiny `geom` module (quad emit, triangle normal, ray-AABB, point-in-AABB) would
  host all of these once and be shared by `world`, `structures`, `csg_runtime`, `nav`.

---

## 6. Smaller notes

- **`STRUCT_ID = u32::MAX` sentinel.** Free-standing structures reuse the region
  mesh/collider slots via a magic id and reuse `RegionMesh` as the return type. It
  works, but a named `MeshSlot { Region(u32), Structures }` (or a dedicated
  `MeshUpdate` type) would document intent and remove the "regions count up from 0 so
  MAX never collides" reasoning that a reader must reconstruct.

- **`RegionMesh` is really a generic "re-upload this mesh" signal.** Many methods
  return `Option<RegionMesh>` meaning "something changed." Renaming to `MeshUpdate`
  (with the slot enum above) reflects how it's actually used and pairs with §4.

- **Tests inside `world.rs` (~900 LOC).** Moving the end-to-end tests to
  `crates/engine/tests/` (they already drive only the public API) would cut the file
  by a quarter and separate "spec" from "implementation." Keep unit-level tests as
  per-module `#[cfg(test)]` in the new submodules.

- **`Region` assumes a single region in places** — `pick_face_hit` hard-codes
  `&self.regions[0]` with a `// Phase 1: single region` note. Worth flagging that the
  multi-region generalization will touch picking when it lands.

---

## Recommended sequence

1. **Mechanical split of `world.rs`** into the `world/` module tree (§1) — no logic
   change, immediate readability win, safe to do first.
2. **Extract `Axis` methods + `geom` helpers** (§5) — removes duplication the split
   would otherwise scatter across new files.
3. **Introduce the `ActiveTool` enum + `Tool` trait** (§2) — the highest-leverage
   change for "scalable/generic"; shrinks `World`, makes exclusivity type-safe, and
   simplifies `app.rs`.
4. **`App` → `Uninitialized`/`Running` states** (§3) and the **overlay pipeline
   table** (§4) — independent cleanups that can follow at any time.

Each step is independently shippable and preserves the existing (thorough) test suite
as the behavioral oracle.
