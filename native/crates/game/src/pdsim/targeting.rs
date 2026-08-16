//! Perfect Dark's target selection and **amortised perception**.
//!
//! Ported from `bot_choose_general_target` (`pd-decomp/src/game/bot.c:1589`).
//!
//! Two things here are worth having independently of the aim model.
//!
//! **Amortised perception.** PD queries exactly *one* other character per tick for
//! distance and line of sight, round-robin via `queryplayernum`. With N characters
//! each is refreshed every N ticks. The cost is constant per bot regardless of how
//! many others exist, which is a strictly better scaling story than our current
//! everyone-every-frame perception — and, more interestingly, it is a *feature*:
//! a bot's knowledge is deliberately stale, so it reacts to where you were a few
//! ticks ago.
//!
//! **Target stickiness.** A bot already attacking a target that is still visible
//! and alive simply keeps it, with no re-decision. Re-deciding every tick is what
//! produces chase-thrash and search-stall, both of which our AI testbed has caught
//! in the existing hunters.
//!
//! Note what the algorithm deliberately does *not* consider: weapons, ammo, or
//! personality. The doc comment in PD says so explicitly. Personality enters only
//! through the veto predicates in [`super::personality`].

use super::personality::{self, BotType, Candidate, Grudge, Threat};

/// Perception state for one simulant: who it knows about, and how stale that
/// knowledge is.
#[derive(Clone, Debug, Default)]
pub struct Perception {
    /// Round-robin cursor (`queryplayernum`). Advances one candidate per tick.
    cursor: usize,
    /// Per-candidate knowledge, indexed the same way the caller indexes candidates.
    known: Vec<Known>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Known {
    /// Result of the most recent (possibly stale) sight query.
    pub in_sight: bool,
    /// Seconds since this candidate was last actually seen. Large = never.
    pub since_seen: f32,
    /// Distance recorded at the last query.
    pub distance: f32,
    /// Whether this slot has ever been queried.
    pub queried: bool,
}

/// A never-seen candidate reports this many seconds since last seen.
pub const NEVER_SEEN: f32 = 1.0e9;

impl Perception {
    /// Resize to match the candidate count, preserving existing knowledge.
    pub fn resize(&mut self, n: usize) {
        if self.known.len() != n {
            self.known.resize(n, Known { since_seen: NEVER_SEEN, ..Default::default() });
            if self.cursor >= n.max(1) {
                self.cursor = 0;
            }
        }
    }

    pub fn known(&self, i: usize) -> Known {
        self.known.get(i).copied().unwrap_or(Known { since_seen: NEVER_SEEN, ..Default::default() })
    }

    /// Which candidate index will be queried this tick, or `None` if there are
    /// none. Exposed so the caller can do the (expensive) raycast for just that
    /// one and hand the answer back to [`Self::tick`].
    pub fn next_query(&self) -> Option<usize> {
        if self.known.is_empty() {
            None
        } else {
            Some(self.cursor % self.known.len())
        }
    }

    /// Advance perception one tick.
    ///
    /// `query` is the fresh sight/distance result for the index [`Self::next_query`]
    /// returned — the caller does one raycast, not N. Every *other* slot only ages.
    ///
    /// The `since_seen` clock for slots we did not query this tick keeps running,
    /// which is the intended staleness: a bot does not know you moved until its
    /// cursor comes back round to you.
    pub fn tick(&mut self, dt: f32, query: Option<(usize, bool, f32)>) {
        for k in &mut self.known {
            k.since_seen += dt;
        }
        if let Some((i, in_sight, distance)) = query {
            if let Some(k) = self.known.get_mut(i) {
                k.in_sight = in_sight;
                k.distance = distance;
                k.queried = true;
                if in_sight {
                    k.since_seen = 0.0;
                }
            }
        }
        if !self.known.is_empty() {
            self.cursor = (self.cursor + 1) % self.known.len();
        }
    }
}

/// The outcome of one target-selection pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    /// Index into the candidate slice, or `None` for "no legal target".
    pub index: Option<usize>,
    /// Whether the previous target was kept without re-deciding (stickiness hit).
    pub sticky: bool,
}

