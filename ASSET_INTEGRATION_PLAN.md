# Asset & System Integration Plan — GoldenEye → BUILD & HIDE

> **Purpose:** inventory everything reusable from the two source projects and plan how it
> lands in **this** repo (Hide and Seek Level Builder), which is now the single home for the
> game. Eventual target is C++, so this doc also flags what is *durable* (survives the port)
> vs *disposable* (throwaway JS glue).

## Source projects

| Project | Path | What it contributes |
|---|---|---|
| **Hide and Seek Level Builder** (this repo) | `.` | The CSG brush editor, BUILD→HUNT loop, navGrid A\* pathing. **The home.** |
| **3DS FPS** | `D:\Claude Code Projects\3DS FPS` | GoldenEye models, animations, weapons, shooting, enemy AI, navmesh, doors, all GE levels. |

## Stack gap (the core integration cost)

| | This repo | 3DS FPS |
|---|---|---|
| three.js | 0.166 | 0.182 |
| Language / bundler | JS + esbuild | TypeScript + Vite |
| Physics | **none** (custom over navGrid) | Rapier3D (WASM) |
| Navigation | A\* `navGrid` / `navWorld` | recast-navigation navmesh |
| Model loading | none yet | `AssetLoader` + `SkeletonUtils` |
| Combat | **none** | full hitscan + enemy AI |

**Consequence:** raw assets and pure-data configs cross this gap for free. All the *combat
logic* in 3DS FPS is written against Rapier raycasts + recast paths, so it cannot be
copied in as-is — it must be either (a) backed by adding Rapier to this repo, or (b)
rewritten against this repo's `raycaster.js` + `navGrid.js`. **This is decision #1 below.**

---

## 1. What we have — MODELS

### Enemy characters — 44 GLBs (`3DS FPS/public/models/enemies/characters/`)
- **Guard/soldier archetypes**, each with 6 interchangeable head variants (alan/joe/karl/mark/martin/pete):
  blue-guard, russian-guard, russian-infantry, janus-marine, janus-special-forces, jungle-commando.
- **Named bosses (single GLB each):** trevelyan, ourumov, xenia, jaws, baron-samedi, boris,
  natalya, valentin, mayday.
- ~100 KB each, **all on one shared skeleton** (hand bones `Bone_8` left / `Bone_9` right).

### Weapon models — 26 dirs (`3DS FPS/public/models/weapons/`)
Each has `gun.glb` (+ `muzzle.glb` for most). Full arsenal: pp7, dd44, kf7, ar33, rcp-90,
klobb, dk5, dk5-silencer, phantom, zmgobj, magnum, golden-gun, gold-pp7, silver-pp7,
pp7-silencer, laser, sniper, shotgun, auto-shotgun + throwables (grenade, grenade-launcher,
rocket-launcher, proximity/remote/timed-mine, detonator).

### GoldenEye levels
All 18 levels exist as extracted/placed objects in `3DS FPS/public/` (facility, dam, aztec,
etc.) — **not needed** for BUILD & HIDE (the CSG editor generates levels) but available as
reference/prefab material.

---

## 2. What we have — ANIMATIONS

~150 GLB clips (`3DS FPS/public/models/enemies/animations/`), indexed to GoldenEye's actual
animation table (hex-prefixed: `00-idle`, `28-walking`, `3F-spotting-bond`, …). Categories:

- **Locomotion:** idle, walk, jog, run (+ unarmed / female / hands-up / pistol variants)
- **Combat:** ~40 fire poses (standing / hip / kneel / roll / dual-wield / pistol / ADS)
- **Hit reactions:** per-body-part (shoulder, arm, hand, leg, neck, taser)
- **Deaths:** ~30 (directional falls, spins, explosions, fetal, stagger-to-wall)
- **Situational:** spotting-bond, look-around, surrender, conversation, stand-up, idle fidgets

**Directly relevant to DESIGN.md enemies:** `3F-spotting-bond` → Spotter, `40-look-around`
→ search/Grid-searcher, walk/jog/run → locomotion, deaths/hits → combat feedback.

The manifest + metadata already exist as data in [`3DS FPS/src/data/AnimationSet.ts`](../3DS%20FPS/src/data/AnimationSet.ts):
- `DEFAULT_ANIMATIONS` — name → path map
- `SPEED_THRESHOLDS` — walk 1.5 / jog 3.5 / run 5.0 m/s (auto-select locomotion clip by speed)
- `FIRE_TIMING` — frame-accurate shot windows per fire clip
- `HIT_ANIMS` / `DEATH_ANIMS` — randomized reaction sets
- `BONE_ZONE_MAP` + `ZONE_DAMAGE_MULTIPLIER` — headshot = 4×, limbs = 0.6× (hit-zone ready)

---

## 3. What we have — GUNS (stats/config)

- **Player weapons — 19 configs** in [`3DS FPS/src/weapons/WeaponConfig.ts`](../3DS%20FPS/src/weapons/WeaponConfig.ts)
  (`WeaponStats`): fire cooldown, magazine, reload, damage, range, model/muzzle paths,
  viewmodel offset/pivot/rotation/scale, recoil (kickback + pitch), zoom FOV, sounds.
  Runtime overrides load from `/config/weapon-config.json` (tuned via the in-game weapon editor).
- **Enemy weapons — 4 configs** in [`3DS FPS/src/data/EnemyWeaponConfig.ts`](../3DS%20FPS/src/data/EnemyWeaponConfig.ts)
  (`EnemyWeaponDef`): pp7/kf7/ar33/rcp90, with fire rate, accuracy, range, and **bone-local
  hand offsets** for attaching the gun GLB to `Bone_9`/`Bone_8` (dual-wield supported).

