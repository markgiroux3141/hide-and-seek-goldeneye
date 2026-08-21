//! Spawn-pad selection — Perfect Dark's rule, ported.
//!
//! The pool is authored in BUILD (`world::tools::spawn_point`); this module decides
//! *which* pad a body entering the level takes. It is a port of
//! `player_choose_spawn_location` (`reference/pd-decomp/src/game/player.c:225`), the
//! function **both** sides of a PD match go through: `bot_spawn` (`bot.c:288`) and
//! `player_start_new_life` (`player.c:528`) both call `scenario_choose_spawn_location`,
//! which falls through to `player_choose_general_spawn_location` over the single
//! `g_SpawnPoints` list. So one rule serves the player and the simulants, and the
//! repo's standing preference — port the rule and cite the line rather than invent a
//! good-sounding heuristic — is satisfied by construction.
//!
//! ## What PD actually does (and what it notably does *not*)
//!
//! It is neither "pick a random pad" nor "pick the pad farthest from the nearest
//! enemy". It is a **filtered shortlist of four, then a random pick from it**:
//!
//! 1. Score every pad by the squared distance to its **nearest enemy** — players and
//!    bots alike — and classify it *bad* / *very bad* by how exposed it is.
//! 2. Fill a 4-slot shortlist in three passes, each starting at a **random pad index**
//!    and walking the list circularly: first `>10 m && !bad`, then `>10 m && !very
//!    bad`, then whatever is left in descending-distance order (bailing once the best
//!    remaining pad has an enemy within 2 m, provided the shortlist isn't empty).
//! 3. Pick uniformly from the shortlist. Only an empty shortlist falls back to a
//!    uniform pick over all pads.
//!
//! The random pick over a *filtered* shortlist is the load-bearing part: a
//! deterministic "farthest pad" rule makes spawns predictable enough to camp, and a
//! plain random pick drops you in someone's crosshairs. Keeping four candidates and
//! rolling between them is what gives PD deathmatch its spawns.
//!
//! ## The one substitution, stated plainly
//!
//! PD's exposure tests are **room**-based: a pad is *very bad* if an enemy stands in
//! its room or that room is on an enemy's screen (`bg_room_is_on_player_screen`), and
//! *bad* if an enemy is in a neighbouring room (`bg_room_get_neighbours`). This game
//! has no rooms — a whole base is often one connected CSG region — so a literal port
//! of those two tests would classify every pad identically and collapse the filter.
//! They are substituted with the closest primitives this engine has:
//!
//! | PD test | here |
//! |---|---|
//! | enemy in the pad's room / room on an enemy's screen → *very bad* | enemy has clear line of sight to the pad |
//! | enemy in a neighbouring room → *bad* | enemy within [`NEAR_PAD_DIST`] of the pad |
//!
//! Everything else is PD's, at PD's numbers: 1 PD unit is 1 cm, so its `1000 * 1000`
//! and `200 * 200` squared thresholds are 10 m and 2 m here, exactly.
//!
//! ## The one deliberate deviation: the desperate pass does not dilute
//!
//! PD's third pass runs `while (sllen < 4)` — it **tops up** a shortlist that already
//! holds good pads with whatever is left, in descending-distance order. That is safe in
//! Perfect Dark and ruinous here, and the difference is pool size, not rule quality:
//! `player_choose_spawn_location` is written against `pads[24]` and a PD multiplayer
//! level authors most of that, so passes 1–2 normally fill all four slots and the
//! desperate pass never runs at all.
//!
//! Our levels author a handful. With four pads — the real shipping level at the time
//! this was written — the arithmetic collapses: pass 1 finds the one pad clear of the
//! pack, the desperate pass tops the shortlist up with the other three *because they are
//! all that is left*, and the final roll is uniform over every pad in the level. The
//! filter computes a perfectly good answer and then throws it away three times out of
//! four. That is the "I respawned in the middle of them" report.
//!
//! So the desperate pass runs **only when the shortlist is empty** — which is what it is
//! for: a level where nothing is safe still has to spawn somebody. Whenever any pad
//! passes the filter, only filtered pads are rolled between. Everything upstream of that
//! line is unchanged, and with a PD-sized pool the two behave identically, because there
//! the desperate pass was already unreachable.
//!
//! PD also validates each candidate with `chr_adjust_pos_for_spawn` (does the body
//! fit?) and only shortlists it if that succeeds. That check happens earlier here —
//! `World::prepare_spawn` resolves every authored pad to a standable nav cell when the
//! pool is built — so by the time this module sees a pad it already fits.