/// `bot_choose_general_target` (`bot.c:1589`), personality vetoes included.
///
/// `current` is the previously chosen candidate id. Returns the new choice.
///
/// The priority order is PD's:
/// 1. An existing target that is still visible and alive is **kept outright**.
/// 2. Otherwise the existing target is validated and dropped if dead, unseeable,
///    or vetoed by personality.
/// 3. With no target, candidates are walked in ascending distance and the first
///    acceptable one wins.
///
/// The one difficulty-dependent branch is step 3: **Meat and Easy sims take the
/// closest enemy even if they cannot see it**, while every other tier prefers a
/// visible target and only falls back to a known-but-unseen one. That single
/// branch is most of why low-tier sims read as oblivious rather than merely
/// inaccurate — they walk confidently toward someone they have no business
/// knowing about, and get shot doing it.
pub fn choose_target(
    candidates: &[Candidate],
    perception: &Perception,
    ty: BotType,
    own: &Threat,
    grudge: &mut Grudge,
    current: Option<u32>,
    takes_unseen_closest: bool,
) -> Selection {
    if ty.pacifist() {
        return Selection { index: None, sticky: false };
    }

    let legal = |i: usize| -> bool {
        let c = &candidates[i];
        let seen = perception.known(i);
        let mut c = *c;
        c.in_sight = seen.in_sight;
        c.distance = if seen.queried { seen.distance } else { c.distance };
        personality::passes_vetoes(ty, own, &c, grudge)
    };

    // ── 1 + 2: keep or validate the existing target ──
    if let Some(cur_id) = current {
        if let Some(i) = candidates.iter().position(|c| c.id == cur_id) {
            let seen = perception.known(i);
            if candidates[i].alive && seen.in_sight && legal(i) {
                return Selection { index: Some(i), sticky: true };
            }
        }
    }

    // ── 3: pick fresh, in ascending distance ──
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let da = effective_distance(candidates, perception, a);
        let db = effective_distance(candidates, perception, b);
        // Personality preference biases the ordering by pulling wanted targets
        // "closer"; the sort itself stays the shared distance sort PD uses.
        let pa = da - personality::preference(ty, &candidates[a], grudge);
        let pb = db - personality::preference(ty, &candidates[b], grudge);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut fallback: Option<usize> = None;
    for &i in &order {
        if !candidates[i].alive || !legal(i) {
            continue;
        }
        let seen = perception.known(i);
        if seen.in_sight {
            personality::note_target(ty, Some(candidates[i].id), grudge);
            return Selection { index: Some(i), sticky: false };
        }
        // Known but not currently visible — the fallback every tier above Easy
        // uses only when nothing is in sight.
        if fallback.is_none() && (takes_unseen_closest || seen.since_seen < NEVER_SEEN) {
            fallback = Some(i);
        }
    }

    if let Some(i) = fallback {
        personality::note_target(ty, Some(candidates[i].id), grudge);
        return Selection { index: Some(i), sticky: false };
    }
    Selection { index: None, sticky: false }
}

/// Distance to use for ordering: the last queried distance if we have one,
/// otherwise the candidate's true distance. Stale-by-design, matching the
/// amortised query.
fn effective_distance(candidates: &[Candidate], perception: &Perception, i: usize) -> f32 {
    let seen = perception.known(i);
    if seen.queried {
        seen.distance
    } else {
        candidates[i].distance
    }
}

/// Whether a tier is oblivious enough to chase an enemy it cannot see (Meat and
/// Easy only). Threaded in from the dial rather than hard-coded so the
/// interpolated dial keeps a sensible cutover.
pub fn takes_unseen_closest(dial_frac: f32) -> bool {
    // Meat and Easy are rows 0 and 1 of six, so the cutover sits between rows 1
    // and 2 — a third of the way up the dial.
    dial_frac < 1.5 / 5.0
}
