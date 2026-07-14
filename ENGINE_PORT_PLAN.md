# Engine Port Plan — BUILD & HIDE → native Rust

> **Purpose:** plan the port of BUILD & HIDE from JS/three.js to a from-scratch
> **native Rust** engine. Companion to [DESIGN.md](DESIGN.md) (the game) and
> [ASSET_INTEGRATION_PLAN.md](ASSET_INTEGRATION_PLAN.md) (what assets/logic survive the port).
>
> **Guiding principle:** *own the engine architecture; integrate proven libraries for the
> solved commodity problems (physics, navmesh, glTF, windowing).* "From scratch" means we own
> the design, the CSG runtime, the renderer architecture, and all glue — not that we reimplement
> rigid-body dynamics or navmesh generation from first principles.

---

## Decisions (locked)

| Decision | Choice | Rationale |
|---|---|---|
| **Language** | **Rust** (native, release binary, no GC) | CSG core is already Rust → reuse verbatim; Rapier is Rust-native; concurrency safety for real-time rebake. |
| **Build vs buy** | Own architecture; buy physics, navmesh, glTF, windowing | Effort goes to the differentiator (CSG-as-gameplay, real-time nav over mutating geometry), not to reinventing solved systems. |
| **Renderer** | **wgpu** with **Vulkan** backend | Native-optimized (Vulkan under the hood on Windows); own the render architecture without hand-writing Vulkan boilerplate. NOT a web target. |
| **Physics / char controller** | **Rapier3D** | Rust-native; 3DS FPS combat was authored against Rapier, so Tier 3–4 combat ports near 1:1. |
| **Navmesh** | **Port the validated grid nav first; Recast only if it proves insufficient** | The JS prototype proved grid A* over voxelized CSG holds at wave scale, and `navWorld` is pure logic that ports nearly free. The frozen-hunt design (below) means nav bakes **once** per hunt — no continuous rebake — so Recast's tiled-rebake power isn't yet needed. Adopt Recast only if grid nav falls short at native scale/quality, or if mid-hunt geometry mutation becomes a committed feature. |
| **CSG core** | **Reuse existing Rust crate** (source verified in hand) | The single hardest algorithmic piece — already written and tuned. Zero FFI. |

### Open decisions (resolve during Phase 0)

- **Renderer abstraction depth:** raw wgpu vs. a thin render-graph layer on top. Decide after the first GLB renders.
- **ECS vs. hand-rolled scene:** `hecs` (lightweight) or `bevy_ecs` (standalone) vs. a simple typed scene graph. Lean `hecs` unless entity counts demand more.
- **Nav approach (grid vs. Recast):** *default to porting the validated grid nav.* Only if it proves insufficient at native scale/quality — or if mid-hunt geometry mutation becomes committed (see "The one hard problem," below) — evaluate `recast-navigation`. If adopted then, confirm the maintained Rust binding supports tiled rebuild at the version needed; fallback is a thin FFI wrapper we own. *(3DS FPS uses `recast-navigation` 0.43 — JS bindings to the same C++ Recast — so the concepts carry if we go that way.)*

### Verified against source (2026-07-14)

