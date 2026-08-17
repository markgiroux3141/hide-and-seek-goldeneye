# Handoff — port Perfect Dark's arsenal (player + enemies), and PD's explosions

State: branch `feat/pd-hunters-ship` @ `c647e8d`, 332 tests green, release built.
The PD **hunter** track is complete and shipped (`DESIGN_PD_SIMULANT_AI.md` §13–19,
`HANDOFF_PD_FIDELITY.md`). Hunters are Perfect Dark simulants on Perfect Dark
animations, drawing from all 44 GoldenEye + 6 PD bodies.

**This handoff is the next piece: the guns.** Our arsenal is still GoldenEye's 23
weapons with one fire mode each. PD's is ~33 weapons with **two functions each** and a
fully-authored data table for all of it.

The recon below is done and verified — every claim here was checked against the decomp
or run locally. Start from the decisions, not the code.

---

## How to work on this track

Two rules the user has stated explicitly, both learned the hard way:

1. **Always go back to the decomp** (`reference/pd-decomp`, gitignored — `reference/README.md`
   says how to re-clone). Do not invent a plausible adjustment to make something look
   right. On the hunter track the *obvious* fix was wrong more often than not: the barrel
   axis was not recoverable from the gun mesh, the pack fighting itself was a missing team
   check rather than a scoring problem, packmates blocking fire was our raycast rather than
   crowding, and "PD's bind pose differs from GoldenEye's" was simply false and cost 38
   bodies. Every one of those had a decomp answer and a headless measurement that settled it.
2. **Do not launch and drive the game yourself.** It steals focus and makes the user's
   machine unusable. When something has to be seen on screen, stop and hand them a specific
   brief: exact command, what to do in-game, what to look at, which screenshot.
   *PowerShell note:* env vars need `$env:NAME = "value"` as a separate statement —
   `NAME=value cmd` is bash-only and fails.

What you can do without them:
* `python tools/pd-assets/pd_model.py info|obj <file.bin> [out.obj]` — parse/convert any PD
  model. `pd_tex.py` decodes textures, `pd_preview.py` CPU-renders a GLB to PNG.
* Headless measurement through the real engine types. The pattern that has repeatedly
  settled arguments on this track: **assert the asset against the decomp**, not against
  itself. `cargo test -p game fire_animation -- --nocapture` is the model — it measures each
  exported clip's barrel yaw and compares it to the `angleoffset` read out of `chraction.c`.
* `cargo test -p game -- world::ai_testbed` runs the AI jank scenarios; `JankMonitor`
  reports stalls, thrash, overlap and occlusion.

```
cargo test --release      # 332 green today
cargo build --release     # the user tests target/release/build-and-hide.exe
```

---

## What PD gives us, and where it is

### The weapon table — `game/invitems.c` (172 KB), fully transcribable

| thing | where | shape |
|---|---|---|
| `struct weapondef` | `include/types.h:3023` | models, 4 anim scripts, **`functions[2]`**, 2 `ammodef`s, aim settings, viewmodel placement, part visibility, name/manufacturer/description text ids, flags |
| `struct funcdef` + 7 subtypes | `include/types.h:2910–3010` | `funcdef_shoot` / `shootsingle` / `shootauto` / `shootprojectile` / `throw` / `melee` / `special` / `device` |
| `struct ammodef` | search `invammo_` | ammo type, casing, **clip size**, reload animation, flags |
| the entries | `game/invitems.c` | 84 `invitem_*` definitions; roughly 40 are weapons, the rest equipment/props/items |
| the MP weapon set | `include/constants.h:2982+` | 33 `MPWEAPON_*` — the arsenal a multiplayer match can draw from, and the natural scope for "all the guns" |

`funcdef_shoot` carries **damage, spread, recoverytime60, recoildist, recoilangle,
slidemax, impactforce, duration60, shootsound, penetration**. `funcdef_shootauto` adds
**initialrpm / maxrpm** (autos spin up — we have a flat fire rate) plus
`turretaccel`/`turretdecel`. `funcdef_shootprojectile` adds **model, scale, speed,
speeddecel, traveldist, timer60, hitspeedpreservationfrac**.

This is the same situation as `attackanimconfig` on the hunter track: **the numbers we
hand-tuned exist as authored data.** In particular `weapondef.muzzlez / posx / posy / posz
/ sway` is the **viewmodel placement** we guessed in `weapon-config.json` and carry as
`WeaponStats::model_offset` / `pivot_offset` / `muzzle_offset`.

### `FUNCFLAG_*` — `include/constants.h:1037–1061`

