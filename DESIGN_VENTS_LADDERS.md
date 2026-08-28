# Vents & Ladders — design

Status: **plan only, nothing built.** Written 2026-08-28 against `feat/named-levels`.

Two new traversal verbs for the level builder:

- **Vents** — a crawlable duct network carved into walls / floors / ceilings, enterable
  only while crouched, textured as bare metal ducting.
- **Ladders** — a decal on a wall face that becomes climbable, giving vertical travel
  without a staircase.

They are grouped in one doc because they share one risk: **both are traversals the nav
grid cannot express**, and the nav grid is the hunters' entire world model.

---

## 1. What the code already says (measured, not assumed)

These are the facts the design is built on. Each was read out of the source, and several
contradict the obvious assumption.

### 1.1 There is no crouch

Grep for `crouch` across `crates/` returns only comments — PD's `CROUCHPOS_SQUAT` spread
modifier (deliberately not ported), a PD weapon function label, and a note in
`pdsim/spread.rs` reading *"our hunters have no crouch posture"*. The player's
`CharacterController` has a fixed `HEIGHT = 6.0 * WT = 1.5 m`.

So crouch is net-new. The good news is that it is *cheap*: the physics seam already takes
the capsule per call —

```rust
// engine/src/sim/physics.rs:500
pub fn move_character(&mut self, dt: f32, radius: f32, half_height: f32,
                      capsule_center: Vec3, desired: Vec3) -> (Vec3, bool)
```

`radius` and `half_height` are **arguments, not state**. Crouch is therefore entirely a
`character.rs` change: interpolate a height, pass the smaller capsule, lower the eye.
No physics API change, no collider rebuild.

### 1.2 The nav grid already refuses to enter a vent — for free

```rust
// engine/src/sim/nav.rs:18
pub const AGENT_HEIGHT_CELLS: i32 = 6;   // 1.5 m

fn is_standable(&self, ix: i32, iy: i32, iz: i32) -> bool {
    ...
    for h in 1..AGENT_HEIGHT_CELLS {
        if self.is_solid_cell(ix, iy + h, iz) { return false; }
    }
    true
}
```

A cell is standable only with **6 cells (1.5 m) of clear headroom**. A duct with a 4-cell
(1.0 m) bore contains no standable cell anywhere along its length. It is not in any
walkable component, `label_components` never reaches it, and A\* cannot route through it.

**Hunters structurally cannot enter a vent. This is not a feature to build; it is a
property to protect.** It falls out of a constant that already exists for other reasons.

> **Rule 1 — the bore ceiling.** Vent interior height must stay **≤ 5 cells (1.25 m)**.
> At 6 cells a "vent" silently becomes a low corridor that hunters walk down, and the
> feature quietly inverts. This wants a hard clamp in the tool plus a test, not a
> convention.

### 1.3 …but "cannot enter" is not "degrades gracefully"

This is the actual havoc, and it is specific. When the player is inside a vent, every
hunter calls `find_path(self.pos, player_pos)` every `REPATH_INTERVAL = 0.4 s`. The goal
cell is not standable, so:

```rust
// nav.rs:1124 — goal resolution
let goal = self.cell_at(...).or_else(|| self.nearest_cell(goal_m))?;
```

`nearest_cell` is a **linear scan of a 48×48×48 box — ~110,000 cells — running
`is_standable` on each**, per call. It exists as a rare fallback, not a per-frame path.

Worse, it *defeats the existing safety net*. The O(1) unreachable refusal at `nav.rs:1131`
compares the start and goal **components** and bails without searching — the fix for a
documented 10 fps playtest. But the snapped goal is a genuine standable cell in the main
component, so the refusal never fires and a **full A\* runs and succeeds** every time.

Three consequences, in severity order:

1. **Cost.** Six hunters × 2.5 Hz × (110k-cell scan + a full successful A\*) is a load
   that does not exist today, sustained for as long as the player stays in the duct.
