//! Concrete level designs authored with the [`builder`](super::builder) API.
//! These are the "prompts made geometry": an author writes one of these, runs
//! the harness, reads the report, and edits until the metrics are good.

use engine::geometry::csg_runtime::{Axis, Side, StairDir};
use engine::geometry::structures::Edge;

use super::builder::{BuiltLevel, LevelBuilder, RoomId};

/// Minimal pipeline check: two rooms, an open doorway, one overlook platform +
/// stair, a spawn. Exists to prove build→bake→report→slot end to end.
pub fn smoke() -> BuiltLevel {
    let mut b = LevelBuilder::new();
    let a = b.room("room_a", 0.0, 0.0, 12.0, 12.0, 0.0, 8.0);
    let c = b.room("room_b", 16.0, 0.0, 12.0, 12.0, 0.0, 8.0);
    // Doorway straddling the x=12..16 wall gap.
    b.passage(a, c, 10.0, 4.0, 8.0, 4.0, 0.0, 6.0);

    let perch = b.platform("perch_b", 17.0, 2.0, 6.0, 6.0, 5.0, true);
    // Stair from room_b floor up to the perch's ZMax edge.
    b.stair_to_platform((20.0, 0.0, 11.0), perch, Edge::ZMax, 0.5, 4.0, true);

    b.spawn_wt(6.0, 0.5, 6.0);
    b.finish()
}

/// A varied two-floor compound: mixed room sizes/shapes (tall atrium, large
/// warehouse, long-skinny gallery, small closets, a medium fight room) wired
/// into loops, with a grand stair up to a landing that both overlooks the atrium
/// AND leads through a doorway onto a real second floor. Direct response to
/// playtest feedback (see LEVEL_DESIGN_HEURISTICS.md): variety, tall ceilings,
/// roomy stairs, platforms that go somewhere.
pub fn varied() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // ---- Ground floor (y=0), deliberately varied footprints + tall ceilings ----
    // Central tall atrium (the big space; the grand stair lives here).
    let atrium = b.room("atrium", 0.0, 0.0, 26.0, 24.0, 0.0, 26.0);
    // Large warehouse to the west (4 m ceiling).
    let warehouse = b.room("warehouse", -30.0, -2.0, 26.0, 26.0, 0.0, 16.0);
    // Long-skinny gallery to the east — a sightline lane.
    let gallery = b.room("gallery", 28.0, -6.0, 6.0, 34.0, 0.0, 14.0);
    // Two small closets to the north (camp spots).
    let closet_a = b.room("closet_a", 2.0, -12.0, 8.0, 8.0, 0.0, 12.0);
    let closet_b = b.room("closet_b", 14.0, -12.0, 8.0, 8.0, 0.0, 12.0);
    // Medium fight room to the south.
    let fight = b.room("fight_room", 4.0, 26.0, 16.0, 14.0, 0.0, 14.0);

    // Spokes to the atrium (generous overlap into both rooms for robust nav).
    b.passage(atrium, warehouse, -6.0, 8.0, 12.0, 8.0, 0.0, 10.0);
    b.passage(atrium, gallery, 24.0, 4.0, 6.0, 8.0, 0.0, 10.0);
    b.passage(atrium, fight, 8.0, 22.0, 8.0, 6.0, 0.0, 8.0);
    b.passage(atrium, closet_a, 3.0, -6.0, 6.0, 8.0, 0.0, 8.0);
    b.passage(atrium, closet_b, 15.0, -6.0, 6.0, 8.0, 0.0, 8.0);
    // Perimeter links → loops (multiple routes, fewer dead-ends).
    b.passage(closet_a, closet_b, 8.0, -10.0, 8.0, 4.0, 0.0, 8.0); // closet loop
    b.passage(warehouse, closet_a, -6.0, -8.0, 10.0, 8.0, 0.0, 8.0); // NW loop
    b.passage(gallery, fight, 18.0, 26.0, 12.0, 6.0, 0.0, 8.0); // SE loop

    // Cover in the big open spaces (full floor-to-ceiling pillars).
    b.pillar_in(atrium, 6.0, 6.0, 2.0);
    b.pillar_in(atrium, 18.0, 16.0, 2.0);
    b.pillar_in(warehouse, -20.0, 8.0, 3.0);
    b.pillar_in(warehouse, -12.0, 18.0, 3.0);

    // ---- Second floor (y=13) reached by the stair->landing->door chain ----
    // Upper room carved above the north closets (floor at y=13).
    let upper = b.room("upper_hall", 2.0, -14.0, 20.0, 12.0, 13.0, 12.0);

    // Landing platform: a generous 10×8 deck at y=13 in the atrium's NE corner.
    // It overlooks the atrium floor (drop 13) AND is the stair's top + the door
    // to the upper floor. The stair is kept on the east wall (below) so it never
    // rises through the landing's sightline west/south across the atrium.
    let landing = b.platform("landing", 16.0, 0.0, 10.0, 8.0, 13.0, false);
    let landing_room = b.last_room();

    // Doorway from the landing into the upper room (through the z=0 wall at y=13).
    // Records the atrium<->upper edge for the graph.
    b.passage(atrium, upper, 16.0, -2.0, 6.0, 4.0, 13.0, 7.0);

    // Grand stair up the atrium's east side: floor -> landing ZMax edge. Rise 13
    // over 13 WT run (1:1 slope, nav-walkable), width 5.
    b.stair_to_platform((21.0, 0.0, 21.0), landing, Edge::ZMax, 0.5, 5.0, true);
    b.link(atrium, landing_room); // stair = the landing's connection to the hall

    b.spawn_wt(13.0, 0.5, 12.0);
    b.finish()
}