25 behaviour flags on a firing function. Several are things we already have in some form
(`BURST3` is our `PD_BURST_ROUNDS`; `NOMUZZLEFLASH` matches the weapons with no flash mesh),
and several are whole features: `STICKTOWALL`, `MAKEDIZZY`, `DISARM`, `FLYBYWIRE`,
`EXPLOSIVESHELLS`, `HOMINGROCKET`, `CALCULATETRAJECTORY` (throwables land on the crosshair),
`DISCARDWEAPON`, `THREATDETECTOR`, `PSYCHOSIS`, `BLUNTIMPACT`, `NOSTUN`, `NOAUTOAIM`.

`WEAPONFLAG_AICANUSE` on `weapondef.flags` says exactly which guns an enemy may hold —
which answers "the player and the enemies should be able to use these guns" from the data
rather than from our judgement.

### Explosions — `game/explosions.c:41`, `struct explosiontype` at `include/types.h:4313`

A **data table** (`g_ExplosionTypes[]`, commented column-by-column in the source) of
rangeh, rangev, changerateh, changeratev, innersize, blastradius, damageradius, duration,
propagationrate, flarespeed, smoketype, sound, damage.

**This matters more than it looks.** `combat/explosives.rs` opens with: *"There is no 3DS
FPS oracle for any of this — the JS shipped the weapon GLBs but never wired a projectile or
explosion system. So this is authored fresh."* PD is the oracle we did not have. Two
structural ideas ours lacks:

* **Separate blast and damage radii**, and separate horizontal/vertical ranges — ours is
  one spherical falloff.
* **Propagation** (`propagationrate`, and the growth rates) — a PD explosion *expands* over
  its duration rather than applying instantly, which is why chain reactions and "the blast
  caught up with me" read the way they do. Relevant to a known open bug: `set_grenades` is
  **default OFF** because hunters catch their own blast — the safe-distance check tests the
  moment of release while the round flies ~1 s. A propagating blast with a real
  `damageradius` may be the honest fix.

Read `explosions.c` around lines 95–340 for how a type is instantiated, propagated and how
it lights the room.

### Assets — verified working

* **All 106 gun models parse with existing tooling, zero failures.** `files/guns/*.bin`;
  `pd_model.py info falcon2.bin` → 802 verts, 565 tris, 8 geometry groups, 14 texconfigs,
  and node types `GUNDL`(8) / `POSITION`(43) / `TOGGLE`(8). Those nodes are the gun's
  animated parts (slide, magazine) and its muzzle position.
* **61 third-person `files/props/chr*.bin`** — the model an *enemy* holds. Already recorded
  in `pd-weapon-mechanics` memory: there are **two models per gun**, and only the `chr*` one
  carries the `CHRGUNFIRE` node (authored muzzle-flash position + size, which doubles as the
  barrel origin). Grabbing the wrong one is an easy mistake, and it is exactly what
  `DESIGN_PD_SIMULANT_AI.md` §15 had to work around by measuring a convention instead.
* `pd_gltf.py` currently exports **characters** (skinned + animated). Its prop path was
  retired. A gun needs a **new export path**: static-ish mesh + named part nodes + textures
  via `pd_tex.py`. Model this on the char path and keep the same discipline — the roster
  manifest (`pd_roster.json`) plus a batch command, so it is reproducible.
* `weapondef` also names a **lo-poly LOD model** per gun. We have no LOD system; ignore it,
  but do not be confused by the `*lod.bin` files.

### Gun animation scripts — `struct guncmd`, `include/types.h`

A **12-opcode bytecode** (`GUNCMD_*` in `constants.h`): END, SHOWPART, HIDEPART,
WAITFORZRELEASED, ALLOWFEATURE, PLAYSOUND, INCLUDE, RANDOM, and a few more. Four scripts per
weapon: equip, unequip, primary→secondary, secondary→primary. Plus `gunviscmds` and
`partvisibility` for which parts show when.

Small enough to port properly if the viewmodel wants it. **Assess before committing** — our
viewmodel is a single mesh with a recoil kick, so most of what these scripts express has
nowhere to land yet.

---

## Our side, and the size of the gap

| | ours | PD |
|---|---|---|
| weapons | 23 GoldenEye (`combat/config.rs`, 875 lines) | ~33 MP weapons |
| fire modes | **one** per weapon | **two** (`functions[2]`) |
| fire kinds | `FireKind::{Hitscan, Projectile, Mine}` | 7 funcdef subtypes |
| enemy classes | 2 (`EnemyWeaponClass::{Pistol, Rifle}`) | per-weapon, `AICANUSE` flag |
| auto fire | flat `fire_cooldown` | `initialrpm` → `maxrpm` spin-up |
| explosions | one authored spherical falloff | 12+ typed, propagating |
| viewmodel placement | hand-tuned `weapon-config.json` | authored `posx/posy/posz/muzzlez/sway` |

