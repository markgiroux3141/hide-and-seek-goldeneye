# Design — Sentry Guns: What Stops Them Eating the Game

Brainstorm capture, 2026-08-20. The turret works ([`crates/game/src/turret.rs`],
[`world/tools/turret.rs`]) — it tracks, spins up and hoses hunters with RC-P90 rounds
at 143 damage/second. It is also currently **free, infinite, and unlimited in number**,
which is the whole problem.

---

## 0. The stake is bigger than balance

The obvious worry is "turrets trivialise combat". The real worry is worse:

> **A cheap infinite turret is the single most effective argument *against* building a
> large base.**

[DESIGN_BASE_SCALE.md](DESIGN_BASE_SCALE.md) opens by naming the gravity every defence
game fights: complexity is a liability by default, and the optimal base collapses toward
*one deep vault behind one chokepoint*. A turret that costs nothing to buy and nothing
to run is the perfect minimalism enabler — it makes that one chokepoint sufficient. It
turns "base as machine" straight back into "base as wall", which is exactly the failure
mode that document exists to prevent.

So turret economics are not a tuning pass to do later. They are **load-bearing on the
central design problem**, and the test for every rule below is:

**Does this make the turret reward bigness, or substitute for it?**

Anything that lets one turret in one corridor be a complete defence fails. Anything that
makes turrets *consume* the things a big base has to spare — floor area, routing, safe
interior movement, power, attention — passes.

---

## 1. Perfect Dark already shipped both answers

Worth stating plainly, because it resolves the "edit mode or game mode?" question
outright: PD has **two** autoguns. They are the same `struct autogunobj`, initialised
two different ways.

| | **Level autogun** | **Deployed Laptop Gun** |
|---|---|---|
| Placed by | the level designer, at author time | the player, mid-fight |
| Ammo | `ammoquantity = 255` — the infinite sentinel (`setup.c:781`) | `min(200, your carried reserve)`, **subtracted from your own ammo** (`propobj.c:17584`) |
| Count | as many as authored | **one per player** (`g_ThrownLaptops[playernum]`) |
| Redeploy | n/a | **detonates the previous one** (`explosion_create_simple(… EXPLOSIONTYPE_LAPTOP …)`) |
| Retrieval | never | walk over it → get the gun back **and its unspent ammo returns to your reserve** (`propobj.c:15262`) |
| Durability | destructible | `maxdamage = 1000` |
| Targets | `targetteam` | `~chr->team` — anyone not on your side |

Two observations that matter:

1. **Infinite ammo is reserved for architecture.** A turret that was always part of the
   building gets 255. A turret the player produced gets exactly what the player paid for
   out of their own pocket. That is the correct instinct and it transfers directly.
2. **Deploying spends the same ammo you would have fired yourself.** No separate turret
   currency. The turret is not extra firepower — it is *your* firepower, relocated and
   automated, at the cost of not carrying it. That is a genuinely elegant balance and it
   is free to copy.

Our BUILD-placed ceiling gun is PD's level autogun. What we are missing is the second
one, and — critically — we gave the first one the second one's job without giving it any
of the second one's costs.

---

## 2. Recommendation: two items, not one

| | **Emplacement** (have it) | **Laptop Gun** (new) |
|---|---|---|
| Placed | BUILD only, bolted to a ceiling | HUNT, thrown/deployed anywhere |
| Verb | **planning** — where will the flow go? | **reaction** — they broke through *here*, now |
| Cost | credits, up front + running | your own carried ammo |
| Count | economic limit, not a hard cap | **one**, redeploy detonates the old |
| Ammo | fixed magazine, does not self-refill | up to 200 from reserve |
| Recovery | repair the wreck | pick it up, unspent ammo refunded |
| Permanence | permanent (add-only editing) | disposable |

They are not redundant because they answer different questions. The emplacement is a bet
you place before you know where the attack comes; the laptop is what you do when the bet
was wrong. A player with both has a real decision every wave: *do I fortify the route I
expect, or keep the credits liquid for the route I don't?*

---

## 3. Ammo — yes, and the choice of limit is the whole design

Everyone agrees turrets need limited ammo. **Which** limit is where the dynamics live.

### (a) Per-turret magazine, refilled for credits · **the v1 pick**
Each turret holds N rounds and does not refill itself. Between waves you pay to restock.

Why this one first: it converts turrets from a **purchase** into an **operating cost**.
"How many can I afford to buy" is a one-time question a player answers once and then
forgets. "How many can I afford to *run*, every wave, forever" is a question that keeps
being asked, and it naturally caps turret count without a cap.

A 20-turret base does not need a rule forbidding it. It just goes bankrupt.

**Anchor the number on PD's 200.** At our 0.07 s cadence that is **14 seconds of
continuous fire** — realistically 6–10 kills against a 100 HP hunter once misses and
slew time are counted. Roughly *one wave per full magazine*, which is a good feel: the
turret is a wave's worth of help, not a permanent solution.

### (b) Shared ammo pool across all turrets · **cheap, good, composable**
One stockpile, every turret draws from it. Adds a nice spread-thin-or-concentrate
decision and makes the marginal turret cost obvious. Works *with* (a) rather than
instead of it.

### (c) Belt-fed from a physical ammo store · **the base-scale winner, later**
A turret must be within range of (or linked to) an ammo crate prop, which you stock.
This is the one that passes the section-0 test hardest: it makes turrets consume **floor
area and routing**, not just credits. A turret needs an ammo room. The ammo room needs
protecting. Suddenly the base has organs, which is precisely
[DESIGN_BASE_SCALE §5](DESIGN_BASE_SCALE.md)'s "base as a machine" lever.

Do (a) now, keep (c) as the growth path — it is the version that makes turrets *require*
a big base rather than excuse a small one.