/// The step-up from `showcase`: a genuinely **huge** hero hall (56×48, 7.5 m
/// ceiling) with a **three-level interior** — a sunken central pit, the main
/// floor, and a mezzanine + catwalk that bridges *over* the pit — plus per-room
/// texture schemes, a sniper window, a headroom-safe CSG down-stair, and a loop.
/// Applies the split-level + go-bigger lessons from the handcrafted slot 1.
pub fn grand() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // ---- Huge hero hall (scheme 0) ----
    b.set_scheme(0);
    let hall = b.room("grand_hall", 0.0, 0.0, 56.0, 48.0, 0.0, 30.0);
    // Sunken central arena pit (16×16, 4 WT deep), reached by a free-standing
    // landing deck + a stair down onto it (no CSG stairwell = no closing-wall
    // geometry to float in the open pit). The whole descent lives inside the pit
    // footprint so no step buries in the solid main floor.
    b.pit(20.0, 16.0, 16.0, 16.0, 0.0, 4.0); // pit floor at y=-4
    // Free-standing stair straight down onto the pit floor (blue platform stair —
    // clean, no CSG closing wall). 1:1, whole descent inside the pit footprint.
    // Anchor at y=-5 so the lowest tread lands flush ON the y=-4 pit floor (a
    // stair-run's lowest tread sits one step above its ground anchor).
    b.stair_ground((20.0, 0.0, 24.0), (25.0, -5.0, 24.0), 4.0, false);

    // ---- West: armory (scheme 1) ----
    b.set_scheme(1);
    let armory = b.room("armory", -30.0, 4.0, 28.0, 26.0, 0.0, 16.0);
    b.passage(hall, armory, -4.0, 12.0, 8.0, 10.0, 0.0, 10.0);

    // ---- East: quarters (scheme 2) + a raised sniper window ----
    b.set_scheme(2);
    let quarters = b.room("quarters", 58.0, 6.0, 24.0, 22.0, 0.0, 14.0);
    b.passage(hall, quarters, 54.0, 12.0, 8.0, 10.0, 0.0, 10.0);
    // Window: a thin 4-WT slot straddling only the x=56..58 wall, in the north bay
    // (z8..11) — clear of the door (z12..22) and both perpendicular walls (z6/z28).
    b.window(55.0, 5.0, 8.0, 4.0, 4.0, 3.0);

    // ---- North: long gallery (scheme 3) + NW loop back to the armory ----
    b.set_scheme(3);
    let gallery = b.room("gallery", 8.0, -26.0, 40.0, 10.0, 0.0, 13.0);
    b.passage(hall, gallery, 16.0, -18.0, 8.0, 20.0, 0.0, 10.0);
    b.void(-8.0, -20.0, 5.0, 26.0, 0.0, 10.0); // armory -> north
    b.void(-8.0, -20.0, 22.0, 4.0, 0.0, 10.0); // -> east to gallery
    b.link(armory, gallery);

    // ---- South: basement (scheme 4) via a headroom-safe CSG down-stair ----
    b.set_scheme(4);
    let basement = b.room("basement", 8.0, 54.0, 40.0, 22.0, -6.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 48.0, 24.0, 30.0, 0.0, 10.0, StairDir::Down, 6);
    b.link(hall, basement);

    // ---- Upper layer: mezzanine balcony (flush to the N/E/W walls) + a catwalk
    //      that bridges wall-to-wall over the pit (no cantilever into space) ----
    let mezz = b.platform("mezzanine", 0.0, 0.0, 56.0, 12.0, 14.0, false);
    let mezz_room = b.last_room();
    // z12..48: from the mezzanine across the pit to the south wall.
    let _catwalk = b.platform("catwalk", 24.0, 12.0, 8.0, 36.0, 14.0, false);
    let catwalk_room = b.last_room();
    b.stair_to_platform((44.0, 0.0, 26.0), mezz, Edge::ZMax, 0.8, 6.0, true);
    b.link(hall, mezz_room);
    b.link(mezz_room, catwalk_room);

    // ===== UPPER WING: mezzanine -> door -> north_loft -> up-stair -> attic =====
    // Walk off the mezzanine (y=14) through a door in the hall's north wall into a
    // loft, then a CSG up-stair a flight higher to an attic.
    b.set_scheme(2);
    let north_loft = b.room("north_loft", 8.0, -16.0, 32.0, 16.0, 14.0, 12.0);
    b.passage(mezz_room, north_loft, 18.0, -2.0, 6.0, 4.0, 14.0, 7.0); // door off the deck
    b.set_scheme(3);
    let attic = b.room("attic", 12.0, -36.0, 24.0, 12.0, 22.0, 10.0);
    b.csg_stair(Axis::Z, Side::Min, -16.0, 16.0, 24.0, 14.0, 26.0, StairDir::Up, 8);
    b.link(north_loft, attic);

    // ===== LOWER WING: hole in the armory floor -> undercroft -> platform stair =====
    // A large room under the armory, entered through a floor hole + a free-standing
    // staircase down (the slot-1 move). Player-walkable; enemy-nav measured below.
    b.set_scheme(4);
    let undercroft = b.room("undercroft", -28.0, 6.0, 24.0, 22.0, -12.0, 10.0);
    b.window(-22.0, -2.0, 12.0, 8.0, 2.0, 10.0); // cut the hole through the floor slab
    b.stair_ground((-18.0, 0.0, 16.0), (-5.0, -13.0, 16.0), 4.0, false);
    b.link(armory, undercroft);

    // ---- Cover: thin full-height pillars on the main floor (scheme 0) ----
    b.set_scheme(0);
    b.pillar_in(hall, 8.0, 40.0, 2.0);
    b.pillar_in(hall, 48.0, 40.0, 2.0);

    b.spawn_wt(10.0, 0.5, 40.0);
    b.finish()
}