use glam::Vec3;

use engine::sim::physics::PhysicsWorld;

/// One authored spawn pad, resolved to a standable point. `yaw` is the authored
/// facing: PD's pads carry a `look` vector and `player_choose_spawn_location` hands
/// back `atan2f(pad.look.x, pad.look.z)` as the spawn angle (`player.c:355`), so
/// whoever takes this pad enters looking the way it was aimed in BUILD.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpawnPad {
    pub pos: Vec3,
    pub yaw: f32,
}

/// Which body is entering the level. This is PD's `prop` parameter to
/// `player_choose_spawn_location`, which exists for exactly one reason: the body being
/// spawned must not score against its own candidate pads (`prop != g_Vars.players[i]->prop`,
/// `player.c:266`). Here it names the caller so `World::choose_spawn_pad` can leave it
/// out of the occupant list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Spawning {
    Player,
    /// A hunter roster slot. Stable across respawn — a hunter respawns *into its own
    /// slot* (see `World::respawn_hunter`), because the index is load-bearing for the
    /// PD target ids, the ORCA agent tags, squad alert and every AI-lab metric.
    Hunter(usize),
}

/// Shortlist capacity — PD's `slpositions[4]` (`player.c:242`). Four candidates is
/// what makes the final pick a roll rather than a formula.
pub(crate) const SHORTLIST: usize = 4;

/// Minimum distance (m) from the nearest enemy for a pad to be shortlisted in the
/// first two passes — PD's `padsqdists[p] > 1000 * 1000` (`player.c:329`), 1000 PD
/// units = 10 m.
const MIN_ENEMY_DIST: f32 = 10.0;

/// The third pass's bail-out: once the best remaining pad has an enemy closer than
/// this, stop filling (provided something is already shortlisted) — PD's
/// `if (!(bestsqdist > 200 * 200) && sllen != 0)` (`player.c:446`), 200 units = 2 m.
const DESPERATE_DIST: f32 = 2.0;

/// Stand-in for PD's "an enemy is in a neighbouring room" *bad* test. Sized at roughly
/// two of this game's rooms, so a pad with someone in the next space along is deferred
/// to the second pass rather than taken outright. Larger than [`MIN_ENEMY_DIST`] on
/// purpose: as in PD, a pad can clear the distance gate and still be *bad*.
const NEAR_PAD_DIST: f32 = 12.0;

/// Per-pad classification, mirroring PD's parallel `verybadpads` / `badpads` /
/// `padsqdists` arrays. `dist_sq` is `None` once the pad has been consumed by a pass
/// (PD sets `padsqdists[p] = -1.0f` for the same reason: no pad is shortlisted twice).
struct PadInfo {
    dist_sq: Option<f32>,
    bad: bool,
    very_bad: bool,
}

/// The chosen pad plus what it took to get there — the second half exists because
/// "I spawned in the middle of them" is otherwise unfalsifiable from a play session.
/// [`super::World::choose_spawn_pad`] logs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpawnChoice {
    /// Index into the pad pool the caller passed in.
    pub pad: usize,
    /// Metres from that pad to the nearest body already in the level, or infinity when
    /// the level is empty (match start). This is the number the complaint is about.
    pub enemy_dist: f32,
    /// Which pass shortlisted the winner: `1` clear and not bad, `2` clear but bad,
    /// `3` the desperate pass (**nothing** was safe), `0` the empty-pool fallback.
    /// A `3` in the log means the level, not the rule, is out of good answers.
    pub pass: u8,
}

/// xorshift64 → an index in `[0, n)`, run on the caller's RNG state so spawn choice is
/// seeded and reproducible in tests. Same generator as `World::rand_below`.
fn rand_below(state: &mut u64, n: usize) -> usize {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x % n.max(1) as u64) as usize
}