---

## 4. Reloading is where the counterplay actually lives

This is the most important mechanic in the document.

**Between waves, for credits** is safe and dull — it is just a bill.

**During the wave, in person** is outstanding: the turret runs dry at the worst moment,
and feeding it means physically walking to a **known, fixed, loud location** while
hunters are alive, and standing still for a moment.

That single rule fixes the "turrets play the game for you" problem at its root, because
it inverts the scaling: **every turret you add is another resupply trip you owe, and
every trip is dangerous.** Ten turrets is not ten times the defence — it is ten errands
under fire. The player who spams turrets has built themselves a chore list in a shooting
gallery.

It also rewards exactly the right kind of base: one with **safe interior routing** to its
own guns. A big base with covered corridors to its emplacements can service them; a small
base with a turret out front cannot. Bigness pays.

Take PD's refund rule for the laptop version (`propobj.c:15262`): picking your own
deployed gun back up returns its unspent ammo. It costs nothing to implement and it means
a player can deploy speculatively without feeling robbed.

---

## 5. Yes, enemies must be able to kill them — but *deliberately*

Non-negotiable. An invulnerable turret has no counterplay and gives the hunter AI nothing
to say. PD gave the laptop `maxdamage = 1000`.

The subtlety: it is not enough for turrets to be *damageable*. Hunters must be able to
**decide** to attack one, or turrets will only ever die to stray splash.

- **Cheap version:** a hunter taking turret fire, with line of sight to the turret,
  retargets it. Reuses the existing damage-source plumbing.
- **Good version:** a breacher archetype that *prioritises* emplacements. Hunters already
  carry rocket launchers, and the explosives port is done — so a rocket hunter that
  deliberately clears turrets before advancing is mostly AI work, not new systems.

**Leave a wreck.** A destroyed turret should drop to a repairable husk costing less than
a new one — the same "darkened remains" treatment destructible props already use. Loss
stays meaningful without being ruinous, and repair becomes another between-waves errand
that a well-routed base performs safely.

---

## 6. The turret is also a liability — and in *this* game that is a feature

Barely explored, nearly free, and specific to this game rather than generic tower
defence:

**A firing turret is a flare that says "the player's things are here."** We already play
`rcp90-fire.wav` out to 34 m, and the gunfire noise-ping system already exists. In a game
whose core verb is *hiding*, an automated gun that opens up on its own — possibly while
you are three rooms away staying quiet — is a genuine strategic cost, not just a
balancing tax.

That gives turret placement a second axis beyond "where is the threat": **do I want this
room to be loud?** A turret guarding the vault advertises the vault. A turret on a decoy
route is misdirection. That is a real decision, and it comes almost free from systems
already built.

Follows from this: a **safety/hold-fire toggle** per turret becomes meaningful, and so
does a silenced (weaker, quieter) variant later.

---

## 7. Limiting the *count* — prefer natural caps to a number

A hard "max 5 turrets" is arbitrary and feels like the designer stepping in. Better:

| Lever | Cost | Verdict |
|---|---|---|
| **Credits** (buy + running cost) | free — economy exists | ✅ the default cap; do this |
| **Ceiling-mount requirement** | done | ✅ already limits *where* — needs interior space |
| **Ammo logistics** (§3c) | medium | ✅✅ caps by floor area and routing, not by decree |
| **Power / uplink** — turrets need a generator; N outlets each | large | ✅✅ the strongest base-scale lever, and the biggest build |
| Hard numeric cap | trivial | 🔵 fallback only if the economy fails to bite |

**Power is the one to want.** It makes turrets require a generator room, generators are
big and make a juicy target, and defending your own power plant is a base-as-machine
organ of exactly the kind DESIGN_BASE_SCALE is asking for. It is also a lot of new
system, so it belongs after the economy pass proves the cheaper caps insufficient.

---

## 8. Where the numbers stand today

| | Now | Proposed v1 |
|---|---|---|
| Cost | free | credits, priced against the RC-P90's 2500 |
| Ammo | infinite | 200 rounds ≈ 14 s of fire ≈ one wave |
| Refill | n/a | manual, in person, from carried ammo |
| Count | unlimited | economic |
| Durability | invulnerable, no collider | destructible, repairable wreck |
| Enemy response | none | fired on when it fires |
| Noise | plays at 34 m | same, but it *matters* |

Damage and cadence themselves look about right and are inherited honestly from the
RC-P90 — the problem was never the gun, it was that nobody paid for it.

---

## 9. Open questions

1. **Does the emplacement belong in HUNT at all?** Add-only editing (DESIGN_IDEAS §7)
   says the base accretes between sessions. Should buying and mounting a turret be a
   BUILD-phase-only act, with the laptop gun as the *only* in-combat option? (I lean
   yes — it keeps the two items cleanly separated by verb.)

2. **Whose ammo does the emplacement eat?** PD's laptop takes it from the player's own
   reserve. For a fixed emplacement, is it a separate purchased belt (simpler, and lets
   turrets be provisioned before a wave) or the same pool the player shoots from
   (harsher, more interesting, one budget)?

3. **Do turrets earn kill bounty?** `Killer::Turret` is already tracked separately from
   `Killer::Player` precisely so this stays answerable. Full bounty makes turrets
   self-funding — probably too generous. Reduced bounty is a lever that directly sets how
   turret-heavy the optimal build is.

4. **Do hunters *learn*?** A pack that routes around a known turret on wave 3 is a much
   better answer to turret spam than any cost. Expensive, but it is the version where the
   base has to keep evolving.

5. **Is one laptop gun per player right for a single-player siege?** PD's cap comes from
   multiplayer memory budgeting as much as design. Two might feel better here.