/// Built from the handcrafted slot-1 study: a genuinely **big** hero hall (44×36,
/// 7 m ceiling) with layered verticality (a mezzanine + a catwalk bridging over
/// the floor), **per-room texture schemes**, a **sniper window** between rooms, a
/// CSG down-stair with proper headroom (no head-bump), and varied side rooms.
pub fn showcase() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // ---- Hero hall: large + tall (scheme 0). Spawn here. ----
    b.set_scheme(0);
    let hall = b.room("grand_hall", 0.0, 0.0, 44.0, 36.0, 0.0, 28.0);

    // ---- West: armory (scheme 1) ----
    b.set_scheme(1);
    let armory = b.room("armory", -28.0, 0.0, 26.0, 24.0, 0.0, 16.0);
    b.passage(hall, armory, -4.0, 8.0, 8.0, 10.0, 0.0, 10.0);

    // ---- East: quarters (scheme 2) + a raised sniper WINDOW into the hall ----
    b.set_scheme(2);
    let quarters = b.room("quarters", 46.0, 4.0, 22.0, 20.0, 0.0, 14.0);
    b.passage(hall, quarters, 42.0, 8.0, 8.0, 10.0, 0.0, 10.0);
    b.window(42.0, 5.0, 20.0, 8.0, 4.0, 4.0); // sill y=5, offset from the door

    // ---- North: long gallery (scheme 3) ----
    b.set_scheme(3);
    let gallery = b.room("gallery", 4.0, -24.0, 36.0, 10.0, 0.0, 13.0);
    b.passage(hall, gallery, 12.0, -16.0, 8.0, 18.0, 0.0, 10.0);
    // NW loop: an L-corridor armory -> gallery so the west side isn't a dead-end
    // (hall -> armory -> corridor -> gallery -> hall).
    b.void(-10.0, -18.0, 5.0, 20.0, 0.0, 10.0); // north out of the armory
    b.void(-10.0, -18.0, 18.0, 4.0, 0.0, 10.0); // east across to the gallery
    b.link(armory, gallery);

    // ---- Basement (scheme 4) via a CSG down-stair with GENEROUS headroom ----
    // ceil=10 => 10 WT (2.5 m) of clearance at the top of the steps — no bump.
    b.set_scheme(4);
    let basement = b.room("basement", 4.0, 42.0, 36.0, 20.0, -6.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 36.0, 18.0, 24.0, 0.0, 10.0, StairDir::Down, 6);
    b.link(hall, basement);

    // ---- Layered verticality in the hall: mezzanine balcony + a catwalk bridge,
    //      reached by a grand free-standing stair (platforms self-texture) ----
    let mezz = b.platform("mezzanine", 2.0, 2.0, 40.0, 10.0, 12.0, false);
    let mezz_room = b.last_room();
    let _catwalk = b.platform("catwalk", 20.0, 12.0, 4.0, 18.0, 12.0, false);
    let catwalk_room = b.last_room();
    b.stair_to_platform((32.0, 0.0, 26.0), mezz, Edge::ZMax, 0.75, 6.0, true);
    b.link(hall, mezz_room);
    b.link(mezz_room, catwalk_room); // the catwalk extends off the mezzanine

    // ---- Thin full-height cover pillars in the hall (scheme 0) ----
    b.set_scheme(0);
    b.pillar_in(hall, 8.0, 26.0, 2.0);
    b.pillar_in(hall, 36.0, 4.0, 2.0);

    b.spawn_wt(22.0, 0.5, 18.0);
    b.finish()
}

