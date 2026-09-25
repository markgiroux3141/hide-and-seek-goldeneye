//! Golden tests for every registered design: each one goes through the game's own
//! load → save → reload path and bakes nav with the one bake `G` uses, and the result
//! is pinned. A change to nav, structures, the CSG fold or the level format that
//! silently reshapes a generated level fails here instead of in a playtest — which is
//! exactly how the stairs-down bug stayed hidden for two months.

use super::*;

/// What each design is expected to bake to: the size, in standable cells, of every
/// walkable component, largest first.
///
/// **Sizes, not a count.** A count alone let the stairs-down bug back in under a mutation
/// test: `grand` bakes to three components with the bug (main, undercroft, pit) *and*
/// without it (main, undercroft, a 16-cell sliver of the undercroft stair). Any change
/// that moves floor between components moves these numbers.
///
/// Brittle on purpose — a golden test is a tripwire, not a spec. When a change reshapes
/// a design **deliberately**, update its row in the same commit and say why there.
///
/// **Not every design is clean, and each exception says why.**
const EXPECTED: &[(&str, &[usize])] = &[
    ("smoke", &[260]),
    // The two hall pillars stop at 8 WT under a 22 WT ceiling; the top of one is a
    // 4-cell island nobody can reach. Correct, and harmless.
    ("arena", &[1408, 4]),
    ("varied", &[2410]),
    ("sprawl", &[3023]),
    ("facility", &[5815]),
    ("linear", &[2150]),
    ("showcase", &[4502]),
    // The undercroft (496) and a 16-cell run of its stair: the flight carries on past the
    // floor hole under the armory slab with no headroom. Real geometry, to be fixed when
    // the designs are ported to the relational builder API.
    ("grand", &[7328, 496, 16]),
    ("pd_lab", &[3952]),
];

/// Load a design into a fresh `World`, the way the harness does (minus prop bounds:
/// no registered design places a prop, and loading the catalog costs a second a test).
fn world_with(built: &BuiltLevel) -> World {
    let mut w = World::new();
    w.load_built_level(built).expect("a design loads in BUILD");
    w
}

fn component_sizes(world: &mut World) -> Vec<usize> {
    let nav = world.bake_level_nav().expect("every design has walkable floor");
    nav.component_sizes().iter().map(|(_, n)| *n).collect()
}