Files you will touch: `combat/config.rs` (the table), `combat/mod.rs` (`Weapon` runtime),
`combat/enemy_weapons.rs` (enemy defs + classes), `combat/viewmodel.rs`, `combat/explosives.rs`,
`combat/attack_anim.rs` (the fire animation is chosen per weapon class today — PD names an
animation *per firing function*), `world/combat.rs` (the shot paths), and `shop.rs` /
`ENEMY_ROSTER` for what is buyable and what enemies carry.

**Note the family precedent.** The hunter track ended up with two body families and two clip
sets coexisting, selected per hunter, with `BODIES=` / `GE_CLIPS=` to narrow them. The same
shape is available here and is probably right: a PD arsenal alongside the GoldenEye one
rather than replacing it. Ask (see below) rather than assuming.

---

## Decisions to put to the user first

Do not start coding until these are answered — each one changes the work materially.

1. **Replace or coexist?** Does the PD arsenal *replace* GoldenEye's 23 weapons, or sit
   alongside them (the way the two body families now do)? Coexisting keeps the shop's 24
   buyable guns and the GoldenEye feel available, at the cost of a weapon-family concept in
   the config. Replacing is simpler but throws away the tuned GoldenEye arsenal.
2. **Secondary fire.** PD's defining weapon feature is `functions[2]`. Giving the player
   access needs a **new control** (PD used a modifier + trigger). Is that wanted, and on
   which key? Without it, half of every PD weapon is dead data — but adding it touches the
   input map, the N64 pad scheme, and the HUD.
3. **Do enemies get secondary functions too?** `chr_attack` picks a firing animation per
   *function*, and `WEAPONFLAG_AICANUSE` gates the weapon, not the function. A hunter with a
   SuperDragon choosing between rapid-fire and its grenade launcher is a real AI decision
   (PD has `botinv_get_dist_config` keyed by weapon **and** `gunfunc`).
4. **Adopt PD's explosion table wholesale?** It would replace the authored `Explosion` specs
   in `combat/config.rs` and add propagation. Strictly better fidelity, and it may fix the
   grenade self-blast that has `set_grenades` defaulted off — but it changes the feel of
   every explosive we already tuned.
5. **Scope of "all the guns."** 33 MP weapons is the clean line. The campaign set adds
   another ~10 (`pp9i`, `cc13`, `kl01313`, `kf7special`, `zzt9mm`, `dmc`, `ar53`, `rcp45`,
   `choppergun`, `watchlaser`) which are mostly re-skins with different stats — cheap once
   the scaffolding exists.

---

## Suggested order

The lesson from the hunter track is that **the data transcribes fast and the systems are
the work**. So build the scaffolding on the boring weapons first and leave every special
case until it can be added as data plus one behaviour.

1. **Assets.** A gun export path in `pd_gltf.py` + roster entries: first-person `guns/*.bin`,
   third-person `props/chr*.bin`, textures via `pd_tex.py`. Verify with `pd_preview.py`, and
   pin the `CHRGUNFIRE` muzzle node — that is the thing §15 had to infer.
2. **Transcribe the table.** `weapondef` + `funcdef_*` + `ammodef` for the MP set, in a new
   `combat/pd_weapons.rs`, with the same provenance discipline as `combat/attack_anim.rs`
   (source line per row, invariants asserted, and a test pinning each row to its exported
   asset). Include `AICANUSE` and the `FUNCFLAG`s even where nothing consumes them yet.
3. **One fire path, two functions.** Generalise `FireKind` to PD's funcdef subtypes and give
   a weapon a primary and secondary. Get the ~20 ordinary guns fully working for **both**
   player and enemy before touching anything exotic. `initialrpm`→`maxrpm` spin-up and the
   burst flags land here and are cheap.
4. **Explosions.** Port `g_ExplosionTypes` + propagation into `combat/explosives.rs`,
   behind a kill-switch so the old authored behaviour is one setter away for A/B — the
   established pattern in this codebase.
5. **Then the special cases**, in rising cost. See below.

---

## Complexities to flag — leave these until the scaffolding is in

Assessed from each weapon's `functions[2]` in `invitems.c`. Grouped by what they actually
need from us, which is the thing that decides their cost.

**Cheap once the two-function scaffolding exists** — these are data plus a flag:
* **Crossbow** (`shoot` + `lethal`) — two damage modes.
* **SuperDragon** (`rapidfire` + `grenadelauncher`) — a second function that is a
  projectile; we already have grenade projectiles.
