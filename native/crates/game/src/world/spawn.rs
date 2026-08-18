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
const SHORTLIST: usize = 4;

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
) -> Option<usize> {
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

    let mut shortlist: Vec<usize> = Vec::with_capacity(SHORTLIST);

    // ── Pass 1: clear of everyone (>10 m) and not bad. ──
    // PD walks circularly from a random index so that with more qualifying pads than
    // shortlist slots it isn't always the same four — the randomisation is in the
    // *start*, not just the final pick.
    circular_fill(&mut shortlist, &mut info, rng, |p| {
        p.dist_sq.is_some_and(|d| d > min_sq) && !p.bad
    });

    // ── Pass 2: still >10 m, now accepting *bad* pads but not *very bad* ones. ──
    circular_fill(&mut shortlist, &mut info, rng, |p| {
        p.dist_sq.is_some_and(|d| d > min_sq) && !p.very_bad
    });

    // ── Pass 3: desperate — take what's left, farthest first. ──
    // No distance gate and no exposure filter: a small level where every pad is
    // watched still has to spawn somebody.
    while shortlist.len() < SHORTLIST {
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
        if best_sq <= DESPERATE_DIST * DESPERATE_DIST && !shortlist.is_empty() {
            break;
        }
        info[best_i].dist_sq = None;
        shortlist.push(best_i);
    }

    // ── Roll between the candidates. ──
    Some(if shortlist.is_empty() {
        // PD's own fallback: a uniform pick over the full set.
        rand_below(rng, pads.len())
    } else {
        shortlist[rand_below(rng, shortlist.len())]
    })
}

/// One of PD's shortlist passes: walk the pads circularly from a random start,
/// appending every pad that satisfies `accept` until the shortlist is full or the walk
/// wraps. Consumes each pad it takes (`dist_sq = None`) so a later pass can't re-take
/// it — PD's `padsqdists[p] = -1.0f`.
fn circular_fill(
    shortlist: &mut Vec<usize>,
    info: &mut [PadInfo],
    rng: &mut u64,
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
            shortlist.push(p);
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
            seen.insert(choose_spawn(&pads, &[], &mut physics, &mut rng).expect("a pad"));
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
            let i = choose_spawn(&pads, &occupants, &mut physics, &mut rng).expect("a pad");
            assert_eq!(pads[i].pos.x, 80.0, "always the pad clear of the occupant");
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
        circular_fill(&mut shortlist, &mut info, &mut rng, |_| true);
        assert_eq!(shortlist.len(), SHORTLIST);
        // …and the chooser still returns an index into the pool.
        let mut rng2 = 99;
        let i = choose_spawn(&pads, &[], &mut physics, &mut rng2).expect("a pad");
        assert!(i < pads.len());
    }
}
