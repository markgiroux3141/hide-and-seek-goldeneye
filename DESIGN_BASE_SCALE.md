# Design — Making a Large, Complex Base *Essential*

> Companion to [DESIGN.md](DESIGN.md) and [DESIGN_IDEAS.md](DESIGN_IDEAS.md).
> Captures the 2026-07-19 brainstorm on the specific question:
> **how do we justify the player building out a very large, complex base —
> turning "build a big base" from an arbitrary task into something essential?**
> Thinking document, not a spec. Nothing here is committed until pulled into DESIGN.md.

---

## The core insight

The crate keystone in [DESIGN_IDEAS.md §1](DESIGN_IDEAS.md) (enemies path to loot, wave
ends when cleared, lose if crates fall) solves **"why build *well*"** — but **not "why build
*big*."** The *optimal* answer to "protect the crates" is a single deep vault behind one
chokepoint: a small, tight base satisfies it perfectly.

Every defense game has gravity pulling toward **minimalism**: less wall to breach, less to
watch, one field of fire. Complexity is a *liability* by default — more rooms = more surface,
more you can't see. So to make a large complex base essential you must do **two things at
once**:

1. Make **small** bases physically *fail* (can't house the loot, can't cover the threat,
   cascade when breached).
2. Make **large** bases actively *pay* (bigness generates advantage that outweighs its
   defense cost).

**Unifying reframe: stop treating the base as a *wall*; treat it as a *machine*.** A wall's
job is to be minimal and impermeable — that rewards small. A machine's job is to *process the
enemy flow* — route it, split it, delay it, grind it, and produce value while doing so.
Machines have parts; parts need space and arrangement. When the base converts incoming enemies
+ time + space into dead enemies + loot, every room is a component with a job, and more
components = more capability.

---

## Forces that make *small* fail (spatial pressure)

### 1. Loot volume outgrows the footprint — clustering punished  ·  **nearly free**
Crates don't stack infinitely, and adjacent crates die together to one blast (the AoE
`falloff_damage` already exists; enemies already carry rocket launchers). Wealth then
*requires* floor area and *forbids* piling it in one room. Bigness becomes a **consequence of
success**, not a chore. Cheapest scale-forcer available.
*Reuses:* crates (planned) + existing blast radius + enemy explosives.

### 2. Multiple ingress / breachers — one corridor can't cover it  ·  **medium**
Today: one spawn point + wall-respecting hunters → this *rewards* the kill-corridor, which is
the whole problem. Add wall-breaching enemies (the `breach_tick` machinery exists but is
disabled in `world/hunt.rs`) or 2–3 rotating spawn doors, and a single chokepoint stops
covering everything → you must **compartmentalize and hold area**. Area coverage is what makes
a base sprawl.
*Reuses:* disabled door-breach path; a breacher enemy variant; spawn-door count as a wave knob.

### 3. Cascade risk demands compartmentalization  ·  **cheap-ish**
Mines/barrels already chain-detonate (`apply_detonations`). Extend the idea: a breached room
lets enemies flood the *next* unless bulkheaded; fire/explosion propagates along open
sightlines. Now bulkheads, blast doors, and isolated vaults are **damage control**, not
decoration. Fortresses and warships are complex *because* of compartmentalization.
*Reuses:* chain-detonation logic; door colliders.

---

## Forces that make *large* pay (bigness generates advantage)

### 4. Depth = time = the primary defense resource  ·  **free with the crate keystone**
Enemy travel time from door to asset *is* your defense budget — every meter is another second
your traps/turrets/simulants get to work. A big base is a literal time-buffer. (This is
[DESIGN_IDEAS.md §4](DESIGN_IDEAS.md)'s self-balancing "well-hidden crates = harder for enemies
*and* for you.")

### 5. The base as a *machine* — function needs rooms  ·  **higher cost; THE complexity lever**
The big one for *complexity* specifically. If the base must *contain systems* — ammo/armor
fabrication, a simulant barracks, healing/rearm stations, power generators, a
radar/turret-control room — and each has placement constraints (power has range; generators are
loud and attract enemies; the med bay must be deep and safe; fabrication belongs near storage),
you get a genuine **spatial optimization puzzle**. The RimWorld/Factorio lever: bases are big
and intricate because *systems must be arranged*, not because the player likes hallways.

### 6. Verticality & routing convert complexity into lethality  ·  **toolkit mostly exists**
Size-gated vents the player fits through but heavy enemies don't ([DESIGN_IDEAS.md §3](DESIGN_IDEAS.md)
"size gating"); voids you herd the pack into; elevation advantage; one-way drops. A cleverly
complex base becomes a *more lethal machine* — complexity you *feel* paying off in kills, not
just in survival math.
*Reuses:* holes, stairs, platforms tools; needs size-gating + fall hazards.

---

## The synthesis I'd build

**Economic pressure creating spatial pressure.** Make crates *produce* value over time (income
scales with how many you hold safe), *forbid* clustering them (AoE), and let enemies *steal*
them (path-to-asset AI). That triangle forces: want *many* crates → spread them → defend
*distributed points* → cover *area* → build *big*. It's cheap — crates + existing blast +
retargeting the existing path AI from player→asset. Then layer **function-rooms (#5)** for the
complexity axis, and **breachers/multi-door (#2)** to kill the single-corridor exploit.

**Ranked by leverage-per-cost given current code:**
1. **#1 + #4** first — nearly free; flips *small → insufficient* and makes depth pay.
2. **#2** — kills the kill-corridor exploit.
3. **#5** — adds the complexity axis (base-as-machine).
4. **#3 / #6** — texture that makes clever layouts *feel* smart.

---

## The one trap

Every lever above collapses if a **single dominant solution** exists — one turret farm at the
door, or simulants good enough to defend without you ([DESIGN_IDEAS.md §5](DESIGN_IDEAS.md)'s
"the game plays itself"). The design job is not *adding* these levers; it's **balancing them so
no single-point answer beats a distributed one.** That is the real work.

---

## Open questions to steer next

1. **Primary pressure that *creates* the bigness** — economic (loot volume you must house and
   distribute), tactical (multi-directional threat you must cover), or functional (systems/rooms
   the base must contain for capability)? They stack, but one should lead.
2. **One persistent fortress that accretes** (the add-only pillar → bigness accumulates across
   sessions as archaeological growth) **or rebuilt/escalated per run** (bigness as a within-run
   arms race)?

---

## Current-code grounding (2026-07-19)

What already exists that these levers reuse:
- CSG editor (walls/doors/holes/pillars/stairs/platforms), frozen-geometry nav bake.
- Non-omniscient perception AI: vision cones + LOS + decaying last-known-position + gunfire
  hearing (`enemy.rs`).
- Fixed spawn point the player builds around; **breakable-door breach path exists but is
  disabled** (`world/hunt.rs`).
- Full weapons + explosives + mines with **chain/sympathetic detonation** (`world/combat.rs`,
  `combat/explosives.rs`).

What's missing that makes building arbitrary today (from the codebase map):
- Enemies path to the **player**, not to any protected asset — no crates/loot exist.
- **No win condition** and no base-tied lose (lose = you personally die).
- **No economy** — building is free and unlimited, so structures carry no opportunity cost.
- **No waves / escalation / enemy archetypes** — one manual hunt, one grunt type.
- **Noise is gunfire-only** — building and movement are silent (blocks "construction is loud").
- **No serialization** — blocks add-only persistence, patch phase, Claude-authoring.
