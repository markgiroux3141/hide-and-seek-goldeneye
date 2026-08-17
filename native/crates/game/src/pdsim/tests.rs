//! Tests for the Perfect Dark simulant port.
//!
//! These assert the *properties the PD constants are supposed to produce*, not
//! the constants themselves — a table typo that preserved the shape would still
//! be caught, and a deliberate retune would fail loudly rather than silently
//! changing how the enemy feels.

use super::difficulty::{tier_for_dial, tuning_for_dial, BotDifficulty};
use super::personality::{self, BotType, Candidate, Grudge, Threat};
use super::targeting::{self, Perception};
use super::zeroing::{turn_toward, Zeroing, MAX_TURN_RATE};
use super::*;

const DT: f32 = 1.0 / 60.0;

fn candidate(id: u32, distance: f32) -> Candidate {
    Candidate {
        id,
        distance,
        in_sight: true,
        alive: true,
        threat: Threat::of(50.0, 1.0),
        armed: true,
    }
}

/// Run a zeroing model to steady state with the target held in sight, and report
/// the largest aim error seen over the final second.
fn settled_error(diff: BotDifficulty, secs: f32) -> f32 {
    let tuning = diff.tuning();
    let mut z = Zeroing::default();
    let mut rng = 0x1234_5678_9abc_def0u64;
    let mut draw = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 40) as f32) / ((1u32 << 24) as f32)
    };
    let steps = (secs / DT) as usize;
    let tail = (1.0 / DT) as usize;
    let mut worst = 0.0f32;
    for i in 0..steps {
        // Sight held, body already pointed at the target so no turn feedback.
        z.tick_shoot_delay(DT, true);
        let e = z.update(DT, &tuning, true, 0.0, false, &mut draw);
        if i >= steps.saturating_sub(tail) {
            worst = worst.max(e.abs());
        }
    }
    worst
}

// ─── The difficulty table ────────────────────────────────────────────────────

#[test]
fn difficulty_table_is_monotonic_in_lethality() {
    // Every lethality field must improve (or hold) as the tier rises. This is
    // what makes interpolating between rows for our 0..10 dial legitimate.
    let rows: Vec<_> = BotDifficulty::ALL.iter().map(|d| d.tuning()).collect();
    for w in rows.windows(2) {
        let (lo, hi) = (&w[0], &w[1]);
        assert!(hi.shoot_delay <= lo.shoot_delay, "reaction must not get slower");
        assert!(hi.zero_time <= lo.zero_time, "zero time must not get longer");
        assert!(hi.max_zero_speed <= lo.max_zero_speed, "aim error must not grow");
        assert!(hi.turn_unzero_mult <= lo.turn_unzero_mult, "turn penalty must not grow");
    }
}

#[test]
fn dark_tier_is_perfectly_accurate_and_instant() {
    let dark = BotDifficulty::Dark.tuning();
    assert_eq!(dark.shoot_delay, 0.0);
    assert_eq!(dark.zero_time, 0.0);
    assert_eq!(dark.max_zero_speed, 0.0);
    assert_eq!(dark.turn_unzero_mult, 0.0);
    // A DarkSim has literally no aim error, ever.
    assert!(settled_error(BotDifficulty::Dark, 5.0) < 1e-6);
}

#[test]
fn meat_reacts_in_about_a_second_and_a_half() {
    let meat = BotDifficulty::Meat.tuning();
    assert!((meat.shoot_delay - 1.5).abs() < 1e-4, "meat shootdelay = 90 ticks = 1.5 s");
    let perfect = BotDifficulty::Perfect.tuning();
    assert_eq!(perfect.shoot_delay, 0.0, "perfect reacts instantly");
}

#[test]
fn settled_aim_error_matches_the_tier_the_table_advertises() {
    // The steady state of the leaky accumulator is `inc / (1 - decay)`, and
    // `angle = speed * 0.025`, so a settled error lands within about a degree of
    // the tier's `force_zero_min_speed` floor. That identity is the reason the
    // table's degree values are readable as "how far off this tier aims".
    for diff in [BotDifficulty::Meat, BotDifficulty::Normal, BotDifficulty::Hard] {
        let floor = diff.tuning().force_zero_min_speed;
        let worst = settled_error(diff, 30.0);
        assert!(
            worst > floor * 0.25,
            "{}: settled error {:.3} rad collapsed well below its {:.3} rad floor",
            diff.name(),
            worst,
            floor
        );
        assert!(
            worst < floor * 1.6,
            "{}: settled error {:.3} rad far exceeds its {:.3} rad floor",
            diff.name(),
            worst,
            floor
        );
    }
}