/// A long, **linear** descending snake: ~10 rooms in a single chain with no
/// branches, the path turning 90° and dropping a flight (CSG down-stair) at three
/// points — a switchback that descends from y=0 to y=−18 over its length. Meant
/// as a skeleton to grow offshoots from later. East along the top, down + south,
/// west, down + south, west, down + south, east along the bottom.
pub fn linear() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // ---- Top run (y=0): entry -> hall_a -> junction, heading east ----
    let entry = b.room("r01_entry", 0.0, 0.0, 14.0, 12.0, 0.0, 12.0);
    let hall_a = b.room("r02_hall_a", 22.0, 0.0, 14.0, 12.0, 0.0, 12.0);
    let junction = b.room("r03_junction", 44.0, 0.0, 14.0, 12.0, 0.0, 12.0);
    b.passage(entry, hall_a, 12.0, 4.0, 12.0, 4.0, 0.0, 8.0);
    b.passage(hall_a, junction, 34.0, 4.0, 12.0, 4.0, 0.0, 8.0);

    // ---- Descent 1: turn south + down a flight into y=−6 ----
    let lower_a = b.room("r04_lower_a", 44.0, 18.0, 14.0, 14.0, -6.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 12.0, 48.0, 53.0, 0.0, 12.0, StairDir::Down, 6);
    b.link(junction, lower_a);

    // ---- Middle run (y=−6): lower_a -> lower_b, heading west ----
    let lower_b = b.room("r05_lower_b", 22.0, 20.0, 14.0, 12.0, -6.0, 12.0);
    b.passage(lower_a, lower_b, 34.0, 22.0, 12.0, 4.0, -6.0, 8.0);

    // ---- Descent 2: turn south + down a flight into y=−12 ----
    let lower_c = b.room("r06_lower_c", 20.0, 38.0, 16.0, 14.0, -12.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 32.0, 24.0, 29.0, -6.0, 6.0, StairDir::Down, 6);
    b.link(lower_b, lower_c);

    // ---- Run (y=−12): lower_c -> vault_a, heading west ----
    let vault_a = b.room("r07_vault_a", -4.0, 40.0, 14.0, 12.0, -12.0, 12.0);
    b.passage(lower_c, vault_a, 8.0, 42.0, 14.0, 4.0, -12.0, 8.0);

    // ---- Descent 3: turn south + down a flight into y=−18 ----
    let deep_a = b.room("r08_deep_a", -8.0, 58.0, 16.0, 14.0, -18.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 52.0, -2.0, 3.0, -12.0, 0.0, StairDir::Down, 6);
    b.link(vault_a, deep_a);

    // ---- Bottom run (y=−18): deep_a -> deep_b -> exit, heading east ----
    let deep_b = b.room("r09_deep_b", 14.0, 60.0, 14.0, 12.0, -18.0, 12.0);
    let exit = b.room("r10_exit", 34.0, 58.0, 16.0, 14.0, -18.0, 12.0);
    b.passage(deep_a, deep_b, 6.0, 62.0, 10.0, 4.0, -18.0, 8.0);
    b.passage(deep_b, exit, 26.0, 62.0, 10.0, 4.0, -18.0, 8.0);

    b.spawn_wt(7.0, 0.5, 6.0);
    b.finish()
}

