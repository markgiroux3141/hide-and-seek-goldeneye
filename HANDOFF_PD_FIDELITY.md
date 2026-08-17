# Handoff — PD fidelity: direction table, free-for-all, barrel axis → then de-spike

State: `main` @ `35346a5`, clean tree, 309 tests green, release built.
Everything Perfect Dark is still behind `PD_LAB=1`. The goal of this handoff is the
three items below, **and then** promoting the track out of the spike.

Read first: `DESIGN_PD_SIMULANT_AI.md` §9–§12 (what the port does, and the three
investigations that produced §12). The decomp lives at `reference/pd-decomp`
(gitignored — `reference/README.md` says how to re-clone). All line references below
are `reference/pd-decomp/src/game/`.

## How to work on this track

Two rules the user has stated explicitly, both learned the hard way:

1. **Always go back to the decomp.** Do not invent a plausible adjustment to make
   something look right. The last three bugs on this track each had a decomp answer,
   and in two of them the *obvious* fix was demonstrably wrong — see the "ruled out"
   list in `35346a5`'s commit message.
2. **Do not launch and drive the game yourself.** It steals focus and makes the user's
   machine unusable. When something has to be seen on screen, stop and hand them a
   specific brief: exact command, what to do in-game, what to look at, which screenshot
   you need. They respond fast to a precise ask.

What you *can* do without them:
* `tools/pd-assets/pd_preview.py <model.glb> <out.png> --clip <clip.glb> --frames 8 --yaw 90 --highlight Bone_9`
  — CPU render, no GPU, no engine. This is what settled "is this animation authored
  sideways?" (it wasn't).
* Headless measurement through the real engine types — spawn a `World`, `toggle_mode()`,
  evaluate a hunter's `LayeredAnimator`, print angles. This is what localised the
  aim-overlay root bug. Destructure `let World { enemies, enemy_arm, char_models,
  enemy_weapon_lib, .. } = &mut world;` to get disjoint field borrows.

```
cargo test --release                  # 309 green
cargo build --release                 # the user tests target/release/build-and-hide.exe
```

---

## 1. The 32-slot direction table

**What PD does.** `chr_attack` (`chraction.c:2825`) picks the firing animation by the
bearing to the target:

```c
angle = chr_get_attack_entity_relative_angle(chr, attackflags, entityid);
groupindex = angle * 5.0937690734863f + 0.5f;   // 5.09377 == 32 / 2π
if (groupindex < 0 || groupindex > 31) groupindex = 0;
index = random() % animgroups[groupindex]->len;
animcfg = &animgroups[groupindex]->animcfg[index];
```

The tables are `g_StandHeavyAttackAnims[race][32]`, `g_StandLightAttackAnims[][32]`,
`g_StandDualAttackAnims[][32]`, the three `g_Kneel*` equivalents, and `g_LieAttackAnims`
(`chraction.c:1039+`). Each of the 32 slots points at a `struct attackanimgroup`
(`{rows, len}`), and each row is an `attackanimconfig` — the same struct we already
transcribed in `combat/attack_anim.rs`. Adjacent slots share groups in runs, so there
are ~5 distinct animation groups per stance, not 32.

`angleoffset` on a row states how far that animation's aim-zero sits off the body's
facing: `DTOR(0)` on the three rows we have, `DTOR(90)` on `var80065918`'s `ANIM_0004`.
The `flip` path mirrors the index for the other-handed case.

**What we do.** One clip per weapon class, used at every bearing
(`FIRE_RIFLE_IDX` / `FIRE_PISTOL_IDX` / `FIRE_DUAL_IDX`).

**Start here — this is an asset job before it is a code job.** We have exactly three
fire clips exported (`native/assets/enemies/pd/animations/04-06`). The table references
many more (`ANIM_0002 / 0003 / 0004 / 0006 / 0032 / 0041 / 0044 / 0045 / 0046 / 034A`).
So step one is extending the export, and **`pd_roster.json`'s 36-slot order is
load-bearing** — `FIRE_*_IDX`, `CHAR_HIT_START` and `CHAR_HIT_START + HIT_CLIPS.len()`
are arithmetic on those indices. Adding clips means either appending past slot 35 or
introducing a separate directional-fire set; do not renumber in place.

Useful baseline already measured: all three current clips aim within **3.4° of the
model's forward** when sampled alone, so they are the forward-facing slots. Any new
clip should be measured the same way before being wired up.

## 2. The free-for-all seam

