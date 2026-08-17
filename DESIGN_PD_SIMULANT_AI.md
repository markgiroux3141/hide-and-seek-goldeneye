# How Perfect Dark Simulant AI Works

Reference notes taken from the Perfect Dark decompilation (`reference/pd-decomp`, gitignored —
see `reference/README.md` to re-clone). Written as a porting guide for our Rust hunter AI.

All line references are `pd-decomp/src/game/`.

---

## 1. Two separate AI systems

Perfect Dark has **two unrelated AI layers**, and it matters not to confuse them:

**AI lists** (`chrai.c`, `chraicommands.c`, `gailists.c`, `docs/ailists.md`) — a bytecode VM
driving *scripted* characters: campaign guards, hostages, cutscene actors. Each command is a
2-byte opcode plus variable-length args. It has labels, goto-next / goto-first, if-statements
and try-statements (pathfinding that branches on failure). Roughly 50 "global" lists handle
generic combat, patrolling and idling; the rest are per-stage.

**Bots / simulants** (`bot.c`, `botinv.c`, `botact.c`, `botcmd.c`, `botmgr.c`, `botroom.c`) —
plain C, no bytecode. This is the multiplayer simulant AI, and it is the one we care about.

The AI-list layer is still worth one idea: **cooperative yielding**. A list runs until it hits
an explicit `yield`, then hands control back and resumes next frame from the same instruction.
The engine never preempts mid-list, so game state can't shift between two commands in a way the
script didn't expect. Any loop must contain a yield or the game soft-locks. That's a cleaner
contract than a fixed CPU budget per character, and it's a good model if we ever give hunters
scripted behaviours.

The rest of this document is about the bot layer.

## 2. The core structural idea: two orthogonal axes

A simulant's config is just **difficulty × type**. They are completely independent, and they
control different things:

- **Difficulty** scales *lethality* — reaction time, aim convergence rate, movement speed.
- **Type (personality)** changes *target selection and goals* — never accuracy.

This separation is the single most portable idea here. Our difficulty dial already does the
first job. Personality is the missing axis.

### Difficulty tiers

`constants.h:347` — seven values: `MEAT`, `EASY`, `NORMAL`, `HARD`, `PERFECT`, `DARK`,
`DISABLED`. Six are real; `DISABLED` is an off switch.

### Personality types

`constants.h:374` — 13 types, each a single behavioural rule:

| Type | Rule |
|---|---|
| `GENERAL` | No modifier — the baseline |
| `PEACE` | Collects weapons but will not engage |
| `SHIELD` | Wants full shield before fighting |
| `ROCKET` | Prefers explosive weapons |
| `KAZE` | Does not keep distance |
| `FIST` | Fists only |
| `PREY` | Targets the newly spawned, poorly armed, or low health |
| `COWARD` | Flees unless it out-guns you |
| `JUDGE` | Targets whoever is winning |
| `FEUD` | Fixes on one player for the whole match |
| `SPEED` | Moves faster |
| `TURTLE` | Moves slower, double shield |
| `VENGE` | Targets whoever last killed it |

## 3. Aim: the "zeroing" model

This is the most valuable thing in the codebase and the part I'd port first.

**Simulants do not snap to a target and they do not roll a hit chance.** They aim a real weapon
in world space, and accuracy is an *emergent* property of how fast their aim converges. The
convergence process is called *zeroing*.

The tuning table is `botdifficulty` at `bot.c:45`, with the authors' own field notes at
`bot.c:56-85`. Fields:

| Field | Meaning |
|---|---|
| `shootdelay60` | Delay between *seeing* the target and shooting. Has a cooldown, so a brief line-of-sight break barely helps you |
| `zerotime60` | How long a full zero onto the target takes |
| `minzerospeed` / `maxzerospeed` | Angular convergence rate. Scaled linearly by the current zero timer, then a **random** value between the scaled min and max is picked |
| `turnunzeromult` | While zeroing, natural turning *un-zeroes* the aim. This is a multiplier on the zero cooldown |
| `zerocloakspeed` | Floor on `maxzerospeed` when the target is cloaked — lets a bot start zeroing quickly |
| `forcezerominspeed` | General floor on `maxzerospeed` for the start of a zero |
| `dizzyamount` | Tranquilliser threshold before the bot degrades to firing at anything in sight, zeroed or not |

Values (`bot.c:87`, times in 60ths of a second, angles via `BADDTOR2`):

| Tier | shootdelay | zerotime | minzero | maxzero | unzero× | dizzy |
|---|---|---|---|---|---|---|
| meat | 90 (1.5 s) | 600 (10 s) | 15 | 30 | 10 | 1000 |
| easy | 60 (1.0 s) | 360 (6 s) | 7 | 14 | 10 | 1000 |
| normal | 30 (0.5 s) | 180 (3 s) | 4 | 8 | 4 | 1500 |
| hard | 15 (0.25 s) | 90 (1.5 s) | 1.5 | 4 | 2 | 2500 |
| perfect | 0 | 45 (0.75 s) | 0 | 2 | 1 | 4000 |
| dark | 0 | 0 | 0 | 0 | 0 | 4000 |

Read the shape of it: from meat to dark, reaction goes 1.5 s → instant and zero time goes
10 s → instant, while `turnunzeromult` goes 10 → 0, meaning weak bots are massively punished
for turning while aiming and Dark sims are not punished at all.

### How the numbers actually become an aim error

Reading `bot_update_zero_angle` (`bot.c:1440`) closely, the mechanism is more interesting than
"convergence rate", and three details matter for a faithful port:

1. **`zeroangle` is an error, not a target.** It is *added to* the true bearing
   (`targetangle = oldangle + angle_to_target + zeroangle`, `bot.c:962`), and the body then
   turns toward that wrong bearing at a capped rate. The bot is never told where its mistake
   is.
2. **The min/max speeds are the magnitude of a randomly-signed increment into a damped
   accumulator**, not an angular rate. Each tick picks a magnitude between the scaled min and
   max, flips its sign on a coin toss, and feeds it into `zerospeed = zerospeed*0.975 + inc`
   at 240 Hz; `zeroangle = zerospeed * 0.025`. The steady state of that filter is `inc/(1-0.975)
   = 40·inc`, so a settled `zeroangle ≈ inc` **in radians** — which is why the table's degree
   values read directly as "how far off this tier aims". Meat wanders ~30°, Normal ~8°,
   Perfect ~2°, Dark not at all. The **random sign** is the load-bearing part: without it the
   aim would creep on from one side and players would learn a safe strafe direction.
3. **The increment is held for 20–40 ticks** (`random3ttl60`), a third to two thirds of a
   second, rather than re-rolled per tick. That hold is what makes the aim drift steadily and
   then change its mind, instead of dithering into a smooth average.

Two further points that are easy to get wrong:

- **Bots fire long before they are zeroed.** The trigger gate is reaction-served plus the
  target within a *45°* cone of the barrel (`chr_is_target_in_fov(chr, 45, false)`,
  `bot.c:3606`) — not "aim has converged". This is why simulants spray wide on first contact
  and tighten up as the zero completes; gating on a finished zero would produce a much less
  interesting and much deadlier enemy.
- **Only yaw carries error.** `zeroangle` is horizontal; vertical aim is handled separately by
  `chr_calculate_aimend` pointing at the target's body. So a simulant's misses are always to
  the side, never high or low, which is why they read as "swinging past you" rather than
  "spraying wildly".

There is also no accuracy fudge hiding elsewhere: `chr->accuracyrating` is only ever set to 0
(`chr.c:1181`). The shot really is a world-space hitscan down a possibly-mis-aimed barrel.

**Why this is the right model for us.** It produces the behaviours players read as "human"
without any hit-roll dishonesty: bots lead poorly at low skill, get thrown off by targets that
force them to turn, are briefly helpless after whipping around a corner, and degrade gracefully
under tranq. Randomising the convergence rate per tick (rather than the shot outcome) means two
bots at the same difficulty still feel individual.

It also composes with the head look-at and aim-offset layers we already have — zeroing is
exactly a target for the cone-clamped aim offset to chase, rather than a replacement for it.

## 4. Target selection

`bot_choose_general_target` (`bot.c:1589`). Notably it does **not** consider weapons, ammo, or
personality — the doc comment says so explicitly. Personality is applied as filters elsewhere.