/// Choose a pad for a body entering the level, given everyone already in it
/// (`occupants`, feet positions — the caller excludes the spawning body itself).
/// Returns the **index** into `pads`, so the caller can also track how many bodies
/// have taken each pad. `rng` is the caller's xorshift state; `physics` serves the
/// line-of-sight test. `None` only when `pads` is empty.
///
/// PD filters `occupants` to *enemies* (`chr_compare_teams(..., COMPARE_ENEMIES)`),
/// which in a teamless deathmatch — the mode this game's `AI=pd` hunters run — is
/// everyone. The caller passes everyone for that reason, and because it is also what
/// keeps a wave from stacking: a packmate already on a pad pushes the next body
/// elsewhere.
///
/// See the module docs for the provenance of every threshold.
pub(crate) fn choose_spawn(
    pads: &[SpawnPad],
    occupants: &[Vec3],
    physics: &mut PhysicsWorld,
    rng: &mut u64,
) -> Option<SpawnChoice> {
    if pads.is_empty() {
        return None;
    }

    // ── Classify every pad against every enemy already in the level. ──
    let min_sq = MIN_ENEMY_DIST * MIN_ENEMY_DIST;
    let near_sq = NEAR_PAD_DIST * NEAR_PAD_DIST;
    let mut info: Vec<PadInfo> = pads
        .iter()
        .map(|pad| {
            let mut best_sq = f32::INFINITY;
            let mut bad = false;
            let mut very_bad = false;
            for &o in occupants {
                let d_sq = o.distance_squared(pad.pos);
                best_sq = best_sq.min(d_sq);
                // PD: pad's room is on an enemy's screen, or an enemy stands in it.
                if crate::enemy::perception_los(physics, o, pad.pos) {
                    very_bad = true;
                }
                // PD: very bad, or an enemy is in a neighbouring room.
                if very_bad || d_sq < near_sq {
                    bad = true;
                }
            }
            PadInfo { dist_sq: Some(best_sq), bad, very_bad }
        })
        .collect();

    // Nearest-occupant distance per pad, kept aside for the diagnostic — `info` consumes
    // `dist_sq` as the passes claim pads, so it can't be read back afterwards.
    let nearest: Vec<f32> = info
        .iter()
        .map(|p| p.dist_sq.map_or(f32::INFINITY, f32::sqrt))
        .collect();

    // `(pad index, which pass took it)`.
    let mut shortlist: Vec<(usize, u8)> = Vec::with_capacity(SHORTLIST);

    // ── Pass 1: clear of everyone (>10 m) and not bad. ──
    // PD walks circularly from a random index so that with more qualifying pads than
    // shortlist slots it isn't always the same four — the randomisation is in the
    // *start*, not just the final pick.
    circular_fill(&mut shortlist, &mut info, rng, 1, |p| {
        p.dist_sq.is_some_and(|d| d > min_sq) && !p.bad
    });

    // ── Pass 2: still >10 m, now accepting *bad* pads but not *very bad* ones. ──
    circular_fill(&mut shortlist, &mut info, rng, 2, |p| {
        p.dist_sq.is_some_and(|d| d > min_sq) && !p.very_bad
    });

    // ── Pass 3: desperate — take what's left, farthest first. ──
    // No distance gate and no exposure filter: a level where every pad is watched still
    // has to spawn somebody.
    //
    // **Only when nothing survived the filter.** PD tops up a partly-filled shortlist
    // here; with a pool barely larger than the shortlist that dissolves the filter into a
    // uniform pick over the whole level. See the module docs — this is the one deviation,
    // and it is a no-op on a PD-sized pool.
    while shortlist.is_empty() {
        let Some((best_i, best_sq)) = info
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.dist_sq.map(|d| (i, d)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break; // no pads left at all
        };
        // PD stops here rather than shortlisting a pad with someone standing on it —
        // unless nothing has been shortlisted, in which case a bad spawn beats none.
        // Reaching this loop at all means the shortlist IS empty, so the guard reduces to
        // "take it anyway"; kept explicit because the condition is PD's and the next
        // person to widen the loop needs to see it.
        if best_sq <= DESPERATE_DIST * DESPERATE_DIST && !shortlist.is_empty() {
            break;
        }
        info[best_i].dist_sq = None;
        shortlist.push((best_i, 3));
    }

    // ── Roll between the candidates. ──
    let (pad, pass) = if shortlist.is_empty() {
        // PD's own fallback: a uniform pick over the full set.
        (rand_below(rng, pads.len()), 0)
    } else {
        shortlist[rand_below(rng, shortlist.len())]
    };
    Some(SpawnChoice { pad, enemy_dist: nearest[pad], pass })
}

