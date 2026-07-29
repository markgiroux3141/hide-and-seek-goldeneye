# Design — Enemy AI & Animation: The Art of the Possible

> Companion to [DESIGN.md](DESIGN.md) and [DESIGN_IDEAS.md](DESIGN_IDEAS.md).
> Captures the 2026-07-27 brainstorm on: **given how far games have come since
> GoldenEye, what is our enemy animation + AI system missing that a modern AAA title
> would have — and does our model rig hold us back?**
> Thinking document + roadmap, not a committed spec. A menu of *potential futures*.

> **⚠ Do this work on a NEW feature branch** (e.g. `feat/ai-modernization` or one
> branch per item below), NOT on `main`. Each item is large and independent; land them
> one at a time behind the difficulty dial / feature gates so the current, playtested
> behavior stays intact. See "How to approach the work" at the end.

---

## Where we are today (the baseline this builds on)

**Rig:** 15 bones (`Bone_1..15`), GoldenEye-derived — pelvis · chest · head ·
(shoulder/elbow/hand)×2 · (hip/knee/foot)×2. All 44 characters share it. No fingers,
one spine joint, no separate neck, no facial bones, no twist/helper bones, no toes.

**Animation:** `engine/src/skeletal/layers.rs` — a `LayeredAnimator` pose stack that is
the *right* architecture: `LocomotionBlendLayer` (1D idle→walk→jog→run blend),
`ClipOverlayLayer` (masked upper-body authored aim/fire pose), `AimOffsetLayer` (chest
swing so the real barrel points at the player), `AdditiveDecayLayer` (recoil). Firing is
a **timer**, not a clip, so hunters run-and-gun. Movement is **decoupled** from animation
(the nav drives position; the model is "purely visual"). 2-bone/foregrip IK was tried and
**scrapped** (fought the authored clips). Death = canned clips + fade; hit reactions off.

**AI:** hand-rolled FSM in `game/src/enemy.rs` (`Idle · Search · Investigate · Alert ·
Chase · Attack · Cooldown · TakeCover · Peek`), grid A* + LOS nav
(`engine/src/sim/nav.rs`), perception cone + search sweep, a difficulty dial that scales
every behavior, squad-alert broadcast + position-nudge separation, reactive aim-dodge,
flanking, cover/peek, grenade flush, footstep/gunshot noise. Emergent-jank shaken out
with the headless AI lab (`game/src/world/ai_testbed.rs` — `TestArena` + `JankMonitor`).

The point of the doc: the **AI/behavior** layer is unbounded by the rig; the **animation**
layer is only *partly* gated by it — less than you'd expect.

---

## Does the 15-bone rig hold us back?

**What the rig does NOT prevent** (all work fine on 15 bones): motion matching, foot IK,
root motion, look-at, full-body IK, additive layers, pose/stride warping, ragdoll. ~90% of
modern locomotion + combat animation needs no rig change.

**What it genuinely caps:**
- **One spine joint** → limited torso twist / lean-into-cover / weight shift / breathing;
  aim reads stiff at extremes (already observed). *Single biggest limiter for "aliveness."*
- **No separate neck** → no independent head/eye tracking. Enemies *looking at* what they're
  thinking about is one of the cheapest, highest-impact AAA "alive" cues, and we can't do it
  cleanly without a neck joint.
- **No fingers** → weapons attach rigidly; no trigger discipline, reloads, gestures, grabbing
  cover edges / ladders.
- **No facial bones / blendshapes** → no expression, blink, lip-sync.
- **No twist / helper bones** → forearm "candy-wrapper" + shoulder collapse (deformation quality).
- **No toe/ball** → feet can't roll (foot IK looks stiffer).

**Upgrade cost / pragmatic path:** adding bones means re-rig + re-skin **44 characters** +
retarget every clip + author correctives — expensive. So DON'T jump to a full facial rig.
The high-ROI move is a *handful* of targeted bones — **2 spine + neck + clavicles + forearm
twist** — or **procedural helper joints** driven in code (twist bones need no new art at all).
That alone unlocks smooth aim, head-tracking, and much better deformation. The rig is the
*least* of our constraints.

---

## Animation: what modern AAA has that we don't

| Technique | What it buys | Our state | Rig change? |
|---|---|---|---|
| **Motion matching** | Natural starts/stops/turns/plants from a pose DB, no state machine | 1D blend; instant turns, no turn-in-place | No |
| **Root motion** | Movement *driven by* animation → weighty, no foot slide | Code moves nav pos; anim purely visual → sliding | No |
| **Foot IK + ground adapt** | Feet plant on slopes/stairs, no float/clip | Deferred; feet float on treads | No (helps w/ toe) |
| **Full-body IK / look-at** | Head/eye tracking, hands to cover/weapon/world | 2-bone IK tried + scrapped | Neck for head-track |
| **Physics hit reactions / active ragdoll** (Euphoria-style) | Unique stagger / wound-grab / balance-catch per hit; ragdoll death that blends back | Canned death clips + fade; flinches off | No |
| **Pose / stride warping** | A run clip bends to any turn radius without sliding | None (our flank curves would benefit) | No |
| **Secondary motion** | Cloth / hair / jiggle, aim sway, breathing | None | Some (jiggle bones) |
| **Facial + lip-sync** | Expression, barks with mouth movement | None | Yes (facial rig) |

