# Enemy retro: animation, AI, combat (2026-09-24)

This is discovery only. No code was changed. It covers four read-only audits: our animation, our AI, our combat, and the Perfect Dark decomp (`reference/pd-decomp`). I checked the headline claims against the code myself.

- Paths are relative to `native/crates/game/src/` unless they are marked `engine/` or `pd:` (for `pd:`, read as `reference/pd-decomp/src/game/`).
- Line numbers are as read today. Another session was editing `world/lifecycle.rs` at the time, so expect some drift.
- **[V]** means I read it and confirmed it. **[I]** means it is inferred and still needs a check, usually in a playtest.

Goal: make hunters cleaner and closer to Perfect Dark's N64 dynamic.

---

## 0. The one-paragraph version

Most of PD's "alive" feel comes from the **body**, not the brain. PD's simulants are simple deciders: distance bands, no cover, no dodge. They feel dynamic because of cheap body mechanisms:

- the legs follow the direction of travel while the torso aims;
- the run clip plays **in reverse** to backpedal;
- velocity is smoothed and slows on arrival;
- a procedural flinch on hit, plus a shove, and they **keep firing**;
- crossfades start from the pose on screen and take 16 ticks.

We did the opposite. We ported PD's *decision* layer faithfully and then added more tactics on top: dodge, flank, cover, suppress. The body layer is still speed-only locomotion under a torso that always faces the target. The result reads as smart but stiff. Our pose-layer stack is a good base for PD's body tricks, because nearly every one of them fits as another layer.

---

## 1. Six findings that change the picture

1. **In the shipped game, every hunter is omniscient, including under `AI=ours`. [V]**
   - `lifecycle.rs:342` sets `omniscient = !shopping && (pd_mode || (pd_omniscience && inst.pdsim.is_some()))`.
   - `pd_omniscience` defaults to true (`world/mod.rs:3116`).
   - `pdsim` is always `Some`, at spawn (`lifecycle.rs:1353`) and at respawn (`respawn.rs:107`).
   - As a result, these never run in play: Search, Investigate, fan-out search, `hear_noise` (gunfire and footsteps), `squad_alert`, and the head-scan sweep.
   - The doc comment says "PD-lab only, a GoldenEye hunter is never affected" (`mod.rs:2231`). That is false.
   - **This is the biggest design question here, because this is a hide-and-seek game.**

2. **We gave PD's simulant brain a PD *guard's* hit reactions. [V]**
   - In PD, `chr_begin_argh` returns early for bots (`pd:chraction.c:3426`: `if (race == RACE_EYESPY || chr->aibot) return;`).
   - A simulant that is shot plays **no hit-reaction clip and is not stunned**. It gets three things: a procedural flinch (`chr_flinch_body` / `chr_flinch_head`), a grunt, and a physical shove (`shotspeed += vec*0.75`). It keeps firing.
   - Ours plays a PD injury clip, stuns, and drops the trigger (`world/combat.rs:1619`).
   - The comment there claims "there is no aibot exemption". That is wrong.
   - Stun takes the maximum of the current and new values (`enemy.rs:1384`), so sustained automatic fire can stun-lock a hunter until it dies. [I]

3. **The body always faces its target, even through walls, and locomotion only reads speed. [V]**
   - The pdsim yaw drives `render_yaw` for every hunter, and `advance_facing` is dead code (`hunt.rs:168`).
   - The gait blend reads only the scalar speed. There is no strafe, backpedal or turn clip.
   - Backpedal, evade and the reposition jukes all play a *forward* gait while the body moves sideways or backwards. [I, on how bad it looks]
   - PD handles this in two ways:
     - It only turns to face the target when `bot_is_about_to_attack` (`pd:bot.c:831`, `:959`): target in sight, recently seen, or nearby. Otherwise it faces the direction of travel.
     - It twists the legs up to ±60° toward velocity while the waist counter-rotates, and reverses the run clip when travel is more than ~94° behind (`pd:player.c:5472`, `bot.c:790`).