`EnemyInstance::pd_target: Option<PdTarget>` is already populated every step by
`pd_lab::step_simulant`, and `emit_pd_shot` already resolves *whoever is on the line*,
so hunter-on-hunter damage, blood, hit parts and authored death reactions all work
today. **What is missing is movement**: `Enemy::update` takes `player_feet`, so a
simulant that targets a packmate shoots it from wherever it happens to be standing
instead of hunting it.

Suggested shape: thread an `EngageTarget { pos: Vec3, id: TargetId }` through
`Enemy::update` in place of `player_feet`, resolved by the `World` from `pd_target`
with the player as the fallback. Consumers to route: `known_player_pos`, `Chase`'s
approach + `flank_point`, the `Attack` standoff/`back_off`, `sample_cover_cell`,
`pick_reposition`, `perceives`/`in_cone`.

**Keep player-specific, do not generalise:** `step.caught`, the `aimed_at` crosshair
sense (there is no packmate crosshair), `set_detectable` (the `N` invisibility toggle),
`squad_alert`, `alert_enemies_to_movement` and `grenade_flush_step`.

This is the largest structural difference from PD remaining, and it is what turns the
lab into an actual combat simulator.

## 3. Barrel axis for weapons with no muzzle-flash model

`EnemyWeaponAsset::barrel_axis()` (`world/mod.rs:1553`) normalises `muzzle_offset`,
which `mesh_muzzle_offset` (`world/mod.rs:1566`) derives from the **muzzle-flash mesh
centroid when there is one, and the gun mesh centroid when there is not**. The second
case is wrong. Measured across the arsenal:

| weapon | muzzle model | resolved axis | |
|---|---|---|---|
| PP7, KF7, AR33, RC-P90, … | yes | `(0, 0, +1)` | correct |
| Sniper Rifle | **no** | `(-0.07, +0.37, +0.93)` | 22° high |
| Rocket Launcher | **no** | `(-0.06, -0.03, -0.998)` | **backwards** |
| Grenade, Proximity/Timed/Remote Mine | no | nonsense | not aimed, harmless |

Dormant — `ENEMY_ROSTER` uses none of them — but the sniper is a plausible roster
entry and `enemy_weapons.rs` already has a test asserting a sniper hangs back at range.

Reproduce by iterating `world.enemy_weapon_lib()` and printing
`(a.name, a.muzzle.is_some(), a.muzzle_offset, a.barrel_axis())`. Fix by deriving the
axis from the gun mesh's longest principal axis (or the farthest vertex from the grip
origin) rather than the centroid, and assert the result per weapon so it cannot regress.

---

## Then: promote out of the spike

Only after 1–3 are working and the user has playtested them.

**What gates it today.** `PD_LAB` → `PdLabConfig::from_env` → `World::enable_pd_lab`,
which also **replaces the level** with `designs::pd_lab()` (a bare room). Per-hunter
gates are `inst.pdsim.is_some()`, `inst.pd_anims`, and `world.pd_lab.is_some()`.

**The real blocker is the family split.** `World::spawn_family` picks GoldenEye *or*
Perfect Dark for the whole wave, because a body must be driven by its own family's
clips (`PD_TEMPLATE_CLIPS`). A mixed roster needs the clip template resolved per
hunter rather than per wave.

**Decisions to put to the user, not to assume:**
* Does the zeroing model replace the hit-roll for *all* hunters? That retires
  `MAX_HIT_RATE` and the per-weapon `accuracy` tuning together — a big, deliberate
  change (see §9 on why the cap is off the PD path).
* The burst cadence and the 3D torso-capsule shot are family-agnostic and strictly
  better; they could graduate to GoldenEye hunters on their own, ahead of the rest.
* Should the radar stay lab-only? It is currently gated on `pd_lab_active()` — one line
  in `app.rs` to show it always.

## Kill-switches that exist (all default ON unless noted)

`set_pd_omniscience`, `set_utility_ai`, `set_local_avoidance`, `set_head_look`,
`set_wall_clearance`, `set_authored_reactions`, `set_ragdoll`, and
`set_grenades` (**default OFF** — hunters were catching their own blast; the
safe-distance check tests the moment of release, but the round flies ~1 s while the
pack keeps closing).

## Traps this track has hit

1. **CPU-side green does not mean it works.** A `MAX_JOINTS` truncation drew every PD
   body as a black fan while all headless checks passed.
2. **Measure through the real stack, not the raw asset.** The aim bug was invisible in
   the clip and only appeared once composed — clip-alone `+1.6°`, through the layer
   stack `−78.2°`.
3. **The obvious fix has twice been wrong.** "Rifle is attached to the wrong hand" and
   "rewrite the aim solver" were both discarded after measurement. Verify before you
   build.