* **Phoenix** (`EXPLOSIVESHELLS`), **Falcon 2** scope/silencer variants, **DY357LX** — stat
  and flag variations on weapons we will already have.
* **Devastator** (`shoot` + `wallhugger`) — `STICKTOWALL` is our existing mine behaviour.
* **Reaper** (`shoot` + `grind`) — a melee function; needs the melee funcdef path.

**Real features, one system each:**
* **Laptop Gun** (`burstfire` + `deploy`) — a **deployable sentry**. Needs a placeable
  entity that acquires and tracks targets on its own. Notably `funcdef_shootauto` already
  carries `turretaccel`/`turretdecel`, so the turret aim is authored. Our ECS
  (`crate::ecs`) is the natural home, and the `EngageTarget` work means a turret can pick a
  target the same way a hunter does. Still the largest single item here.
* **FarSight** (`shoot` + `targetlocator`) — **sees and shoots through walls**. Touches the
  renderer (a through-wall overlay) and the hitscan (penetration is already a `funcdef`
  field, so the shot half may be nearly free). The *targeting* half is the work.
* **Slayer** (`shoot` + `flybywire`) — a **player-steered rocket** (`FUNCFLAG_FLYBYWIRE`).
  Needs a camera takeover and a controllable projectile. Self-contained but touches input
  and the camera.
* **Tranquilizer** — `MAKEDIZZY` and PD's `dizzyamount` degradation, which
  `DESIGN_PD_SIMULANT_AI.md` §9 already lists as deliberately unported. The simulant model
  has the hook; nothing drives it.
* **Psychosis Gun** (`FUNCFLAG_PSYCHOSIS`) — makes a hunter attack its own side. **Much
  cheaper than it used to be**: §14 gave the FSM an arbitrary `EngageTarget` and §16.1 gave
  us a team predicate (`pd_lab::is_friend`). This is close to "flip the target's team for N
  seconds" now.
* **N-Bomb** — an area effect on an explosion type we will already have.

**No system to attach to; defer or decline:**
* **Cloaking Device** — no invisibility system (the `N` toggle is a dev aid, not a
  mechanic). The simulant model *does* carry `zerocloakspeed` already, unused.
* **Shield** / **Combat Boost** — no shield system; §9 lists `SHIELD`/`TURTLE`'s double
  shield as unported for exactly this reason.
* **X-Ray Scanner**, **Threat Detector**, **IR Scanner**, **Night Vision** — HUD/render
  features, no gameplay dependency, easy to do badly.
* **Disarm** (`FUNCFLAG_DISARM`) — needs a hunter to be able to lose its weapon, which
  touches the weapon-attach and AI weapon-choice paths.

---

## Traps this track will hit — carried forward from the hunter port

1. **CPU-side green does not mean it works.** A `MAX_JOINTS` truncation once drew every PD
   body as a black fan while all headless checks passed. Hand off a playtest brief.
2. **Measure through the real stack, not the raw asset.** An aim bug was invisible in the
   clip and only appeared once composed: clip-alone `+1.6°`, through the layer stack `−78.2°`.
3. **A doc comment asserting an incompatibility is not evidence of one.** "PD's bind pose is
   not GoldenEye's" cost the roster 38 bodies and was false. Measure load-bearing claims.
4. **A test can pass because of the bug you are about to fix.** Three did on the hunter
   track. When a change makes one default universal, expect the tests that encoded the old
   default to be load-bearing in ways nobody wrote down.
5. **Porting a table means porting its filters too.** We took `bot_choose_general_target`'s
   distance walk and its vetoes and left out one line of team check — the whole difference
   between a squad and a free-for-all. `WEAPONFLAG_AICANUSE` and the `FUNCFLAG`s are the
   equivalents here.
6. **An explicit flag must outrank a mode default.** `enable_pd_lab` pinned a body set and
   silently ate `BODIES=ge` for a whole playtest. Apply environment/user choices last, and
   log the resolved answer (`World::roster_summary` is the pattern).

## Context worth reading first

* `DESIGN_PD_WEAPON_MECHANICS.md` — the earlier weapon-side recon, including the two-models-
  per-gun finding and the `attackanimconfig` pointer. **Start here.**
* `DESIGN_PD_SIMULANT_AI.md` §13 (direction tables — the model for transcribing PD data with
  provenance), §15 (why the barrel axis is a convention, which the `CHRGUNFIRE` node makes
  moot), §19 (the two clip/asset conventions that bit us).
* `combat/attack_anim.rs` — the reference example of a transcribed PD table in this codebase:
  source line per row, invariants asserted, assets pinned by test.
* Memory: `pd-weapon-mechanics`, `pd-direction-table`, `explosives-port`, `pd-asset-conversion`.