/// A large, full multi-wing / multi-floor "facility" — the everything level.
/// Uses the whole toolkit: a huge tall central atrium, four sprawling wings with
/// varied room sizes/shapes, several loops, a free-standing grand stair up to a
/// mezzanine perch, a CSG stair that climbs a flight to an upper room, a CSG
/// stair that descends a flight to a basement, and full-height cover pillars.
/// Three floors: basement (y=-8), ground (y=0), upper (y=8..10).
pub fn facility() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // ================= GROUND FLOOR (y=0) =================
    // Huge tall central atrium (spans all three floors).
    let atrium = b.room("atrium", 0.0, 0.0, 34.0, 30.0, 0.0, 40.0);

    // ---- West wing: atrium -> armory (large) -> closet (small) ----
    let armory = b.room("armory", -42.0, 0.0, 26.0, 26.0, 0.0, 16.0);
    let closet_w = b.room("closet_w", -38.0, -14.0, 12.0, 14.0, 0.0, 12.0);
    b.passage(atrium, armory, -18.0, 10.0, 20.0, 6.0, 0.0, 10.0);
    b.passage(armory, closet_w, -34.0, -14.0, 6.0, 16.0, 0.0, 10.0);

    // ---- East wing: atrium =DOUBLE hall= east_hall (large) -> gallery (skinny) ----
    let east_hall = b.room("east_hall", 48.0, -2.0, 26.0, 24.0, 0.0, 18.0);
    let gallery = b.room("gallery", 50.0, 22.0, 26.0, 6.0, 0.0, 13.0);
    b.passage(atrium, east_hall, 32.0, 8.0, 18.0, 6.0, 0.0, 10.0); // hall 1
    b.passage(atrium, east_hall, 32.0, 16.0, 18.0, 4.0, 0.0, 10.0); // hall 2 => east loop
    b.passage(east_hall, gallery, 52.0, 20.0, 10.0, 6.0, 0.0, 8.0);

    // ---- North wing: atrium -> mess (medium) -> kitchen (small) ----
    let mess = b.room("mess", 2.0, -34.0, 24.0, 20.0, 0.0, 14.0);
    let kitchen = b.room("kitchen", 28.0, -30.0, 12.0, 12.0, 0.0, 12.0);
    b.passage(atrium, mess, 10.0, -16.0, 8.0, 18.0, 0.0, 10.0);
    b.passage(mess, kitchen, 22.0, -26.0, 8.0, 6.0, 0.0, 8.0);

    // ---- South wing: atrium -> barracks (medium) -> two bunk closets ----
    let barracks = b.room("barracks", 2.0, 42.0, 26.0, 18.0, 0.0, 14.0);
    let bunk_a = b.room("bunk_a", 4.0, 60.0, 10.0, 10.0, 0.0, 10.0);
    let bunk_b = b.room("bunk_b", 16.0, 60.0, 10.0, 10.0, 0.0, 10.0);
    b.passage(atrium, barracks, 12.0, 28.0, 8.0, 16.0, 0.0, 10.0);
    b.passage(barracks, bunk_a, 5.0, 58.0, 8.0, 6.0, 0.0, 8.0);
    b.passage(barracks, bunk_b, 17.0, 58.0, 8.0, 6.0, 0.0, 8.0);
    b.passage(bunk_a, bunk_b, 12.0, 62.0, 6.0, 6.0, 0.0, 8.0); // bunk loop

    // ---- Big NW loop: armory -> (L corridor along the top) -> mess ----
    b.void(-32.0, -16.0, 6.0, 18.0, 0.0, 8.0); // leg 1 (north out of armory)
    b.void(-32.0, -16.0, 42.0, 6.0, 0.0, 8.0); // leg 2 (east across to mess)
    b.link(armory, mess);

    // ================= UPPER FLOOR (y=8..10) =================
    // CSG stair UP a flight into the east_hall north wall -> upper_east room (y=8).
    let upper_east = b.room("upper_east", 48.0, -24.0, 24.0, 20.0, 8.0, 12.0);
    b.csg_stair(Axis::Z, Side::Min, -2.0, 52.0, 60.0, 0.0, 18.0, StairDir::Up, 8);
    b.link(east_hall, upper_east);

    // Free-standing grand stair up to a mezzanine balcony over the atrium (perch).
    let mezz = b.platform("mezzanine", 2.0, 1.0, 20.0, 6.0, 10.0, false);
    let mezz_room = b.last_room();
    b.stair_to_platform((12.0, 0.0, 20.0), mezz, Edge::ZMax, 0.5, 6.0, true);
    b.link(atrium, mezz_room);

    // ================= BASEMENT (y=-8) =================
    // CSG stair DOWN a flight into the armory south wall -> basement (y=-8).
    let basement = b.room("basement", -42.0, 34.0, 26.0, 22.0, -8.0, 12.0);
    b.csg_stair(Axis::Z, Side::Max, 26.0, -36.0, -28.0, 0.0, 16.0, StairDir::Down, 8);
    b.link(armory, basement);

    // ---- Cover: full-height pillars in the big rooms (auto-appended last) ----
    b.pillar_in(atrium, 6.0, 22.0, 3.0);
    b.pillar_in(atrium, 24.0, 10.0, 3.0);
    b.pillar_in(armory, -30.0, 10.0, 3.0);
    b.pillar_in(armory, -24.0, 18.0, 3.0);
    b.pillar_in(east_hall, 56.0, 6.0, 3.0);
    b.pillar_in(mess, 10.0, -26.0, 2.0);
    b.pillar_in(barracks, 12.0, 50.0, 2.0);

    b.spawn_wt(16.0, 0.5, 15.0);
    b.finish()
}

