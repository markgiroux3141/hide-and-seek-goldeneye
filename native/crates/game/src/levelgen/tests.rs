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