4. **The fire animation never plays. [V]**
   - The aim overlay is pinned to one frame (`shoot.0`) and never advances (`lifecycle.rs:1216`, `combat.rs:1833`). The burst is only a timer.
   - The 32-slot direction table is fully ported (`attack_anim.rs`), but it only contributes timing, aim limits and one held frame.
   - The authored raise, turn and shoulder motion is thrown away.
   - An engaged hunter already holds its gun up, so there is **no visible windup**. Nothing tells you a shot is coming.

5. **The physics ragdoll death no longer plays by default. [V]**
   - `pd_reaction` falls back to `Torso` and always returns a row, so every bullet death plays an authored clip (`combat.rs:1446`).
   - The ragdoll is only reached under `GE_CLIPS=1`, or as the *stagger* for a non-lethal blast.
   - Blast deaths use a stale `hit_part` from an earlier bullet, so an explosion can play a headshot death, and nobody is thrown.
   - The "much better, I like the look of that" ragdoll from July is effectively switched off.

6. **Hit and death clips pop in and out. [V]**
   - Two animation brains (`AnimPlayer` for one-shots, `LayeredAnimator` for everything else) are joined by a boolean hard switch (`hunt.rs:343`).
   - A one-shot crossfades from the mixer's *hidden* clip, not from the pose on screen.
   - Aim, head-look and foot IK all drop out in the same frame.
   - When the one-shot ends, locomotion comes back with no blend at all.
   - `AnimPlayer::start` throws away any fade already in progress.

---

## 2. What works (keep it)

**AI and movement**
- **One chokepoint for every move.** `Enemy::try_step` handles doors and step heights for all three brains.
- **ORCA crowd behaviour.** Doorway funnels, ringing the player, fan-out.
- **The standoff dead-band with back-off.** Weapon-scaled standoff (`standoff_for`). The shotgun no longer fires full-auto. Both old "open" issues are fixed.
- **Engagement rules.** Attack is LOS-debounced. The engagement band is 3D. A hunter out of sight advances rather than holds, which is PD's rule.
- **Fetch** as a first-class state with per-pickup write-off. The vent stake-out.

**Combat**
- **PD's trigger model.** Zeroing aim, burst cadence (the measured lethality limiter), per-round spread, the PD reload rule.
- **Friendly fire** comes out of the nearest-body hit test, the way it does in PD.
- **One death funnel** (`start_death`) for bounty, scoreboard and respawn.

**Animation**
- **The pose-layer stack** (`engine/skeletal/layers.rs`) is the right architecture:
  - a locomotion blend space with a shared foot phase;
  - an upper-body clip overlay;
  - cone-clamped aim offset;
  - head look;
  - an additive recoil layer.
- **Ground-adaptive foot IK** and the ragdoll substrate are both solid and playtest-confirmed.
- **The PD data ports** were done with measurement discipline: one shared GE/PD animation bank, direction table, injury/death tables, thud frames.

**Harnesses**
- `probe_hunt` sweeps real levels and is the right acceptance bar.
- `ai_testbed` has 36 world-level scenarios. No AI test is `#[ignore]`d; the only ignored tests are benchmarks.

---

## 3. What doesn't work

### 3a. AI

**Brains and dead paths**
- **There are three decision brains, and one is dead in shipping.**
  - `Enemy::update` picks between them (`enemy.rs:1622`):
    - `pd_step` under `AI=pd`;
    - `util_step`, the utility layer, which is the default;
    - the legacy FSM, which only runs in tests.
  - **Most of `enemy.rs`'s 33 unit tests exercise the FSM**, which nothing ships. Only a couple set `set_utility(true)`.
  - The FSM and utility paths have **diverged** (`enemy.rs:2333` and nearby):
    - utility Chase ignores `move_toward`'s "arrived" result;
    - Investigate reads a different field in each;
    - Cooldown always goes to Chase in utility, while the FSM re-evaluates.
  - About 250 lines of Attack, Cooldown, TakeCover and Peek are copy-pasted between the two paths.
