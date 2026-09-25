# 05 — Beyond the editor: engine, AI, the game, and the verdict

## The engine, briefly

**Render.** Forward, single pass, opaque + alpha-test only. Ambient + up to 32 point
lights, four of which cast real omnidirectional shadows via an R32F cube array rendered
second-depth with 5-tap PCF. That shadow path is better than most hobby engines manage.
Missing, and each one is a cheap-to-medium addition: fog (zero hits in 14 shaders), a
skybox, a transparent pass, vertex colour on level meshes (`TexVertex.color` exists and
is written white; the shader never reads it), MSAA, any post. No atlas: one texture per
(theme, zone) bind group, so draw calls ≈ regions × distinct (theme, zone) pairs. The
`shade()` function is copy-pasted into four WGSL files. A `naga` test compiles every
shader, which is the right habit.

**Physics.** Rapier. Level collision is the fold trimesh, so slopes and curves collide
for free if anything could author them. Player is a kinematic capsule with 1 WT autostep
and a 50° slope limit, which is what caps stair steepness at 45°.

**Nav.** A 0.25 m voxel grid baked once per hunt from *brush membership*, not from the
mesh. Doors, ramps and vents are post-bake overlays so nothing re-bakes mid-hunt. A*
with connected-component labels for O(1) unreachability. Two polygon-navmesh attempts
failed because the grid is volumetric and the trimesh is surface-based; the grid is
settled. Its permanent tax: every new geometric feature must be re-expressed as boxes.

**Skeletal.** A layer stack (locomotion blend, upper-body clip overlay, chest aim
offset, head look-at, recoil, foot IK, two-bone IK) on a shared 15-bone rig that both
GoldenEye and Perfect Dark bodies fit. Barrel axes are measured per clip from the
asset, not assumed; a test disproved the assumption once.

## The AI, briefly

Three co-resident models, all live and switchable: the utility-scored FSM (`enemy.rs`,
11 states with hysteresis), the Perfect Dark simulant (`pdsim/`: difficulty and
personality as orthogonal axes, aim convergence, four distance bands, and the trigger
driven by where the barrel actually points rather than a dice roll), and the raw FSM as
a kill switch. Perception is cone + range + LOS + hearing + decaying last-known
position, with omniscience as a separate *knowledge* override — a distinction most
projects blur. ORCA avoidance, wall clearance, stuck detection, squad noise alerts,
cover/peek sampling. Mature. What does not exist: any second enemy *type*.

## The game versus `DESIGN.md`

`DESIGN.md` describes a loop — build under a budget and timer, hide, survive waves that
each break a lazy building strategy, patch between waves, scavenge, escalate — and names
the sealed-box problem as the thing to solve first. Measured against it:

| Design element | Status |
|---|---|
| Build phase with budget/timer | Not built. Editing is unconstrained and BUILD-only. |
| Hunt phase | Built, as a PD deathmatch: respawns both sides, first to 10. |
| Patch phase (build during/between waves) | Not built. 38 `mode != Build` guards forbid it. |
| Economy | Session-only wallet, kill bounty, spendable on guns only. No build costs. |
| Waves / escalation / six archetypes | Not built. One archetype, a 0..10 dial. |
| Destructible geometry | Not built. Props are destructible; CSG is not. |
| Breakable doors (the archetype delay element) | Built, then **disabled 2026-07-16**; the code sits dead. |
| Misdirection (noise, darkness, decoys) | Partial: real noise pings, authored lights, player-only vents/ladders. |
| Traversability validation | Built (NAV tab, probes). |
| Serialisable levels for sharing/AI authoring | Built (format v4, levelgen, skill). No sharing UI. |

Roughly: milestone one of the vertical slice is done and exceeded (build → hunt →
hunters pathfind over what you built), and the loop past that has not started. The
project drifted toward what was measurable and portable — AI fidelity, nav correctness,
asset decoding, editor tools — and away from the unproven core loop. `DESIGN_IDEAS.md`
already has the keystone fix (crates enemies path toward, add-only building after the
first hunt, wave ends when the base is cleared). None of it is hard engine work; most
of it is `world/` logic and the editor exposing a few new volumes and props.

This matters for the editor roadmap in one way: **the editor's flexibility is only
"creative" in play if the hunt rewards structure.** A deathmatch on a big base plays the
same on a small one. The items in roadmap §5 (one-way drops, trigger volumes, breakable
panels, dark zones) are where editor flexibility and game design meet.

## Is it slop, or does it have merit?

Slop has a signature: tests that assert nothing, docs that describe code that does not
exist, five names for one concept, dead code nobody labelled, plans that claim success
without measurement, and an absence of "we tried X and it failed". Checked against that:

- **Tests.** 844, and the ones sampled are behavioural: a raycast-to-collider round trip
  with no GPU; a hunter that walks to the gun, not the player; a GoldenEye body playing
  Perfect Dark clips (which disproved the comment above it); an ORCA pack funnelling
  through a doorway; two benchmarks and a cost assertion. One debugging probe left in.
  Not slop.
- **Docs.** Status lines are specific ("BUILT + green, awaiting playtest", "Nothing here
  is built", "PARKED"). Plans are corrected in place with sections titled "what the
  build learned this document had wrong". Two reverted navmesh attempts are documented
  with root causes and the acceptance harness that was missing. `DESIGN.md` is stale and
  13 docs are orphaned; that is rot, not fabrication. Not slop.
- **Naming and structure.** Conventions hold (`arm_/cancel_/confirm_/adjust_/update_*_preview`,
  WT vs metres always annotated). The god structs are real debt, honestly diagnosed in
  two refactor docs. Dead code is labelled with a date and a decision. Not slop.
- **Measurement.** Constants cite the bug they fixed and often the number. The memory
  notes record "five measurements that each overturned the plan". Barrel axes, PD scale,
  spawn dilution, nav islands were all *measured* rather than assumed. Not slop.
- **Depth.** A working BSP CSG editor with incremental rebake, a real shadow path, a
  skeletal layer stack with IK, ORCA, a ROM-format decoder verified by shape, a port of
  another game's bot model to the line. None of that is achievable by pattern-matching.

So: **real merit**, on three fronts. As an indie CSG editor and engine it is past where
most solo projects ever get. As an AI lab with instrumentation it is unusually rigorous.
As a record of how to run an AI-assisted project — measure, write down what was wrong,
keep the reasons in the code — it is exemplary.

The caveats are also real. Seven weeks, one author account, 366k lines inserted and 25k
deleted: nobody has read all of this, and the app layer proves it (7.1k lines, zero
tests, one 2,870-line function). The game the design doc describes is not here; a very
good GoldenEye/Perfect Dark deathmatch with a great editor is. The AI fidelity
investment (50 PD clips, 44+6 bodies, a simulant port) is enormous relative to the
absent enemy *design*. And the ripped assets make it unshippable publicly as it stands.

If I had to compress it: this is a serious engine and editor built by someone who
learned the discipline of "don't trust the plan, measure it" and wrote that discipline
into the repo. Its weakest point is not quality but focus. The next month decides
whether it becomes the game in `DESIGN.md` or stays a superb toy.
