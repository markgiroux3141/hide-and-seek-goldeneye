# Handoff — Perfect Dark enemy omniscience

**DONE 2026-08-17**, working tree on `main` (there is no PD feature branch — an earlier
draft of this file claimed `feat/pd-asset-import`, which does not exist). 306 workspace tests
green, release built, still **nothing committed**: 86 changed/untracked files covering the
whole PD track.

Full write-up: `DESIGN_PD_SIMULANT_AI.md` §10 (what PD does + what we ported) and §11 (the
roadmap for everything else that separates our AI from PD's).

---

## What shipped

PD-lab hunters always know where the player is and walk to the live position. They no longer
lose you and fan-out search.

| File | Change |
|---|---|
| `game/src/enemy.rs` | `omniscient` field, `set_omniscient` / `is_omniscient`, **`known_player_pos`** (the one knowledge accessor), `lose_contact`, and the `acquired` gate on the three blind states |
| `game/src/world/mod.rs` | `pd_omniscience: bool` (default ON) + `set_pd_omniscience` / `pd_omniscience` |
| `game/src/world/lifecycle.rs` | applies it per step, gated on `inst.pdsim.is_some()` |
| `game/src/world/ai_testbed.rs` | 3 scenarios (see below) |

The design in one line: **omniscience is a knowledge policy, not a rewrite.** Every movement
consumer reads `known_player_pos(player_feet)`, which returns the live position for an
omniscient hunter and today's `last_known` otherwise.

Three things that were not obvious and cost time:

1. **The belief must be exempted from the lapse.** `alert_served` re-arms once `since_seen`
   passes `ENGAGE_MEMORY`. Without an omniscience exemption a hunter that can't see you
   re-serves its reaction delay every tick and stands in `Alert` forever.
2. **`Search` / `Investigate` need no special case in the utility layer.** Both score only on
   `!engaged`, and omniscient ⇒ permanently engaged, so they fall to zero on their own. The
   legacy FSM does need the explicit `acquired = perceived || omniscient` gate.
3. **Our standoff band already was PD's dist config.** `standoff ± STANDOFF_HYST` with
   advance / hold / `back_off` maps one-for-one onto PD's ADVANCE / OK / BACKUP, and because
   `Attack` is LOS-gated while `Chase` is not, PD's key line (`OK && !insight → ADVANCE`) was
   already emergent. Only the knowledge rule was actually missing.

## Verified against the decomp, not guessed

The previous version of this handoff flagged the PD movement side as unread. It has been read:

- `bot_choose_general_target` (`bot.c:1589`) refreshes `chrdistances[]` unconditionally and
  falls through to *"Use closest out of sight chr"* for **every** tier when nobody is visible.
  Only cloak / `CHRCFLAG_HIDDEN` removes a target. **Omniscience is unconditional, not
  alert-gated** — that was the open question.
- `botcmd_tick_dist_mode` (`botcmd.c:39`) buckets distance against a per-weapon `{min,max}`
  band and re-issues `chr_go_to_prop(chr, targetprop, GOPOSFLAG_RUN)` — the target's **live**
  coordinates. There is no last-known position anywhere in the simulant path.

## Testing it

```powershell
cd native
$env:PD_LAB = 1
$env:PD_LAB_COUNT = 6   # optional — defaults to PLAYTEST_WAVE_SIZE (4); 1 = duel mode
cargo run --release
```

`G` HUNT, `I` invincible, **`N` invisible — the direct test**: an invisible player cannot be
perceived, so an omniscient hunter should still walk right up to you. `=` / `-` sweep the tier.
Lab boots at NormalSim (`pd_lab::LAB_TIER`).

Headless: `cargo test --release -p game omniscien -- --nocapture`

- `pd_omniscient_hunter_finds_a_player_it_cannot_see` — hunter in a corner with a wall between
  it and the player paths through a 1.5 m gap and closes to 0.74 m, never entering
  Search/Investigate.
- `pd_omniscience_kill_switch_restores_the_search` — same arena, flag off, falls back to the sweep.
- `a_goldeneye_hunter_is_never_omniscient` — the normal game is untouched.

## Still open (see §11 for the full ordered list)

**The free-for-all seam is the top item.** `EnemyInstance::pd_target` is populated and
simulants already shoot each other, but `Enemy::update` takes `player_feet`, so a simulant that
targets a packmate shoots it from wherever it stands instead of hunting it. Threading an
`EngageTarget { pos, id }` through in place of `player_feet` is the single largest remaining
structural difference from PD.

## Two traps this track has hit repeatedly

1. **Verify on the GPU, not just headlessly** — see the `pd-in-game-screenshots` memory. (Not
   yet done for this change: it is AI-only and has no rendering path, but the walk-up behaviour
   has not been eyeballed live.)
2. **Read the log** — `hunter N shot hunter M`, `HUNTER DOWN`, `PD injury table`,
   `PD SIMULANT LAB: ...` all print at INFO.