- **The simulant's speed multiplier is computed and never applied.** `SimOutput::speed_mult` is only shown on the overlay. The `pd_lab.rs` header claims otherwise.

**Difficulty dial**
- **One dial drives three unrelated things:** the PD tier, our tactics intensity, and hunter HP (1× to 4×) (`mod.rs:3714`).
- The level default is dial 4, but **every `TestArena` pins the maximum** (`ai_testbed.rs:57`). The lab never tests what people actually play.

**Path following**
- **No path smoothing.** A* is 4-connected on 0.25 m cells. The mover visits every cell to within 0.1 m. The heading flips 90° per cell on any approach where the hunter can't see its destination.
- **The heading turns instantly.** The only rate-limited yaw is the simulant's.
- **Stuck recovery is passive.** The hunter stands still for 0.4 s. It doesn't repath or sidestep.
- **ORCA has no wall constraints.** If avoidance pushes the velocity into a wall, it falls back to the raw preferred velocity.

**Tactics and timing**
- **Cover has no reservation and no arrival facing.** The pack can pick the same cell.
  - This is the likely cause of the open report "ran up the stairs into another room instead of shooting".
- **The reaction delay is paid twice.** Utility `Alert` adds one, and pdsim's `shootdelaytimer` adds another.

### 3b. Combat

**Damage**
- **Every enemy weapon does 8 damage** (`ENEMY_DAMAGE`, `combat/enemy_weapons.rs:127`). Sniper, shotgun and pistol all hit the same. The player has 100 HP plus 100 armour, so it takes about 25 hits to die.
- **`EnemySecondary.damage` is never read.** An enemy "grenade launcher" secondary fires an 8-damage hitscan.
- **Two hit classifiers disagree.**
  - Damage is decided by impact *height* (`HitZone`: head ×4, torso ×1, legs ×0.6, arms count as torso).
  - The reaction is decided by the nearest *bone* (`HitPart`).
  - So a raised forearm at head height takes 4× damage and plays a forearm flinch.
  - PD's ratios, relative to torso, are head 2× and limbs 0.5× (`pd:chraction.c:4729`).
- **Hit part and blood are worked out on the wrong pose.** The code uses `inst.anim.skinning_matrices` (the hidden idle or band pose) instead of `final_pose`, which includes aim and IK (`combat.rs:1570`).
- **Friendly fire always hits the torso.** The impact point is hard-coded to 55% of body height (`combat.rs:2241`).

**Bugs**
- **Bounty is paid for every death, including hunter-on-hunter kills and self-kills** (`start_death`, `combat.rs:1435`).
- **Knockback always comes from the player's eye**, even when a packmate or turret did the killing (`combat.rs:1612` and `1643`).

**Readability**
- **Gunfire isn't positional.** It plays at a flat 0.7 volume, with no falloff.
- **Misses leave no trace:** no spark, no whizz.
- **No damage-direction cue and no view flinch.**
  - PD pushes and flinches the player on every hit (`pd:chraction.c:4795`).

**Weapons**
- **Dual wield is only a handicap.** It widens spread ×1.5 and shares one magazine and one cadence. In PD, both hands fire.
- **Hunters can pick up explosive weapons and fire them as unlimited hitscan.** [I] The pickup filter lets any weapon through, and `clip == 0` means "never reload".

**Fairness**
- **Crouching doesn't shrink the player's hit capsule.** [I] It is fixed at 0.75 to 1.55 m, while crouch height is 0.75 m.

### 3c. Animation

**Missing animations**
- **Reload, fetch, crouch, ladder and vent** have no animation. Reload is a timer with the gun left raised.
- **An unarmed engaged hunter raises empty hands into a gun hold.** [I] `want_aim` has no unarmed check (`hunt.rs:370`).

**Foot sliding**
- The gait anchor speeds (0 / 1.5 / 3.5 / 5.0 m/s) are guesses carried over from the 3DS JS. They were never measured from the clips.
- `stride_scale` only corrects for avoidance slowdown, and only when foot IK is on.
- The PD gait clips are authored in place. So the fix is to measure stride from how far the planted foot sweeps.