2. **Nonsense goals.** `nearest_cell` searches in **3D within ~6 m** with no line-of-sight
   or reachability weighting. The nearest standable cell to a player in a wall duct is
   frequently *on the other side of that wall*, or on the floor below. Hunters would
   converge on a point with no relationship to the vent mouth.
3. **No exit condition.** Hunters are **omniscient by default**: `known_target_pos`
   returns the player's live position unconditionally and `lose_contact` is suppressed
   (`enemy.rs:1557`). They never give up, and will hold `Chase` on an unreachable target
   forever.

   *(Corrected during implementation: this doc first attributed that to `AI=pd` being the
   shipped default. It is not — `AiMode::from_env` defaults to `ours`. Omniscience is a
   **separate** flag, `pd_omniscience`, which is on by default and applies in both modes
   via `lifecycle.rs:338`. The consequence is the same, but it is not mode-gated, and
   there turn out to be **three** movement brains to keep in step — `pd_step`, the
   utility scorer, and the FSM kill-switch — not one. See §3.)

   There *is* a partial guard: `move_toward` treats a `None` path and a degenerate
   1-waypoint path as "arrived **or** unreachable" so the caller re-targets
   (`enemy.rs:2843-2857`). But with omniscience the re-target instantly re-picks the
   player, so it loops rather than resolving.

> **Rule 2 — vents need an explicit hunter response, not an implicit one.** The engine
> will not fall over on its own, but the default behaviour is expensive and reads as
> broken. See §3.

### 1.4 Perception is physics, not nav — and that is the whole game

`can_see` → `perception_los` is a **Rapier raycast against the CSG trimesh**
(`enemy.rs:1392`), completely independent of the nav grid. The vent bore is real geometry
with a real collider, so a hunter can **see and shoot the player through a vent mouth**
already, with no work.

This is the asymmetry that makes vents a *hiding* mechanic rather than an invincibility
box: **vents break navigation, not sight.** It is already true. Preserve it — do not
"fix" LOS into ducts.

### 1.5 The catch test has no LOS check, and that constrains crouch

```rust
// enemy.rs:1400
fn catches(&self, player_feet: Vec3) -> bool {
    self.dist_to(player_feet) < CATCH_DIST                   // 0.3 m, lateral
        && (player_feet.y - self.pos.y).abs() < CATCH_VERT   // 0.75 m
}
```

No line-of-sight, no solidity test — pure proximity. Today that is safe through a
`WALL_THICKNESS = 1 WT = 0.25 m` wall only by arithmetic accident: both capsules have
radius 0.25 m, so two centres cannot approach closer than 0.5 m through a wall, and
0.5 m > `CATCH_DIST`.

That margin is **0.2 m, and it is load-bearing.**

> **Rule 3 — crouch shrinks height only, never radius.** The instinct when fitting a
> capsule into a 1.0 m bore is to shrink the radius too. Do not: a 1.0 m bore already
> clears a 0.25 m radius, and shrinking it lets the player's centre approach the duct
> wall far enough that a hunter pressed against the far side **catches them through it**.
> Add a test pinning this.

Ceiling vents are fine unaided (a duct above a room floor is ≫ 0.75 m up). **Floor-level
wall vents are the exposure**, and Rule 3 is what closes it.

### 1.6 Ladders are the dangerous half, not vents

A vent is a *horizontal* traversal the grid merely declines to enter. A ladder is a
*vertical* one the grid cannot represent at all:

```rust
const MAX_STEP: i32 = 1;    // 0.25 m between adjacent cells
const STAIR_STEP: i32 = 2;  // 0.5 m, and ONLY inside authored stair volumes
```

A ladder-only route between two floors produces a **nav island** — exactly the failure
that cost slot1 15% of its walkable floor behind two staircases, and exactly the failure
that produced the 10 fps playtest. `nav_issues.rs` opens by naming this as its reason for
existing.