Per tick it does a small amount of work, amortised:

1. **One** other character is queried per tick (round-robin via `queryplayernum`) for distance
   and line-of-sight. With N players each is refreshed every N ticks. Cheap by construction.
2. Last-seen timestamps update for everyone currently visible.
3. All characters are sorted into `chrnumsbydistanceasc` — an insertion sort, tiny N.

Then selection, in priority order:

- Uplinking/objective-locked → clear target and return.
- Already attacking, target still visible and alive → **keep it**. Explicit target stickiness.
- Otherwise validate the current target, dropping it if: dead, invisible and not currently in
  sight, now a teammate, fails the peace check, or (when out of sight) fails the coward check.
- No target → walk candidates in **ascending distance** order. First one in sight wins.
  **Meat and Easy sims take the closest enemy even if not visible**, while everyone else
  prefers a visible target and falls back to the nearest known-but-unseen one. That single
  branch is most of why low-tier sims feel oblivious.
- Existing target that went out of sight → switch to any visible enemy by distance, else keep
  the old target.

The personality filters are separate predicates:

- `bot_passes_peace_check` (`bot.c:1537`) — a PeaceSim refuses to target anyone who is
  unarmed. It only fights the armed.
- `bot_passes_coward_check` (`bot.c:1557`) — a CowardSim scores its own weapon and the
  candidate's through `botinv_score_weapon`, and declines the fight unless it leads by a
  margin of 30 points.

That is the pattern worth copying: **selection is one shared algorithm; personality is a set of
veto predicates and score adjustments layered on top.** It maps almost directly onto a utility
scorer — each personality becomes a weight set plus optional hard vetoes.

## 5. Movement speed

`bot_calculate_max_speed` (`bot.c:1096`). Base speed derives from the character model's
*height* — taller bodies move faster — then:

- `TURTLE` → ×3.5 and `SPEED` → ×14.0, **bypassing difficulty entirely**
- otherwise by difficulty: meat 5.0, easy 6.2, normal 7.6, hard 9.4, perfect/dark 11.2
- then posture: squat ×0.35, duck ×0.5, and ×0.5 when arriving within 200 units of a
  destination (a deceleration ramp)
- carrying a case/briefcase replaces the height term with a fixed penalty

Note that speed personalities *override* the difficulty scale rather than multiplying it — a
SpeedSim moves the same whether it's Meat or Dark. Clean separation of concerns again.

## 6. Weapon choice

`botinv.c` (42 KB). The central routine is `botinv_score_weapon`, which returns **two** scores
for a weapon in a given firing function, and is reused for opponent weapons — which is how the
coward comparison works. `bot_find_pickup` (`bot.c:1865`, ~430 lines) drives pickup hunting
against a criteria parameter, with `bot_can_do_critical_pickup` distinguishing urgent needs
(no weapon, no ammo) from opportunistic ones.

## 7. Porting plan for our engine

**Status: items 1–6 are built** (`native/crates/game/src/pdsim/`, wired in behind `PD_LAB=1`
via `world/pd_lab.rs`). What follows is the original plan, kept because the ordering argument
still holds; see §9 for what the built version actually does and does not cover.

Ordered by value:

1. **Zeroing aim.** Replace whatever aim convergence we have with a `zerotimer` per hunter plus
   a difficulty-indexed tuning table mirroring `botdifficulty`. Keep the randomised
   min/max convergence rate — it's what stops same-tier hunters feeling identical. Feed the
   result into the existing cone-clamped aim offset rather than writing to the transform.
   This subsumes the "difficulty tiers" item already on the AI modernization roadmap.
2. **Separate the two axes.** Our difficulty dial (0–10) maps onto the lethality table by
   interpolation — PD's six tiers are coarse and we can lerp between rows. Then add personality
   as an independent per-hunter field.
3. **Personality as veto predicates + utility weights.** The peace/coward pattern drops
   straight into our scorer. Good first three for hide-and-seek: a *prey* hunter that pushes
   weak targets, a *coward* that disengages when out-gunned, and a *feud* that fixates on one
   player — fixation in particular is great for the hide-and-seek fantasy.
4. **Amortised perception.** The round-robin one-target-per-tick LOS query is a better fit for
   our packmate-LOS work than querying everyone every frame, and it degrades predictably as
   hunter count rises.
5. **Target stickiness.** Explicitly keep an in-sight target rather than re-deciding each tick.
   This is likely a partial fix for the search-stall and chase-thrash defects the AI testbed
   caught, which are re-decision problems.
6. **Difficulty-gated obliviousness.** The meat/easy "chase the nearest even if unseen" branch
   is a cheap, legible way to make low difficulty feel bad at hunting rather than just
   inaccurate.

### Deliberately not porting

- The AI-list bytecode VM. Our utility layer already covers behaviour selection, and a second
  scripting system would be a parallel authority over the same decisions.
- Shield mechanics (`SHIELD`, `TURTLE` double shield) — we have no shield system.
- Tranq/dizzy degradation — no tranquilliser weapon.

## 8. Source map

| Path | Size | Contents |
|---|---|---|
| `src/game/bot.c` | 113 KB | Difficulty table, targeting, speed, pickups, main tick |
| `src/game/botinv.c` | 42 KB | Weapon scoring and inventory reasoning |
| `src/game/botact.c` | 14 KB | Action execution |
| `src/game/botcmd.c` | 7 KB | Command layer |
| `src/game/botmgr.c` | 7 KB | Spawn / lifecycle |
| `src/game/botroom.c` | 4 KB | Room and position finding |
| `src/game/chr.c` | 175 KB | Shared character sim (guards + simulants) |
| `src/game/chraction.c` | 466 KB | Action state machine |
| `src/game/chrai.c`, `chraicommands.c` | 30 / 246 KB | AI-list VM |
| `src/game/gailists.c` | 174 KB | Global AI list data |
| `src/include/constants.h` | — | `BOTDIFF_*` at :347, `BOTTYPE_*` at :374 |
| `docs/ailists.md`, `docs/chrs.md` | — | Upstream docs |

Key functions: `bot_tick` :909 · `bot_calculate_max_speed` :1096 · `bot_passes_peace_check`
:1537 · `bot_passes_coward_check` :1557 · `bot_choose_general_target` :1589 ·
`bot_find_pickup` :1865 · `bot_tick_unpaused` :2445

## 9. What the port actually does

*(Written while this was `PD_LAB=1`. It is the shipped hunter AI now — see §17 for what
the promotion moved, and read the `PD_LAB` references below as "every hunter".)*

`native/crates/game/src/pdsim/` — the model, which knows nothing about this game and is unit
tested against the PD constants (29 tests):

| Module | Contents |
|---|---|
| `difficulty.rs` | `g_BotDifficulties` in SI units, plus interpolation onto our 0–10 dial |
| `zeroing.rs` | The aim model — closed-form port of PD's 240 Hz accumulator, so it is frame-rate independent |
| `personality.rs` | The 13 `BOTTYPE_*` types as veto predicates + preference scores |
| `targeting.rs` | `bot_choose_general_target`: amortised round-robin perception, target stickiness, the difficulty-gated oblivious branch |

`world/pd_lab.rs` — the seam into the game. What changes when a hunter is a simulant:

| | Our hunter | PD simulant |
|---|---|---|
| Aim | body faces the AI heading, eased at a fixed rate | body yaw is the model's yaw, carrying its live aim error |
| Shot | `rand() < accuracy * (1 - dist/range)` | real hitscan down the barrel, no roll |
| Fire gate | FSM entered `Attack` | reaction served + target within 45° of the barrel |
| Reaction | one `AiTuning::alert` constant | `shootdelaytimer` that decays rather than resets |
| Speed | difficulty multiplier | difficulty tier, or a personality override |
| Damage ceiling | `MAX_HIT_RATE` throttle on landed shots | none — lethality is governed by aim alone |

### Animation is no longer left out

The original note below said animation stayed on our stack. Three pieces of Perfect
Dark's animation *logic* have since been ported, on top of PD's own clips:

* **`attackanimconfig`** (`game/src/combat/attack_anim.rs`) — the authored per-animation
  aim / shoot / recoil windows and aim limits, replacing our hand-set `FIRE_TIMING`
  guesses. See `DESIGN_PD_WEAPON_MECHANICS.md` §10.5.
