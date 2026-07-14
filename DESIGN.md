# Design Doc — Working Title: **BUILD & HIDE**

> A dynamic hide-and-seek FPS where the level editor *is* the game. The player builds a level under a time limit, hides, and then survives waves of enemies who hunt them through the geometry they just made. Built on an existing GoldenEye-style engine with a fast CSG brush editor.

---

## 1. Core Concept

The level geometry is the player's primary weapon. Instead of guns-first combat, the game is about **delaying and misdirecting an inevitable search** using structures the player designs on the fly. The fast, intuitive CSG editor already built for authoring custom levels becomes the in-game verb.

**The loop:**
1. **Build phase** — player spawns in a starter room with a time limit and a build budget. They construct rooms, corridors, doors, traps, decoys, and hide.
2. **Hunt phase** — waves of enemies enter and search the level trying to find and eliminate the player.
3. **Patch phase** — a short window between waves to scavenge materials from destroyed geometry and reinforce.
4. Repeat, escalating.

**Why this works:** the concept is only viable *because* the CSG editor is fast enough for real-time iteration. The tool is the game. This sidesteps the usual "level design is slow" problem and makes building a moment-to-moment action.

---

## 2. The Central Design Problem (solve this first)

**The sealed-box problem:** if the player can build arbitrary geometry, the optimal strategy is a windowless cube with no entrance — and the game is trivially won. Every good version of this game accepts that the player can never be *truly* unfindable; they can only **buy time and misdirect**.

Enforcement options (pick one or combine):
- **Everything is destructible.** No true safe room — only degrees of delay. Doors are just walls that cost enemies more time and make noise when breached (noise = information for the player).
- **Objectives force exposure.** Player must periodically reach a terminal, arm a device, or grab an item mid-hunt. Hiding becomes a rhythm of cover and exposure, not turtling.
- **Traversability validation.** The spawner verifies enemies have a path to every region before a wave starts, so the player cannot wall themselves into the void.
- **Construction is loud.** Enemies get a lead on where the player was building. The fortress's existence is itself a clue.

Every mechanic below should be evaluated against: *does this reward misdirection and skillful building, or does it reward optimal turtling?*

---

## 3. Build Economy

The resource system is what turns a single round into a progression loop.

**Budget sources (combine for a risk/reward economy):**
- **Enemies defeated** — kills grant build currency. Encourages the player to *engage* rather than only hide, creating a tension between safety and income.
- **Time survived** — a passive drip so a cautious player still accrues resources.
- **Scavenged materials** — reclaim currency/materials from destroyed geometry during the patch phase, so a collapsing fortress feeds the next one.
- (Optional) **Objective completion** — bonus budget for the forced-exposure objectives above.

**Spending:** budget buys brushes/volume, doors, traps, gadgets, and placeable weapons (below). Consider separate currencies for *structure* vs. *gadgets* so players can't dump everything into turrets, or keep it unified for simplicity — decide after playtesting.

**Design tension to preserve:** the player should constantly weigh "spend now to survive this wave" against "save/invest to survive later waves." Kills-for-budget makes this sharp because getting kills requires exposure.

---

## 4. Mechanic Families

### 4.1 Delay elements (the "door" family)
Block or slow movement; ideally also emit information.
- **Breakable doors** — cost enemies time to breach, emit an alert/noise when broken (tells the player where the breach is). The archetype.
- **Locked doors** — require a lever/key placed elsewhere, forcing the enemy pack to split up.
- **One-way drops** — player can descend, enemies can't climb back up.
- **Crawlspaces / size gating** — sized so the player fits but heavier enemy types don't. Turns level design into a movement filter.

### 4.2 Misdirection (the deeper half)
Control enemy *attention* rather than block movement. This is where the skill ceiling lives.
- **Decoy rooms** — obvious-looking hiding spots that eat enemy search time.
- **Remote noisemakers** — trigger to pull the pack toward the wrong wing.
- **Destructible lights + darkness** — build a dark maze; enemy vision cones become meaningful.
- **False walls / secret passages** — enemies only find them via a slow "search" behavior.
- **Goal:** a skilled player *herds* hunters through their level like a maze instead of passively hiding.