The mitigating fact: the codebase **already has the machinery**, because stairs needed the
same relaxation. `bake()` builds a parallel `stair: Vec<u8>` grid alongside `solid`, and
`step_limit()` is the single function that reads it:

```rust
fn step_limit(&self, a: (i32,i32,i32), b: (i32,i32,i32)) -> i32 {
    if self.is_stair_cell(a) && self.is_stair_cell(b) { STAIR_STEP } else { MAX_STEP }
}
```

A hunter-climbable ladder is a third plane on that same pattern. This is the genuine fork
— see §4.

### 1.7 The tools are already 80% built

| Need | What exists |
|---|---|
| Carve an opening in a picked face | `OpeningKind::Hole` — crosshair pick, scroll-size, frame + protoroom carve (`tools/opening.rs`) |
| Push/pull a volume | Draw tool's **signed** scroll depth: up extrudes, down insets through zero (`tools/draw.rs`) |
| Register a new modal tool | `Tool` enum + `EditorAction::ArmTool` in `radial/mod.rs` — the documented single seam; do not add to `on_key_pressed` |
| Incremental re-bake | `rebuild_affected_regions(&[ids])` |
| A vent-cover mesh | `MeshId::MetalGrate` (`metal_grate_secondary.glb`) already in the prop catalog |

A vent tool is `OpeningKind::Vent` + a run-extrude. It is an increment, not a subsystem.

### 1.8 Zone 4 is free and is literally named "tunnel"

```rust
// engine/src/render/textures.rs:7
//   4 = tunnel legacy (flat color, never emitted), 5 = stair/doorframe …
```

with a test at `textures.rs:724` asserting *no* scheme currently defines zone 4. It is the
natural home for vent interior surfaces: it lets ducts read as bare metal regardless of
the room theme they pass through, without stealing a zone in use.

Cost to note honestly: **this spends the last free zone slot.** Anything later wanting a
per-surface material channel has to widen the zone space first.

### 1.9 The grey vent texture cannot be found by grep

`assets/textures/` holds 1,024 BMPs, overwhelmingly named `tempImgEd0005.bmp` and similar.
`themes.json` carries 392 extracted schemes (`facility_01`…`facility_11`, `silo_*`, …) but
no zone is named for its content. Nothing in the repo maps "vent duct" to a file, and the
source GE level OBJ/MTLs are no longer checked in.

**This is a visual identification task, not a search task** — the TEXTURES panel exists
precisely to browse and judge. The texture should be picked there by eye and its name
wired in. Do not guess a filename.

### 1.10 Persistence probably needs no version bump

`persist.rs` states its own rule: a new `#[serde(default)]` field is *not* a
version-worthy change (it cites both named levels and theme hotkeys as precedent). Vent
geometry is ordinary brushes, already persisted. A ladder is best modelled as an ECS
entity and rides `ecs/persist.rs` exactly as `Door` does. Format stays **v4**.

---

## 2. The vent tool

**Arming.** New `Tool::Vent`, reached from the Tools ring and a key, dispatching through
`EditorAction::ArmTool(Tool::Vent)`.

**Placement.** Reuse `resolve_opening_placement` with a new `OpeningKind::Vent`:

- Fixed cross-section, clamped to Rule 1 — proposal **4 × 4 WT (1.0 × 1.0 m)**.
- Unlike `Door`, **Y faces are legal** (floor and ceiling ducts are the point).
- The crosshair ghost is the existing preview quad.

**The run (push-pull).** After the mouth is placed, the tool enters a depth step
reproducing the draw tool's signed scroll: scroll extends the duct along the face normal
in 1 WT steps. Confirm carves the run as a single `Op::Subtract` brush, `frame`-marked so
`uv_zones` routes its reveal surfaces to the vent zone.

**Chaining.** A duct that only goes straight is not a system. Two options, and I'd take
the second:

- *(a)* Re-arm the tool on the run's end face to turn a corner. Zero new concepts;
  every bend is an author click.