### Sounds (`3DS FPS/public/sounds/`)
- **weapons/**: per-gun fire wavs, reload, empty, silenced
- **enemies/**: `pain-1`…`pain-26`, bullet-hit, pistol, rifle
- **player/**, **doors/**, music (`102 Facility.mp3`)

---

## 4. What we have — SHOOTING MECHANIC

### Player side (`3DS FPS/src/weapons/`)
- **`ShootingSystem.ts`** — hitscan: ray from camera, `castRayAndGetNormal`, returns hit
  point/normal/collider → entity. **Rapier-dependent.**
- **`WeaponSystem.ts`** (324 lines) — switching, ammo, reload state, HUD, fire cooldown.
- **`WeaponViewmodel.ts`** (279 lines) — first-person gun: bob, sway, recoil, reload
  raise/lower, rendered as a separate overlay scene. **Mostly engine-neutral three.js.**
- **`BulletDecalManager.ts`** — bullet-hole decals (ring buffer + atlas).

### Enemy side
- **[`EnemyCharacter.ts`](../3DS%20FPS/src/entities/EnemyCharacter.ts)** (961 lines) — skinned model + `AnimationMixer`,
  crossfade locomotion by speed, weapon-to-hand-bone attach, muzzle flash, timed shots via
  `FIRE_TIMING`, randomized hit reactions & deaths, **damage-painting** (vertex colors darken
  at hit point), fade-out cleanup. Animation half is engine-neutral; movement half is **Rapier
  kinematic controller**.
- **[`EnemyAI.ts`](../3DS%20FPS/src/ai/EnemyAI.ts)** (383 lines) — state machine
  `idle→alert→chase→attack→cooldown`, vision cone + LOS raycast, navmesh pathfinding,
  **GoldenEye-style probabilistic hit** (accuracy × distance falloff). Logic is portable;
  LOS ray + path calls are **Rapier/recast-specific**.

---

## Portability tiers (summary)

| Tier | Items | Effort to land here |
|---|---|---|
| **1 — Assets** | 44 characters, ~150 anims, 26 weapon dirs, all sounds | **Copy.** Engine/language-neutral. Durable through C++. |
| **2 — Data configs** | `AnimationSet`, `WeaponConfig`, `EnemyWeaponConfig` | **Copy + convert TS→JS** (strip types). Pure data. Durable. |
| **3 — Portable logic** | `WeaponViewmodel`, `EnemyCharacter` (anim half), `EnemyAI` (state machine) | Adapt: swap Rapier/recast calls for local raycaster/navGrid. |
| **4 — Engine-bound** | `ShootingSystem`, `EnemyCharacter` (physics half), enemy LOS/path | Rewrite against this repo's systems, OR adopt Rapier here. |

---

## Decision #1 — physics/nav backing (blocks Tier 3–4)

The combat code assumes Rapier + recast. Two paths:

- **A. Adopt Rapier + recast in this repo.** Highest fidelity, lets Tier 3–4 come over with
  minimal rewrite, matches where a C++ engine lands anyway (a real physics/nav layer).
  Cost: add WASM deps to the esbuild pipeline; reconcile with the existing capsule player and
  `navGrid`. Bigger up-front lift.
- **B. Keep this repo's raycaster + navGrid; rewrite the glue.** Enemy LOS uses `raycaster.js`,
  pathing uses `navGrid`/`navWorld` A\*, shooting uses `raycaster.js` against CSG meshes.
  Cost: rewrite `ShootingSystem` + `EnemyAI` LOS/path + `EnemyCharacter` movement against
  existing systems. Keeps the bundle light; more porting now, and re-diverges from the C++ layer.

**Recommendation:** **B for the first vertical slice** (get animated GoldenEye enemies +
player shooting working over the existing navGrid fast, no new deps), then **A when combat
proves out** and precision physics matters — which also better matches the C++ target.

---

## Proposed phased plan

**Phase 0 — Bank assets (no code).** Copy Tier 1 into this repo's public/asset path; copy Tier 2
configs converted to JS. Repurpose the `assets/enemies/README.md` (currently Mixamo-oriented)
to describe the GoldenEye set. *Durable; do first regardless of Decision #1.*

**Phase 1 — Animated enemy (replace the capsule).** Port `EnemyCharacter`'s animation half to
JS: load a character GLB + idle/walk/run clips, drive an `AnimationMixer`, crossfade by speed.
Keep the existing `navGrid` movement from [enemy.js](src/game/enemy.js) driving position; the
model is visual. **Immediately upgrades the current HUNT phase.**

**Phase 2 — Player shooting.** Bring `WeaponViewmodel` (engine-neutral) + a raycaster-backed
`ShootingSystem` rewrite. Add health to the player. Wire kills → enemy hit reactions/deaths.
This delivers the "retain GoldenEye-style gunplay" line from DESIGN.md §4.3.

**Phase 3 — Enemy combat AI.** Port `EnemyAI` state machine with LOS via `raycaster.js` and
pathing via `navGrid`. Attach enemy weapons to hand bones; probabilistic hit on the player.
Now the HUNT phase is a real GoldenEye firefight, not just touch-to-catch.

**Phase 4 — Map DESIGN.md enemy roster onto GE assets.** Spotter = `spotting-bond` behavior,
Grid-searcher = `look-around` sweep, Breacher = heavy model + wall-break, etc. Content pass.

---

## C++ note (carry-forward)

- **Durable:** every Tier 1 asset (glTF/GLB, sounds), every Tier 2 config (they're data
  tables — port to structs/JSON), and the *design* of the AI state machine + fire-timing model.
- **Disposable:** all three.js `AnimationMixer`/loader/viewmodel glue, and whichever
  physics/nav choice from Decision #1 — the C++ engine supplies its own.
- Keep asset **source** (not just derived GLB) in-repo so the C++ export can be re-derived.