### 4.3 Placeable weapons & automation
Give the player agency *during* the hunt, not just before it.
- **Drone guns / auto-turrets** — placed and manually filled with ammo (inspired by the Perfect Dark laptop gun sentry). Finite ammo means they're a resource to manage, not a fire-and-forget win button. Consider: line-of-sight arc, friendly-fire on doors, whether enemies can destroy them.
- **Traps** — one-shot placeables triggered by proximity or remotely (crushers, floor collapses, gas). Consumable, budget-priced.
- **Manual weapons** — retain the GoldenEye-style gunplay for direct engagement; kills feed the build economy, closing the loop.

### 4.4 The patch phase (tower-defense engine)
Do **not** make it build-once. A short reinforcement window between waves where the player scavenges wrecked geometry and rebuilds. The level becomes a persistent, progressively-destroyed ruin the player keeps adapting. This single decision probably does more for replayability than any individual gadget — tension escalates naturally because the player patches the fortress *while it's failing*.

---

## 5. Enemy Escalation

Waves should attack the player's **building assumptions**, not just scale HP. Each new enemy type should invalidate a lazy strategy and reward a structural counter.

| Enemy | Behavior | Strategy it breaks |
|---|---|---|
| **Grunt** | Wanders, poor perception | (baseline) |
| **Breacher** | Ignores doors, goes through walls | Relying on doors/walls for safety |
| **Spotter** | Permanently marks player position for the whole wave once it sees you | Static hiding — forces relocation, creates panic |
| **Sound sensor** | Detects/punishes player movement & building noise | Moving or building freely during a wave |
| **Thermal unit** | Sees through walls for a few seconds | Thin-wall hiding |
| **Grid searcher** | Methodical room-by-room sweep | "Hide in a random room" |

Design rule: introduce each type so the player *learns the counter through the failure it causes*.

---

## 6. Stretch: Asymmetric Multiplayer

The biggest opportunity. One player builds and hides; the others are the hunters. The fast CSG editor is exactly what makes a "builder vs. hunters" match viable, and human hunters sidestep the AI-authoring problem entirely (humans supply the intelligence). Worth prototyping once the single-player loop is proven.

---

## 7. Key Risks

- **Build-phase pacing.** Too long = dead air; too short = punishing for players who aren't fast with the editor. Likely need **prefab rooms / stamps** as a floor so slow builders aren't dead on arrival. Time limits may need to scale with player skill or be player-set.
- **Snowball balance.** If hiding is too strong, the hunt is boring; if hunters are too strong, building feels pointless. The **misdirection mechanics** are the pressure valve — they reward skill instead of optimal turtling. Balance around the fun middle.
- **Turret trivialization.** Ammo-fed drone guns must not become fire-and-forget. Finite ammo, destructibility, and arc/LOS limits keep them a managed resource.
- **Economy exploits.** Kills-for-budget could create degenerate farming loops if enemies are cheap to kill in a chokepoint. Watch for it.

---

## 8. Open Technical Questions (for implementation)

1. **Navmesh:** Do enemies pathfind in real time over the CSG geometry as it's built, or is there a bake step? This heavily determines which mechanics are cheap vs. expensive:
   - Real-time nav → destructible walls, breachers, and mid-hunt building are cheap.
   - Bake step → need incremental re-bake on geometry change, or restrict building to the build/patch phases only.
2. **Destruction model:** Is CSG geometry destructible at runtime, and at what granularity (whole brush vs. partial)? Scavenging and breaching depend on this.
3. **Sound propagation:** Is there (or can there be) a sound/awareness system for noise-based misdirection and the sound-sensor enemy?
4. **Vision system:** Do enemies have vision cones / LOS checks today? Needed for darkness, decoys, and the spotter.
5. **Save/serialize:** Can player-built levels be serialized mid-run for the patch phase and (later) for sharing?

---

## 9. Suggested First Milestone (vertical slice)

Prove the core loop before building breadth:
1. Build phase with a timer + a single budget source (start with time-survived, simplest).
2. One enemy type (grunt) that pathfinds to the player over built geometry.
3. Breakable doors (delay + noise) as the one delay mechanic.
4. One wave, win/lose condition.

If *that* is fun, the concept holds and everything else is content. If it isn't, the problem is in the loop, not the feature list.