Confirmed by inspecting `D:\Claude Code Projects\GoldenEye Level Editor\spike\` and `D:\Claude Code Projects\3DS FPS\` (CSG seam + Rapier/Recast deps independently re-verified 2026-07-14):

- **CSG crate seam — RESOLVED, better than assumed.** `spike/csg-wasm-bench/csg-wasm/src/csg.rs` is a **pure-Rust BSP CSG core** (`Plane`, `Polygon`, `Node`, `csg_union`, `csg_subtract`, `polygons_to_mesh`) with **zero** `wasm_bindgen`/`js_sys` (verified: 0 refs in `csg.rs`, 386 lines) — works in `[f32;3]`, returns `(Vec<f32>, Vec<f32>, Vec<u32>)`, which drops straight into a wgpu vertex buffer and a Rapier trimesh collider. `wasm_bindgen` lives **only** in `lib.rs` (JSON parse + `js_sys` array wrapping — **~303 lines**, discarded wholesale; the plan previously said "~50" — it's bigger, but still 100% discardable). Reuse `csg.rs` verbatim; rewrite only the wrapper. *(Crate is `crate-type = ["cdylib"]`; add `"rlib"` to consume it as a normal dependency.)*
- **Two more reusable pure-Rust crates (same clean structure — `wasm_bindgen` only in `lib.rs`):**
  - `shadow-lighting/lighting-wasm` — **BVH + shadow-bake** lighting (`bake.rs`, `bvh.rs`, `stencil.rs`, `subdivide.rs`). **Upgrades lighting from "defer/rewrite" to Tier-1 reusable.**
  - `cave-painting-rust/cave-wasm` — marching-cubes cave generator (`marching.rs`, `noise.rs`, `chunk.rs`, `world.rs`). Reusable verbatim, but **caves are currently out of scope** — this is dormant reuse, only live if caves return.
- **Combat ports ~1:1, not a rewrite.** 3DS FPS uses `@dimforge/rapier3d-compat` — the same dimforge Rapier as the native `rapier3d` crate. `ShootingSystem.ts` = `new RAPIER.Ray(...)` + `world.castRayAndGetNormal(...)`; `EnemyAI.ts` LOS = `world.castRay(...)`. These transliterate directly to native `rapier3d` query calls. **ASSET_INTEGRATION_PLAN.md Decision #1 is settled by the language choice** (native Rapier is the default; the JS already speaks Rapier), collapsing Tier 4 from "rewrite" to "API transliteration."

---

## Locked stack

| Concern | Crate | Note |
|---|---|---|
| Renderer | `wgpu` (Vulkan backend) | native-optimized; own the architecture, not the boilerplate |
| Physics + kinematic char controller | `rapier3d` | Rust-native; matches combat's existing assumptions |
| Navmesh | **ported grid nav** (`navWorld` → Rust) | validated in JS at wave scale; bakes once per hunt. Recast is a *later* option, not the default (see Decisions) |
| CSG core | **`spike/csg-wasm-bench/csg-wasm/src/csg.rs`** (pure Rust) | reuse verbatim; discard `lib.rs` wasm wrapper |
| Lighting / shadow bake | **`lighting-wasm`** (pure Rust BVH baker) | reuse verbatim — Tier-1, not a rewrite |
| Cave generation | `cave-wasm` (pure Rust marching cubes) | dormant — reusable verbatim only if caves return to scope |
| Math | `glam` | SIMD, ecosystem standard |
| Window / input | `winit` | commodity |
| glTF / GLB load + skinning | `gltf` | 44 characters + ~150 animation clips |
| Audio | `kira` (or `rodio`) | GoldenEye sound set |
| Build / deps | `cargo` | eliminates the C++ build tax |

---

## Engine ↔ Game boundary

**Engine** = domain-agnostic runtime. Knows nothing about hide-and-seek. Renderer, CSG runtime,
collision/physics + character controller (Rapier), nav (Recast), resource/asset loading, audio,
input, math, scene/entity model, serialization. If it would be equally at home in an unrelated
FPS, it is engine.

**Game** = everything in DESIGN.md — the CSG-editor-*as-gameplay*, the BUILD→HUNT→PATCH loop,
economy, enemy roster, wave logic. The game **links and calls** the engine.

**Hard rule:** the dependency is one-way. Engine code never references game code. Enforce the
*direction*, but let the actual API surface **emerge** from what the game needs — do not design a
speculative engine API up front and build the game against it (the classic from-scratch-engine
trap; you abstract the wrong things).

**Project-specific subtlety:** in most engines CSG is editor-only and bakes to static runtime
geometry. Here, **brushes are authored at runtime during the BUILD phase** — so "CSG + its
generated mesh + the collision/nav derived from it" is a *runtime engine subsystem*, not editor
tooling. It is the thing this engine is fundamentally *for*. **But note the cadence** (locked by
the game design): geometry is authored during BUILD, then **frozen for the HUNT** (only door
overlays change, not the mesh). So the subsystem must make **runtime authoring + a fast one-time
bake at the BUILD→HUNT transition** cheap — *not* continuous rebake during combat. See "The one
hard problem."

```
game/         BUILD & HIDE: build-hunt-patch loop, economy, enemy roster, waves, CSG-as-verb UI
  │ (one-way dependency)
  ▼