* **The 32-slot direction tables** (same file) — *which* attack animation a burst plays,
  chosen from the bearing to the target rather than from the weapon alone. See §13.
* **Per-hit-part reaction tables** (`game/src/combat/hit_anim.rs`) — `g_DeathAnimsHuman*`
  and `g_InjuryAnimsHuman*` keyed by which body part the shot hit, with each row's own
  playback `speed` and `endframe`. That last field is the interesting one: a PD flinch
  is usually *the first 20 frames of a death animation*, not a purpose-made clip, which
  our mixer could not express until `AnimPlayer::play_once_scaled` landed.

Both apply to hunters wearing PD bodies only, so they A/B against GoldenEye hunters
playing the very same animations. The one piece still missing is the rows' `flip`
flag — PD mirrors a single animation to cover both sides, and we have no pose
mirroring, so `flip == true` rows are dropped and a table emptied by that falls back
to its mirror partner. GoldenEye shipped most of those mirrors as separate clips, so
each side still keeps a correctly-sided reaction.

### Deliberately left on our existing stack

**Movement, pathing, local avoidance, cover and animation.** PD's movement layer is built on
hand-authored pads and waypoint lists our levels do not have, and swapping it in would regress
the ORCA / nav / foot-IK work for no gain. The thing that reads as "a PD simulant" in a
firefight is the aim and the engagement timing. So the existing FSM still decides *where the
hunter goes*; the simulant decides *where it looks and when it shoots*.

*(Amended 2026-08-17 — §10 ports one thing out of that list: the **knowledge** rule that
picks the destination. The navigation to it is still ours.)*

**The `MAX_HIT_RATE` cap is dropped on the PD path**, and this is a real behavioural change.
That cap exists because our hunters roll for hits, so a fast weapon would otherwise delete the
player in a burst — an artificial lethality limiter on top of an artificial accuracy model. The
zeroing model replaces both, and keeping the cap would clip the top of exactly the range the
difficulty table expresses (Dark and Hard would both saturate it and become indistinguishable).
The consequence is intended: a well-zeroed high tier with an automatic weapon kills fast, which
is what a DarkSim does. If the lab needs a ceiling for playability it belongs on the *weapon*,
not on a global damage throttle.

### Measured behaviour

From the headless scenarios in `world/ai_testbed.rs` (`cargo test -p game pd_ -- --nocapture`),
one simulant against a stationary player at 10 m in an empty box:

| Metric | MeatSim | DarkSim | PD table says |
|---|---|---|---|
| Worst aim error | 29.5° | 0.00° | 30° / 0° |
| First shot | 1.50 s | 0.60 s | 1.5 s reaction / 0 s |
| Time to kill | 6.5 s | 3.1 s | — |

The DarkSim's 0.60 s is pure turn time — it owes no reaction at all. The 2.1× kill-time
separation is produced entirely by where the barrel is pointing.

### Known gaps

- ~~**Target selection is exercised but not stressed.**~~ **Closed.** Every live hunter is
  now a candidate alongside the player (`pd_lab::PdActor` / `PdTarget`), and a simulant's
  round damages whichever body is nearest along its barrel — so friendly fire is emergent,
  not special-cased, and it runs through the same `hit_enemy` a player shot does. Measured
  in a 6-DarkSim lab run: 62 hunter-on-hunter hits and 2 deaths inside ~20 s.

  *(Amended — that measurement was taken with **no team check**, which §16.1 has since
  ported. Hunters no longer pick each other as targets unless `PD_LAB_FFA=1`; a stray round
  crossing a packmate still hits it, which is the PD-faithful half of the above.)*

  ~~**What is still missing is movement, not damage.**~~ **Closed** — see §14. `Enemy::update`
  takes an `EngageTarget` now, so the whole chase / standoff / cover / search chain runs
  against a packmate the way it runs against the player.
- `SHIELD` / `TURTLE`'s double shield and the tranq `dizzyamount` degradation are still
  unported, for the same reason as before: no shield system, no tranquilliser.
- Cloak handling is present in the model (`zerocloakspeed`) but nothing drives it.

## 10. Omniscience — how a simulant knows where you are (2026-08-17)

Perfect Dark's simulants come and find you. Ours did not: they ran a cone-gated perception
FSM with a last-known position, so breaking line of sight put them into a fan-out sweep of
the level. This section is what PD actually does — read out of the decomp, not guessed — and
what we ported.

### What PD actually does

Two functions, and neither of them contains a last-known position.

**`bot_choose_general_target` (`bot.c:1589`) — the knowledge.** Every tick the bot advances a
round-robin pointer by one opponent and refreshes *that one's* sight test. But it refreshes
`aibot->chrdistances[i]` for that opponent unconditionally, and it keeps a full
`chrnumsbydistanceasc` ordering of everyone. Sight is amortised; *position* is not a belief at
all — it is read straight off `trychr->prop->pos`. Target choice then walks the distance-sorted
list:

1. the closest opponent **in sight** wins;
2. Meat/Easy skip that and just take the closest, seen or not (this is our `takes_unseen_closest`);
3. and if nobody at all is in sight, every tier falls through to
   *"Use closest out of sight chr"* — `closestavailablechrnum`.

So a simulant essentially always has a target. The only thing that removes one is
`bot_is_target_invisible` — cloak, the `CHRCFLAG_HIDDEN` flag, or `!g_Vars.bondvisible`. Not
walls. **Omniscience is unconditional, not alert-gated.**

**`botcmd_tick_dist_mode` (`botcmd.c:39`) — the movement.** Distance to the target is bucketed
against a per-weapon `{min, max}` band (`g_BotDistConfigs`, in PD units ≈ cm):

| Config | min | max | metres |
|---|---|---|---|
| `CLOSE` (melee) | 0 | 120 | 0 – 1.2 |
| `PISTOL` | 300 | 450 | 3 – 4.5 |
| `DEFAULT` | 300 | 600 | 3 – 6 |
| `SHOOTEXPLOSIVE` | 600 | 1200 | 6 – 12 |
| `KAZE` | 150 | 250 | 1.5 – 2.5 |
| `FARSIGHT` | 1000 | 2000 | 10 – 20 |
| `FOLLOW` | 0 | 250 | 0 – 2.5 |
| `THROWEXPLOSIVE` | 450 | 700 | 4.5 – 7 |

→ `BACKUP` below min, `OK` inside the band, `ADVANCE`/`GOTO` above it. `OK` stands still
(`chr_try_stop`); everything else re-issues `chr_go_to_prop(chr, targetprop, GOPOSFLAG_RUN)`,
which paths to the target prop's **live coordinates**. Then the two rules that make it feel
relentless:

- `if (newmode == OK && !insight) newmode = ADVANCE;` — at perfect range but no sightline,
  it closes anyway. This single line is most of "they always find you."
- `BACKUP` with no sightline also becomes `ADVANCE`, and when sight returns the mode is pinned
  to `OK` for a random 20–140 ticks (0.33–2.3 s) so the bot doesn't oscillate advance/back-up
  around a corner.
- Re-pathing is throttled to `distmodettl60 = TICKS(60)` — once a second, or on a mode change.

Note the ladder is *distance* first and sight only as a modifier. A PD bot with a wall between
you will walk around the wall and into your face, because with no sight there is no `OK` band
to stop in. That is the intended feel.

### What we ported

`Enemy::known_player_pos` (`native/crates/game/src/enemy.rs`) — one accessor that every
movement decision reads, so omniscience is a *policy* rather than a rewrite:

```rust
fn known_player_pos(&self, player_feet: Vec3) -> Option<Vec3> {
    if self.omniscient { Some(player_feet) } else { self.last_known }
}
```

Consumers routed through it: `Chase`'s approach target and `Investigate`'s walk-to spot, in
both the utility layer and the legacy FSM. Three supporting changes fall out of it:

- **The blind states become unreachable.** In the utility layer that needs no special case —
  `Search` and `Investigate` both score only when `!engaged`, and an omniscient hunter is
  permanently engaged. The FSM gets an explicit `acquired = perceived || omniscient` gate.
  `pick_search_point` is simply never called for these hunters.
- **`lose_contact()`** replaces the four scattered `→ Investigate` transitions. An omniscient
  hunter drops to `Chase` instead, which is exactly PD's `OK && !insight → ADVANCE`.
