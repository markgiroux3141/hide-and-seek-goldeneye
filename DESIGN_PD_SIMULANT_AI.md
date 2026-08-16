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

## 9. What the port actually does (`PD_LAB=1`)

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

### Deliberately left on our existing stack

**Movement, pathing, local avoidance, cover and animation.** PD's movement layer is built on
hand-authored pads and waypoint lists our levels do not have, and swapping it in would regress
the ORCA / nav / foot-IK work for no gain. The thing that reads as "a PD simulant" in a
firefight is the aim and the engagement timing. So the existing FSM still decides *where the
hunter goes*; the simulant decides *where it looks and when it shoots*.

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

- **Target selection is exercised but not stressed.** The lab's candidate set is just the
  player, so the personality vetoes that compare *between* candidates (Prey, Judge, Venge,
  Feud) are covered by unit tests rather than by play. A multi-simulant deathmatch would need
  enemy-versus-enemy damage, which the game does not have.
- `SHIELD` / `TURTLE`'s double shield and the tranq `dizzyamount` degradation are still
  unported, for the same reason as before: no shield system, no tranquilliser.
- Cloak handling is present in the model (`zerocloakspeed`) but nothing drives it.