#[test]
fn harder_tiers_settle_tighter_than_easier_ones() {
    let meat = settled_error(BotDifficulty::Meat, 30.0);
    let normal = settled_error(BotDifficulty::Normal, 30.0);
    let hard = settled_error(BotDifficulty::Hard, 30.0);
    let perfect = settled_error(BotDifficulty::Perfect, 30.0);
    assert!(meat > normal, "meat {meat:.3} should wander more than normal {normal:.3}");
    assert!(normal > hard, "normal {normal:.3} should wander more than hard {hard:.3}");
    assert!(hard > perfect, "hard {hard:.3} should wander more than perfect {perfect:.3}");
}

// ─── Zeroing behaviour ───────────────────────────────────────────────────────

#[test]
fn zero_progress_fills_in_sight_and_drains_out_of_sight() {
    let tuning = BotDifficulty::Normal.tuning();
    let mut z = Zeroing::default();
    let mut rng = 1u64;
    let mut draw = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 40) as f32) / ((1u32 << 24) as f32)
    };
    for _ in 0..(3.0 / DT) as usize {
        z.tick_shoot_delay(DT, true);
        z.update(DT, &tuning, true, 0.0, false, &mut draw);
    }
    let zeroed = z.progress(&tuning);
    assert!(zeroed > 0.9, "3 s of clear sight should nearly finish a 3 s zero, got {zeroed:.2}");

    for _ in 0..(2.0 / DT) as usize {
        z.tick_shoot_delay(DT, false);
        z.update(DT, &tuning, false, 0.0, false, &mut draw);
    }
    assert!(
        z.progress(&tuning) < zeroed * 0.5,
        "losing sight for 2 s should drain most of the zero"
    );
}

#[test]
fn turning_un_zeroes_and_punishes_weak_tiers_hardest() {
    // Zero fully, spin at the maximum turn rate for half a second, then measure
    // how long the bot needs with clear sight to get back on target. Recovery
    // *time* is the quantity that matters in a fight — the remaining fraction is
    // not comparable across tiers, since each has a different `zero_time`
    // denominator (Meat keeps a larger share of a far longer zero).
    //
    // This is the mechanic that makes low-tier sims helpless against a target
    // that keeps making them swing.
    let mut recovery = Vec::new();
    for diff in [BotDifficulty::Meat, BotDifficulty::Normal, BotDifficulty::Hard] {
        let tuning = diff.tuning();
        let mut z = Zeroing::default();
        let mut rng = 99u64;
        let mut draw = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            ((rng >> 40) as f32) / ((1u32 << 24) as f32)
        };
        for _ in 0..(30.0 / DT) as usize {
            z.tick_shoot_delay(DT, true);
            z.update(DT, &tuning, true, 0.0, false, &mut draw);
        }
        assert!(z.progress(&tuning) > 0.95, "{} failed to zero", diff.name());

        for _ in 0..(0.5 / DT) as usize {
            z.tick_shoot_delay(DT, true);
            z.update(DT, &tuning, true, MAX_TURN_RATE, false, &mut draw);
        }
        assert!(
            z.progress(&tuning) < 0.95,
            "{}: turning at full rate must cost real zero progress",
            diff.name()
        );

        let mut secs = 0.0;
        while z.progress(&tuning) < 0.95 && secs < 60.0 {
            z.tick_shoot_delay(DT, true);
            z.update(DT, &tuning, true, 0.0, false, &mut draw);
            secs += DT;
        }
        recovery.push((diff, secs));
    }
    let meat = recovery[0].1;
    let normal = recovery[1].1;
    let hard = recovery[2].1;
    assert!(meat > normal, "meat needs {meat:.2}s to recover, normal only {normal:.2}s");
    assert!(normal > hard, "normal needs {normal:.2}s to recover, hard only {hard:.2}s");
    // A DarkSim is not punished for turning at all (`turn_unzero_mult` = 0).
    let dark = BotDifficulty::Dark.tuning();
    let mut z = Zeroing::default();
    z.tick_shoot_delay(DT, true);
    z.update(DT, &dark, true, MAX_TURN_RATE, false, || 0.5);
    assert_eq!(z.progress(&dark), 1.0, "a DarkSim never loses its zero");
}