- **The belief never lapses.** `alert_served` is normally re-armed once `since_seen` passes
  `ENGAGE_MEMORY`; an omniscient hunter is exempt, or it would re-serve its reaction delay
  every tick it couldn't see you and stand in `Alert` forever.

**Knowledge is kept strictly separate from perception.** `perceived`, `perception_los`,
`since_seen` and `last_known` are untouched, so the aim model — deliberately fed a raw raycast
of its own, see `world/pd_lab.rs` — and the LOS-gated `Attack` state keep the contract they
were tuned against. An omniscient hunter with a wall in the way knows exactly where to walk and
still cannot shoot through it.

Scope: PD-lab hunters only, gated per hunter on `EnemyInstance::pdsim.is_some()` so it travels
with the model rather than with the mode. Kill-switch `World::set_pd_omniscience` (default ON).
Scenarios in `world/ai_testbed.rs`: `pd_omniscient_hunter_finds_a_player_it_cannot_see`,
`pd_omniscience_kill_switch_restores_the_search`, `a_goldeneye_hunter_is_never_omniscient`.

### What we did *not* need

Our standoff band already is PD's dist config: `combat::standoff_for(range)` derives a
per-weapon standoff and `STANDOFF_HYST` widens it into a `{standoff ± 1.2 m}` band, with
advance / hold / `back_off` arms that map one-for-one onto ADVANCE / OK / BACKUP. And because
`Attack` is LOS-gated while `Chase` is not, "at good range but blind → keep closing" already
falls out. The genuinely missing piece was only ever the knowledge rule.

The BACKUP-hysteresis timer has no analogue yet, but nothing has been seen oscillating —
`back_off` only runs inside `Attack`, which requires sight.

## 11. Roadmap: what else separates our AI from PD's

The question behind this section was "PD has waypoints, standoff points etc. that we don't."
Half of that turns out to be false — see the standoff note above — and the other half is real.
Ordered by how much behaviour each buys per unit of work.

**1. An arbitrary-target engagement (the free-for-all seam).** ~~Our whole FSM is written against
`player_feet`.~~ **Done — see §14.** `Enemy::update` takes an `EngageTarget { pos, id }` that the
`World` resolves from `pd_target`, so chase, standoff, cover sampling, flanking and perception
all measure against whoever the simulant picked. The catch check, the crosshair aim-sense and the
`detectable` toggle stayed player-specific on purpose; §14 says why for each.

**2. Authored cover points instead of runtime sampling.** PD's cover is level data
(`setupcover.c`), unpacked by `cover_unpack`, with per-point flags (`COVERFLAG_OMNIDIRECTIONAL`,
`COVERFLAG_AIMDIFFROOM`) and selected by a criteria bitmask in `chr_assign_cover_by_criteria`
(`chraction.c:15428`). Two properties ours lacks: (a) a **reservation lock** —
`cover_is_in_use` / `COVERFLAG_AIBOTINUSE` stop two bots claiming the same spot, where our
`sample_cover_cell` will happily hand the same nav cell to the whole pack; (b) a **facing**, so
a bot arrives oriented out of cover rather than having to re-acquire. The cheap version keeps
runtime sampling and adds just the reservation + arrival facing; the full version is a
BUILD-mode cover-point tool, which the object/light placement UI already has the shape for.

**3. Waypoints are not what the name suggests.** PD's `waypoint` graph (`setupwaypoints.c`,
`chr->act_gopos.waypoints[]`) is its *navigation mesh* — a hand-authored, sparse, room-linked
graph, because an N64 cannot afford a grid A\*. Our baked nav grid is a strictly better version
of the same thing for our levels; the navmesh experiment that regressed and was reverted is the
standing evidence. **This is the one item here not worth porting.** The genuinely missing
capability is not waypoints but **off-mesh links**: drops, vaults and jump-downs on the existing
grid, so a hunter can take a shortcut a player would. That is roadmap #6, and it belongs on the
grid, not on a graph.

**4. Room / portal awareness.** PD reasons in rooms constantly — `botroom_find_pos`,
`bg_room_get_neighbours`, cover filtered by "same room as target" or "neighbouring rooms only",
squad spread by room. We have `Region`s in BUILD, but the AI never sees them; it reasons purely
in metres. Exposing a coarse room id per nav cell would let the search fan out *by room* instead
of by farthest-point sampling, let cover prefer a different room from the target, and give
squad alert a much better spread rule than a radius.

**5. Posture.** Crouch/duck/stand (`bot_guess_crouch_pos`) with the speed penalties in §5, and
crouching behind low cover. We have neither the animation set nor the capsule work, so this is
the expensive one — but it is what makes PD's cover reads legible from across a room.

**6. The deceleration ramp.** One line of PD's speed function: ×0.5 within 200 units (2 m) of
the destination. Cheap, and it would soften the arrive-and-snap our hunters do at a search or
reposition point.

**7. Weapon-aware distance configs.** We derive one standoff from one range number. PD indexes
a table by *weapon and firing function* — a grenade launcher wants 6–12 m, a thrown grenade
4.5–7 m, a Farsight 10–20 m, a melee bot 0–1.2 m. Our explosives currently reuse the generic
band. Small, and it makes weapon choice read in the movement.

Not worth porting, unchanged from §7: the AI-list bytecode VM, shields, and tranq/dizzy.

## 12. How often does a bullet actually land? (2026-08-17)

Reported from playtest: *"when they have an automatic weapon, every single bullet hits me
in a row and I die almost instantly."* Both halves are real, and they have different causes.
This section is the full chain in Perfect Dark — four independent gates, only two of which we
had ported — and the measurements that show which one was actually doing the damage.

### PD's chain, in the order a round passes through it

**1. Does the bot pull the trigger at all?** `bot_tick` (`bot.c:3600`). Reaction served
(`shootdelaytimer60 >= shootdelay60`), target in sight, and the target within a **45°** cone of
the barrel. Note what is *not* required: that the aim has converged. Simulants open fire long
before they are zeroed, which is why they spray on first contact and tighten up. §3 covers this.

**2. Where is the barrel pointing?** The zeroing error (§3) — `zeroangle`, added to the true
bearing. It is a damped random walk whose increment is re-rolled only every 20–40 ticks, so it
is essentially **constant across a burst**. Settled magnitudes: Meat ~30°, Normal 4–8°,
Hard 1.5–4°, Perfect 0–2°, Dark exactly 0.

**3. Where does this individual round go?** `bgun_calculate_bot_shot_spread` (`bondgun.c:5142`),
applied per shot to the aim vector in **two axes**. A weapon's `spread` field reads as
±spread/4 degrees of worst case per axis (derivation in `pdsim::spread`), RMS ≈ spread/12:

| Weapon | `spread` | worst case / axis |
|---|---|---|
| DY357 Magnum, Sniper Rifle, Laser | 0 | 0° |
| Falcon 2 | 1 | 0.25° |
| Mauler, Magsec, K7 Avenger, RC-P120, Dragon, Laptop | 6 | 1.5° |
| AR34 | 8 | 2° |
| CMP150, Callisto NTG | 9 | 2.25° |
| Cyclone (magazine discharge) | 25 | 6.25° |
| Shotgun | 30 | 7.5° |
| Reaper | 56 | 14° |

Two details worth keeping. The offset is `(U − 0.5) · U` — a *product* of two uniforms, sharply
centre-weighted (RMS is a third of the peak, where a uniform would be over half), so most rounds
land near the middle and the occasional one flies wide. And the widest values sit on the
**automatics** while the marksman weapons are exactly zero: PD deliberately makes a sniper's
accuracy a pure statement about the shooter's tier.

**4. How many rounds arrive, and how fast?** PD's automatics are `FUNCFLAG_BURST3` rows.
`bot_tick` (`bot.c:3644`) counts against that flag: three rounds spaced `nextbullettimer60 = 5`
ticks apart, then a pause of `botact_get_shoot_interval60` = `unk24 + unk25` = **24 ticks (0.4 s)**
before the next burst — the same 6+18 for the AR34, K7 Avenger, CMP150, Callisto and Laptop Gun.
Damage is small per round (AR34 1.4, K7 1.5, CMP150 1.0) against a player pool of 8
(`bondhealth = (maxdamage − damage) × 0.125`, `player.c:879`), so ~6 rounds kill an unarmoured
player — two bursts.