- *(b)* A dedicated segment mode where each click commits a segment and re-anchors to the
  new end face, Esc ends the network. Same carve, far less clicking, and it matches how
  the draw tool already keeps you in its depth step to scroll on.

**Exit at the far end.** The mouth carve's protoroom trick applies: without a second mouth
a duct dead-ends in solid. The tool should require an explicit **exit mouth** and refuse —
loudly, in the NAV report — to leave a duct with fewer than two mouths. A one-mouth vent
is a pocket the player can be trapped in.

**Texturing.** Vent surfaces emit zone 4, themed from a new `vent` entry in `themes.json`.

---

## 3. Making hunters behave sanely about vents

Doing nothing gives the §1.3 failure. The cheapest honest fix reuses a mechanism that is
already in the codebase and already proven — **the door overlay**.

Doors ride the frozen grid as live post-bake state, and `NavDoor` carries **two** flags:

```rust
struct NavDoor { open: bool, passable: bool, center: Vec3, clearance: f32 }
```

`passable: false` is described in-source as what "makes *player-only* a real wall rather
than a suggestion". A vent mouth is the same shape of object: a known portal, at a known
`center`, that hunters may never traverse.

**Proposal — vent mouths as first-class nav portals.**

1. `bake` records vent mouths in a `vents: Vec<NavVent>` list with a `center` (the mouth,
   in the room, on the standable side).
2. `find_path` gains one guard **before** the `nearest_cell` fallback: if the goal is
   inside a vent volume, return the nearest **mouth** rather than scanning 110k cells.
   This kills the cost (§1.3.1) and the nonsense goals (§1.3.2) in one change, and it is
   strictly cheaper than what happens today.
3. Hunters get an explicit response to "my target is in a duct" — stake out the mouth.
   Under `AI=pd`'s omniscience that is *correct* behaviour, not a workaround: PD bots
   converge on where you are, and a hunter kneeling at the grille you are crawling toward
   is the good version of this feature. It also makes §1.4 pay off — they can already
   shoot down the duct.

That leaves the exit condition (§1.3.3) as a tuning question rather than a hang: a hunter
staking a mouth is *doing something*, so holding Chase is no longer a freeze.

**Validation.** `nav_issues.rs` should learn vents as a recognised class, so the NAV tab
distinguishes "this duct is deliberately hunter-proof" (Info) from "this duct strands a
pickup / a spawn pad" (Error). The tab's whole thesis is separating connectivity findings
from traversability ones; vents are a *third* category — deliberate asymmetry — and if it
isn't taught, every vent will light the panel up red.

---

## 4. Ladders — the decision that has to be made first

