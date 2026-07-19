# Design Ideas — Brainstorm Capture & Evaluation

> Companion to [DESIGN.md](DESIGN.md). This captures a stream-of-consciousness idea dump,
> sorts it into themes, gives each idea an honest verdict, shows how the strongest ones
> combine, and adds new ideas that fill the gaps. It is a **thinking document**, not a spec —
> nothing here is committed until pulled into DESIGN.md or a milestone.

**Verdict key:** ✅ works · 🟡 works with caveats · 🔴 problem / needs rework · 🔵 good but future / out of current scope

---

## 0. The big fork: there are two games in this dump

Before evaluating individual ideas, the most important observation: these fragments describe
**two coherent but different games**, and the most ambitious ideas belong to the second one.

### Game A — **Siege Survival** (what [DESIGN.md](DESIGN.md) and the vertical slice already are)
One persistent base. Enemies flood in through a fixed door and try to destroy/steal your
stuff and kill you. You build (add-only across sessions), defend, and clear the wave. Self-contained,
single-session-to-few-session loop. **Close to current scope and shippable.**

Ideas that fit Game A: inventory crates, on-person loot, simulant defenders, the fixed spawn
door, destructible doors/barricades/barrels, enemy perception & memory, add-only building,
ladders/vents/voids/stairs, auto-lighting, body armor.

### Game B — **Raider** (the overworld meta)
A persistent world of AI-owned bases. You raid them, steal is the *only* way to acquire, you
capture and then must defend multiple bases, splitting your time; a radar warns you when your
base is under attack while you're away. This is a **strategy/immersive-sim at 3–5× the scope**,
and it **re-introduces the terrain overworld that the port explicitly cut** (see the
`engine-port-scope` memory / [ENGINE_PORT_PLAN.md](ENGINE_PORT_PLAN.md)).

Ideas that fit Game B: overworld map, attacking other bases, steal-only economy, capture-and-own,
defend-while-away, radar, AI-authored base content.

### Recommendation
**Build Game A now. Treat Game B as the North Star.** The payoff: every Game A system
(crates, simulants, theft, add-only building, Claude-authored layouts) should be built
**forward-compatible** with an overworld, even though the overworld isn't built yet. That way
the brainstorm isn't wasted — it becomes the architectural constraint that keeps Game A from
painting itself into a corner. Don't let Game B's scope derail the vertical slice.

---

## 1. Win condition & wave structure

| Idea | Verdict | Notes |
|---|---|---|
| Time-based waves don't work — a long linear base waits out the clock | 🔴→✅ | You're right, and the flaw generalizes: **any passive win condition is exploitable**. |
| Waves run "until enemies destroyed" instead | 🟡 | Fixes the timer exploit but risks a new one: a turtle + turret + chokepoint could farm a finite spawn *safely*, and an infinite spawn can never be won. |
| Fixed, immovable, unremovable spawn door — all enemies flood through it | ✅ | Great. Partially solves the sealed-box problem (there is always exactly one guaranteed entrance) and gives the level a legible "front." |

**Synthesis — this is the keystone fix.** The clean win condition falls out of combining three
of your own ideas: **enemies prioritize destroying/stealing your crates** (§4), **crates are what
they path toward** (not the player), and the **wave ends when the base is cleared**. Now the
long-linear-base exploit dies on its own: enemies don't wander looking for a hidden player, they
make a beeline for your loot, so a base that's "safe" by being far away is also a base that fails
its objective. Lose condition = your crates are destroyed/stolen past a threshold. Win = wave
cleared with crates intact. **The objective is emergent from enemy behavior, not a bolted-on terminal.**

**Caveat on the single fixed door:** one entrance invites one kill-corridor. Pressure valves:
a *breacher* enemy that ignores walls (already in DESIGN.md §5), multiple/rotating spawn doors in
later waves, or door position that shifts per wave. Keep the "can't be moved/removed" rule — just
don't let it be the *only* way in forever.

---

## 2. Doors, barriers & destructibles