### What we had, and what the measurements said

We had gates 1 and 2 and neither of the others: one purely horizontal ray per shot with no
per-round variation, and a flat, gapless 8 rounds/second. Measured in the lab, one simulant on a
stationary player at a 7 m standoff:

| | before | after |
|---|---|---|
| DarkSim, rounds landed / fired | 13 / 13 | 13 / 13 |
| NormalSim, rounds landed / fired | 13 / 24 (54%) | 13 / 20 (65%) |
| DarkSim time-to-kill at 10 m | 3.27 s | 5.78 s |
| MeatSim time-to-kill at 10 m | 14.0 s | 20.7 s |

**The surprise is in the first row, and it is worth stating plainly: per-shot spread did not fix
"every bullet in a row", because at a 7 m standoff PD's spread does not either.** A rifle's ±2°
worst case is a few centimetres against a 0.35 m torso at that distance. A DarkSim in Perfect
Dark also lands essentially every round at knife range — that is what a DarkSim *is*. Spread
matters at range, and for the wide-cone weapons where it is most of the weapon's identity, but
it is not the thing keeping the player alive.

The thing keeping the player alive is the **burst gap**, and it is worth roughly a factor of two
on sustained incoming fire. The lab now measures the rhythm directly and it matches PD's:

```
0.1, 0.1, 0.4, 0.1, 0.1, 0.4, 0.1, 0.1, …
```

Three rounds tight, then a pause. Inside a burst the rounds actually come *faster* than our old
flat cadence — the point was never to slow the gun down, it is that the gap gives the player a
window to break line of sight and makes incoming fire read as a rhythm rather than a wall.

So the ordering of responsibilities, which is the real finding: **the tier sets the hit fraction
(Normal 0.65 against Dark 1.00 at the same range with the same gun), and the cadence sets how
fast that fraction kills you.** The weapon's cone is a texture on top of both. That is asserted
now, in `pd_the_hit_fraction_is_set_by_tier_not_by_the_weapon`.

### What we ported

* `pdsim/spread.rs` — `bgun_calculate_bot_shot_spread` as a pure function, with the screen-space
  → radians derivation written out, PD's centre-weighted distribution, and the dual-wield ×1.5.
  PD's crouch ×0.5 and `INVAIMFLAG_ACCURATESINGLESHOT` ×0.25 are not ported: we have no crouch
  posture and no per-weapon aim flags.
* `EnemyWeaponDef::spread` / `::automatic` — the table matched by role (our arsenal is
  GoldenEye's, so a one-to-one gun mapping does not exist; the ordering is what is preserved).
* `World::emit_pd_shot` is now a **3D** shot: it elevates to the target's chest (PD's
  `chr_calculate_aimend` drops the aim by 0.4 × eye height), applies the spread in yaw *and*
  pitch, and tests against a torso *capsule* — a vertical segment plus a radius — rather than an
  infinite vertical cylinder. A round can now miss high or low, which a purely horizontal model
  made impossible.
* The burst cadence in `World::enemy_combat_step`, gated to PD simulants carrying an automatic
  so GoldenEye hunters keep their tuned flat cadence.

### Still open

* **`MAX_HIT_RATE` stays off the PD path**, for the reason in §9 — it is a lethality throttle on
  top of an artificial accuracy model, and re-imposing it would flatten the top of the difficulty
  table. The burst gap is the honest version of the same idea and is now doing that job.
* **Aggregate pack DPS is not a PD-faithful problem, but it is a real one.** Six simulants
  converging (which §10's omniscience now guarantees) multiply the incoming rate by six, and
  Perfect Dark's own answer to that is a large level and an armour pickup rather than an AI rule.
  If the lab needs a ceiling for playability, armour is the faithful lever.
* PD's `bot_tick` also lets a *dizzy* bot (tranquillised past `dizzyamount`) fire at anything in
  sight, zeroed or not. No tranquilliser, so nothing drives it.

---

## 13. The 32-slot direction table (2026-08-17)

Perfect Dark does not have one fire animation per weapon. It has 32 animation *groups*
per stance, indexed by the bearing from the character to its target — `chr_attack`
(`chraction.c:2825`):

```c
angle = chr_get_attack_entity_relative_angle(chr, attackflags, entityid);
groupindex = angle * 5.0937690734863f + 0.5f;   // 5.09377 == 32 / BADDTOR(360)
if (groupindex < 0 || groupindex > 31) groupindex = 0;
index = random() % animgroups[groupindex]->len;
animcfg = &animgroups[groupindex]->animcfg[index];
```

We had exactly one clip per weapon class, used at every bearing.

### What the bearing actually buys: time

Adjacent slots share a group in runs, so the three human standing tables resolve to
4–6 distinct groups each — not 32. The thing that varies across them is
`shootstartframe`, and it grows with the turn the guard has to make:

| where you are | animation | first round on frame |
|---|---|---|
| dead ahead | `ANIM_0002` | 23 |
| flank | `ANIM_0032` | 30 |
| behind | `ANIM_0006` | 39 (of 121) |

So **coming at a rifleman from behind is worth 16 frames — over half a second — and
that number is authored, not tuned.** That is the gameplay reason the table is worth
fifteen extra animations, and it is the thing to watch for in a playtest.

### `angleoffset` is a turn tolerance, not an aim correction

Each row states how far its animation's aim-zero sits off the body's facing. PD does not
correct for that — it *targets* it: the row's `angleoffset` is passed to `chr_turn` as
the turn **tolerance** (`chraction.c:10758`), so the body settles at
`bearing − angleoffset` and the animation's own authored aim lands on the target. A
`DTOR(90)` animation is played with the torso deliberately left facing 90° away.

That maps onto our stack cleanly, because the simulant already owns its yaw:
`Simulant::yaw` is now the **body** yaw and turns toward `bearing + error − angleoffset`,
while `Simulant::barrel_yaw()` (`yaw + angleoffset`) is what the round is fired along and
what the firing cone is measured against. `SimOutput` carries both.

### Which way is positive

`chr_get_angle_to_pos` (`chraction.c:13787`) returns `atan2f(dx, dz) − theta` wrapped
into `[0, BADDTOR(360))` — the same `atan2(x, z)` convention this game uses for yaw, so
the two are directly comparable. **Positive is the character's left**, which the source
states outright at `chraction.c:9313` (`// aimendsideback positive is aiming left`).

Three independent things agree with that, which is why it is not being taken on trust:

1. The tables put the `DTOR(90)` rows at slots 10–15 (bearings 112°–169°) and their
   `DTOR(270)` mirror partners at 16–21.
2. Every table is exactly symmetric under `i -> 31 - i` — not under `i -> -i`, because
   `group_index`'s `+ 0.5` puts the slot boundaries half a slot off the axis.
3. **The assets themselves.** `world::tests::each_fire_animation_aims_where_pd_says_it_does`
   measures each clip's barrel yaw through the real layer stack and compares it to the
   `angleoffset` read out of `chraction.c`. Eighteen agreements, worst case 14.9°, and
   the `DTOR(270)` rows come out on the character's right. The C table and the ROM
   animations were transcribed independently; they had no way to agree by accident.

### The asset job

`pd_roster.json` gained 15 clips at slots **36–50** — appended, never inserted, because
`FIRE_*_IDX` / `CHAR_HIT_START` / `+ HIT_CLIPS.len()` are arithmetic on the frozen 0–35
layout. `AttackAnimConfig::slot` joins each transcribed row to its exported file, pinned
by `world::tests::direction_table_rows_point_at_the_clips_they_name`.

Every `endframe` in the transcribed table falls inside its clip's real frame count — an
independent check on the field-order reading of the C struct literal, since a
mis-assigned column would have produced windows past the end of the animation.

### What runs per burst

`chr_attack` picks the row **once, at the instant the burst begins**, and holds it for
the whole animation (`chr->act_attack.animcfg`). `World::start_enemy_fire` does the same:
resolve the bearing, index the table, roll within the group, then `install_fire_row`
points the timing windows, the authored aim cone, the aim-overlay clip and hold frame,
and the barrel axis at that row together. The axis is the part that cannot be skipped —
each animation holds the gun its own way — so every clip a hunter might play is measured
against **its own body** at spawn into `EnemyInstance::fire_axes`.

A burst that ends on a sideways clip hands the hold back to the forward one, because
between bursts our hunters keep the weapon up and tracking (PD's chr leaves the attack
action entirely), and a 90°-off clip would otherwise leave the chest-aim pinned at its
cone limit until the hunter fired again.

### Deliberately not ported

* **The `flip` path.** `chr_attack` mirrors the group index for the other-handed case;
  we have no pose mirroring. Same reason the `flip` rows are dropped from the hit tables.
* **The kneel and lie tables** (`g_Kneel*AttackAnims`, `g_LieAttackAnims`) and
  `g_RollAttackAnims` / `g_WalkAttackAnims` — no kneel, prone or roll posture here. The
  rows are read and understood; there is nothing to attach them to.
* **`RACE_SKEDAR`.** One group of `ANIM_034A` at all 32 slots; no Skedar, nothing
  directional in it.
* **Heavy's missing right-hand mirror.** PD authored one sideways two-handed animation
  (`ANIM_0004`, drawn left) and no twin, so where the light table mirrors it the heavy
  table uses the aim-forward turn-around row instead. That asymmetry is transcribed as
  found and asserted, so nobody "fixes" it by inventing a mirror.

## 14. The free-for-all seam (2026-08-17)

§9's known gaps said hunter-on-hunter damage worked but **movement did not**:
`Enemy::update` took the player's position, so a simulant that picked a packmate shot it
from wherever it happened to be standing rather than hunting it.

`update` now takes an `EngageTarget { pos, id }` — the player by default, a packmate when
a simulant has chosen one — and every geometric decision measures against it:
`known_target_pos`, the `Chase` approach and its `flank_point`, the `Attack`
standoff/`back_off`, `evade_step`, `sample_cover_cell`, `pick_reposition`, `perceives` /
`in_cone`, and the LOS gate. So the full chase → standoff → cover → peek → reposition
chain runs against an arbitrary target.

Four rules stay **player-specific**, and the reasons are not symmetry:

| rule | why |
|---|---|
| the catch check (`Enemy::catches`) | a hunter cannot catch a packmate, and this is what ends the hunt |
| the `aimed_at` crosshair sense | there is no packmate crosshair to feel |
| `detectable` (the `N` invisibility toggle) | it is an observe aid for watching hunters work; extending it would blind the whole squad to each other |
| `squad_alert`, `alert_enemies_to_movement`, `grenade_flush_step` | `World`-level, all about the player |

`Enemy::can_see` is where the third one lives — one function, so the exemption is stated
once instead of repeated at nine call sites.

**The target is one frame stale**, because `pd_lab::step_simulant` runs after the FSM.
That matches PD's own ordering (`bot_tick` picks the target, then the action layer acts on
it) and the alternative is running target selection twice per step.