/// One golden test per design, so they run in parallel and a failure names its design.
/// The list must cover [`DESIGNS`] — [`every_design_has_a_golden_test`] checks it does.
macro_rules! golden {
    ($($name:ident),* $(,)?) => {
        const GOLDEN: &[&str] = &[$(stringify!($name)),*];
        mod golden {
            $( #[test] fn $name() { super::check_design(stringify!($name)); } )*
        }
    };
}
golden!(smoke, arena, varied, sprawl, facility, linear, showcase, grand, pd_lab);

#[test]
fn every_design_has_a_golden_test() {
    for (name, _) in DESIGNS {
        assert!(GOLDEN.contains(name), "design '{name}' is registered but not in golden!()");
        assert!(
            EXPECTED.iter().any(|(n, _)| n == name),
            "design '{name}' is registered but has no expected component sizes"
        );
    }
}

/// The contract the harness relies on — what an author opens is what was analyzed —
/// plus the pinned shape:
///
/// 1. the design survives save → reload intact,
/// 2. it bakes to identical nav either side of that round trip,
/// 3. and to its expected walkable components, cell for cell.
fn check_design(name: &str) {
    let built = design(name).unwrap_or_else(|| panic!("'{name}' is not a registered design"));
    let mut before = world_with(&built);
    let path = std::env::temp_dir().join(format!("bah_levelgen_golden_{name}.json"));
    before.save_level(&path).expect("saves");

    let mut after = World::new();
    after.load_level(&path).expect("reloads");
    let _ = std::fs::remove_file(&path);

    let verdict = roundtrip_check(&built, &after);
    assert!(verdict.starts_with("round trip: OK"), "{name}: {verdict}");

    let sizes = component_sizes(&mut after);
    assert_eq!(
        component_sizes(&mut before),
        sizes,
        "{name}: nav differs between the in-memory level and the file it saved"
    );
    let expected = EXPECTED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .expect("checked by every_design_has_a_golden_test");
    assert_eq!(
        sizes, expected,
        "{name}: walkable components (cells each, largest first) changed"
    );
}

/// The harness may overwrite its own output and an explicitly named quick slot, never a
/// level an author saved.
#[test]
fn the_harness_never_overwrites_an_authored_level() {
    let dir = std::env::temp_dir().join("bah_levelgen_refuse_foreign");
    std::fs::create_dir_all(&dir).unwrap();
    let write = |file: &str, name: &str| {
        let p = dir.join(file);
        std::fs::write(&p, format!("{{\"name\": {name:?}}}")).unwrap();
        p
    };

    let authored = write("my_base.json", "My Base");
    assert!(refuse_foreign(&authored, "grand").is_err(), "an authored level is refused");

    let own = write("levelgen_grand.json", &generated_name("grand"));
    assert!(refuse_foreign(&own, "grand").is_ok(), "its own previous output is fine");
    assert!(
        refuse_foreign(&own, "arena").is_err(),
        "another design's output is not this design's to replace"
    );

    let slot = write("slot7.json", "anything");
    assert!(refuse_foreign(&slot, "grand").is_ok(), "an explicitly named slot is fine");

    assert!(refuse_foreign(&dir.join("absent.json"), "grand").is_ok(), "a new file is fine");
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── The analyzer ──────────────────────────────────────────────────────────────

use super::builder::LevelBuilder;
use engine::geometry::csg_runtime::{Axis, Side, StairDir};

/// Run the whole analysis on a built level, the way the harness does (minus the file).
fn analyze(built: &BuiltLevel) -> analyze::ReportData {
    let mut w = world_with(built);
    let nav = w.bake_level_nav().expect("bakes");
    w.calculate_nav_issues();
    let issues = w.nav_issues().expect("findings");
    analyze::Analysis::new("test", &nav, &w, built, issues).data()
}

fn check<'a>(d: &'a analyze::ReportData, name: &str) -> &'a analyze::Check {
    d.checks.iter().find(|c| c.check == name).expect("check exists")
}

/// Two rooms whose air boxes share a face have no wall between them — one space. The
/// report says so when nobody declared it; a 1 WT gap keeps a wall and says nothing.
#[test]
fn rooms_sharing_a_face_are_reported_as_merged() {
    let mut b = LevelBuilder::new();
    b.room("west", 0.0, 0.0, 12.0, 12.0, 0.0, 12.0);
    b.room("east", 12.0, 0.0, 12.0, 12.0, 0.0, 12.0); // x=12: flush against west
    b.spawn_wt(6.0, 0.0, 6.0);
    let d = analyze(&b.finish());
    assert_eq!(d.merged, vec![["west".to_string(), "east".to_string()]]);
    assert_eq!(check(&d, "merged rooms").status, analyze::Status::Warn);

    let mut b = LevelBuilder::new();
    b.room("west", 0.0, 0.0, 12.0, 12.0, 0.0, 12.0);
    b.room("east", 13.0, 0.0, 12.0, 12.0, 0.0, 12.0); // a 1 WT wall survives
    b.spawn_wt(6.0, 0.0, 6.0);
    assert!(analyze(&b.finish()).merged.is_empty());
}

