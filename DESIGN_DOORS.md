# Doors — design & plan

Functional, authorable doors for the native game: pick one in the **O** object panel,
place it, set its hinge/slide, and have it open — for the player *and* for the hunters.

Everything below is grounded in measurements of what is actually in the repo today
(2026-08-18), not in what the old design notes assumed.

---

## 1. What already exists (and what it means)

### The assets are already here — as static scenery

All **13 door GLBs** are already in `native/assets/props/` and already in the
`CATALOG` under `PropCategory::Doors` (`native/crates/game/src/props.rs:387+`). They
render, place, move, rotate, snap, duplicate, ground, delete and persist today. They
just don't *do* anything.

So this is **not** an asset job or a placement job. It is a behaviour job on top of a
finished authoring pipeline.

### Measured facts about those 13 models

Read straight out of the GLB accessor bounds:

| model | size (X × Y × Z, m) | thin axis | centred on width axis? |
|---|---|---|---|
| `bathroom_door` | 1.20 × 2.52 × 0.13 | **Z** | no (cx −0.132) |
| `metal_door_2` | 1.27 × 2.92 × 0.20 | **Z** | yes |
| `jail_door` | 1.05 × 2.75 × 0.08 | **Z** | yes |
| `elevator_door` | 0.85 × 2.45 × 0.08 | **Z** | no (cx +0.128) |
| `brown_sliding_door` | 0.80 × 2.18 × 0.17 | **Z** | no (cx −0.080) |
| `glass_door` | 0.50 × 2.58 × 0.17 | **Z** | yes |
| `metal_safe_door` | 1.16 × 1.16 × 0.34 | **Z** | no (cx −0.240) |
| `blast_door` | 4.55 × 3.84 × 0.42 | **Z** | yes |
| `big_metal_door` | 7.02 × 4.65 × 0.39 | **Z** | yes |
| `metal_door` | 0.20 × 2.43 × **1.21** | **X** | yes |
| `grey_door` | 0.13 × 2.70 × **1.20** | **X** | yes |
| `wooden_door` | 0.15 × 2.71 × **1.43** | **X** | yes |

Four things fall out of this table, and each one kills a plausible assumption:

1. **Every door is one mesh, one node, no frame, no authored pivot.** There is no
   hinge in the asset to read. The pivot *must* be derived from the AABB.
2. **The thin axis is not consistent.** `metal_door`, `grey_door` and `wooden_door`
   are thin in **X**; the other ten are thin in **Z**. Hard-coding "width is X" would
   make three doors swing about their own face. Derive per model: *width axis = the
   wider of local X/Z; normal = the thinner*.
3. **Four models are not centred on their width axis.** The hinge edge must come from
   `prop_bounds` min/max, never from the origin.
4. **Two models are room-sized.** `big_metal_door` is 7 m (already pre-scaled to 0.6 →
   4.2 m) and `blast_door` is 4.55 m, against a ~6 m room. These want to be *vertical
   shutters*, not swinging panels.

### The ECS scaffold is waiting for exactly this

`native/crates/game/src/ecs/components.rs` already defines, persists and documents:

- `Door { state, opening_type, open_frac }` — **inert**, nothing drives it
- `DoorState { Closed, Opening, Open, Closing }`
- `OpeningType { Swing, Slide }`
- `Interactable { radius }` — reserved, unused
- `ComponentData::Door { opening_type }` (`ecs/persist.rs:36`) — round-trips already

The comments in that file literally say *"that's the follow-up door task"*. The
scaffold was built for this.

### The nav door overlay is built and **dormant**

`native/crates/engine/src/sim/nav.rs` has a complete live door system:

- `NavWorld::set_doors(&[Brush])` — attaches a per-door overlay onto the frozen grid
- `DOOR_COST: i32 = 50` — A\* penalty so hunters *prefer* an open route but will still
  path through a closed door rather than give up