#[test]
fn zero_cannot_outrun_the_reaction_delay() {
    // A bot must never finish converging and then stand there waiting for
    // permission to shoot (`bot.c:1481`).
    let tuning = BotDifficulty::Meat.tuning();
    let mut z = Zeroing::default();
    let mut rng = 7u64;
    let mut draw = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 40) as f32) / ((1u32 << 24) as f32)
    };
    for _ in 0..(1.0 / DT) as usize {
        z.tick_shoot_delay(DT, true);
        z.update(DT, &tuning, true, 0.0, false, &mut draw);
        assert!(
            z.zero_timer <= z.shoot_delay_timer + 1e-6,
            "zero timer {} outran shoot delay {}",
            z.zero_timer,
            z.shoot_delay_timer
        );
    }
}

#[test]
fn brief_sight_break_barely_helps_the_target() {
    // The original field note: "It has a cooldown, so a brief break in sight will
    // have little effect." Ducking behind cover for a fifth of a second must not
    // buy a fresh reaction time.
    let tuning = BotDifficulty::Normal.tuning();
    let mut z = Zeroing::default();
    for _ in 0..(0.45 / DT) as usize {
        z.tick_shoot_delay(DT, true);
    }
    assert!(z.may_shoot(&tuning) || z.shoot_delay_timer > 0.4);
    let before = z.shoot_delay_timer;
    for _ in 0..(0.2 / DT) as usize {
        z.tick_shoot_delay(DT, false);
    }
    assert!(
        z.shoot_delay_timer > before - 0.25,
        "a 0.2 s break should cost about 0.2 s, not reset the clock"
    );
}

#[test]
fn aim_error_changes_sign_over_time() {
    // The randomly-signed increment is what makes the aim cross the target rather
    // than creep on from one side. Without it, players could learn a safe strafe
    // direction.
    let tuning = BotDifficulty::Normal.tuning();
    let mut z = Zeroing::default();
    let mut rng = 0xdead_beefu64;
    let mut draw = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 40) as f32) / ((1u32 << 24) as f32)
    };
    let (mut saw_pos, mut saw_neg) = (false, false);
    for _ in 0..(20.0 / DT) as usize {
        z.tick_shoot_delay(DT, true);
        let e = z.update(DT, &tuning, true, 0.0, false, &mut draw);
        if e > 1e-3 {
            saw_pos = true;
        }
        if e < -1e-3 {
            saw_neg = true;
        }
    }
    assert!(saw_pos && saw_neg, "aim must wander to both sides of the target");
}

// ─── Turning ─────────────────────────────────────────────────────────────────

#[test]
fn turn_is_rate_limited_and_takes_the_short_way() {
    // PD's cap is ~212 deg/s.
    let deg_per_sec = MAX_TURN_RATE.to_degrees();
    assert!((deg_per_sec - 211.7).abs() < 1.0, "turn cap was {deg_per_sec:.1} deg/s");

    let (yaw, rate) = turn_toward(0.0, std::f32::consts::PI, DT);
    assert!(yaw > 0.0 && yaw < MAX_TURN_RATE * DT + 1e-6);
    assert!((rate - MAX_TURN_RATE).abs() < 1e-3);

    // Short way round: from 0.1 rad the shortest path to 6.0 rad is *backwards*.
    let (yaw, _) = turn_toward(0.1, 6.0, DT);
    assert!(yaw < 0.1 || yaw > 6.0, "should have turned the short way, got {yaw}");
}

#[test]
fn a_full_reversal_takes_about_the_right_time() {
    // 180 deg at 211.7 deg/s is ~0.85 s. That window is why a bot ambushed from
    // behind is briefly helpless.
    let mut yaw = 0.0f32;
    let mut t = 0.0f32;
    for _ in 0..(3.0 / DT) as usize {
        let (n, _) = turn_toward(yaw, std::f32::consts::PI, DT);
        yaw = n;
        t += DT;
        if (yaw - std::f32::consts::PI).abs() < 1e-3 {
            break;
        }
    }
    assert!((t - 0.85).abs() < 0.1, "180 deg reversal took {t:.2} s");
}