**Pickups and grip**
- **A pickup of a different weapon class keeps the old grip and barrel axis** (`tools/pickup.rs:606`).
- The directional fire rows then look for the new class, find nothing, and are silently refused.

**Skeleton issues**
- **The `Blend_*` seam joints are not updated by the procedural layers.** [I, high confidence] On PD bodies these are siblings that carry a baked half-rotation. Aim, head look and knee IK rotate `Bone_n` only, so the elbow and knee seams probably crease.
- **Death clips skip foot IK and ignore walls.** Corpses on stairs float or clip. [I]

**Memory cost**
- **Each hunter deep-clones a full 51-clip `AnimPlayer`**, plus more copies inside its layers.
- `install_fire_row` clones a clip on every burst.

### 3d. Flags

- **Real toggles (env var or level setting):**
  - `AI`, `BODIES`, `GE_CLIPS`, `ARSENAL`, `PD_EXPLOSIONS`, `ARMED_HUNTERS`, `PD_LAB*`, `ANIM_DEBUG`
  - `GE_BODIES` no longer exists, so older notes that mention it are stale.
- **Setter-only (tests reach them, players can't):**
  - `utility_ai`, `pd_omniscience`, `local_avoidance`, `wall_clearance`, `head_look`, `foot_ik`, `ragdoll`, `authored_reactions`, `hit_reactions`, `grenades`
- **There are five hit-reaction branches** (`combat.rs:1607-1678`):
  - death;
  - the PD injury table;
  - the ragdoll stagger;
  - the GE flinch;
  - "sim style", which is no flinch.
- **In the default game, only two of them run: death and the PD injury table.**
- **Dead code:**
  - `breach_tick`
  - `advance_facing` / `TURN_RATE`
  - the `is_fire_clip` guard
  - `AnimPlayer::fire_window`
  - the locomotion clips 0–3 inside every `AnimPlayer`
  - `SimOutput::speed_mult`
- **Stale docs.** This list is long, but each item is a one-line fix. Every one of them misleads the next session.
  - Hit-roll text in `combat.rs:1935`, `:2042`, `:2287`, `mod.rs:505` and `enemy_weapons.rs:75-176`.
  - "AI=pd (the default)" in `enemy.rs:2836` and `combat.rs:~1773`.
  - "no aibot exemption" in `combat.rs:1622`.
  - "nothing consumes thud" in `hit_anim.rs:22` and `:96`.
  - "36 slots / PD_LAB only" in `mod.rs:731`.
  - "rotations free" in `ragdoll.rs:11`.
  - `repro_chase_walk_in_place` in `ai_testbed.rs:402`, which doesn't exist.
  - `DESIGN_AI_PD_VS_OURS.md` §6, which says ours has "perception, search".

---

## 4. The PD mechanisms we don't have, ranked by feel per unit of effort

These are from the decomp. `mrg` is a crossfade length in 60 Hz ticks.

| # | Mechanism | What it gives | Decomp to read | Cost |
|---|---|---|---|---|
| 1 | **Procedural flinch.** Body gets a random 3-bit direction at about 15° on shoulders and waist. Head gets an octant from the shot angle, snapped 60–85°. 30 ticks: 10 up, 20 decay. | A hit reads as a hit on any action, with no clip and no stun | `chr.c:1532-1600`, `chr_handle_joint_positioned:1768-1830`, tick at `chr.c:2725` | ~60 lines, fits as an `AdditiveDecayLayer` |
| 2 | **Hit shove.** `shotspeed += dir*0.75`, clamped to 1.5 and damped. | The body gets pushed by the shot | `chraction.c:4915`, `chr.c:640`, `bondmove.c:1841` | small, nav-safe through `try_step` |
| 3 | **Velocity smoothing** (`0.945·v + input` at 240 Hz, about a 75 ms ramp) **plus arrival ×0.5 within 2 m** | Starts and stops stop snapping | `bot.c:bot_update_lateral:1152`, `bot_calculate_max_speed:1096` | ~10 lines |
| 4 | **Facing only when "about to attack"**, otherwise face the direction of travel | No more wall-staring crab-walk; runs read as runs | `bot.c:bot_is_about_to_attack:831`, `:959` | small, in `pdsim` |
| 5 | **Leg/torso twist plus reversed-run backpedal.** Leg angleoffset is clamped to ±60° and slewed at 6°/tick; the waist counter-rotates; the clip plays in reverse beyond 93.6°; clip speed ∝ move speed, capped at 1.2 | This is PD's entire strafe look, and it ends the moonwalking | `player.c:5472-5700`, table `player.c:4146-4190`, `bot.c:bot_apply_movement:763` | medium, needs a signed rate in `LocomotionBlendLayer` |
| 6 | **Crossfade from the pose on screen** (16-tick merge; wait for the merge before re-choosing, which is `CHRHFLAG_NEEDANIM`; speed tweening) | Hits and deaths stop popping | `lib/model.c:model_copy_anim_for_merge:1733`, `model_set_animation2:1777` | medium |
| 7 | **Directional deaths.** Shot from behind: 2 in 5 fall forward. Wall behind: slump. Explosion: 8-octant throw. | Deaths that respond to the room and the shot | `chr_begin_death:3180-3260`, `chr_yeet_from_pos:3609` | medium; for blasts, just send them to our ragdoll |
| 8 | **Wounded gait.** A leg hit gives a limp; an arm hit gives an arm-clutch run, for the rest of that life. | Visible damage history | `chr_gopos_choose_animation:5852` | needs clips `01F8/01F9/020A/020D` |
| 9 | **Aim-idle breathing loop.** Tiny frame loops inside an attack clip at speed 0.1, e.g. `ANIM_0002` frames 35–40. | The aim pose stops being a mannequin | table `player.c:4155` | small |
| 10 | **Play the real attack clip for each burst**, from `fire.start` to `shoot.0` | A readable windup; the direction table's authored turn finally shows | our `enemy_combat_step` | small–medium |
| 11 | **Pose mirroring** (`flip`) | Restores about a third of PD's rows and random left/right variety | `lib/anim.c:424` | medium–large |
| 12 | **Per-character tempo** (`speedrating`, `arghrating`) and guard aim sway (a 1 s sine, 14°→1° by range) | No two hunters move in lockstep | `chr_get_ranged_speed:1491`, `chraction.c:9296` | small |

**Solo-guard behaviours belong to Classic Level mode, not to the Hunt simulants:**
- dodge-on-aim (sidestep or jump-out when inside 20° of your aim, roll < 100/256);
- morale ("safety" < 3 → retreat, warn friends, surrender);
- bored/look-around idles;
- surprise reactions;
- grenade with one thrower per squad;
- voice quips.

Read `gailists.c:2020-2060` and `:2733-2860`, and `chraicommands.c:6472`.

---

## 5. Suggested plan

The stages are ordered like the levelgen retro. The user playtests after each stage before the next one starts.

**Stage 0: tell the truth (bugs plus docs; almost no change to feel)**

*Bugs:*
- Resolve hit part and blood on `final_pose` (`combat.rs:1570`).
- Pay the bounty only when the player (or their turret) made the kill.
- Pass the shooter's position into the knockback.
- Clear `hit_part` on blast damage.
- Give friendly fire its real impact point (`combat.rs:2241`).
- Stop hunters picking up explosive weapons they can't fire properly.

*Docs and dead code:*
- Fix every stale doc in §3d. Update `DESIGN_AI_PD_VS_OURS.md` and the `pd_lab.rs` header.
- Delete `breach_tick`, `advance_facing`/`TURN_RATE`, the `is_fire_clip` guard and `AnimPlayer::fire_window`.
- Make `pdsim` non-optional, which removes three `is_some()` branches.

**Stage 1: settle two decisions (you need to make these)**

- **What hunters know.** Replace the hidden omniscience coupling with a per-level `Knowledge` setting in the PLAY tab: `Omniscient | Perceive | PdAware`.
  - `PdAware` is `bot_is_about_to_attack` plus our search once that goes false.
  - A hide-and-seek level probably wants `Perceive` or `PdAware`.
  - Add lab scenarios at dial 4 and with `Perceive`.
- **Which PD we are emulating.** Hunt uses PD *simulant* reactions (flinch, shove, no stun, keep firing). Classic Level uses PD *guard* reactions (injury tables with PD's gating).
  - This collapses the five reaction branches into `ReactionStyle { Simulant, Guard }`.
  - Blast deaths go to the ragdoll.
  - Decide separately whether bullet deaths use authored clips, the ragdoll, or authored clips that hand off to the ragdoll.

**Stage 2: animation foundation (the fluidity fix)**
- Crossfade from the pose on screen, in and out of one-shots, over about 0.27 s.
- Ease the aim, look and IK weights instead of zeroing them.
- Measure gait anchor speeds from how far the planted foot sweeps.
- Base `stride_scale` on actual ground speed whatever the IK setting.
- Share clips through `Arc`.

**Stage 3: PD locomotion**
- Mechanisms 3, 4 and 5 from §4: velocity smoothing and arrival slowdown; facing gated on "about to attack"; leg twist and reversed backpedal.
- Rate-limit the heading.
- Look-ahead path smoothing: skip to the farthest coplanar waypoint with clear LOS, and keep stairs cardinal.

**Stage 4: PD hit feel**
- Procedural flinch layer and shove (mechanisms 1 and 2), with no stun for `Simulant`.
- Anti-stun-lock gating for `Guard`.
- Directional deaths (mechanism 7).

**Stage 5: combat readability and identity**
- A visible windup, by playing the attack clip.
- The aim-idle loop.
- Positional gunfire (reuse `door::falloff_volume`).
- Miss sparks near the player, a damage-direction wedge, and a view flinch on hit.
- Per-weapon enemy damage, including the secondary.
- PD's NPC shotgun distance multiplier (`pd:chraction.c:4515`).
- One hit classifier.
- A crouch-aware player hit capsule.

**Stage 6: structure (can run alongside stages 2 to 5, since it touches different files)**
- Delete the FSM and port its tests to the utility layer.
- Pull Attack, Cooldown, Cover and Peek out into one shared executor.
- Split `enemy.rs` into perception, knowledge, decide_utility, decide_pd, locomotion, tactics, fetch and vitals.
- Split `world/combat.rs` into player fire, hunter fire, explosives and damage, with one `DamageEvent { victim, attacker, amount, point, dir, part, kind }`.
- Split the difficulty dial into three: tier, tactics and HP.
  - Stop scaling hunter HP under `AI=pd`; PD bots don't have it.
- Move `PD_DIST_BANDS` and `PD_EXPLOSIONS` out of the parked `pd_weapons.rs`.

**Stage 7: asset-heavy work (larger jobs)**
- Walk-attack and run-attack clips per wield mode, exported through `pd_gltf.py`. This also fixes the stale grip after a pickup and the empty-hand aiming.
- The wounded gait.
- Pose mirroring.
- The `Blend_*` seam fix-up after the procedural layers.
- Turn-in-place clips.
- Stop animations that settle on a planted foot (`model_set_anim_speed_auto`).
- Cover reservation.
- Weapon scoring for fetch (`botinv_score_weapon`).
- For Classic Level only: the solo-guard behaviours.

**Not recommended:** Recast. The grid stays as the nav runtime; that question is closed.

---

## 6. Inferred items to confirm in a playtest or a probe first

- How bad the forward-gait-while-strafing looks (§1.3).
- Stun-lock under sustained automatic fire (§1.2).
- `Blend_*` seam creasing at the elbows and knees under aim or IK (§3c).
- The crouch hit capsule (§3b).
- Explosive weapons in hunter hands (§3b).
- Corpses floating on stairs (§3c).