engine/       scene/entities, CSG runtime subsystem, renderer (wgpu), resource/asset, input,
              audio, wrappers over Rapier (collision + char controller) and Recast (nav)
  │
  ▼
third_party/  wgpu, rapier3d, recast bindings, glam, winit, gltf, kira, in-house CSG crate
platform/     window, filesystem, timing
```

Practically: a Cargo **workspace** with `engine`, `game`, and the CSG crate as members.

---

## The one hard problem (center the plan on this)

FPS dynamics, collision, and hitscan are **solved, standard work**. The project-defining problem is
still *collision + nav derived from CSG* — but the **game design has already narrowed it**, and the
JS prototype has already **validated the answer**. Be precise about which version of the problem you
are solving, because the plan previously over-scoped it.

**What the design actually requires (locked):** phases are separate and the level **freezes during
the HUNT**. So the demands are:
1. **BUILD phase:** author/mutate brushes at runtime with instant visual feedback (no enemies yet —
   no nav or combat pressure). This is the existing editor's job; it already does this via
   incremental per-region CSG rebuilds.
2. **BUILD→HUNT transition:** one **fast bake** — voxelize/collect the frozen geometry into the nav
   representation and per-region collision bodies. Happens **once** per hunt.
3. **HUNT phase:** geometry is immutable; the only dynamic obstacles are **doors**, handled as a
   **live cost/blocking overlay** on the static nav — **no re-bake** (validated in JS:
   `navWorld.doorGrid` + `DOOR_COST`, breach = flip a flag the pathfinder reads live).

**What this means — the JS sweep already retired the risk:**
- Grid A* over voxelized CSG **held at wave scale** → grid nav ports as the default; Recast's
  tiled-rebake machinery is **not** required by the current design.
- Dynamic obstacles (doors) ride the static grid as an overlay → **destruction needs no
  re-voxelization**. This is the key finding to carry into native.

**Design commitments this implies:**
- **Per-region collision bodies** so a BUILD-phase edit rebuilds one body, not the world (already
  the editor's model), and so the transition bake is incremental-friendly.
- **Grid nav baked once at the transition**, plus a **dynamic overlay** for doors/gadgets.
- **Keep the bake fast and local** rather than building a continuous-rebake pipeline you don't need.

**Deferred (NOT committed — do not pay for it up front):** *real-time collision + nav over geometry
that mutates mid-HUNT* — breachers punching arbitrary walls, mid-hunt building (DESIGN.md §4–5, §8
Q1). This is the version that would justify Recast tiled rebake + continuous collider regeneration.
It is a **possible future direction, explicitly avoided by the current phase-freeze design.** If you
later commit to it, that is the moment to adopt Recast and revisit this section — with a real
requirement to design against, not a speculative one.

---

## Portability tiers (Rust-adjusted; extends ASSET_INTEGRATION_PLAN.md)

- **Free / verbatim:** all Tier-1 assets (GLB/sounds); all Tier-2 data configs (→ Rust structs or
  runtime JSON via `serde`); **three pure-Rust crates — `csg.rs`, `cave-wasm`, `lighting-wasm`**
  (drop the `lib.rs` wasm wrappers).
- **Transliteration (JS → native, same API family):** the Rapier-based combat — `ShootingSystem`
  ray casts and `EnemyAI` LOS map directly from `rapier3d-compat` to native `rapier3d`.
- **Cheap port (pure logic, no engine deps):** `navWorld` (grid voxelization + A* + LOS + door
  overlay + solid reconstruction for stairs/platforms — all already dependency-free and unit-tested)
  → **the primary nav runtime**, not just a validation helper; the `EnemyAI.ts` state machine;
  weapon / fire-timing tables; the CSG *region* data model; the level serialization schema. Keep the
  "verify a path exists to every region before a wave" validation running against this same grid.
- **Rewrite against engine (small):** the CSG wasm wrapper (`csg-wasm/src/lib.rs`, ~50 lines);
  mesh upload glue; the raycaster (→ Rapier/Recast queries); all three.js glue.
- **Keep JS as the spec:** do **not** delete the JS game. It is the playable design reference and
  the behavioral regression oracle. Port in vertical slices and diff behavior against it.

---

## The three subsystems

**FPS dynamics** — Rapier `KinematicCharacterController`: kinematic capsule (not full dynamic
physics on the player), sweep-based move-and-slide, step offset for stairs, ground snap, gravity,
jump. Port the tuning constants (radius/height/step) straight from `src/game/player.js`. Weapon
feel (bob/sway/recoil) from `WeaponViewmodel.ts` is nearly engine-neutral.

**Collision** — Two layers: (1) Rapier rigid bodies for props/ragdolls/projectiles; (2) the
**CSG → collision mesh pipeline** (ours): on brush change, regenerate the affected region's
collider and update its Rapier static body. Hitscan = a Rapier ray query with layer filtering.

**Nav** — port `navWorld` (grid voxelization + A* + LOS + door overlay), baked **once** at the
BUILD→HUNT transition from the frozen geometry. Doors/gadgets are a live overlay, no re-bake. Run
the "verify a path exists to every region before a wave starts" validation (DESIGN.md §2) against
this grid. **Recast is deferred** (see "The one hard problem"): only adopt it if grid nav proves
insufficient at native scale/quality, or if mid-hunt geometry mutation becomes committed — then a
tiled navmesh with dirty-tile rebake is the right tool and DESIGN.md §8 Q1 comes back into play.

---

## Phase plan (mirrors the JS phases → diffable against the reference build)

0. **Skeleton + commodity integration.** Cargo workspace; `winit` window; `wgpu` clears the screen;
   load & draw one GLB via `gltf`; link `rapier3d` with a smoke test. No game yet. *(No Recast —
   nav is the ported grid.)*
1. **CSG runtime slice (the risky one — do it early).** Wire in the CSG crate; author one brush at
   runtime → mesh → Rapier static collider. Fly-cam. Prove BUILD-phase authoring feels instant.
2. **Transition bake + FPS dynamics.** On BUILD→HUNT, bake the frozen geometry into the grid nav
   (port `navWorld`) and per-region colliders. Rapier kinematic capsule over the collision world;
   stairs, gravity, jump. Feel-match the JS player.
3. **One enemy pathfinds** to the player over the ported grid nav (JS Milestone, DESIGN.md §9.2).
4. **Breakable door via the overlay model → collider removed + nav overlay flag flipped + noise.**
   *This slice proves the validated thesis natively: a built element is destroyed and nav+collision
   react instantly, with no re-voxelization.* (True mid-hunt CSG mutation stays deferred.)
5. **Shooting + hit reactions**, then the build/economy loop, then the enemy roster as content.

**Gate:** if Phases 1–4 feel-match the JS reference and stay fast, the architecture is right and the
rest is content. If any slice is slow or fragile, fix it there before building breadth (same
philosophy as DESIGN.md §9).

---

## First concrete steps

1. `cargo build` the in-house CSG crate standalone from `spike/csg-wasm-bench/csg-wasm/` (add
   `"rlib"` to `crate-type`); confirm the `csg.rs` public API (`csg_subtract`/`csg_union`/
   `polygons_to_mesh`) works outside the wasm wrapper.
2. Stand up the Cargo workspace (`engine`, `game`, `csg`) with the locked stack wired (no Recast)
   and a window that clears via wgpu/Vulkan.
3. Prototype Phase 1 (one runtime brush → mesh → Rapier collider) as the risk-burndown spike, then
   Phase 2's transition bake into the ported grid nav.
