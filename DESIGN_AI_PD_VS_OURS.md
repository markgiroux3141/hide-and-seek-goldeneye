# Design — What in the hunter AI is Perfect Dark's, and what is ours

> Audit written 2026-08-18, from the code on both sides: `native/crates/game/src/{enemy.rs,
> pdsim/,world/pd_lab.rs}` against `reference/pd-decomp/src/game/{bot.c,botcmd.c,chraction.c}`.
> Prompted by the observation that hunters dodge when you aim at them — which is ours, not PD's.
>
> Companion to `DESIGN_PD_SIMULANT_AI.md` (the model itself) and `HANDOFF_PD_WEAPONS_PARKED.md`.

## The one-line answer

**Perfect Dark owns the trigger; we own the feet.** Everything about *where a hunter looks,
when it fires and whether the round lands* is a port of PD's bot model. Everything about
*where a hunter goes* — every state, every juke, every search, every piece of cover — is ours,
and PD has no equivalent for most of it.

---

## 1. Purely Perfect Dark (ported, in the shipping game)

| What | Where ours lives | Decomp source |
|---|---|---|
| **Aim convergence (zeroing)** — the error cone that shrinks as a bot settles on a target, and re-opens when the target forces a turn | `pdsim/zeroing.rs` | `bot_update_zero_angle`, `bot.c:1443` |
| **No hit roll.** A real hitscan down the actual barrel; connecting depends only on where the barrel is | `world/pd_lab.rs`, `world/combat.rs` | `bot.c` / `botact.c` |
| **Fire gate: 45° cone**, deliberately generous — bots open fire while still converging, which is why they spray on first contact | `pdsim/mod.rs` `FIRE_FOV_HALF_ANGLE` | `chr_is_target_in_fov(chr, 45, false)`, `bot.c:3606` |
| **Reaction as a decaying `shootdelaytimer`**, not a per-engagement constant | `pdsim/difficulty.rs` | `aibot->shootdelaytimer60` |
| **Difficulty tiers** scaling lethality only (reaction, convergence, speed) | `pdsim/difficulty.rs`, `tier_for_dial` | `BOTDIFF_*`, `bot_calculate_max_speed` |
| **Personality axis** (target selection + vetoes: peace / coward / feud / venge / kaze), orthogonal to difficulty | `pdsim/personality.rs`, `pdsim/targeting.rs` | `bot_choose_general_target` `bot.c:1589`, `bot_passes_*_check` |
| **Per-shot spread table** by weapon | `pdsim/spread.rs` | `botact` spread fields |
| **Omniscient knowledge** — a hunter always knows where you are; sight gates only *shooting*, never *knowing* | `Enemy::set_omniscient`, `known_target_pos` | `aibot->chrsinsight` is LOS-only; position is never gated |
| **Burst cadence as the lethality ceiling** (PD's burst gap), replacing our old global `MAX_HIT_RATE` | `pdsim/`, `world/pd_lab.rs` | `botact` fire timing |
| **Directional fire animations + PD bodies** | `combat/attack_anim.rs`, the direction table | `player_choose_third_person_animation` |

`world/pd_lab.rs`'s header table is the authoritative before/after for the promotion out of
`PD_LAB`, and it is accurate.

## 2. Purely ours (the 3DS FPS port, plus everything added since)

None of this exists in PD's bot code in any form.

**The state machine.** `AiState::{Idle, Search, Investigate, Alert, Chase, Attack, Cooldown,
TakeCover, Peek}` (`enemy.rs:180`), originally transliterated from `3DS FPS/src/ai/EnemyAI.ts`,
now selected by our **utility scorer** (`util_score`, `enemy.rs:1514`) rather than by
transitions.

**Perception as a knowledge gate.** A 12 m / ±60° sight cone (`DETECTION_RANGE`,
`DETECTION_HALF_CONE`), a 3.5 m proximity sense, a peripheral search cone, head-scan sweeps.
PD has none of this for bots — the cone in PD is a *firing* cone, not a *seeing* one.

**Everything that makes a hunter look for you:** search-point fan-out assigned by `World`,
investigate-last-known-position, the gunfire noise ping, squad alert propagation.

**Every evasive or tactical movement:**

* **reactive aim-dodge** (`EVADE_*`, gated by `AiTuning::dodge`) — the hunter senses your
  crosshair (`AIM_SENSE_COS`) and jukes off the line. **This is the one the user noticed, and
  it is ours outright.**
* **burst-and-reposition** (`REPOSITION_*`), **flanking** (`FLANK_MAX_ANGLE`), **suppressing
  fire while closing** (`SUPPRESS_BAND`), **cover + peek-fire** (`COVER_*`, `TakeCover`/`Peek`).

**The difficulty dial** (0–10 → `DiffParams`/`AiTuning`, `world/mod.rs:3046`) — our own ramp
over eight knobs. It *feeds* PD's tier via `tier_for_dial`, but the knobs themselves
(`dodge`, `flank`, `cover`, `suppress`, `sense`, health multiplier) are ours.

**The whole locomotion stack:** nav-grid A*, ORCA/RVO local avoidance, wall-clearance nudge,
stuck detection, per-state speeds, head look-at, foot IK. Deliberately kept — see §4.

## 3. Hybrids — PD *data* inside our logic

Easy to mistake for ports in either direction:

* **Engagement range**: `g_BotWeaponConfigs` bands are transcribed, and the far edge ×4
  (clamped 25–200 m) becomes `WeaponStats::range` (`arsenal.rs:342`).
* **Standoff**: `standoff_for(range)` is a **fraction of that range** clamped to 2.5–11 m
  (`enemy_weapons.rs:198`) — *our* rule over PD's number. PD's own rule is different (§4).
* **Spread**: PD's table, matched to GoldenEye weapons **by role** (shotgun / hose / precision).
* **Damage**: 1 PD unit = 25 HP, derived from shots-to-kill agreeing on both sides.

## 4. What PD *actually does* where we improvise

This is the part worth reading before deciding to emulate PD more closely, because PD's
combat movement is far simpler than ours — and simpler in ways that will be felt.

### 4a. Combat movement is a four-state distance mode

`botcmd_tick_dist_mode` (`botcmd.c:39`) is the whole of it. Distance to target against the
weapon's band from `g_BotDistConfigs` (the same table we already use for range):

| mode | condition | action |
|---|---|---|
| `BACKUP` | closer than the band's min | `chr_run_from_pos` — run directly away |
| `OK` | inside the band | **`chr_try_stop` — stand still and shoot** |
| `ADVANCE` | past the band's max | `chr_go_to_prop` — run at the target |
| `GOTO` | past the third limit | `chr_go_to_prop` — same, from further out |

Two details that matter: **`OK` requires line of sight** (`!insight` demotes it to `ADVANCE`,
so a bot never stands still behind a wall), and there is an explicit anti-oscillation hack —
if the target leaves sight during a backup, the bot advances and then holds `OK` for a random
20–140 ticks, which the source itself explains is to stop the backup/advance loop around a
corner. Mode changes are also rate-limited to once a second (`distmodettl60`).

### 4b. Bots never strafe, never dodge, never take cover

Measured, not assumed:

* `aibot->speedmultsideways` is **written to zero in every branch that writes it**
  (`bot.c:206, 1063, 1066, 1070, 1073`, `botmgr.c:160`) and never to anything else. Bot
  movement is forward-along-path or nothing.
* `chr_try_sidestep` exists (`chraction.c:6804`) but its **only caller is
  `chraicommands.c:593`** — an AI-list command used by hand-authored *single-player guard*
  scripts. No bot code path reaches it.
* There is no cover selection in the bot code at all. `botroom_find_pos` reads `cover` pads,
  and its only caller is the **King of the Hill** scenario.

So a PD simulant's answer to "the player is aiming at me" is: nothing. It keeps shooting.
What reads as movement in a PD firefight is the *decoupling* — a bot walks its distance-band
path while its body faces its aim target, so the locomotion blend plays sidestep/backpedal
animations (`bot_apply_movement`, `bot.c:763`). It looks like strafing; it is a walk cycle
seen from the side.

### 4c. Bots never search, because they never lose you

There is no equivalent of `Search` or `Investigate`. Target selection
(`bot_choose_general_target`) considers every living opponent regardless of visibility —
`bot_is_target_invisible` is a **cloak** check, not a line-of-sight one. Out of sight simply
means `ADVANCE` instead of `OK`. Our omniscience flag already ports this knowledge rule.

### 4d. The action ladder, in priority order

`bot_tick_unpaused` (`bot.c:2445`) each frame: reload check → weapon switch → cloak/RCP120
special cases → scenario command (CTF/KOTH/etc., all inapplicable to us) → **attack the
selected target** → follow a teammate within 300 units → go pick up a weapon/ammo. Plus:
schedule a reload when out of ammo, **or** below half a clip and the target has not been seen
for 2 seconds (`bot.c:2470`).

## 5. If we want to run "as close to PD as possible"

The honest shape of it, in the order that makes each step judgeable on its own:

1. **A mode switch, not a rewrite.** `AI=pd|ours` (default `ours`), exactly like `ARSENAL=`
   and `BODIES=`. Everything below hangs off it, and the existing AI stays the default until
   the comparison says otherwise.
2. **Distance-mode movement** replacing the `Chase`/`Attack`/`Cooldown` movement: implement
   `botcmd_tick_dist_mode` verbatim over our nav (BACKUP / OK / ADVANCE / GOTO), including
   the LOS demotion, the anti-oscillation override and the 1 s rate limit. Use PD's own
   `g_BotDistConfigs` band rather than our `standoff_for` fraction.
3. **Switch off what PD does not have**, in PD mode only: aim-dodge, flank, cover/peek,
   burst-and-reposition, suppress. This is a flag on `AiTuning`, not a deletion — the doc-level
   promise is that both AIs remain runnable.
4. **The PD action ladder** replacing our utility selector in PD mode: attack → follow a
   teammate → (we have no pickups yet, so stop there). With omniscience on, `Search` and
   `Investigate` become unreachable, which is faithful.
5. **PD's reload rule** (out of ammo, or <½ clip and target unseen 2 s) in place of ours.
6. Keep, unchanged, in both modes: nav/A*, ORCA, wall clearance, foot IK, head look-at,
   animation. PD's own movement layer is built on hand-authored pads and waypoint lists our
   levels do not have; the parts of it that matter are the *decisions* above, not the pathing.

### The design tension worth deciding first

**A PD-faithful hunter cannot be hidden from.** It always knows where you are, never searches,
and walks the shortest path to your live position — which is a deathmatch AI, and this game is
hide-and-seek. Omniscience is already a flag (`Enemy::set_omniscient`), so the real question
is which of these the PD mode is *for*:

* **a faithful PD deathmatch** — omniscient, no search, distance-band combat; or
* **PD's fighting, our hunting** — keep perception/search/investigate as the knowledge layer,
  and take PD's model only from the moment contact is made.

The second is closer to what the game is, and is also strictly less work — it is items 2, 3
and 5 without item 4. Worth choosing before any code is written, because item 4 is the one
that changes what the game *is*.

---

## 6. What shipped (2026-08-18) — `AI=pd|ours`

**Option A: the faithful PD deathmatch**, chosen deliberately over the "PD's fighting, our
hunting" hybrid. All five items above, item 4 included, so a PD-mode hunter is a Perfect
Dark simulant end to end and `Search`/`Investigate` are genuinely unreachable. Nothing was
removed: `AI=ours` is the default and every behaviour below still runs under it.

```text
AI=ours   (default) everything in §2 — perception, search, the utility scorer, standoff
                    combat, aim-dodge, flank, cover/peek, burst-and-reposition, suppress
AI=pd               omniscient, no search, four-mode distance-band combat, none of the
                    above evasive/tactical movement, PD's reload rule
```

Resolved from the environment in `World::new` and **re-applied last at boot** (`app.rs`,
after the `PD_LAB` and `BODIES` blocks) so an explicit `AI=` cannot lose to a mode default,
and logged unconditionally. `an_explicit_ai_mode_outranks_the_lab` pins it.

| item | where it landed |
|---|---|
| the switch | `enemy::AiMode`, `AiTuning::mode`, `World::set_ai_mode` |
| distance-mode movement | `pdsim/distmode.rs` (pure + unit-tested), executed by `Enemy::pd_step` |
| the band per weapon | `EnemyWeaponDef::dist_cfg` + `enemy_weapons::dist_band_for`, off `g_BotDistConfigs` |
| switching off what PD lacks | `World::ai_tuning` zeroes `dodge`/`flank`/`cover`/`suppress` |
| the action ladder | `Enemy::pd_step`'s header comment maps all seven rungs |
| the reload rule | `World::enemy_reload_step` + `EnemyInstance::{loaded, reload_timer}` |

Unchanged in both modes, as promised: nav/A*, ORCA, wall clearance, foot IK, head look-at,
animation, and the whole `pdsim` aim/fire model (which was already PD's).

### What the AI lab measured

`world/ai_testbed.rs`, one hunter against a stationary player in a 15 m room with a pillar,
`BotDifficulty::Normal`, 20 s:

| | engaged | still | in band | `OK` mode | band err | lateral | time to kill |
|---|---|---|---|---|---|---|---|
| `AI=ours` | 17.4 s | 94% | 79% | — | 0.61 m | **4.8 m** | 10.8 s |
| `AI=pd` | 19.1 s | 100% | 100% | 100% | 0.64 m | **0.0 m** | 10.6 s |

Three results worth keeping, each of which contradicted the expectation the tests were
first written against:

* **Standing still is not the discriminator.** Our hunter is *already* 94% stationary
  against a stationary target — it reaches its standoff and its jukes are short. What
  separates the models is **lateral travel**: ours weaves ~5 m around the bearing, PD's
  moves sideways exactly 0.0 m. The assertion had to be rewritten around the orbit metric.
* **The two rules land at nearly the same distance.** `standoff_for` (0.61 m off the band
  centre) and `g_BotDistConfigs` (0.64 m) agree on the default roster, so "PD holds a
  better distance" is not a property that holds. They diverge per weapon — see the sniper
  row below — which is a playtest observation, not a unit test.
* **PD's model is not more lethal.** 10.6 s vs 10.8 s to a kill. The difference is entirely
  in how the fight *reads*, which is the thing CPU-side green cannot judge.

### The one deliberate oddity

**A PD-mode sniper charges you.** `botinv_get_dist_config` gives PD's own Sniper Rifle
`BOTDISTCFG_DEFAULT` — 3–6 m — and scores it 28 out of a possible 188, because bot combat
in Perfect Dark is a rush and not a duel at range. Our `standoff_for` hangs a sniper back
at ~11 m. The GoldenEye guns are mapped onto PD's bands by role, taking whatever PD's
nearest counterpart asks for rather than what looks sensible, so the sniper is ported as
PD has it. `AI=ours` keeps the standoff. This is the clearest single case of the two models
disagreeing, and it is left as PD wrote it.

### Unreachable on purpose

Rung 6 of the action ladder — "follow a teammate within 300 units" — is reached in PD only
when `bot_choose_general_target` returns nothing. With omniscience there is always a target
(the player, if a chosen packmate dies), so the rung is unreachable here and is left
unwritten rather than written and dead. `Alert` goes the same way: PD has no reaction-delay
*state*, it has `shootdelaytimer60`, which `pdsim/difficulty.rs` already ports.