## 15. The barrel axis (2026-08-17)

`EnemyWeaponAsset::barrel_axis` derived from `muzzle_offset`, which came from the
muzzle-flash mesh centroid *when there was one and the gun mesh centroid when there was
not*. The second branch was wrong: measured across the arsenal it put the Sniper Rifle
22° high and the Rocket Launcher **backwards** (`(-0.06, -0.03, -0.998)`).

The fix is not a cleverer estimator. The eighteen weapons that *do* carry a flash mesh
give the ground truth — all resolve to `+Z` within 4° — and against that every
mesh-derived estimator fails on a third of them:

| estimator | worst error on a known-answer gun |
|---|---|
| gun-mesh centroid (the old fallback) | 176° (Moonraker Laser) |
| furthest vertex from the origin | 178° (Phantom) |
| longest bounding-box axis, signed by the centroid | 180° (six guns) |
| furthest vertex along `+Z` | 91° (Moonraker Laser) |

Because the models are not placed consistently relative to their origin: the DD44
occupies `z` in `[-269, 0.2]` with its flash at `z = +64` — modelled entirely *behind* a
muzzle-at-the-origin — while the AR33 sits entirely in front of a grip-at-the-origin.
Both point `+Z`, and nothing about the vertices distinguishes the two cases.

**Perfect Dark does not derive a direction from the gun either.**
`chr_calculate_aimend` (`chraction.c:9200`) reads a named node for the muzzle *position*
(`MODELPART_CHRGUN_GUNFIRE`, falling back to `MODELPART_CHRGUN_0001`) and takes the firing
*direction* from the character's own aim angle (`sinf(aimangle)`, `chraction.c:9254`),
using the muzzle point only to offset the ray's origin.

So the axis is a **convention of the asset set**, measured where a weapon declares it and
inherited from `BARREL_MODEL_AXIS` where it does not — and that constant was `-Z`, the
exact opposite, which is how the rocket launcher ended up backwards. Nothing caught it
because the old fallback never reached the constant; it produced a per-weapon wrong
answer instead. Now asserted for all 23 weapons, with both branches required to stay
populated so neither becomes untested.

## 16. "They don't come and get me" — three defects the playtest found (2026-08-17)

Reported from the first playtest of §13–15: hunters on a lower floor, heads showing above
the edge, that neither climbed the stairs nor shot. *"It's like they can see me, so
they're just waiting there, but they can't shoot me either."*

Three independent causes, each with a decomp answer. The first was made visible by §14
rather than caused by it; the other two were latent from before and only bite in a level
with floors.

### 16.1 There was no team check

`bot_choose_general_target` walks candidates in ascending distance and takes the first
acceptable one — but only after
`chr_compare_teams(botchr, trychr, COMPARE_ENEMIES)` (`bot.c:1699`), and it separately
invalidates an existing target that turns out to be a friend (`bot.c:1675`). We ported the
distance walk and the vetoes and **not the team check**, so every hunter was every other
hunter's enemy.

A packmate is nearly always the closest visible character there is. So the
ascending-distance walk picked a packmate essentially every time, and once §14 made the FSM
manoeuvre against its chosen target, the pack stopped coming for the player at all — which
is what the overlay in the playtest screenshot showed: `#0 → #2`, `#2 → #3`, one already
dead.

§9 called the old behaviour "friendly fire is emergent, not special-cased". That was true,
and it was harmless while it only affected *shooting*. It was also, without anyone saying
so, a **free-for-all configuration applied unconditionally**.

The fix restores PD's own distinction, which is not a mode switch but a predicate:
`COMPARE_ENEMIES` returns true whenever `(g_MpSetup.options & MPOPTION_TEAMSENABLED) == 0`
*or* the teams differ (`chraction.c:14880`). So free-for-all in Perfect Dark is literally
teams-disabled. `pd_lab::is_friend` is that predicate: hunters are one team, the player is
the other, and `PdLabConfig::free_for_all` (`PD_LAB_FFA=1`) turns the check off to get the
old behaviour back deliberately. Default **off**, because the hunters are a squad.

Teammates are excluded from the candidate list rather than vetoed downstream, in the same
place PD excludes them. Leaving them in and trusting the distance sort does not work, for
the reason above.

**Stray-round friendly fire is unchanged.** `emit_pd_shot` still resolves whoever is on the
line, so a round that crosses a packmate hits it. That is PD-faithful — teammates there can
absolutely shoot each other by accident, they just do not *target* each other — and it is
what the §9 measurement was really demonstrating.

### 16.2 The engagement band was measured laterally

Perfect Dark has both `prop_get_distance_to_prop` (3D) and
`prop_get_lateral_distance_to_prop` (XZ), and `botcmd_tick_dist_mode` — the function that
decides BACKUP / OK / ADVANCE / GOTO against `g_BotDistConfigs` — measures with the **3D**
one (`botcmd.c:98`). We used the lateral one for everything.

A hunter one storey below its target therefore read a comfortable 5.3 m, concluded it was
sitting exactly at its 4.8 m pistol standoff, and planted — when the real separation was
6.1 m through a floor slab. `Enemy::engage_dist` is the 3D version and now drives the band:
the `Chase`→`Attack` entry gate, the `Attack` hold / advance / back-off, the `Attack`
distance bail and the `Cooldown` re-acquisition, in both the FSM and the utility layer.

`dist_to` stays lateral and stays the **arrival** test (`Search` / `Investigate` waypoints),
which is the split PD keeps its two functions for. Perception also stays lateral on purpose:
`DETECTION_RANGE` and the cone were tuned as a ground-plane reach, and PD's own sight test
is a raycast rather than a distance, so nothing there was making a decision about an
unreachable target.

