# Handoff — authorable spawn points, respawning, and a scoreboard

> Written 2026-08-18, at the end of the `AI=pd|ours` session. Nothing below is built.
> Companion to `DESIGN_AI_PD_VS_OURS.md` §6 (the AI switch this follows).

## The pivot

Today a hunt is a **one-shot encounter**: hunters flood in at one fixed marker, the player
spawns wherever the fly-cam happened to be, and death ends it (`YOU DIED` → `R`).

The new dynamic is a **deathmatch loop**:

* the player authors **any number of spawn points** anywhere in the level, in BUILD;
* at `G`, simulants spawn from that pool — **and so does the player**, at one of the same
  points, not where they entered play mode;
* when anyone dies — simulant or player — they **respawn** from the pool;
* **kills and deaths are counted** and shown.

This is the shape Perfect Dark's multiplayer already has, which matters: the hunter AI is
now PD's, and PD's bots were written for exactly this loop.

## Decide these four things with the user before writing code

Each one changes the work, and none can be inferred from the request:

1. **One pool or two?** "the player will spawn at one of these points too" reads as a
   single shared pool. Confirm — separate player/simulant pools is the other common design
   and costs a flag on the marker, not a rewrite.
2. **How many simulants?** One per spawn point, or the existing wave size
   (`World::set_wave_size`, `PLAYTEST_WAVE_SIZE = 4`) distributed round-robin over the
   pool? "any number of simulants spawn into the level via the spawn points" is ambiguous
   between the two.
3. **Does the round ever end?** Score limit, timer, or endless? Today `caught` and
   `player_dead` end it; with respawn both stop being terminal, and something has to
   replace them or the game has no shape.
4. **Respawn delay** — instant, or a beat (PD uses a short one)? And is the player's
   respawn automatic or a keypress?

**Check the decomp before inventing the spawn-choice rule.** PD picks a spawn pad rather
than taking a random one, and the repo's standing preference is to port the rule and cite
the line rather than guess a good one. Look for the multiplayer spawn selection in
`reference/pd-decomp/src/game/mplayer/setup.c` before writing a "farthest from the nearest
enemy" heuristic — if PD's rule is that, port it; if it is something else, port that.

## What is already in place

**The format anticipated this.** `LevelFile::spawn_point` is already persisted with the
comment *"Persisted so the format is ready for an authorable spawn point even though it's
fixed today"* — `native/crates/game/src/world/persist.rs:52`. It is a single `[f32; 3]`,
so it becomes a list (or moves onto the entity layer, below).

| what | where |
|---|---|
| the fixed marker + its constant | `world/mod.rs:841` `SPAWN_MARKER_POS`, `world/mod.rs:2108` `World::spawn_point` |
| marker rendering (both modes) | `world/mod.rs:3172` |
| resolve marker → standable cell, build the search-point pool | `world/hunt.rs:873` `prepare_spawn` |
| spawn the wave in a ring around it | `world/lifecycle.rs:732` `spawn_wave` |
| player entry into HUNT (floor under the fly-cam) | `world/lifecycle.rs:581` `toggle_mode` — `floor_under(camera.pos)` → `CharacterController::new`, records `hunt_spawn` |
| whole-wave respawn (the difficulty-change reset) | `world/lifecycle.rs:696` `restart_hunt` |
| player death → restart | `app.rs:2401` → `World::restart_after_death` |
| corpse lifecycle (ragdoll → settle → fade → despawn) | `world/lifecycle.rs:559` `advance_ragdolls` |
| HUD quad builders to copy for a scoreboard | `hud/mod.rs` — `credits_quads`, `danger_quads`, `death_quads` |

**The authoring pattern already exists twice.** A spawn point is the *third* placeable
after props and lights, and should follow them rather than invent a third way:

* `world/tools/prop.rs` + `world/tools/prop_gizmo.rs` — the `O` object palette, crosshair
  ghost, click-to-place, gizmo edit, delete.
* `world/tools/light.rs` — the same, for something with **no mesh**, which is exactly a
  spawn point's situation (it needs a marker ghost and a synthetic pick box; see
  `light.rs:21` and `:261`).
* Both persist as **authored ECS entities** (`ecs::EntityData`, `world/persist.rs`
  `entities`). Note the v3 comment: point lights rode the existing `entities` collection
  with **no schema change** — a `SpawnPoint` component can do the same, so the level
  format version need not move at all.

## Traps this specific work will hit

* **Undo.** `world/history.rs:43` snapshots `spawn_point`. Authored spawn points must join
  the undo snapshot or placement/deletion silently won't undo — the same trap every
  placeable has hit.
* **Respawn must reuse the roster slot.** `EnemyInstance` indices are load-bearing far
  beyond the roster: `pd_lab::PdTarget::Hunter(i)`, `EnemyInstance::pd_target`, the ORCA
  agent tags (`lifecycle.rs:457`), squad alert, and every AI-lab metric key off the index.
  Respawning by pushing a new instance renumbers everything mid-fight. **Respawn in
  place** — reset the `Enemy`, re-add its collider, keep the slot.
* **Colliders.** Death removes the hitscan capsule; respawn has to re-add it
  (`physics.add_enemy_collider`) and resync, or the new body is unshootable.
* **`AI=pd` + respawn is a different game.** An omniscient pack that respawns forever is
  continuous pressure with no lull — which is faithful to PD deathmatch and may be
  unplayable in a hide-and-seek level. The spawn-choice rule (trap: never spawn anyone
  inside someone else's engagement band) is what makes it survivable. Expect to A/B this
  with `AI=ours`.
* **Existing tests assert the fixed marker.** `world/tests.rs:144` and `:189` both assert
  hunters spawn near `SPAWN_MARKER_POS`; they need rewriting rather than deleting — the
  property they protect ("the wave arrives together, near an authored point") still holds.
* **The AI lab builds levels with no spawn points.** `TestArena::build` /
  `build_pd` author brushes only, then `toggle_mode`. Either they place a spawn point, or
  the spawn path needs a documented fallback (the old marker) when the pool is empty.
  Do not let an empty pool mean "no hunters" — the whole lab would go quiet.

## Suggested milestones

1. **Authoring** — place/delete spawn points in BUILD via the `O` palette, marker drawn in
   both modes, persisted on the entity layer, undo-safe. Headless test: place 3, save,
   load, assert 3.
2. **Spawn selection** — the wave *and* the player draw from the pool at `G`, using PD's
   rule. Headless test: the player no longer spawns under the fly-cam.
3. **Respawn loop** — per-slot respawn for both sides after the agreed delay, colliders
   restored, indices stable.
4. **Scoreboard** — kills/deaths per side, HUD readout in the `hud/mod.rs` idiom.

## Verification

Headless only: `cargo test` (the `world::tests` + `world::ai_testbed` suites), then
`cargo build --release` and hand over a playtest brief. **Do not launch and drive the game
— it hijacks the user's machine.** And remember the standing lesson: CPU-side green says
nothing about feel; every AI defect this project has had passed the full suite.

## Repo state at handoff

The `AI=pd|ours` work is committed on **`feat/ai-mode-switch`** and pushed, *not* merged to
`main`, and is **awaiting playtest**. Decide up front whether this work branches from that
branch (if the AI switch is staying) or from `main`.