// ─── Target selection ────────────────────────────────────────────────────────

#[test]
fn perception_queries_one_candidate_per_tick_round_robin() {
    let mut p = Perception::default();
    p.resize(4);
    let mut seen = Vec::new();
    for _ in 0..8 {
        seen.push(p.next_query().unwrap());
        p.tick(DT, None);
    }
    assert_eq!(seen, vec![0, 1, 2, 3, 0, 1, 2, 3], "cursor must cycle one per tick");
}

#[test]
fn unqueried_candidates_go_stale_rather_than_updating() {
    let mut p = Perception::default();
    p.resize(3);
    // Only ever tell it about candidate 0.
    for _ in 0..30 {
        p.tick(DT, Some((0, true, 5.0)));
    }
    assert_eq!(p.known(0).since_seen, 0.0);
    assert!(p.known(1).since_seen > 0.4, "candidate 1 was never queried, must be stale");
    assert!(!p.known(1).queried);
}

#[test]
fn a_visible_living_target_is_kept_without_re_deciding() {
    let cands = [candidate(1, 10.0), candidate(2, 2.0)];
    let mut p = Perception::default();
    p.resize(2);
    p.tick(DT, Some((0, true, 10.0)));
    p.tick(DT, Some((1, true, 2.0)));
    let mut g = Grudge::default();
    // Currently on the *far* target. Stickiness must keep it even though a much
    // closer one is visible — this is what stops chase-thrash.
    let sel = targeting::choose_target(
        &cands,
        &p,
        BotType::General,
        &Threat::of(50.0, 1.0),
        &mut g,
        Some(1),
        false,
    );
    assert_eq!(sel.index, Some(0));
    assert!(sel.sticky);
}

#[test]
fn with_no_target_the_nearest_visible_candidate_wins() {
    let cands = [candidate(1, 10.0), candidate(2, 2.0)];
    let mut p = Perception::default();
    p.resize(2);
    p.tick(DT, Some((0, true, 10.0)));
    p.tick(DT, Some((1, true, 2.0)));
    let mut g = Grudge::default();
    let sel = targeting::choose_target(
        &cands,
        &p,
        BotType::General,
        &Threat::of(50.0, 1.0),
        &mut g,
        None,
        false,
    );
    assert_eq!(sel.index, Some(1), "closest visible candidate");
    assert!(!sel.sticky);
}

#[test]
fn low_tiers_chase_the_nearest_enemy_even_unseen() {
    // The single branch that makes Meat and Easy sims read as oblivious.
    assert!(targeting::takes_unseen_closest(0.0), "meat");
    assert!(targeting::takes_unseen_closest(0.2), "easy");
    assert!(!targeting::takes_unseen_closest(0.5), "normal and up");
    assert!(!targeting::takes_unseen_closest(1.0), "dark");
}

// ─── Personality ─────────────────────────────────────────────────────────────

#[test]
fn peace_sim_refuses_unarmed_targets_and_never_fires() {
    let mut unarmed = candidate(1, 5.0);
    unarmed.armed = false;
    let own = Threat::of(50.0, 1.0);
    let g = Grudge::default();
    assert!(!personality::passes_vetoes(BotType::Peace, &own, &unarmed, &g));
    assert!(personality::passes_vetoes(BotType::Peace, &own, &candidate(2, 5.0), &g));
    assert!(BotType::Peace.pacifist());
}

#[test]
fn coward_sim_declines_a_fight_it_does_not_lead_by_30() {
    let own = Threat::of(50.0, 1.0);
    let g = Grudge::default();
    let mut stronger = candidate(1, 8.0);
    stronger.in_sight = false;
    stronger.threat.weapon_score = 40.0; // only a 10-point lead — not enough
    assert!(!personality::passes_vetoes(BotType::Coward, &own, &stronger, &g));

    let mut weaker = stronger;
    weaker.threat.weapon_score = 10.0; // a 40-point lead — engage
    assert!(personality::passes_vetoes(BotType::Coward, &own, &weaker, &g));

    // A cornered CowardSim still fights what is already in its face.
    let mut adjacent = stronger;
    adjacent.in_sight = true;
    assert!(personality::passes_vetoes(BotType::Coward, &own, &adjacent, &g));
}