| Idea | Verdict | Notes |
|---|---|---|
| Locks don't make sense — they trivialize protection | 🟡 | True **in the hide-from-hunter frame** (a lock that stops enemies = free safe room). But locks are *good* in the **raiding frame**: when *you* raid, a locked vault forces you to find the key while defenders converge — that's tension, not trivialization. Frame-dependent, not wrong. |
| Destructible doors | ✅ | Already the DESIGN.md archetype (delay + noise on breach). The breach system exists in code but is currently disabled ([hunt.rs `build_doors`](native/crates/game/src/world/hunt.rs#L424)); the thesis (destroy element → collider + nav react instantly, no re-bake) is proven. |
| Time-release doors | 🟡 | Niche for Game A, but **excellent for Game B raids**: a vault that opens on a timer once you start hacking it → defenders get a warning window → a race. |
| Barricades, destructible crates, explosive barrels | ✅ | All standard and proven. **Explosive barrels are a free win right now** — the explosives/mines arsenal just landed ([explosives.rs](native/crates/game/src/combat/explosives.rs), rocket/grenade launchers, proximity/remote/timed mines in assets). A barrel is just a static prop with an HP that calls the existing `falloff_damage` blast on death. Enemy rockets chain-detonating your barrels = emergent chaos for zero new tech. |

---

## 3. Building primitives & the level toolkit

| Idea | Verdict | Notes |
|---|---|---|
| Ladders | 🟡 | Works, but needs a climb locomotion state on the controller + a climb anim. Small but non-zero. |
| Vents / crawlspaces | ✅ | This **is** DESIGN.md §4.1 "size gating" — sized so the player fits but heavy enemy types don't. Turns geometry into a movement filter. High value, cheap. |
| Voids to fall into | ✅ | One-way drops / hazards. Cheap (a trigger volume + fall damage or instant-kill). Doubles as a trap you herd enemies into. |
| Auto square-spiral staircase for tall vertical runs | ✅ | Best implemented as a **prefab/stamp** the editor emits. Directly addresses the DESIGN.md §7 "slow builders are dead on arrival" risk. Ship a small stamp library (stairwell, room, corridor) alongside this. |
| Auto point-light placement (optional in build, always in play; manual override) | 🟡 | Sensible default. **Independent of shadow baking** (which is deferred per `engine-port-scope`), so cheap: drop a point light at each room/brush centroid. Cosmetic-only until the lighting model lands — but note **destructible lights + darkness** is a DESIGN.md misdirection mechanic that depends on this, so it's a foundation, not just polish. |

---

## 4. Inventory, crates & the theft economy

| Idea | Verdict | Notes |
|---|---|---|
| Inventory crates in rooms; enemies steal or destroy them | ✅✅ | The **keystone** — see §1. This is what makes the win condition honest and turns "defend" into "defend *something specific*." |
| Some inventory carried on person, lost on death | 🟡 | Good stakes, but **presupposes a death/respawn model**. If death = game over, "lost on death" is redundant. It only means something if death = respawn-at-base-minus-carried-loot. So this quietly commits you to a persistence/continue model — decide that first. |
| Well-hidden crates = better protected but harder for *you* to reach | ✅✅ | Elegant and self-balancing: **your own security becomes your own friction.** The deeper you bury the vault, the longer *you* take to resupply from it mid-fight. Almost no code cost — it's a consequence of geometry, not a system. |
| Stealing/being-stolen-from as the *only* acquisition dynamic | 🔵 | Bold, coherent design pillar — but it's a **Game B pillar**. It only works if there's a rich overworld of bases to steal from. Adopting it commits the whole game to the raiding direction. Great North Star; incompatible with a self-contained Game A. |
| Body armor, grades, life packs; give to simulants | 🟡 | Standard RPG-lite depth layer. Works, ties cleanly into the crate economy (armor = loot you store/steal/carry). It's **depth-on-top, not core-loop-critical** — add after the loop is fun. |

**New idea — loot value drives enemy pathing (crates as bait).** Enemies path to the
highest-value *reachable* crate. This hands you a lever: place a fake high-value-looking crate to
**herd the pack** toward a kill-room. This merges the DESIGN.md "decoy room" mechanic with the crate
mechanic — misdirection stops being a separate feature and becomes a property of the economy.

---

## 5. Allied simulants

| Idea | Verdict | Notes |
|---|---|---|
| Recruit simulant soldiers to defend the base | ✅ | Feasible — allied AI/combat is cheap (per `enemy-port-recon`); it reuses the enemy FSM + nav with the target flipped. |
| Command layer: guard crates, reload wall guns, replace proximity mines | 🟡 | A small RTS command system — moderate but doable. Note **half of it is already built**: the arsenal has proximity/remote/timed mines, so "replace mines" and "reload wall guns" are commands over systems that exist. |
| Simulants defend while you're away; self-service at health/armor stations | 🔵 | The "while away" half is Game B (needs the overworld). Self-servicing (retreat to a health station when hurt) is a nice utility-AI behavior usable in Game A. |

**The real risk — the game plays itself.** If simulants are good enough to defend without you,
the player becomes a spectator. This is the #1 thing to balance. Pressure valves:
- **Upkeep** — simulants cost resources per wave; an idle army bankrupts you.
- **Hard cap** — few enough that they can't blanket the base.
- **They're killable and their gear is lootable** — a dead simulant drops its armor/gun into the enemy economy.
- **They need you** — orders are coarse (guard *here*), so the player's judgment on *where* is what wins, not micro.

Keep the player's decisions load-bearing: simulants execute, the player *positions and prioritizes.*

---

## 6. Enemy AI

| Idea | Verdict | Notes |
|---|---|---|
| Procedural animation (Perfect Dark style) — smooth walk/run blend, fire while moving | 🟡 | **Real, significant tech lift.** Today enemies use fixed GE clips with crossfades + locomotion *bands* (idle/walk/jog/run — [hunt.rs `band_for_speed`](native/crates/game/src/world/hunt.rs#L9)), and `enemy-port-recon` flags skinned animation as the hard part. Full procedural (IK foot-planting, arbitrary aim-while-move) is a big jump. **Recommend the 80/20 middle:** an *additive upper-body aim layer* over a *speed blend-tree* (blend adjacent bands instead of snapping) gets you most of the PD feel — fire-while-moving, no foot-slide — without full procedural rigging. |
| Neural-net AI / "how smart can we make them?" | 🔴 | **Push-back.** NN policies are the wrong tool for shipping enemies: expensive to train, non-deterministic, near-impossible to tune *for fun* or debug. What makes F.E.A.R./PD enemies *feel* smart is **classic technique** — utility AI or GOAP + a good perception model + squad coordination + audio barks that *announce* their reasoning ("flanking left!"). You can make them very smart this way, and every decision stays inspectable. NN buys unpredictability, not fun. Spend the effort on perception + barks. |
| Different difficulties | ✅ | Trivial — scale perception radius, reaction time, accuracy, memory duration (§below). |
| Range of enemy weapons | ✅ | Already arriving — the full arsenal + dual-wield landed (recent commits). |
| Omniscience level — do they know where you are, or must they search? Do they remember layout/crate locations from prior visits? | ✅✅ | **The most important AI question, and you framed it right.** The answer that makes the game good: **not omniscient.** Enemies run a perception model (vision cones + hearing + a decaying *last-known-position*), which DESIGN.md already calls for. Concrete model below. |
| Priorities: search base to destroy/steal + eliminate player | ✅✅ | See §1 — this is the keystone that makes the win condition emergent. |

**Concrete memory model (answers the omniscience question):**
- **Short-term (per-wave):** last-known-position that *decays* to a search behavior. This is what makes relocating pay off — the core hide skill. Never omniscient.
- **Long-term (across raids, Game B):** a base you've raided remembers its static layout and known crate spots, so **repeat raids escalate** (defenders pre-position). Symmetrically, *your* base's attackers "learn" it over successive assaults.
- Make memory-duration a **difficulty knob** — a clean one-dial axis from "goldfish grunt" to "veteran that remembers everything."

---

## 7. Build/edit rules — add-only after first gameplay

| Idea | Verdict | Notes |
|---|---|---|
| Fully editable within a build session, but once you've had a gameplay session those edits lock; the next build session can only **add**, not modify. Add doors to existing rooms, add new rooms. | ✅✅ | **One of the strongest and most original ideas in the dump.** |

Why it's good:
- **Kills the "re-seal the box every session" exploit** — you can't retroactively fix a hole the enemy found; you can only build *around* it.
- **Bases accrete like real fortresses** — archaeological growth, each session a visible stratum. Great identity.
- **Early decisions carry permanent weight** → strategic depth, and it makes the *first* build session tense.

It also maps cleanly onto the tech: **brushes become append-only after the first bake.** The nav
re-bake only ever *adds* geometry, never invalidates existing paths — simpler than arbitrary edits.

**One caveat + valve:** pure add-only can hard-brick a player who put a room in a terrible spot.
Give a **scarce demolition currency** — you *can* remove, but it costs real resources, so it hurts.
Consequence without a dead end.

---

## 8. Tooling, content & multiplayer

| Idea | Verdict | Notes |
|---|---|---|
| Test whether Claude Code can drive the build toolkit to author levels (automate 100s) | ✅ | **Worth doing, and it's more than a gimmick.** (1) It's the cleanest test of the toolkit's design — if an agent can drive it, the API is clean. (2) It **needs a serializable/scriptable level format** (the DESIGN.md §8.5 open question), which you want anyway for the patch phase and MP. (3) It's the **content pipeline for Game B** — AI-authored enemy bases for the overworld come for free. Prioritize the serialize format; the AI-authoring falls out of it. |
| Multiplayer | 🔵 | DESIGN.md §6 already flags asymmetric MP (one builder vs. human hunters) as the top stretch — human hunters sidestep the whole AI-authoring problem. Same serialize format unlocks it. Future. |

---

## 9. Game B — the overworld meta (parked, but coherent)

All 🔵 — compelling, and they hang together as a real game, but they are **post-Game-A** and
**re-introduce cut terrain**. Captured so the vision isn't lost:

- Terrain overworld with scattered base entrances.
- Attack other AI bases defended by simulants; steal weapons/resources back to your base.
- While you're away, AI can attack your base → **radar** warns you → race home to defend.
- Capture and *own* bases → many bases, split your time strategically (4X-lite time management).
- Bases with better loot are harder to infiltrate (risk/reward tiering).

**Forward-compat checklist for Game A** so Game B stays reachable:
- Levels must **serialize/deserialize** (also needed for patch phase, Claude-authoring, MP).
- Crates/loot need **stable identity** and value so they can move between bases.
- Simulants need an **owner + orders** model that isn't hardcoded to "the one base."
- Enemy AI's **long-term memory** hook (§6) should exist even if unused in Game A.

---

## 10. New ideas (filling the gaps)

1. **Noise as the universal information currency.** Building, breaching, gunfire, crate-cracking,
   and footsteps all emit on **one** sound-propagation system. It's the connective tissue for
   enemy hearing, the sound-sensor enemy (DESIGN.md §5), player intel ("something's breaching the
   east vault"), *and* stealth-raiding in Game B. Build it once; six mechanics light up.

2. **Reinforcement tied to the door, not a clock.** The fixed spawn door disgorges enemies; a
   wave ends when the base is cleared *and* the door is held/sealed. Pacing without a timer to game.

3. **Explosive barrels + mines as the buildable trap layer — available now.** Zero new tech: the
   explosives blast (`falloff_damage`) and proximity/remote/timed mines already exist. Barrels are
   static props with HP that detonate; mines are already placeable. This *is* the DESIGN.md §4.3
   trap family, mostly already in the box.

4. **Your defense teaches your offense.** Because you both build and raid, building a base is how
   you learn to *break* one. The skill transfers both directions — and enemy bases can be seeded
   from saved layouts (yours, Claude's, other players'), tying §8 content straight into Game B.

5. **Demolition currency** (see §7) as the single pressure valve that makes add-only humane.

6. **Simulant upkeep/loyalty** (see §5) as the single pressure valve that stops the AI army from
   playing the game for you.

---

## 11. Recommended near-term subset (pull into the vertical slice)

The vertical slice (DESIGN.md §9) is: build + timer + one grunt + breakable door + one wave.
These additions are **cheap, high-leverage, and mostly reuse what just shipped:**

1. **Crates + "enemies target crates" + "wave ends when cleared, lose if crates fall."** Replaces
   the exploitable timer with an honest objective. *This is the highest-value change in the whole dump.*
2. **Fixed spawn door** as the enemy ingress. Cheap, and it makes the level legible.
3. **Explosive barrels** — a static destructible prop on the existing blast. Nearly free given the arsenal.
4. **Perception + decaying last-known-position** for the grunt (not omniscient). Makes hiding *mean* something in the slice.

Everything else — simulants, add-only building, procedural anim, armor, the overworld — is
**post-slice**. Prove the crate-defense loop is fun first; if it is, the rest is content and depth.

---

## 12. Open questions to resolve before committing

1. **Persistence model.** Is there death/respawn? "Loot lost on death" and "carried inventory" are
   meaningless without deciding this. (§4)
2. **Game A or Game B as the target?** Everything downstream — steal-only economy, terrain,
   simulant ownership — forks on this. (§0)
3. **Serialize format.** Needed by the patch phase, Claude-authoring, MP, *and* Game B. Probably the
   most leveraged single piece of infrastructure to build early. (§8)
4. **Re-bake cost of add-only nav.** Append-only geometry should make incremental nav re-bake
   tractable — confirm before committing to mid-session building. (§7)