A ladder is a wall decal that becomes climbable. The render/authoring half is easy: place
a quad on a picked face (the door tool's face-pick, minus the carve), store it as an ECS
entity with a `Ladder` component beside `Door`, drive climbing off a volume test in
`character.rs` that overrides gravity while the player is inside it.

The nav half is a genuine fork, and it decides the work:

**Option A — player-only ladders.** Ladders do not touch the grid. A floor reachable only
by ladder is a nav island, accepted deliberately. `nav_issues` already has the vocabulary
(`overclimb_edges`, player-only climbs measured against `JUMP_APEX`); ladders become a
third recognised player-only traversal so the tab reports them as intent, not error.
Cheap, safe, and it makes ladders a pure escape tool.
*Risk:* islands are the documented cause of the worst perf bug in this project. Every
ladder-only area is a place hunters can never follow, and pickups/spawn pads stranded up
there are orphans.

**Option B — hunter-climbable ladders.** Add a `ladder: Vec<u8>` plane to `bake` exactly
as `stair` is built, and extend `step_limit` to return a tall limit when both cells are
ladder cells. Hunters climb; connectivity is preserved; no islands.
*Risk:* this is the one place the grid's "cannot climb" invariant gets relaxed, and the
`STAIR_STEP` doc comment is an essay about how carefully that had to be scoped
("**both** ends, not either"). A vertical run also breaks A\*'s 4-connectivity assumption
in a way the stair relaxation never did, and hunters have no climb animation.

**Recommendation: A first, B as a later opt-in per ladder.** `NavDoor.passable` is the
precedent — the same authored object being hunter-traversable or not is already an
established pattern here, and it lets the author choose per placement instead of the
engine choosing globally.

---

## 5. Controls

The requested binding is **L or R + C-Down**. One real conflict:

```rust
// gamepad.rs:233,266
let c_down = pressed_raw(CODE_C_DOWN) || pressed(BTN_C_DOWN) || ry < -C_STICK_THRESHOLD;
let pitch_axis = (c_down as i32 - c_up as i32) as f32;
world.gamepad_look(dt, sx, sy, aim_mode, pitch_axis, input);
```

`aim_mode` is exactly `L || R`, and C-Down is the pitch-down axis. So **L+C-Down is
already the primary way to aim downward** — the single most common thing a player does
while holding aim. Binding crouch to it would make aiming down crouch every time.

Options, cheapest first:

- **Hold-duration split** — a short L+C-Down still pitches, a held one crouches. The
  `b_button_edges` helper already implements exactly this hold/tap discrimination for B.
- **Z + C-Down** instead. Z is fire; the combination is unused and unambiguous.
- **Context gate** — L+C-Down crouches only when a vent mouth is within reach, pitches
  otherwise. Invisible when it works, mysterious when it doesn't.

Keyboard gets a plain Ctrl hold regardless, so the feature is testable before the pad
question is settled.

---

## 6. Suggested order

Each stage is independently shippable and independently playtestable.

| # | Stage | Why here |
|---|---|---|
| 1 | **Crouch** — capsule height + eye lerp in `character.rs`, keyboard bind only. Rule 3 test. | Zero AI risk. Standalone feel change. Unblocks everything. |
| 2 | **Vent geometry** — `OpeningKind::Vent`, fixed bore, signed-scroll run, zone 4, two-mouth rule. | Author can build ducts; hunters ignore them by construction (§1.2). |
| 3 | **Vent nav portals** — `NavVent`, the `find_path` mouth guard, NAV-tab vent class. | Removes the §1.3 cost and the nonsense goals. **Do not ship 2 without 3.** |
| 4 | **Hunters stake out mouths** — the AI response proper. | The behaviour that makes vents fun rather than merely safe. |
| 5 | **Ladders, Option A** — decal, climb volume, player-only, NAV reporting. | Isolated from all vent work. |
| 6 | **Ladders, Option B** — per-ladder `hunter_passable`, `ladder` bake plane. | Only if 5 proves the islands are a problem in play. |
| 7 | **Pad binding** — whichever §5 resolution wins. | Needs the hardware in hand. |

---

## 7. Decisions (locked 2026-08-28)

1. **Ladder nav — Option A, player-only.** Ladders do not touch the grid. Islands are
   accepted deliberately and taught to the NAV tab as a third player-only traversal
   alongside jump-height climbs. Option B (§4) stays available as a per-ladder
   `hunter_passable` opt-in if playtest shows the islands hurt.
2. **Crouch binding — hold-duration split on L+C-Down.** A tap still pitches the view
   down; a hold crouches. Reuses the `b_button_edges` hold/tap helper rather than adding
   a new discrimination mechanism. Keyboard gets a plain Ctrl hold.
3. **Vent chaining — segment mode.** Each click commits a segment and re-anchors the tool
   to the new end face; Esc ends the network. Mirrors the draw tool staying in its depth
   step to scroll on.

### Still open

- **Vent bore** — proposal 4×4 WT (1.0 m). Rule 1 caps it at 5 cells. Confirm in play.
- **The grey texture** — needs picking by eye in the TEXTURES panel (§1.9). Blocks the
  visual half of stage 2, nothing else.
