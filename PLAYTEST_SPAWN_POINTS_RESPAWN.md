# Playtest brief — the deathmatch loop

> Built 2026-08-18 on `feat/ai-mode-switch` (on top of the unmerged `AI=pd|ours` work).
> 354 tests green; `cargo build --release` done. **Not committed** — say the word.
>
> Implements `HANDOFF_SPAWN_POINTS_RESPAWN.md` in its four milestones. The four decisions
> you took: **one shared pool**, **keep the wave-size setting**, **score limit**, **2 s
> auto-respawn both sides**.

## Run it

```
native/target/release/build-and-hide.exe
```

Optional env: `SCORE_LIMIT=n` (kills to win; `0` = endless), plus the existing
`AI=pd|ours`, `ARSENAL=`, `BODIES=`, `PD_LAB=1`.

## What changed, in the order you'll meet it

**1. Authoring.** `O` opens the object panel; the `◄ ►` arrows by the title now cycle
through a third tab, **SPAWNS**. `+ Place Spawn Point` arms it, then click a floor: a pad
drops **facing the way the camera is looking**, and placement disarms so the next click
grabs its gizmo. Move it with the arrows, press `T` to switch to **Rotate** to re-aim its
facing (the little nub out front shows where a body will look), `Del` deletes it. Undo and
save/load both cover pads — they ride the level file's existing `entities` list, so the
format version did not move and your saved slots still load.

**2. At `G`, everyone comes from the pool.** The player included — *not* from where the
fly-cam was. Facing comes from the pad.

**3. Everyone respawns 2 s after dying,** from the same pool. Your death screen now says
`YOU DIED / RESPAWNING`; `R` just skips the remaining wait. `G` is still how you leave.

**4. Scoreboard, top-right:** `YOU k-d SIMS k-d / limit`. First side to the limit ends the
round with a `YOU WIN` / `SIMS WIN` screen; `R` starts the next one at 0–0.

## The five things I actually want your eyes on

1. **Does the pack arriving from scattered pads read as better or worse than one door?**
   This is the biggest feel change. Author 4–6 pads spread through a real base and compare
   against 1 pad (which reproduces the old single-ingress cluster exactly).
2. **`AI=pd` + respawn — is it survivable?** The handoff's own worry: an omniscient pack
   that respawns forever is pressure with no lull. PD's spawn filter is what's supposed to
   make it survivable (nothing spawns within 10 m of you, or anywhere you can see, if a
   clear pad exists). **A/B it against `AI=ours`.** If `AI=pd` is unplayable, the honest
   knobs are the wave size and `RESPAWN_DELAY`, not the spawn rule.
3. **The corpse pop.** I chose PD's 2 s literally, and a corpse's fade takes longer than
   that — so a body still fading **vanishes** at the moment its slot is reused. Watch a
   kill at close range and tell me whether it reads badly. One-line fix if so: gate the
   respawn on the fade completing (costs ~1.5 s more absence), or lengthen `RESPAWN_DELAY`.
4. **Where you respawn.** Twice now I've had the rule pick correctly in tests but only you
   can say whether it *feels* fair — do you ever come back somewhere immediately lethal, or
   somewhere so far away the round goes quiet?
5. **Pad authoring ergonomics.** The pick box is the pad's floor square plus ~0.6 m of
   height, because the marker is nearly flat and a shallow fly-cam angle otherwise can't
   click it. Is selecting a pad fiddly?

## Where PD's rule came from, and the one place I substituted

`world/spawn.rs` ports `player_choose_spawn_location` (`reference/pd-decomp/src/game/player.c:225`)
— the function **both** sides go through (`bot.c:288`, `player.c:528`, over the one
`g_SpawnPoints` list, which is why one shared pool was the faithful answer to your first
decision). It is neither random nor "farthest from the nearest enemy": it scores each pad
by distance to the nearest enemy, classifies it bad/very-bad by exposure, fills a
**4-slot shortlist in three passes** from a random start, and then **rolls between the
four**. PD's own thresholds port exactly (1 PD unit = 1 cm, so `1000*1000` → 10 m and
`200*200` → 2 m).