/// A **sprawling** compound authored the way a human builder wanders: make a
/// room, carve a door, run a hallway to the next room, then go back and branch a
/// new chain off elsewhere. No central hub-and-spoke — the map rambles outward in
/// several directions, with a couple of loops, a big tall atrium, a CSG staircase
/// cut into a wall that climbs a flight to a mezzanine, and full-height cover
/// pillars. Direct response to feedback (LEVEL_DESIGN_HEURISTICS.md).
pub fn sprawl() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // --- Core chain: foyer -> hub -> atrium (east), tall ceilings ---
    let foyer = b.room("foyer", 0.0, 0.0, 16.0, 14.0, 0.0, 14.0);
    let hub = b.room("hub", 26.0, -2.0, 20.0, 18.0, 0.0, 16.0);
    let atrium = b.room("atrium", 56.0, -4.0, 24.0, 24.0, 0.0, 24.0);
    b.passage(foyer, hub, 14.0, 4.0, 14.0, 5.0, 0.0, 8.0); // foyer -> hub hallway
    // Two parallel hub<->atrium halls => a loop (multiple routes east).
    b.passage(hub, atrium, 44.0, 4.0, 14.0, 5.0, 0.0, 8.0);
    b.passage(hub, atrium, 44.0, 10.0, 14.0, 5.0, 0.0, 8.0);

    // --- North branch off the hub: a chain up to a nest ---
    let nest = b.room("nest", 24.0, -30.0, 16.0, 14.0, 0.0, 12.0);
    b.passage(hub, nest, 30.0, -18.0, 5.0, 18.0, 0.0, 8.0);

    // --- West branch off the foyer, then a corner (L) hall down to a cellar:
    //     foyer -> west_wing -> (L corridor) -> cellar -> foyer = a big SW loop ---
    let west_wing = b.room("west_wing", -32.0, -4.0, 18.0, 20.0, 0.0, 16.0);
    let cellar = b.room("cellar", -4.0, 26.0, 18.0, 14.0, 0.0, 12.0);
    b.passage(foyer, west_wing, -16.0, 4.0, 18.0, 5.0, 0.0, 8.0);
    b.passage(foyer, cellar, 4.0, 12.0, 6.0, 16.0, 0.0, 8.0);
    // L corridor west_wing -> cellar (two carved boxes + one logical edge).
    b.void(-30.0, 14.0, 5.0, 16.0, 0.0, 8.0); // vertical leg (down the west side)
    b.void(-30.0, 26.0, 28.0, 5.0, 0.0, 8.0); // horizontal leg (across to cellar)
    b.link(west_wing, cellar); // closes the SW loop

    // --- CSG STAIRCASE: cut into the atrium's north wall, climbing a flight (6 WT)
    //     up to a mezzanine — the up/down-arrow tool, used at last ---
    let mezz = b.room("mezzanine", 56.0, -30.0, 24.0, 20.0, 6.0, 14.0);
    b.csg_stair(Axis::Z, Side::Min, -4.0, 60.0, 66.0, 0.0, 24.0, StairDir::Up, 6);
    b.link(atrium, mezz); // the stair connects the atrium to the upper mezzanine

    // --- Cover: full-height pillars in the big rooms (appended last, carve-proof) ---
    b.pillar_in(atrium, 62.0, 2.0, 3.0);
    b.pillar_in(atrium, 72.0, 12.0, 3.0);
    b.pillar_in(hub, 32.0, 4.0, 2.0);
    b.pillar_in(west_wing, -26.0, 2.0, 3.0);

    b.spawn_wt(8.0, 0.5, 7.0);
    b.finish()
}

