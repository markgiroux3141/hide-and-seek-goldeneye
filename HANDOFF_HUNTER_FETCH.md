# Handoff — hunters must actively go and find a gun

> **RESOLVED 2026-08-21.** Built as recommended: `AiState::Fetch`, a dominant score, its
> own `set_fetch_target` channel, one executor shared by all three decision layers. The
> `AI=pd` open question was answered by *porting* rung 7 (`bot_pick_up_weapon`) rather
> than arming PD hunters at spawn. Geometric regressions live in the AI lab
> (`an_unarmed_hunter_walks_to_the_gun_not_to_the_player` + the ammo, `AI=pd` and
> armed-control siblings) and both of them fail on the pre-fix build. The design record
> is folded into `DESIGN_PICKUPS.md`; the diagnosis below is kept as written.
>
> Two corrections to what is below: the give-up guard was *not* in the plan and turned out
> to be needed (nav-grid arrival can park a hunter short of a pickup it can never collect —
> without it, a statue), and `an_unarmed_hunter_fetches_a_weapon_and_arms_itself` did not
> need hardening in place: the hardened version is a new AI-lab arena, because the old one
> could not fail — with a single quiet hunter and nobody firing, `last_known` stays `None`,
> `Search` still scores, and even the broken build wandered to the gun. **A noisy player is
> what makes the bug reproduce**, and that is why the lab test pings `hear_noise`.
>
> The four "open design questions" at the bottom are all still open.

> Written 2026-08-21 at the end of the pickups session (`DESIGN_PICKUPS.md`). Pickups
> themselves are **built, shipped and playtest-confirmed**; this is the one part that
> does not work. Everything below is diagnosis, not speculation — the mechanism is
> identified with file:line evidence and the fix direction is a design decision, not a
> hunt.

## The symptom

Playtest, verbatim:

> "they still b line right for me with no gun in their hand. If I happen to be near a
> gun and they walk over it by accident they'll pick it up but they don't purposely go
> look for one."

So: an empty-handed hunter walks **at the player**, not at the gun. Collection itself
works — walk one over a pickup and it arms itself correctly.

## Root cause (confirmed)

The fetch behaviour was routed through the **search-target channel**
(`Enemy::assign_search_target`), on the reasoning that a hunter which cannot shoot
should drop into the blind states and walk where it is told. That seam does not work,
for a reason specific to the utility decision layer that is **on by default**:

`native/crates/game/src/enemy.rs:1700` — the utility scorer:

```rust
AiState::Search => {
    if !engaged && !perceived && self.last_known.is_none() { 0.6 } else { 0.0 }
}
AiState::Investigate => {
    if !engaged && !perceived && self.last_known.is_some() { 0.7 } else { 0.0 }
}
```

`Search` is scoreable **only while `last_known` is `None`**. `last_known` is set the
first time the hunter perceives the player (`enemy.rs:1261`) and is cleared in just two
narrow places (`:1363`, `:1942` — the Investigate give-up paths). So once a hunter has
seen the player even once:

1. `Search` scores `0.0` — permanently, until Investigate happens to clear the belief.
2. The scorer picks **`Investigate`** instead.
3. `Investigate`'s executor (`enemy.rs:1934`) walks to
   `known_target_pos(target_pos)` — the player's last known position.