**The substitution, stated plainly:** PD's exposure tests are *room*-based (enemy in the
pad's room, room on an enemy's screen, enemy in a neighbouring room). This game has no
rooms — a whole base is often one connected CSG region — so a literal port would classify
every pad identically and collapse the filter. Very-bad became "an enemy has clear line of
sight to the pad"; bad became "an enemy within 12 m". Everything else is PD's.

## Four things I found that contradicted the plan on the page

1. **The undo trap was already closed.** The handoff warned that authored pads must join
   the undo snapshot "or placement silently won't undo". They didn't need to: `history.rs`
   already snapshots the whole authored entity set, so riding the entity layer got undo for
   free. No change to `history.rs` at all.
2. **The two tests that assert the fixed marker didn't need rewriting.** The handoff
   expected `world/tests.rs:144` and `:189` to need rework. They pass untouched — because
   an empty pad pool falls back to the old fixed marker, which turned out to be *PD's own
   guard* (`if (g_NumSpawnPoints > 0)`, `playerreset.c:398`) rather than a workaround. That
   same fallback is what keeps the AI lab's arenas and the levelgen harness working
   unchanged, which the handoff listed as a separate trap to solve.
3. **The spawn roll had to get its own RNG stream.** Folding it into the existing
   `char_rng` shifted every downstream combat draw and **failed a PD lab scenario**
   (`pd_mode_tops_up_a_partial_clip_once_you_are_out_of_sight`) without changing the
   behaviour that test measures — its magazine lands exactly on `clip/2`, and whether one
   more round leaks out after the visibility flip decided the result. `spawn_rng` is
   separate for that reason, and combat outcomes are now byte-identical to before
   regardless of how many bodies have entered the level.
4. **The HUD font would have shipped garbled text.** `CHARSET` contained only the letters
   the old strings needed, and `layout_text` *silently drops* an unatlased char rather than
   drawing a box — so `SIMS WIN` rendered as `SIS IN` and the score separator `-` vanished.
   Added `M`, `W`, `-`, plus a test that asserts every string the HUD prints is fully
   atlased, so the next new string can't lose letters quietly.

## Known gaps (deliberate, not oversights)

* **Splash kills credit nobody.** Neither `Projectile` nor `Mine` records an owner, so an
  explosive kill scores the victim's death and no kill (`Killer::Unattributed`, asserted by
  a test so it stays a known shape). Threading an owner through `combat::explosives` would
  close it — out of scope for this pass; say if you want it.
* **The world freezes during your 2 s death beat.** Hunters don't keep fighting each other
  while you're down. That matches the beat you chose; letting the sim run on through a dead
  player is a larger change to the hunter loop.
* **Hunter-on-hunter kills don't advance the pack's side total** (only that slot's own
  tally), or a free-for-all pack would win the round against itself without touching you.

## Files

| what | where |
|---|---|
| PD's spawn rule, ported + cited | `native/crates/game/src/world/spawn.rs` |
| pad authoring (place/delete/marker/query) | `native/crates/game/src/world/tools/spawn_point.rs` |
| respawn loop (both sides, slot reuse) | `native/crates/game/src/world/respawn.rs` |
| scores + win condition | `native/crates/game/src/world/scoreboard.rs` |
| the `SpawnPoint` component | `native/crates/game/src/ecs/components.rs` |
| pool build + pad choice at G | `native/crates/game/src/world/hunt.rs` `prepare_spawn` / `choose_spawn_pad` |
| player entry + per-hunter pads | `native/crates/game/src/world/lifecycle.rs` `enter_player` / `hunter_entry` |
| scoreboard + end screens | `native/crates/game/src/hud/mod.rs` |