/// Loops are counted with corridors as nodes: two parallel halls between the same two
/// rooms are a real second route; one corridor serving three rooms is not a loop.
#[test]
fn loops_count_parallel_halls_and_not_shared_corridors() {
    let mut b = LevelBuilder::new();
    let a = b.room("a", 0.0, 0.0, 12.0, 16.0, 0.0, 12.0);
    let c = b.room("c", 20.0, 0.0, 12.0, 16.0, 0.0, 12.0);
    b.passage(a, c, 10.0, 2.0, 12.0, 4.0, 0.0, 10.0);
    b.passage(a, c, 10.0, 10.0, 12.0, 4.0, 0.0, 10.0);
    b.spawn_wt(6.0, 0.0, 6.0);
    assert_eq!(analyze(&b.finish()).loops, 1, "two halls between a and c = one loop");

    // A corridor along the top touching three rooms that are otherwise walled apart.
    let mut b = LevelBuilder::new();
    for (name, x) in [("r1", 0.0), ("r2", 12.0), ("r3", 24.0)] {
        b.room(name, x, 6.0, 10.0, 10.0, 0.0, 12.0);
        b.void(x + 3.0, 3.0, 4.0, 4.0, 0.0, 10.0); // a door from the corridor into it
    }
    b.void(0.0, 0.0, 34.0, 4.0, 0.0, 10.0); // the corridor, z 0..4
    b.spawn_wt(5.0, 0.0, 10.0);
    let d = analyze(&b.finish());
    assert_eq!(d.loops, 0, "a shared corridor is a hub, not a loop");
    assert!(d.rooms.iter().all(|r| r.degree == 2), "each room reaches the other two");
}

/// A deck overlooks the room it stands in when sighted from its edge at the player's
/// eye. The first analyzer sighted from 0.4 m above the deck's centre and reported
/// wide mezzanines as overlooking nothing.
#[test]
fn a_perch_is_sighted_from_its_edge_at_eye_height() {
    let mut b = LevelBuilder::new();
    let hall = b.room("hall", 0.0, 0.0, 40.0, 40.0, 0.0, 24.0);
    b.platform("deck", 0.0, 0.0, 40.0, 12.0, 12.0, false);
    let deck = b.last_room();
    b.link(hall, deck);
    b.spawn_wt(20.0, 0.0, 30.0);
    let d = analyze(&b.finish());
    let perch = d.perches.iter().find(|p| p.name == "deck").expect("the deck is a perch");
    let o = perch.overlooks.iter().find(|o| o.room == "hall").expect("it sees the hall");
    assert!(
        o.seen as f32 / o.total as f32 > 0.5,
        "a 12 WT deck along one wall sees most of a 40×40 hall ({} / {})",
        o.seen,
        o.total
    );
}

/// Stair treads are steps, not floors: a room with a CSG stair down to a basement has
/// two floors, not one per tread.
#[test]
fn stair_treads_are_not_floors() {
    let mut b = LevelBuilder::new();
    let top = b.room("top", 0.0, 0.0, 24.0, 20.0, 0.0, 14.0);
    let low = b.room("low", 0.0, 27.0, 24.0, 20.0, -6.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 20.0, 8.0, 14.0, 0.0, 10.0, StairDir::Down, 6);
    b.link(top, low);
    b.spawn_wt(12.0, 0.0, 10.0);
    let d = analyze(&b.finish());
    let ys: Vec<i32> = d.floors.iter().map(|f| f.y).collect();
    assert_eq!(ys, vec![-6, 0], "two floors; the six treads between them are steps");
    assert_eq!(check(&d, "reachable").status, analyze::Status::Pass, "{:?}", check(&d, "reachable"));
}

/// A deck with too little headroom above it has no standable floor at all — a
/// different finding from a deck that is merely cut off, with a different fix.
#[test]
fn a_deck_under_a_low_ceiling_has_no_floor_rather_than_no_route() {
    let d = analyze(&designs::smoke());
    let perch = d.rooms.iter().find(|r| r.name == "perch_b").unwrap();
    assert_eq!(perch.cells, 0, "5 WT deck in an 8 WT room leaves 3 WT of headroom");
    let reach = check(&d, "reachable");
    assert_eq!(reach.status, analyze::Status::Fail);
    assert!(reach.detail.contains("no standable floor"), "{}", reach.detail);
}