4. **That is the beeline.**
5. The fetch target is only ever read by `Search` (`enemy.rs:1925`, and the legacy
   FSM's copy at `:1335`). It is unreachable, so it steers nothing.

The suppression itself works — `set_detectable(false)` + `set_omniscient(false)` do make
`perceived` and `engaged` false. It is the *destination* that never arrives.

### Why the tests passed anyway (read this before writing new ones)

`Enemy::is_engaged()` (`enemy.rs:1030`) covers `Alert | Chase | Attack | Cooldown |
TakeCover | Peek` — **not `Investigate`**. So
`an_unarmed_hunter_ignores_the_player_and_goes_shopping` asserted `!is_engaged()` over
5 seconds and passed while the hunter was in `Investigate`, walking straight at the
player. The test measured the wrong thing.

**Any new test must assert about position, not state**: that the hunter's distance to
the *gun* decreases and/or that it reaches it, in a level where the gun and the player
are in opposite directions. That is the only assertion the current bug cannot satisfy.
`an_unarmed_hunter_fetches_a_weapon_and_arms_itself` does test arrival — and passes —
but its arena puts one hunter, one gun and a stationary player in a 40 m room, which is
apparently forgiving enough to succeed by accident. Harden it: player between the hunter
and nothing, gun 180° the other way.

## The fix direction

Fetching is a **behaviour**, so it belongs in the behaviour scorer rather than smuggled
through a belief-gated one. Recommended shape:

1. A new `AiState::Fetch` (or `Collect`).
2. A score that **outranks everything** when the hunter cannot shoot — an unarmed hunter
   has no better option, so this is one of the few genuinely dominant scores in the
   table. Zero when it has a working weapon.
3. An executor that `move_toward`s the fetch point, with the same arrival/repath handling
   `Search` uses.
4. The fetch point delivered explicitly — a `set_fetch_target(Option<Vec3>)` on `Enemy`
   set from the world each step, *not* reusing `search_target` (whose owner is the
   fan-out coordinator; sharing it is what made the two fight).

Then all three decision layers need an answer, and only one of them is currently right:

| Layer | Live when | What to do |
| --- | --- | --- |
| Utility scorer (`util_choose`/`util_score`/`util_enter`, `enemy.rs:1690–1950`) | **default** | Add the `Fetch` behaviour. This is the one that matters. |
| Legacy FSM (`enemy.rs:1317+`) | `utility` kill-switch off | Add the same state, or accept it degrades. |
| PD ladder (`pd_step`) | `AI=pd` | **Design decision**: PD bots do collect weapons (`bot_pick_up_weapon`), but the ported ladder is "omniscient, no search, distance-band combat" and has no notion of going anywhere that isn't the target. Simplest honest answer: under `AI=pd`, hunters spawn armed (skip the empty-handed rule) until someone ports the real thing. |

## What is already built and working — do not rebuild it

All in `native/crates/game/src/world/tools/pickup.rs` unless noted.

- `hunter_want(inst) -> Option<HunterWant>` — Weapon (holding nothing) / Ammo (has a gun,
  no rounds anywhere). Correct.
- `best_pickup_for(want, holding, from)` — nearest useful un-taken pickup. A weapon
  pickup satisfies both wants; an ammo crate only satisfies the gun it feeds. Correct.
- `hunter_fetch_target(inst)` — the two above composed. Returns the right point; nothing
  consumes it usefully.
- `hunter_pickup_step` / `grant_hunter_pickup` — collection, re-equip, spare mags, the
  two-hunters-one-gun race, the audible cue. Playtest-confirmed working.
- `EnemyInstance.reserve` + `enemy_reload_step` — hunter ammo is finite in **both** AI
  models now (it was infinite, and only counted under `AI=pd`). Reloads draw from it.
- The firing gates: `start_enemy_fire` (`world/combat.rs`) and the shot pump both refuse
  an unarmed hunter. Gating the pump alone was a bug — the burst still started and the
  log still said `hunter firing (Unarmed, primary)` while dealing no damage.
- `hunters_start_unarmed()` = the flag AND `has_weapon_pickups()`. A level with no guns
  on the floor arms everybody, player included (`grant_fallback_sidearm`). This is what
  keeps every pre-pickups level and every AI-lab arena working; **it fixed 19 failing
  tests in one change**, so do not weaken it.
- The suppression call site: `world/lifecycle.rs:~295`, in the per-hunter loop. Both
  `set_detectable` and `set_omniscient` must be suppressed — omniscience is *knowledge*,
  not perception, so it bypasses visibility entirely and a hunter with it on fights
  bare-handed.

## Reproduce it headlessly, not by eye

There is an AI lab for exactly this (`world/ai_testbed.rs`, `TestArena` + `JankMonitor`,
see the `ai-testbed` memory). Build an arena with the player and a lone gun on **opposite
sides** of an unarmed hunter and measure distance-to-gun over time. A `cargo test` that
fails is worth more here than another playtest, because the symptom ("walks at me") and
the correct behaviour ("walks at the gun") differ only in *direction*, which is exactly
what a headless assertion can see and a state check cannot.

## Env flags that matter while working on this

- `OWN_ALL=1` — player owns every gun. **Turn this off**: it also let the player hoover
  guns off the floor (fixed — a duplicate gun now only converts to ammo if the reserve
  is genuinely low — but it still hides the empty-handed start).
- `ARMED_HUNTERS=1` — hunters spawn with their roster weapon (the kill-switch).
- `AI=pd|ours` — which decision model. `ours` is the default and the one to fix.
- The startup log prints a pickup census: `pickups: N placed (N weapon, N ammo) —
  hunters start EMPTY-HANDED; OWN_ALL=1 …`. Check it first, every time.

## Open design questions for the fresh context

1. **Does a shopping hunter avoid the player, or merely ignore it?** Currently it ignores
   the player completely — it will walk right past you to reach a gun. Fleeing would be
   more believable and is more work (it needs a threat-aware path, not just a
   destination).
2. **What does an unarmed hunter do when there is no gun available at all?** Today: an
   ordinary fan-out search until one respawns. Reasonable, but untested as *behaviour*
   (only as "does not engage").
3. **Should a hunter prefer a better gun, not just any gun?** `best_pickup_for` is
   nearest-first with no notion of weapon quality. PD scores weapons
   (`g_BotWeaponConfigs`); the data is already transcribed in `combat/pd_weapons.rs`.
4. **Should a killed hunter drop its gun as a live pickup?** The obvious next step for
   the floor economy, and it would make the fetch behaviour matter far more.