### 16.3 A hunter with no sightline held its standoff

`botcmd_tick_dist_mode` says this twice, in both version branches (`botcmd.c:135`
and `:163`): an out-of-sight target turns `BOTDISTMODE_OK` (hold) **and**
`BOTDISTMODE_BACKUP` (give ground) into `BOTDISTMODE_ADVANCE`. A PD bot never holds a
distance it cannot see along.

We had no such rule. What we had was `ATTACK_LOS_GRACE` — a 0.3 s *debounce* on the
`Attack`→`Chase` bail, added because the AI lab caught an Attack↔Chase thrash once ORCA
could settle a hunter on a corner seam. That debounce is right for what it was for and
wrong as a substitute for this: where the sightline **flickers** rather than cleanly
breaking — heads bobbing over a floor edge is the perfect generator — the grace resets
before it can expire, so the hunter neither bails to `Chase` (which would path it up the
stairs) nor advances. It stands in its standoff band indefinitely. Exactly the report.

`Attack` now advances whenever `!los`, before consulting the band at all, in both the FSM
and the utility arm. The grace keeps its original job of debouncing the *state* bail; it no
longer implies standing still.

### What this changes outside the lab

16.2 and 16.3 are in the shared FSM, so **GoldenEye hunters in the normal game get them
too**. Both are strictly better and neither is behind a flag: a hunter that closes when it
cannot see you is the behaviour the standoff was always meant to express, and an engagement
band that ignores height was never intentional. 16.1 is `PD_LAB`-only, since `pd_target` is
only populated for simulants.

The regression tests are `enemy::tests::a_target_a_floor_above_is_out_of_the_standoff_band`,
`enemy::tests::an_attacking_hunter_with_no_sightline_advances_instead_of_holding`, and the
team halves of `world::tests::simulants_can_target_and_shoot_each_other`.

## 17. Out of the spike: this is the game's AI now (2026-08-17)

Everything above was behind `PD_LAB=1`. It is not any more. `PdHunters` is on every
`World`, every hunter carries a `Simulant`, wears a Perfect Dark body driven by
`PD_TEMPLATE_CLIPS`, and shoots a real hitscan down a possibly-mis-aimed barrel.

`PD_LAB` still exists and still means something — a bare test room, a pinned tier, the
per-simulant debug overlay — but it no longer decides *which AI runs*.

### What the flag used to gate, and where each piece went

| gate | before | now |
|---|---|---|
| `PD_LAB=1` → `designs::pd_lab()` | replaced the level with a bare box | still does, and that is all it does |
| `spawn_family` | Perfect Dark **in the lab**, GoldenEye everywhere else | Perfect Dark whenever its assets loaded; GoldenEye is the fallback |
| `pdsim: pd_lab.map(..)` | only lab hunters carried a simulant | every hunter does — the model is family-agnostic |
| `pd_actors` | built only in the lab | always built |
| `emit_pd_shot`'s packmate victims | lab only | always |
| the debug overlay | lab only | still lab only |
| the radar | lab only | **always in HUNT** — a player-facing feature |

The one genuine blocker the old handoff named — "`spawn_family` picks GoldenEye *or*
Perfect Dark for the whole wave, a mixed roster needs the clip template resolved per
hunter" — turned out not to be on the path. A mixed roster is only needed if both families
should appear *in one wave*, and the decision was that they should not: the directional
fire table and the authored per-hit-part reactions name PD clips, so the body and the
animation fidelity travel together and the all-one-family rule is the thing keeping them
consistent. `World::set_force_ge_family` (`GE_BODIES=1`) flips the whole wave the other
way, which is what keeps the A/B tests and the no-PD-assets fallback alive.

### The hit roll is retired

Every hunter fires a real shot. Three things went at once, and they had to go together:

* `rand() < accuracy · (1 − dist/range)` — the probability model,
* `MAX_HIT_RATE` — the global 4 hits/s ceiling, which existed *because* of the roll (a
  rolled full-auto would otherwise delete the player in one burst),
* `DiffParams::accuracy_mult` / `falloff_ease` and `EnemyWeaponDef::accuracy` — the
  difficulty and per-weapon levers that scaled it.

Keeping the ceiling on top of the zeroing model would have clipped the top of exactly the
range the difficulty table expresses: Hard and Dark both saturate 4/s and become
indistinguishable. The honest ceiling is Perfect Dark's burst gap, which
`enemy_combat_step` applies to the *cadence* rather than to the damage.

**How well a hunter shoots is no longer on `DiffParams` at all.** The dial still has one
lethality axis — the same position now selects a PD zeroing tier through
`tier_for_dial_frac` instead of multiplying a probability. `difficulty_params_ramp_from_baseline_to_brutal`
asserts that ordering where it now lives.

### The boot dial moved down

The app pinned `DIFFICULTY_MAX` at boot. Under the roll that meant "1.6× accuracy"; under
zeroing it means **DarkSim** — zero reaction delay, zero aim error, kills on sight from
across the room. So the boot position is now `dial_for_tier(HUNT_TIER, ..)`, i.e. Normal:
a 0.5 s reaction and ~8° of wander, which is where the model reads as behaviour rather
than as an execution. `=` / `-` still sweep the whole table live.

### Two defects the promotion exposed

Neither was caused by it. Both were latent in the lab and became the game's behaviour.

**A burst in flight blocked the plant.** `util_score`'s `Attack` arm scored 0 when
`firing && state != Attack` — "don't initiate the plant mid-suppress-burst, so the pack
pushes through chokepoints instead of stopping to shoot the instant it's barely in range".
Written when only the FSM pulled the trigger, so a burst meant "mid-approach volley" and
came in short bursts with gaps to plant in. The PD model owns the trigger and fires
near-continuously at the top tiers, so the exemption never lifted: `Chase` (0.7) won
forever and the hunter ran the player down to point-blank, firing. The FSM's
`Chase`→`Attack` gate had the same `!fire_anim` hole.

Dropped from both. Nothing is lost, because `Attack` advances to its standoff on its own —
entering it early does not stop a hunter, it just closes at a jog instead of a run. And it
matches `botcmd_tick_dist_mode`, where whether the bot is shooting is not an input to the
distance mode at all.

This is also what `orca_a_pack_funnels_through_a_doorway` had been passing on: its pack
crossed the doorway because firing kept them in `Chase`. The scenario now puts the player
**off-axis from the gap** so the wall blocks the sightline, which is what should force a
funnel — sightline, not distance.

**Packmates blocked each other's line of sight.** The simulant sight check used the
capsule-blocking cast, so in a converging pack the front hunter occluded the ones behind
it and they stood in `Attack` without firing (the AI lab measured 663 occluded frames on
one hunter in 15 s). `chr_has_los_to_chr` (`chraction.c:6533`) tests
`CDTYPE_OBJS | DOORS | PATHBLOCKER | BG | AIOPAQUE` — **`CDTYPE_CHRS` is not in the
mask** — and disables both characters' own perimeters for the cast. Another body never
breaks a PD bot's sight. Switched to the world-geometry cast, which also makes the sight
check agree with `emit_pd_shot`, where a body on the line is not an obstruction but the
thing that gets shot. The capsule-blocking variant had no callers left and is deleted.

### What a player will notice

* Hunters are Perfect Dark characters, and they turn-and-shoot directionally (§13).
* They shoot properly rather than rolling dice, with no damage ceiling — a well-zeroed
  hunter with an automatic kills fast, and that is the intended consequence.
* **They flinch.** PD's authored per-hit-part reactions are on by default for a PD body,
  so a hit plays the reaction for the part that was hit. The old sim-style
  no-flinch behaviour is the GoldenEye family's (`hit_reactions`), and
  `hits_flinch_only_when_hit_reactions_enabled` pins it there.
* The radar is always up in HUNT.
* ~~The body roster is 6, down from 44.~~ **Not a cost after all — see §18.** A GoldenEye
  body plays the Perfect Dark clip set correctly, so the roster is all 44 of them plus the
  6 Perfect Dark ones, every hunter on PD's animations.

## 18. The roster widened: 44 GoldenEye bodies on Perfect Dark's animations (2026-08-17)