**Highest feel-impact for THIS game:** **root motion + foot IK** (kills the sliding/floating
that reads as "gamey," especially on our stairs/platforms), and **physics-based hit reactions**
(we turned canned flinches off because they looked bad mid-fight — physics ones wouldn't, and
they'd make combat land).

---

## AI: what modern AAA has that we don't

Our FSM + grid nav is the classic architecture; the modern stack layers on top.

1. **Decision architecture beyond FSM.** Behavior Trees (modular, designer-friendly),
   **Utility AI** (score every option by context → emergent, situation-right choices), or
   **GOAP** (F.E.A.R.'s planner). *Irony:* F.E.A.R.'s celebrated AI — flushing, flanking,
   suppressing while others advance, using cover — is **exactly the six behaviors we
   hand-coded**. Utility/GOAP makes those **emergent + composable** instead of special-cased
   FSM branches, so a 7th behavior isn't "thread another flag through `Enemy::update`."

2. **Local avoidance (RVO / ORCA).** The modern answer to the crowd jank we fought (the
   `separate_enemies` position-nudge + anti-grind hold). Agents steer smoothly around each
   other and the player instead of oscillating. **Would retire `separate_enemies` entirely.**

3. **Spatial query system (EQS / TQS).** Our cover sampler (`sample_cover_cell`) is a
   hand-rolled mini-EQS. A general *scored* spatial query ("best cell: no LOS from player, LOS
   to player nearby, near an ally, far from a grenade, low exposure") would unify
   cover / peek / flank / grenade-spot / search into one designer-tunable system.

4. **Richer perception + memory.** Sound *propagation* with occlusion (not just a radius),
   light/shadow-based stealth, a proper last-known-position **belief model** with decay +
   coordinated spreading search, priority-ranked stimuli. Ours (cone+sweep+LOS + footstep/gunshot
   noise) is solid GoldenEye-plus but doesn't model *why* it lost you.

5. **NavMesh + off-mesh links.** Grid → navmesh with **jump / vault / mantle / climb / ladder**
   links and cover-to-cover smart links — what lets enemies *traverse* a base like the player.
   Big deal for a hide-and-seek base.

6. **A squad brain + roles.** Explicit role allocation (suppressor / flanker / grenadier),
   "covering fire while I move" hand-offs, callouts synced to actions. We have a broadcast +
   separation; a coordinator would slot + sequence tactics.

7. **An AI Director** (Left 4 Dead) — a meta-AI pacing spawns / intensity to player stress.
   *Extremely* on-theme for a hunt/hide game.

8. **Telegraphs & micro-reactions.** Grenade wind-up ("Frag out!"), flinch on near-miss,
   heads-down under suppression. Cheap layers that read as intelligence.

9. **Frontier (art of the possible):** ML/learned motion matching (Ubisoft ships it for
   compression), neural animation synthesis, experimental LLM-driven behavior/barks. Where
   "potential futures" points, not standard AAA yet.

---

## Bang-for-buck ranking (for *this* game)

1. **Local avoidance (RVO)** — retires a whole class of jank; medium effort, huge movement-quality win. *No rig change.*
2. **Neck + 2 spine joints (or procedural helper bones) + look-at** — cheapest "aliveness" upgrade; head-tracking enemies feel dramatically smarter. *Small, targeted rig add.*
3. **Root motion + foot IK** — kills sliding/floating, especially on stairs. *No rig change.*
4. **Utility AI or GOAP decision layer** — makes the six behaviors emergent, everything after cheaper. *No rig change.*
5. **Physics hit reactions / ragdoll** — the combat *feel* payoff. *No rig change.*
6. **NavMesh + traversal links + AI Director** — the "big base you actually hunt through" vision. *No rig change.*

Only #2 touches the rig, and it's a small targeted addition — not a re-rig. The biggest levers
are the **decision architecture** (FSM → Utility/GOAP) and **movement** (RVO + root motion),
both of which our layered-pose + nav foundation is well-positioned to grow into.

---

## How to approach the work

- **New feature branch, one item at a time.** Each of the six is large and independent. Do not
  do them on `main`; land each behind the difficulty dial or a feature flag so the current,
  live-confirmed behavior is never regressed.
- **Lean on the headless AI lab** (`game/src/world/ai_testbed.rs`). Every one of these should be
  developed against `TestArena` scenarios + `JankMonitor` invariants first — the lab already
  caught four emergent defects. Add scenarios that assert the new capability before/while building it.
- **Keep the difficulty-0 baseline untouched.** The whole AI scales off one dial
  (`DiffParams` / `AiTuning`); new behaviors should be 0 at difficulty 0, like the current six.
- **Suggested first slice:** RVO local avoidance (retire `separate_enemies`) — self-contained,
  high-impact, testable in the lab, and it removes the crowd-jank foundation the other movement
  work would otherwise inherit.

> Related: [DESIGN_IDEAS.md](DESIGN_IDEAS.md) (game-direction pillars),
> [DESIGN_BASE_SCALE.md](DESIGN_BASE_SCALE.md) (why a big base), and the memory notes
> `perfect-dark-ai` / `ai-testbed` (what's built + the lab).