#[test]
fn feud_sim_fixates_on_its_first_target_forever() {
    let cands = [candidate(1, 10.0), candidate(2, 2.0)];
    let mut p = Perception::default();
    p.resize(2);
    p.tick(DT, Some((0, true, 10.0)));
    let mut g = Grudge::default();
    let own = Threat::of(50.0, 1.0);

    // First pick: only candidate 0 has been queried, so it is the one in sight.
    let first =
        targeting::choose_target(&cands, &p, BotType::Feud, &own, &mut g, None, false);
    assert_eq!(first.index, Some(0));
    assert_eq!(g.feud_target, Some(1));

    // Now the nearer candidate becomes visible. A GeneralSim would take it; a
    // FeudSim may not — its nemesis is vetoed-in for the whole match.
    p.tick(DT, Some((1, true, 2.0)));
    let second =
        targeting::choose_target(&cands, &p, BotType::Feud, &own, &mut g, None, false);
    assert_eq!(second.index, Some(0), "feud must not switch to the closer target");
}

#[test]
fn prey_sim_prefers_the_weak_over_the_merely_close() {
    let mut healthy = candidate(1, 3.0);
    healthy.threat.health_frac = 1.0;
    healthy.threat.since_spawn = 100.0;
    let mut wounded = candidate(2, 12.0);
    wounded.threat.health_frac = 0.2;
    wounded.threat.since_spawn = 100.0;
    let g = Grudge::default();
    let bonus = personality::preference(BotType::Prey, &wounded, &g);
    assert!(bonus > 0.0, "wounded target must be preferred");
    assert_eq!(personality::preference(BotType::Prey, &healthy, &g), 0.0);
    // The bonus is a bias, not an override: it must be able to reorder a 9 m gap.
    assert!(bonus > wounded.distance - healthy.distance);
}

#[test]
fn venge_sim_hunts_its_last_killer() {
    let mut g = Grudge::default();
    let victim = candidate(1, 20.0);
    assert_eq!(personality::preference(BotType::Venge, &victim, &g), 0.0);
    g.last_killer = Some(1);
    assert!(personality::preference(BotType::Venge, &victim, &g) > 0.0);
}

#[test]
fn speed_personalities_override_difficulty_rather_than_scaling_it() {
    // A SpeedSim moves identically at Meat and at Dark — PD's clean separation of
    // the two axes.
    let meat_speed = Simulant::new(BotDifficulty::Meat, BotType::Speed, 1).speed_mult();
    let dark_speed = Simulant::new(BotDifficulty::Dark, BotType::Speed, 1).speed_mult();
    assert_eq!(meat_speed, dark_speed);

    // A GeneralSim does not.
    let meat_gen = Simulant::new(BotDifficulty::Meat, BotType::General, 1).speed_mult();
    let dark_gen = Simulant::new(BotDifficulty::Dark, BotType::General, 1).speed_mult();
    assert!(dark_gen > meat_gen);

    // And a TurtleSim is slower than any difficulty makes anyone.
    let turtle = Simulant::new(BotDifficulty::Dark, BotType::Turtle, 1).speed_mult();
    assert!(turtle < meat_gen);
}

// ─── The dial ────────────────────────────────────────────────────────────────

#[test]
fn the_difficulty_dial_spans_meat_to_dark() {
    assert_eq!(tier_for_dial(0.0, 10.0), BotDifficulty::Meat);
    assert_eq!(tier_for_dial(10.0, 10.0), BotDifficulty::Dark);
    // Endpoints must reproduce the table rows exactly, not an interpolation of them.
    let lo = tuning_for_dial(0.0, 10.0);
    assert!((lo.shoot_delay - BotDifficulty::Meat.tuning().shoot_delay).abs() < 1e-6);
    let hi = tuning_for_dial(10.0, 10.0);
    assert!((hi.zero_time - BotDifficulty::Dark.tuning().zero_time).abs() < 1e-6);
}