§17 said the body roster dropping from 44 to 6 was "the one real cost of the decision".
It was not a cost — it was an assumption, and the assumption was wrong.

`PD_TEMPLATE_CLIPS` claimed "PD's bind pose is not GoldenEye's, and driving a PD body with
a GE clip produces a confidently-posed wrong figure". Measured, in
`tests::a_goldeneye_body_can_play_the_perfect_dark_clips`:

* **The bind orientations are identical** — `0.0°` apart on all 15 shared bones.
* Only the rest *lengths* differ (1.00–1.27×, i.e. body proportions), which a
  rotation-driven clip is indifferent to. 38 differently-proportioned GoldenEye bodies
  already share one clip set.
* **Every one of the 51 Perfect Dark clips drives a GoldenEye skeleton**: 15/15 bones
  bound, finite person-sized figure through the whole clip, on all 38 GoldenEye bodies.
  The 15 appended directional fire clips included.

And the check that makes those numbers trustworthy rather than merely reassuring: the two
games share an animation bank, so posing one GoldenEye body with slot *n* of each template
is the same animation twice, decoded from two different ROMs. **30 of the 36 shared slots
agree to within 0.3°.** The six that do not — 19, 20, 23, 25, 26, 27, at 28–104° apart —
are exactly the six `pd_roster.json` documents as deliberately *different* animations. A
transform error could not produce that pattern.

### What the asymmetry actually was

The doc's conclusion held in one direction only, and not for the stated reason. A
**GoldenEye clip on a Perfect Dark body** does break (9° mean, 62° worst joint-axis error):
the PD rig carries 15 extra `Blend_*` joints — its seam-hiding half-rotation frames — that
a GoldenEye clip has no channels for, so they stay at bind while their owning bones rotate.
That is what the one-family rule was really protecting, and it is why
`World::goldeneye_clips` is ignored for a Perfect Dark body.

### What changed

* `PD_TEMPLATE_CLIPS` is loaded **twice** — `pd_anim_template` bound to a PD rig,
  `pd_anim_template_ge` bound to body 0's. A clip binds its channels to one skeleton, so
  the same animation data needs one template per rig.
* `spawn_family` is replaced by `wave_bodies` (which bodies a wave draws from — **all** of
  them) and `body_clips` (which template drives a given body). The wave resolves the
  template **per hunter** instead of per wave.
* `EnemyInstance::pd_anims` now means "is on the Perfect Dark clip set" rather than "wears
  a Perfect Dark body". It is true for both families.
* The old single `force_ge_family` flag conflated two things that are now orthogonal, and
  is split: `BodySet` (who spawns) and `goldeneye_clips` (what animates them).

| | bodies | clips | directional table |
|---|---|---|---|
| default | 44 GE + 6 PD | Perfect Dark | yes |
| `BODIES=ge` | 44 GE | Perfect Dark | yes |
| `BODIES=pd` | 6 PD | Perfect Dark | yes |
| `GE_CLIPS=1` | 44 GE | GoldenEye (legacy) | no |
| `PD_LAB=1` | 6 PD | Perfect Dark | yes |

`PD_LAB` pins Perfect Dark bodies, because the lab exists to look at Perfect Dark
specifically — and it gives the headless scenarios a one-call way to get a PD-bodied hunter.

**That pin ate the flag it should have deferred to.** The app applied `BODIES` *before* the
`PD_LAB` block, so `enable_pd_lab`'s `BodySet::PerfectDark` clobbered an explicit
`BODIES=ge` and the wave stayed Perfect Dark with nothing to say why. It survived a whole
playtest because environment variables persist for a shell session and the lab is usable on
a real level — so "the lab is still on from an hour ago" is invisible in a way "the lab
replaced my level with a bare box" would not have been.

The roster block runs **last** now: whatever the environment asks for outranks a mode
default. And the boot log states the resolved answer unconditionally
(`World::roster_summary`), because a flag losing a silent fight with a default is exactly
the class of bug that has no symptom until the hunters are on screen. Pinned by
`tests::an_explicit_body_set_outranks_the_lab_pin`.

A 12-hunter wave measured: 11 GoldenEye bodies + 1 Perfect Dark, all 12 on the PD clips,
51 template slots each, 5 measured fire axes each. The families appear in proportion to the
catalog (38:6), so Perfect Dark bodies are the rarer sight — exporting more of the 65
shared-rig PD bodies is what would even that up, and it is still just an asset job.

### Two tests whose premise this invalidated

Recorded because both were *correct* checks that stopped meaning anything, which is a
different thing from a broken test.

* `the_chest_aim_axis_is_measured_per_body` required a GoldenEye hunter and a Perfect Dark
  one to measure *different* barrel axes, on the grounds that equal axes would mean the
  calibration had become a constant. Both families run the same clips now, and identical
  bind orientations mean the same clip through either rig yields the same chest-local
  direction — legitimately equal. Rewritten as `..._per_clip`: the axis has to vary with
  the **animation**, which is the property `fire_axes` and `install_fire_row` actually
  depend on.
* `hit_zones_scale_damage_by_impact_height` probed for blood at a height derived from a zone
  boundary. That is a statement about where the *mesh* is, and it broke the moment bodies
  changed size. It now probes the centre of the body's own posed bounds, measured the way
  `hit_enemy` measures it.

## 19. Two playtest findings on the widened roster (2026-08-17)

### 19.1 The GoldenEye bodies floated 1.09 m

Immediately visible, and the exact number is the tell: **1301.7 units × `CHAR_SCALE`**.

A Perfect Dark clip carries an *absolute* root translation of `1301.7` — PD's rest hip
height — on top of the animation's own vertical motion. A GoldenEye clip carries `0` and
relies on the bind. That pedestal is **load-bearing** for a Perfect Dark body, whose
vertices are stored bone-local so the root lift is what puts the geometry in place, and
**pure double-counting** on a GoldenEye body, whose vertices are model-space with real
inverse-bind matrices. `tools/pd-assets/pd_gltf.py` documents both conventions; what it does
not say is that the clips are therefore not interchangeable *for position*, only for pose.

So the feet-seating offset is a property of the **(body, clip set)** pair. The sweep in
`World::new` chose its seating idle by body *family* — correct while a family implied its own
clips, wrong the moment GoldenEye bodies started playing Perfect Dark's. It now measures both
and `body_feet_offset(body, pd_clips)` picks, with the hunter's own `pd_anims` selecting.

Measured on GoldenEye body 0: `0.909 m` on the GoldenEye clips, `-0.178 m` on the Perfect
Dark ones. The difference is the pedestal.

**The pedestal cannot simply be stripped**, which was the first idea. The same channel
carries the deaths' fall: it swings 1071–1193 units over a death clip, 76 over a walk, 106
over a fire. It is real animation, not a constant.

Pinned by `tests::a_hunters_feet_are_on_the_floor_on_either_clip_set`, which asserts the
thing a player sees — the lowest skinned vertex sits at the hunter's feet — across both clip
sets on both body families.

### 19.2 Hunters fired through their hurt animations

Not new with the GoldenEye bodies; it predates them and applied to Perfect Dark ones equally.

Perfect Dark calls **`chr_stop_firing`** (`chraction.c:9414`) immediately before entering
`ACT_ARGH`, at both injury sites (`:3476` and `:3520`): both hands drop, the aim-end resets,
the fire slots are freed. And leaving `ACT_ATTACK` stops `chr_tick_attack` pumping shots at
all.

Ours kept firing because **firing is a timer**. `fire_elapsed` runs in `enemy_combat_step`,
which knows nothing about the mixer or `Enemy::stun` — so the FSM correctly froze while
stunned and the shot pump carried on regardless. `World::stop_enemy_fire` is the port, called
wherever a hit reaction stuns: the authored injury table, the ragdoll stagger, and the canned
GoldenEye flinch.

**A detail worth recording, because it says the mix was ours rather than PD's:** the injury
handler returns early for `chr->aibot` (`chraction.c:3427`), so **PD's own simulants never
flinch**. They take the hit and keep shooting — which is exactly the "sim style" branch
`hit_enemy` still offers with `authored_reactions` off. What we had was a guard's flinch
paired with a simulant's trigger discipline, which is neither. The two coherent options are
"flinch and stop firing" (a PD guard) or "neither" (a PD simulant); the flinch is worth
having, so the trigger now drops with it.