/// One of PD's shortlist passes: walk the pads circularly from a random start,
/// appending every pad that satisfies `accept` until the shortlist is full or the walk
/// wraps. Consumes each pad it takes (`dist_sq = None`) so a later pass can't re-take
/// it — PD's `padsqdists[p] = -1.0f`.
fn circular_fill(
    shortlist: &mut Vec<(usize, u8)>,
    info: &mut [PadInfo],
    rng: &mut u64,
    pass: u8,
    accept: impl Fn(&PadInfo) -> bool,
) {
    let n = info.len();
    if n == 0 || shortlist.len() >= SHORTLIST {
        return;
    }
    let start = rand_below(rng, n);
    for k in 0..n {
        if shortlist.len() >= SHORTLIST {
            return;
        }
        let p = (start + k) % n;
        if accept(&info[p]) {
            info[p].dist_sq = None;
            shortlist.push((p, pass));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(x: f32, z: f32) -> SpawnPad {
        SpawnPad { pos: Vec3::new(x, 0.0, z), yaw: 0.0 }
    }

    /// An empty pool yields nothing — the caller (`prepare_spawn`) is responsible for
    /// the legacy-marker fallback, exactly as PD guards with `if (g_NumSpawnPoints > 0)`.
    #[test]
    fn empty_pool_chooses_nothing() {
        let mut physics = PhysicsWorld::new();
        let mut rng = 1;
        assert!(choose_spawn(&[], &[], &mut physics, &mut rng).is_none());
    }

    /// With nobody in the level yet (match start) every pad qualifies in pass 1, so the
    /// choice is a roll across the pool — and over many rolls it does not always land
    /// on the same pad. This is the property that stops a wave stacking on one pad.
    #[test]
    fn match_start_rolls_across_the_whole_pool() {
        let mut physics = PhysicsWorld::new();
        let pads: Vec<SpawnPad> = (0..6).map(|i| pad(i as f32 * 20.0, 0.0)).collect();
        let mut seen = std::collections::HashSet::new();
        let mut rng = 0x1234_5678;
        for _ in 0..60 {
            seen.insert(choose_spawn(&pads, &[], &mut physics, &mut rng).expect("a pad").pad);
        }
        assert!(seen.len() > 1, "the pick varies across the pool, got {seen:?}");
    }

    /// The core of PD's filter: a pad with an enemy standing on it is rejected in
    /// favour of one that is clear, whenever a clear pad exists. With no world geometry
    /// every pad is in line of sight (so all are *very bad*), which leaves the distance
    /// gate doing the work — and that is enough to keep the spawn off the occupant.
    #[test]
    fn occupied_pads_lose_to_distant_ones() {
        let mut physics = PhysicsWorld::new();
        // Two pads on top of the occupant, one far away.
        let pads = vec![pad(0.0, 0.0), pad(1.0, 0.0), pad(80.0, 0.0)];
        let occupants = [Vec3::ZERO];
        let mut rng = 0xABCD_EF01;
        for _ in 0..40 {
            let c = choose_spawn(&pads, &occupants, &mut physics, &mut rng).expect("a pad");
            assert_eq!(pads[c.pad].pos.x, 80.0, "always the pad clear of the occupant");
        }
    }

    /// When *every* pad is compromised the third pass still returns one — a deathmatch
    /// cannot stall for want of a nice spawn. PD's desperate pass has the same job.
    #[test]
    fn all_pads_compromised_still_spawns_someone() {
        let mut physics = PhysicsWorld::new();
        let pads = vec![pad(0.0, 0.0), pad(0.5, 0.0)];
        let occupants = [Vec3::new(0.25, 0.0, 0.0)]; // within 2 m of both
        let mut rng = 7;
        let p = choose_spawn(&pads, &occupants, &mut physics, &mut rng);
        assert!(p.is_some(), "the desperate pass always produces a pad");
    }

    /// The shortlist never exceeds four, so the final roll is over at most PD's four
    /// candidates however large the authored pool grows.
    #[test]
    fn shortlist_caps_at_four_candidates() {
        let mut physics = PhysicsWorld::new();
        let pads: Vec<SpawnPad> = (0..40).map(|i| pad(i as f32 * 30.0, 0.0)).collect();
        // Reach into the passes via a single call's observable behaviour: with 40 clear
        // pads, 200 rolls must still land on at most 4 distinct pads *per call chain*
        // — but each call re-randomises the start, so instead assert the invariant
        // directly by rebuilding the shortlist here.
        let mut info: Vec<PadInfo> = pads
            .iter()
            .map(|_| PadInfo { dist_sq: Some(f32::INFINITY), bad: false, very_bad: false })
            .collect();
        let mut shortlist = Vec::new();
        let mut rng = 99;
        circular_fill(&mut shortlist, &mut info, &mut rng, 1, |_| true);
        assert_eq!(shortlist.len(), SHORTLIST);
        // …and the chooser still returns an index into the pool.
        let mut rng2 = 99;
        let c = choose_spawn(&pads, &[], &mut physics, &mut rng2).expect("a pad");
        assert!(c.pad < pads.len());
    }

    /// **A small pool must not dilute its own filter.** The regression for "I respawned in
    /// the middle of them", and it is arithmetic rather than taste.
    ///
    /// PD's desperate pass tops up a partly-filled shortlist, which is free when the pool
    /// is `pads[24]`: passes 1–2 fill all four slots and it never runs. With a four-pad
    /// level — the real one this was reported on — pass 1 finds the single clear pad and
    /// the desperate pass then adds *the other three, because they are all that is left*,
    /// so the final roll is uniform over every pad including the ones the pack is next to.
    /// Three times in four the filter's answer is discarded.
    ///
    /// **Two things this arena has to get right, and both were wrong the first time.**
    /// The bodies stand ~4 m off the near pads, not on them: PD's desperate pass already
    /// refuses a pad with someone inside [`DESPERATE_DIST`], so occupants *on* the pads
    /// make the dilution invisible. And there is a **wall**, because `very_bad` is "an
    /// enemy can see the pad" — in open space every pad is visible from everywhere, every
    /// pad is very bad, passes 1–2 find nothing and the desperate pass is the only one that
    /// ever runs. A level with walls is the case this filter exists for.
    #[test]
    fn a_small_pool_still_avoids_the_pads_the_pack_is_near() {
        let mut physics = PhysicsWorld::new();
        // A solid slab across z ≈ 20, blinding the two halves to each other. A door
        // collider is the cheapest way to get world geometry into a bare `PhysicsWorld`,
        // and `raycast_world_only` treats it as the wall it is.
        physics.add_door_collider(Vec3::new(-40.0, -1.0, 19.0), Vec3::new(40.0, 3.0, 21.0));
        // Three pads on the near side, one alone behind the wall.
        let pads = vec![pad(0.0, 0.0), pad(8.0, 0.0), pad(16.0, 0.0), pad(8.0, 40.0)];
        // Two bodies loitering 4 m off each near pad — close enough to make it *bad*,
        // far enough that PD's own 2 m bail-out does not already reject it.
        let occupants: Vec<Vec3> = pads[..3]
            .iter()
            .flat_map(|p| [p.pos + Vec3::new(0.0, 0.0, 4.0), p.pos + Vec3::new(0.6, 0.0, 4.0)])
            .collect();
        let mut rng = 0x0BAD_F00D;
        let mut picks = std::collections::HashMap::new();
        for _ in 0..200 {
            let c = choose_spawn(&pads, &occupants, &mut physics, &mut rng).expect("a pad");
            *picks.entry(c.pad).or_insert(0) += 1;
        }
        assert_eq!(
            picks.keys().copied().collect::<Vec<_>>(),
            vec![3],
            "spawned somewhere other than the one pad clear of the pack: {picks:?}"
        );
        // …and it got there through the filter, not through the desperate pass.
        let c = choose_spawn(&pads, &occupants, &mut physics, &mut rng).expect("a pad");
        assert_eq!(c.pass, 1, "the clear pad should be a pass-1 pick");
        assert!(c.enemy_dist > 30.0, "and genuinely far from everyone");
    }

    /// …and the desperate pass is still reachable, still reports itself, and still picks
    /// the **farthest** pad rather than rolling. With every pad compromised there is no
    /// good answer, so a deterministic best one beats a random bad one — and `pass == 3`
    /// in the log is what tells the author the level is short of pads.
    #[test]
    fn with_nothing_safe_the_desperate_pass_takes_the_farthest_pad() {
        let mut physics = PhysicsWorld::new();
        // All three within the 10 m gate of the occupant, so passes 1 and 2 find nothing.
        let pads = vec![pad(1.0, 0.0), pad(4.0, 0.0), pad(9.0, 0.0)];
        let occupants = [Vec3::ZERO];
        let mut rng = 5;
        for _ in 0..40 {
            let c = choose_spawn(&pads, &occupants, &mut physics, &mut rng).expect("a pad");
            assert_eq!(c.pass, 3, "this is the desperate pass, and it should say so");
            assert_eq!(pads[c.pad].pos.x, 9.0, "the farthest of a bad set");
            assert!((c.enemy_dist - 9.0).abs() < 1e-3, "and it reports the real distance");
        }
    }
}