#[test]
fn the_dial_is_monotonic_between_tiers() {
    let mut prev = tuning_for_dial(0.0, 10.0);
    for i in 1..=40 {
        let d = i as f32 * 0.25;
        let cur = tuning_for_dial(d, 10.0);
        assert!(cur.shoot_delay <= prev.shoot_delay + 1e-6, "reaction regressed at dial {d}");
        assert!(cur.zero_time <= prev.zero_time + 1e-6, "zero time regressed at dial {d}");
        prev = cur;
    }
}

// ─── End to end ──────────────────────────────────────────────────────────────

#[test]
fn a_simulant_acquires_converges_and_fires() {
    let mut sim = Simulant::new(BotDifficulty::Normal, BotType::General, 42);
    let cands = [candidate(1, 8.0)];
    // Target dead ahead along +X (bearing 0), simulant starts facing away.
    sim.yaw = std::f32::consts::PI;

    let mut fired_at = None;
    let mut errors = Vec::new();
    for i in 0..(6.0 / DT) as usize {
        let out = sim.tick(SimInput {
            dt: DT,
            candidates: &cands,
            own: Threat::of(50.0, 1.0),
            query: Some((0, true, 8.0)),
            bearing_to_target: Some(0.0),
            target_in_sight: true,
            aim_offset: 0.0,
            dial_frac: 0.5,
        });
        if out.want_fire && fired_at.is_none() {
            fired_at = Some(i as f32 * DT);
        }
        if i > (4.0 / DT) as usize {
            errors.push(out.aim_error.abs());
        }
    }

    let t = fired_at.expect("a NormalSim should open fire within 6 s");
    // It must not fire before its 0.5 s reaction, and must not need to be fully
    // zeroed first — PD fires while still converging.
    assert!(t >= 0.5 - 1e-3, "fired at {t:.2} s, before the 0.5 s reaction delay");
    assert!(t < 1.6, "took {t:.2} s to open fire, too hesitant for a NormalSim");

    // By the end it should be tracking closely but never perfectly.
    let worst = errors.iter().cloned().fold(0.0f32, f32::max);
    assert!(worst > 0.0, "a NormalSim should never be perfectly still on target");
    assert!(worst < 0.25, "settled aim error {worst:.3} rad is far too wide");
}

#[test]
fn a_pacifist_never_fires_no_matter_how_good_its_aim() {
    let mut sim = Simulant::new(BotDifficulty::Dark, BotType::Peace, 7);
    let cands = [candidate(1, 4.0)];
    for _ in 0..(5.0 / DT) as usize {
        let out = sim.tick(SimInput {
            dt: DT,
            candidates: &cands,
            own: Threat::of(50.0, 1.0),
            query: Some((0, true, 4.0)),
            bearing_to_target: Some(0.0),
            target_in_sight: true,
            aim_offset: 0.0,
            dial_frac: 1.0,
        });
        assert!(!out.want_fire, "a PeaceSim must never pull the trigger");
    }
}

#[test]
fn a_dark_sim_is_lethal_immediately() {
    let mut sim = Simulant::new(BotDifficulty::Dark, BotType::General, 3);
    let cands = [candidate(1, 6.0)];
    // Already facing the target: no turn needed, no reaction delay, no error.
    let out = sim.tick(SimInput {
        dt: DT,
        candidates: &cands,
        own: Threat::of(50.0, 1.0),
        query: Some((0, true, 6.0)),
        bearing_to_target: Some(0.0),
        target_in_sight: true,
        aim_offset: 0.0,
        dial_frac: 1.0,
    });
    assert!(out.want_fire, "a DarkSim fires on the first frame it sees you");
    assert!(out.aim_error.abs() < 1e-6, "a DarkSim has no aim error");
}

#[test]
fn losing_the_target_stops_the_bot_firing() {
    let mut sim = Simulant::new(BotDifficulty::Dark, BotType::General, 5);
    let cands = [candidate(1, 6.0)];
    let out = sim.tick(SimInput {
        dt: DT,
        candidates: &cands,
        own: Threat::of(50.0, 1.0),
        query: Some((0, false, 6.0)),
        bearing_to_target: Some(0.0),
        target_in_sight: false,
        aim_offset: 0.0,
        dial_frac: 1.0,
    });
    assert!(!out.want_fire);
}