/// A 3×3 grid of rooms (center = a tall hall) wired into a grid graph for dense,
/// multi-route flow, plus a balcony perch over the hall reached by a stair — the
/// multiplayer target: many rooms, loops everywhere, verticality, an overlook.
pub fn arena() -> BuiltLevel {
    let mut b = LevelBuilder::new();

    // 3×3 grid: 12 WT rooms on a 14 WT pitch (2 WT walls between).
    const PITCH: f32 = 14.0;
    const SIZE: f32 = 12.0;
    const H: f32 = 8.0;
    let names = [
        ["nw", "n", "ne"],
        ["w", "hall", "e"],
        ["sw", "s", "se"],
    ];
    // Build rooms; hall (center) is tall so it spans to the upper balcony.
    let mut ids: [[Option<RoomId>; 3]; 3] = Default::default();
    for row in 0..3 {
        for col in 0..3 {
            let x = col as f32 * PITCH;
            let z = row as f32 * PITCH;
            let tall = row == 1 && col == 1;
            let height = if tall { 22.0 } else { H };
            ids[row][col] = Some(b.room(names[row][col], x, z, SIZE, SIZE, 0.0, height));
        }
    }

    // Horizontal doorways (connect col↔col+1) and vertical (row↔row+1): the grid
    // graph. Each passage straddles the 2 WT wall gap into both rooms.
    for row in 0..3 {
        for col in 0..2 {
            let a = ids[row][col].unwrap();
            let c = ids[row][col + 1].unwrap();
            let x = col as f32 * PITCH + 10.0; // spans +10..+16 across the wall
            let z = row as f32 * PITCH + 4.0;
            b.passage(a, c, x, z, 6.0, 4.0, 0.0, 6.0);
        }
    }
    for row in 0..2 {
        for col in 0..3 {
            let a = ids[row][col].unwrap();
            let c = ids[row + 1][col].unwrap();
            let x = col as f32 * PITCH + 4.0;
            let z = row as f32 * PITCH + 10.0;
            b.passage(a, c, x, z, 4.0, 6.0, 0.0, 6.0);
        }
    }

    // Cover in the hall (two pillars) for sightline breaks / camp corners.
    let hx = PITCH; // hall min corner = (14,14)
    b.pillar(hx + 2.0, hx + 2.0, 2.0, 0.0, 8.0);
    b.pillar(hx + 8.0, hx + 8.0, 2.0, 0.0, 8.0);

    // Upper balcony over the hall (perch), a thin ledge along the hall's north
    // inner edge at y=9 WT — overlooks the hall floor 9 WT below.
    // x[15..25] z[14..16], top y=9.
    let perch = b.platform("balcony", hx + 1.0, hx, 10.0, 2.0, 9.0, false);

    // A single straight staircase up the hall, 1:1 slope so nav can climb it:
    // rise 9 WT over 9 WT of run (z 25 -> 16), landing on the balcony's ZMax edge.
    let perch_room = b.last_room();
    b.stair_to_platform((hx + 6.0, 0.0, hx + 11.0), perch, Edge::ZMax, 0.5, 4.0, true);
    // The stair is the balcony's connection to the hall (keeps it off dead-ends).
    b.link(ids[1][1].unwrap(), perch_room);

    // Spawn on the hall floor.
    b.spawn_wt(hx + 6.0, 0.5, hx + 6.0);
    b.finish()
}