- `door_blocking(from, to) -> Option<usize>` — "the first shut door on this segment"
- `break_door(i)` — flips one door live, **no re-voxelisation**

`grep` says **the game never calls `set_doors`** — the old breakable-door / spawn-seal
idea was scrapped. This overlay is exactly the right mechanism for openable doors and
it is sitting there unused. That is the single biggest saving in this plan.

### The 3JS FPS repo has a complete working reference

`D:\Claude Code Projects\3DS FPS\src\`:

- `entities/DoorEntity.ts` (355 lines) — swing/slide animation, pivot group, hinge-side
  mesh mirroring, kinematic collider sync, auto-close timer, open/close sounds
- `editor/definitions/DoorDefinitions.ts` — the property schema (hinge side, swing
  direction, slide direction, trigger radius, open duration)
- `tools/DoorPlacer.ts` — ghost preview + trigger-radius wireframe + hotkeys
- `public/models/doors/` — only 3 doors, and they are the *same* GoldenEye assets we
  already have at higher coverage (bathroom, brown sliding, grey swinging)
- `public/sounds/doors/` — `swing-open/close`, `slide-open/close/during` (mono
  `pcm_s16le`, 9.8–44 kHz) — **directly usable, no conversion**

Worth stealing from it: the **property schema** and the **animation state machine**.
Not worth stealing: the models (we have more), the placer (ours is better — real
gizmos, undo, persistence).

### Sounds

Two sources, both ready:

- **GoldenEye soundpack** (`goldeneye-soundpack/fx/`, AIFF → needs the established
  `ffmpeg -c:a pcm_s16le` conversion from [[goldeneye-soundpack]]): 16 door cues —
  `metal_door_open1/2`, `metal_door_close1/2`, `slide_door`, `sliding_door1`,
  `liftdoor`, `stone_door1/2`, `stonedoor2`, `train_door`, `train_door_slide`,
  `moredoor3`, `doorknob_jiggle`, `door_defuser`
- **3JS FPS** `public/sounds/doors/*.wav` — 5 files, already `pcm_s16le`, copy as-is

That is enough to give every door class its own voice.

---

## 2. The gaps — what is genuinely missing

Four, and only four:

| # | Gap | Why it matters | Cost |
|---|---|---|---|
| G1 | **Rotated colliders.** `PhysicsWorld::add_prop_collider(min, max)` builds an *axis-aligned* cuboid (`physics.rs:203`). A door swung 90° needs a rotated box that moves each frame. | A swung door would still block the doorway it just cleared. | Small — rapier takes an `Isometry`; add `add_door_collider(center, half, rot)` + `set_door_pose(handle, ...)`. |
| G2 | **No distance attenuation in audio.** `AudioManager::play(name, volume)` (`audio.rs:98`) is flat — a fixed linear gain. Nothing in the codebase scales volume by distance; `ENEMY_FIRE_VOL` is a constant `0.7`. | In a **hide-and-seek** game, a door opening is *information*. A door across the map at full volume is actively misleading. | Small — a call-site helper `vol * clamp(1 − d/range, 0, 1)`. Not a spatial-audio system. |
| G3 | **Doors are currently baked permanently solid for nav.** `prop_solid_boxes()` (`world/tools/prop.rs:219`) emits every placed prop's AABB into `structure_solids` *before* `nav::bake` at BUILD→HUNT. Nav is baked once and never rebuilt. | A door baked this way can never open for a hunter. | Small — *exclude* doors from that list and register them on the live `set_doors` overlay instead. |
| G4 | **No interaction input.** `Interactable` exists but nothing reads it. In HUNT, `E` = weapon function, `F` = detonate mines, `R` = reload/restart. | Only matters if doors are manual. See §4. | Depends on the decision below. |

Everything else — panel, ghost, gizmos, snap, duplicate, undo, persistence, the
instanced draw path — is **already done and needs no change**. In particular
`prop_draws()` returns a full `Mat4` per instance, so a swinging or sliding door is
purely a different matrix: **zero renderer work**.

---

## 3. Design

### 3.1 A door is a prop with a `Door` component

No new subsystem. It mirrors the `destructible: Option<DestructibleDef>` pattern that
already works:

```rust
// props.rs
pub struct DoorDef {
    pub kind: OpeningType,        // catalog default; authored value can override
    pub slide_axis: SlideAxis,    // Sideways | Up  (shutters)
    pub open_sound: &'static str,
    pub close_sound: &'static str,
}
pub struct PropDef {
    // ...existing...
    pub door: Option<DoorDef>,    // Some(..) on the 13 Doors rows, None elsewhere
}
```

Proposed catalog assignment (all 13 become functional — it is a data table, so there
is no reason to leave any as scenery):

- **Swing:** `bathroom_door`, `wooden_door`, `grey_door`, `metal_door`, `metal_door_2`,
  `jail_door`, `glass_door`, `metal_safe_door` (a safe hatch — a swing with a short
  arc)
- **Slide sideways:** `brown_sliding_door`, `elevator_door`
- **Slide up (shutter):** `blast_door`, `big_metal_door` — the two room-sized ones.
  This needs one new `SlideAxis` value; it is the honest reading of what those GE
  assets are (Depot/Runway vehicle doors and Facility blast doors).

### 3.2 Deriving the hinge — the part the assets don't give us

At bake, from the registered `prop_bounds` AABB (`register_prop_bounds`, already
populated at startup for every catalog prop):

```
width_axis  = argmax(size.x, size.z)      // per-model — measured to vary
normal_axis = the other one
hinge_point = AABB edge on the chosen side of width_axis, at the model's base
```

Then the per-frame draw matrix is a pre-rotation about the hinge, in the prop's local
frame, folded into the matrix `prop_draws` already builds:

```
world = T(prop.pos) · R_y(prop.yaw) · T(hinge) · R_y(open_frac · open_angle) · T(−hinge) · S
```

Sliding is simpler still — a local translation along `width_axis` (or `+Y` for a
shutter) of `open_frac · slide_distance`.

**Hinge side (Left/Right)** just picks which edge of `width_axis` the pivot sits on.
**Swing direction (In/Out)** is the sign of the rotation. The 3JS version mirrored the
mesh (`scale.x *= -1`) for a right hinge; we don't need to — picking the other AABB
edge is cleaner and avoids the winding flip.

### 3.3 Authored properties (the "SELECTED DOOR" inspector)

The panel already has the exact slot for this: `app.rs:711–775` renders a **SELECTED
LIGHT** inspector (colour picker + two sliders) and a **SELECTED PROP** block. A
**SELECTED DOOR** block goes right beside them, shown when the selected prop's catalog
row has `door: Some(..)`.

| property | applies to | default |
|---|---|---|
| Opening type | all | from catalog |
| Hinge side (Left / Right) | swing | Left |
| Swing direction (In / Out) | swing | Out |
| Open angle | swing | 90° |
| Slide axis (Sideways / Up) | slide | from catalog |
| Slide direction (+ / −) | slide | − |
| Slide distance | slide | = door width |
| Speed | all | 1.0× |
| Trigger radius | all | 3.0 m |
| Auto-close delay (0 = stays open) | all | 3.0 s |
| Opens for | all | Both |

**"Opens for" (Player / Hunters / Both)** is not in the 3JS reference — it is specific
to this game. A door only *you* can open is a hiding advantage; a door only *they* can
open is a trap. Cheap to implement (one enum check in the trigger test), and it is the
kind of lever the base-scale design work has been looking for.

### 3.4 Show the hinge, don't just spell it

The user asked to "set hinge points". A dropdown alone is guesswork. When a door is
selected in BUILD, draw the **swing arc** (or the slide path) as a ghost overlay
through the existing gizmo-mesh channel — the same one `prop_gizmo.rs` already uses for
the translate arrows and the rotate torus. You see the arc sweep before you ever enter
HUNT, and flipping hinge side visibly mirrors it.

This is a small amount of work for most of the perceived quality of the feature.

### 3.5 Hunters and doors — the reason this matters for *this* game

The nav overlay (§1) does the heavy lifting:

1. At BUILD→HUNT, each placed door emits a `Brush` for its closed footprint and they
   all go to `nav.set_doors(&brushes)` — **excluded** from `prop_solid_boxes` (G3).
2. `DOOR_COST = 50` makes A\* prefer an open detour but keeps a shut-door route finite,
   so a hunter is never stranded.
3. When `door_blocking(from, to)` reports a shut door on the hunter's next segment, the
   hunter walks to it, triggers the open (a natural pause = the animation), and
   continues. The existing `break_door(i)` becomes `set_door_open(i, bool)` — same live
   flag, honest name.
4. Because the overlay is read *live* by A\*, a door opening or closing needs **no
   re-bake**. This is why the dormant overlay is worth so much here.

The emergent payoff: **you hear a door open, so you know a hunter found your wing.**
That is a real hide-and-seek mechanic falling out of the plumbing, and it is exactly
what G2 (distance attenuation) has to exist for.

### 3.6 Should a closed door block line of sight?

**Recommendation: yes** — and this is a deliberate departure worth calling out.

Destructible props are explicitly *excluded* from `raycast_world_only` (`physics.rs:399–417`)
so that placing crates can't change the hunter perception baseline. Doors have to be
the opposite: if a shut door doesn't block sight, hiding behind one is meaningless and
the whole point of the feature evaporates.

So doors get their own collider set (`door_colliders`), included in `raycast_world_only`
when shut and excluded when open. **This changes hunter perception and needs its own
playtest pass** — it is the one part of this plan that can move the difficulty needle.

### 3.7 Persistence

`ComponentData::Door { opening_type }` already round-trips. Extend the variant with the
authored fields, each `#[serde(default)]`, so existing **v3** level files keep loading
unchanged and no format bump is needed. That is the composition-first payoff working as
designed.

---

## 4. How a door opens — SETTLED: manual, on B

An earlier draft of this document proposed auto-open on proximity as the
"GoldenEye-faithful" option. **That was wrong.** GoldenEye and Perfect Dark open doors
with the **B button** — the use button — not by walking up to them.

So doors are manual, and the key is **B**, which turns out to be the tidy answer rather
than a compromise:

- It is context-sensitive, exactly as in GE: `use_door()` is tried first and returns
  `false` when no door is in reach, falling through to the key's other meaning (reload).
- `B` in BUILD is already the door-*carve* tool, so one key means "door" in both modes.
- The N64 pad's B is already dual-purpose (`gamepad.rs` `b_button_edges`, PD's
  tap-reload / hold-toggle), so adding "door first" there follows an existing pattern.

The cost of manual opening is that hunters now need explicit door-opening behaviour
rather than getting it free — that is M3's job, and it is worth it: a hunter *choosing*
to open a door is an audible, locatable event, which is the whole point in a
hide-and-seek game.

---

## 4a. Placement: anywhere, or only in a door frame?

One measurement settles the shape of the answer. The world runs at GoldenEye N64
proportions and the ripped assets do not:

| | |
|---|---|
| Room height (`H`, `levelgen/designs.rs`) | 8 WT = **2.0 m** |
| CSG doorway carve (`DOOR_WIDTH`×`DOOR_HEIGHT`) | 3×7 WT = **0.75 × 1.75 m** |
| Player capsule (`character.rs`) | **1.5 m** tall, eye at 1.35 m |
| Door GLBs | **2.18 – 2.92 m** tall |

**Every door prop is taller than a standard room.** So fitting a door to its opening was
never a stylistic choice — at `PROP_SCALE` the panel goes through the ceiling. M1
therefore scales each door row by its own measured height (`DOOR_FIT / <height>` in
`props.rs`, the divisor doubling as a record of the source asset).

That makes the opening the natural anchor, and the anchor already exists:
`opening.rs:157` sets `frame.door = true` on the carve brush, so **the geometry already
knows which holes are doorways** — and `.door` is precisely the flag `nav::set_doors`
consumes.

**Decision: snap-to-frame primary, free placement as the escape hatch.** Frame-snap
supplies four things free placement makes us guess: the fit, the hinge and width axis
(so §3.2's derivation and its "thin axis varies" trap drop out), the flush position and
facing, and a nav footprint that makes `door_blocking` meaningful. Free placement still
earns its keep for closet doors on a flat wall, decoration, and the two shutters, which
will never fit a 0.75 m carve.

Knock-on to watch: at 0.75 m the carve is narrower than most panels even after
height-fitting, so a frame fit will be slightly non-uniform. If that reads badly, widen
`DOOR_WIDTH` — it only affects newly cut doors, since existing levels store their own
carve dimensions.

*Status: the scale fit shipped in M1; the snap itself is M2.*

## 5. Milestones

Sized to the established rhythm: build → hand off → playtest → sign off.

**M1 — Doors open (player). ✅ SHIPPED** — engine 72 / game 399 tests green, release
clean, awaiting playtest. Built as described below, plus: door props scaled to the
doorway (§4a), `DoorAccess` (Player / Hunters / Both / Locked) landed early since it
was one enum on the use path, and door audio got the distance falloff (G2) since a door
cue is worthless flat. Notable structural choice: the tick runs as `door_system` on the
**ECS seam** (`ecs/systems.rs`, previously an empty documented no-op) rather than as a
`World` method — the bake resolves everything needing the catalog and the GLB bounds
into a transient `DoorGeom` component, so the per-step work touches nothing outside the
ECS but the physics borrow. `door_world_matrix` is the single source of truth for where
a panel is, shared by the draw list and the collider pose, so the door you see and the
door you bump into cannot drift apart.


`DoorDef` on the catalog rows; `door_system` in `ecs/systems.rs` (the seam is already
wired into `world/lifecycle.rs fixed_step`, currently an empty system list); swing +
slide + shutter animation; hinge derivation from `prop_bounds`; the folded draw matrix;
rotated moving collider (G1); proximity trigger; auto-close timer; GE sounds converted
and assigned per door class.
*Deliverable: walk up to a placed door in HUNT and it opens with the right noise.*

**M2 — Authoring.**
The SELECTED DOOR inspector; the swing-arc / slide-path ghost in BUILD (§3.4); extended
`ComponentData::Door` persistence with serde defaults.
*Deliverable: set hinge side and watch the arc flip before testing.*

**M3 — Hunters use doors.**
Exclude doors from `prop_solid_boxes` (G3); wire `set_doors`; rename `break_door` →
`set_door_open`; hunter open-and-pass; doors block LOS when shut (§3.6);
distance-attenuated door audio (G2).
*Deliverable: a hunter opens a door to reach you, and you hear it coming.*

**M4 — Optional, not scoped yet.**
Locked doors + keys (`doorknob_jiggle` is already in the soundpack for the rattle);
shootable/breachable doors (`DOOR_HP` and the old breach code still exist in
`world/mod.rs`, disabled); double doors as a paired-entity link (the ECS `AuthoredId`
cross-entity link was designed for exactly this).

M1+M2 is the feature the request describes. **M3 is what makes it matter for a
hide-and-seek game** and I'd argue it belongs in the same arc rather than after it.

---

## 6. Cleanup this task should absorb

Flagged in [[ecs-scaffold]] and still outstanding:

- Delete the dead breakable-`Door` struct + `breach_tick` in `world/mod.rs:1207+` — it
  is disabled, unreferenced, and shares a name with the new component. Keep `DOOR_HP`
  only if M4 is wanted.
- `world/hunt.rs:892/971/1001` still touches the old `self.doors` vec; it is only ever
  empty (`world/tests.rs:204` asserts "no spawn door built").
