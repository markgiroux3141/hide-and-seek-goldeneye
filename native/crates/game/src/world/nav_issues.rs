//! Nav validation — the report behind the BUILD **NAV** tab.
//!
//! The question this answers is not "is there a path" (A\* answers that, live) but
//! *why not*, and it exists because the author and the hunters move by two different,
//! independently-correct models: the player is a rapier capsule that collide-and-slides,
//! autosteps 0.25 m, falls any distance and jumps ~0.76 m; a hunter is a cell centre on
//! a 4-connected 0.25 m grid that climbs one cell and cannot jump at all. So "I can walk
//! there" genuinely does not imply "a hunter can", and on the shipping level 15% of the
//! walkable floor turned out to be cut off from the rest — in two chunks the size of
//! rooms — while reading as an AI bug ("some enemies just get lazy").
//!
//! Two classes of finding, kept apart because they have opposite fixes:
//!
//! - **Connectivity** — nobody can get there. Islands, the nearest gap to each, and any
//!   authored object stranded on one.
//! - **Traversability** — they can path it but not walk it. Pinch points (a corridor the
//!   body does not fit down) and player-only climbs.
//!
//! A single "make the grid more permissive" knob would trade one for the other, which is
//! why the report keeps them separate and fixes nothing itself. See
//! `DESIGN_NAV_VALIDATION.md`.

use super::*;
use engine::sim::nav::{NavClimb, NavGap, NavPinch, NavWorld};

/// How loud a finding is — drives the panel colour and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavSeverity {
    /// A clean verdict, stated out loud. A panel that says nothing is indistinguishable
    /// from a panel that is broken.
    Ok,
    /// Context (counts, the main component) — true, not a problem.
    Info,
    /// Worth looking at; may well be deliberate.
    Warn,
    /// Something in this level cannot be reached or cannot be walked.
    Error,
}

/// One line of the report, for the panel and the log alike.
pub struct NavLine {
    pub text: String,
    pub sev: NavSeverity,
}

/// A walkable component that is not the main one, plus how close it comes to it.
pub struct NavIsland {
    pub id: u32,
    pub cells: usize,
    /// Share of all standable floor, percent.
    pub pct: f32,
    /// The closest approach to the **nearest other component**, and which one that is.
    /// Not "to the main component": islands chain, and measuring against main alone
    /// turns two 0.5 m steps into an unfixable 3.5 m cliff (see
    /// [`NavWorld::nearest_neighbour_gap`]).
    pub gap: Option<NavGap>,
    pub gap_to: Option<u32>,
}

/// An authored object nobody can walk to.
pub struct NavOrphan {
    /// What kind of thing it is ("spawn pad", "pickup", …).
    pub kind: &'static str,
    /// Which one ("PP7", "pad 3") — enough to find it in the level.
    pub label: String,
    pub pos: Vec3,
}

/// Everything the NAV tab knows, computed in one pass off one baked grid.
pub struct NavIssues {
    /// Wall-clock for the whole Calculate, ms — the reason this is a button.
    pub calc_ms: f32,
    pub total_cells: usize,
    /// Number of walkable components (1 = nothing is cut off).
    pub components: usize,
    /// Which component is "the level" (the largest).
    pub main: Option<u32>,
    pub main_cells: usize,
    pub islands: Vec<NavIsland>,
    pub orphans: Vec<NavOrphan>,
    /// Pads `prepare_spawn` will silently drop at G, by pool index — the exclusion is
    /// what stopped a 10 fps playtest, and it overrules a placement the author made on
    /// purpose, so the panel says so rather than leaving it in the log.
    pub excluded_pads: Vec<(usize, Vec3)>,
    /// Total cells in a corridor too narrow for a hunter's body…
    pub pinch_cells: usize,
    /// …a spread of representative spots (one per 2 m), narrowest first.
    pub pinches: Vec<NavPinch>,
    pub narrowest: f32,
    /// Doors that mark **no** nav cells, by index — invisible to pathing, so hunters
    /// walk straight through them and the door system never fires.
    pub blind_doors: Vec<usize>,
    /// Total doors attached to the grid, and the fewest cells any one of them marks.
    /// A door hanging on one or two cells is a door a body can be beside without the
    /// segment test ever sampling it.
    pub door_count: usize,
    pub thinnest_door: usize,
    /// Total player-only climbs, and how many of them would reconnect a cut-off area.
    pub climb_edges: usize,
    pub joining_climbs: usize,
    /// Representative climbs, the reconnecting ones first.
    pub climbs: Vec<NavClimb>,
}

/// A corridor narrower than this cannot hold a hunter's body at all, however well
/// centred: it will grind along both walls for the whole length.
const PINCH_BLOCK_M: f32 = 2.0 * ENEMY_RADIUS;
/// …and one narrower than *this* cannot hold it **on the grid line**. Nav walks cell
/// centres, so the closest a hunter's centre ever comes to a wall is half a cell; add
/// the body's own half-width and that is the width below which the model clips wall
/// geometry even though A\* is happy. This is the "tight hallways where they stop
/// moving" half of the report, and it is derived rather than tuned.
const PINCH_TIGHT_M: f32 = 2.0 * (ENEMY_RADIUS + nav::cell_size_m() * 0.5);
/// Spacing (m) between reported representatives — a long tight corridor is one problem,
/// not four hundred findings.
const CLUSTER_M: f32 = 2.0;
/// How many representatives of each traversability class to list.
const REPRESENTATIVES: usize = 6;

/// Keep one item per [`CLUSTER_M`] cube, in the order given (so sort first by whatever
/// makes the best representative). Turns thousands of adjacent cells into a short list
/// of places to fly to.
fn cluster<T: Copy>(items: &[T], pos: impl Fn(&T) -> Vec3, limit: usize) -> Vec<T> {
    let mut seen: std::collections::HashSet<(i32, i32, i32)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        let p = pos(it);
        let key = (
            (p.x / CLUSTER_M).floor() as i32,
            (p.y / CLUSTER_M).floor() as i32,
            (p.z / CLUSTER_M).floor() as i32,
        );
        if seen.insert(key) {
            out.push(*it);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// Which spawn pads `prepare_spawn` keeps and which it drops, as **one** rule.
///
/// Pads are grouped by walkable component and the largest group wins: "connected to the
/// level" means "connected to where everyone else is", and the biggest group is the only
/// non-arbitrary way to say which side that is. Returns the winning component and the
/// indices of the pads outside it.
///
/// It lives here, and the runtime calls it, so the panel's "IGNORED AT G" list cannot
/// drift from what G actually does — the failure mode a second copy of this rule would
/// have is a report that is confidently wrong.
pub(crate) fn partition_pads(pads: &[spawn::SpawnPad], nav: &NavWorld) -> (Option<u32>, Vec<usize>) {
    if pads.len() <= 1 {
        return (pads.first().and_then(|p| nav.component_at(p.pos)), Vec::new());
    }
    let comps: Vec<Option<u32>> = pads.iter().map(|p| nav.component_at(p.pos)).collect();
    let mut counts: std::collections::HashMap<Option<u32>, usize> =
        std::collections::HashMap::new();
    for c in &comps {
        *counts.entry(*c).or_insert(0) += 1;
    }
    let main = counts
        .iter()
        .max_by_key(|(c, n)| (**n, c.unwrap_or(0)))
        .map(|(c, _)| *c)
        .unwrap_or(None);
    let dropped = comps
        .iter()
        .enumerate()
        .filter(|(_, c)| **c != main)
        .map(|(i, _)| i)
        .collect();
    (main, dropped)
}

impl World {
    /// Run the whole validation pass and cache it (the NAV tab's **Calculate**).
    ///
    /// Bakes its own grid in BUILD — that is the ~0.5 s this button exists to keep off
    /// every edit — and reuses HUNT's frozen one when there is one, so asking mid-hunt
    /// reports the grid the hunters are actually walking rather than a fresh guess at it.
    pub fn calculate_nav_issues(&mut self) {
        let t0 = Instant::now();
        let (mut issues, overlay) = match self.nav.take() {
            Some(nav) => {
                let out = self.nav_pass(&nav);
                self.nav = Some(nav);
                out
            }
            None => {
                // The same solid set G bakes from: free-standing structures plus placed
                // props, which block hunters even though they are not CSG.
                let mut solids = self.structure_solid_boxes();
                solids.extend(self.prop_solid_boxes());
                let stair_volumes = self.stair_run_solid_boxes();
                match nav::bake(&mut self.regions, &solids, &stair_volumes) {
                    Some(nav) => self.nav_pass(&nav),
                    None => (NavIssues::empty(), ColoredMesh::default()),
                }
            }
        };
        issues.calc_ms = t0.elapsed().as_secs_f32() * 1000.0;
        // The full report at info, one line at warn. "The panel is for fixing; the log is
        // for noticing" — twenty warn lines per button press trains you to ignore them,
        // and the detail is on screen anyway.
        let lines = issues.lines();
        log::info!(
            "nav validation:\n{}",
            lines
                .iter()
                .map(|l| format!("  {}", l.text))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let faults = lines.iter().filter(|l| l.sev == NavSeverity::Error).count();
        if faults > 0 {
            log::warn!("nav: {faults} problem(s) found — see O → NAV for what and where");
        }
        self.nav_overlay = Some(overlay);
        self.nav_issues = Some(issues);
        self.nav_overlay_rev += 1;
    }

    /// Findings + overlay off **one** grid. Both branches above go through it so BUILD
    /// pays for the bake once rather than once per output.
    fn nav_pass(&self, nav: &NavWorld) -> (NavIssues, ColoredMesh) {
        let issues = self.collect_nav_issues(nav);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        Self::push_nav_overlay(&mut vertices, &mut indices, nav, &issues);
        (issues, ColoredMesh { vertices, indices })
    }

    /// The cached findings, or `None` until Calculate has run.
    pub fn nav_issues(&self) -> Option<&NavIssues> {
        self.nav_issues.as_ref()
    }

    /// Whether the walkable-component overlay is being drawn.
    pub fn nav_overlay_on(&self) -> bool {
        self.nav_overlay_on
    }

    /// Show/hide the overlay. Independent of Calculate on purpose: you leave it on,
    /// edit the geometry, and re-Calculate to see whether the island closed.
    pub fn toggle_nav_overlay(&mut self) {
        self.nav_overlay_on = !self.nav_overlay_on;
        self.nav_overlay_rev += 1;
    }

    /// The overlay mesh, or `None` when it is off or nothing has been calculated.
    pub fn nav_overlay_mesh(&self) -> Option<&ColoredMesh> {
        self.nav_overlay_on.then(|| self.nav_overlay.as_ref()).flatten()
    }

    /// Bumped whenever the overlay changes. The mesh runs to ~90k vertices, so the app
    /// uploads on a revision change rather than every frame.
    pub fn nav_overlay_rev(&self) -> u32 {
        self.nav_overlay_rev
    }

    /// One text block for the headless tools (`profile_hunt`) and the `G` log.
    pub fn nav_issue_report(&mut self) -> String {
        if self.nav_issues.is_none() {
            self.calculate_nav_issues();
        }
        self.nav_issues
            .as_ref()
            .map(|i| {
                i.lines()
                    .iter()
                    .map(|l| {
                        let tag = match l.sev {
                            NavSeverity::Error => "!! ",
                            NavSeverity::Warn => " ! ",
                            _ => "   ",
                        };
                        format!("{tag}{}", l.text)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// Everything, off one grid. Split out so both branches of `calculate_nav_issues`
    /// share it (and so it only needs `&self`).
    fn collect_nav_issues(&self, nav: &NavWorld) -> NavIssues {
        let sizes = nav.component_sizes();
        let total: usize = sizes.iter().map(|(_, n)| n).sum();
        let main = nav.main_component();
        let main_cells = sizes.first().map(|(_, n)| *n).unwrap_or(0);

        let islands: Vec<NavIsland> = sizes
            .iter()
            .skip(1)
            .map(|(id, n)| {
                let near = nav.nearest_neighbour_gap(*id);
                NavIsland {
                    id: *id,
                    cells: *n,
                    pct: *n as f32 / total.max(1) as f32 * 100.0,
                    gap: near.map(|(_, g)| g),
                    gap_to: near.map(|(c, _)| c),
                }
            })
            .collect();

        // ── Orphans: authored things standing where nothing can reach them ──
        // A gun nobody can fetch is a level bug the pickups feature made possible, and
        // it is invisible in play — the hunter simply never goes for it.
        let mut orphans = Vec::new();
        let stranded = |p: Vec3| main.is_some() && nav.component_at(p) != main;
        let pads = self.authored_spawn_pads();
        for (i, p) in pads.iter().enumerate() {
            if stranded(p.pos) {
                orphans.push(NavOrphan {
                    kind: "spawn pad",
                    label: format!("pad {i}"),
                    pos: p.pos,
                });
            }
        }
        // Each query is collected before the `stranded` probe: the ECS borrow is live for
        // the length of an `iter()`, and `component_at` reads `&self`.
        let pickups: Vec<(Vec3, &'static str)> = self
            .ecs
            .world()
            .query::<(&crate::ecs::Transform, &crate::ecs::Pickup)>()
            .iter()
            .map(|(t, p)| (t.pos, p.weapon))
            .collect();
        for (pos, weapon) in pickups {
            if stranded(pos) {
                orphans.push(NavOrphan {
                    kind: "pickup",
                    label: weapon.to_string(),
                    pos,
                });
            }
        }
        let turrets: Vec<Vec3> = self
            .ecs
            .world()
            .query::<(&crate::ecs::Transform, &crate::ecs::Turret)>()
            .iter()
            .map(|(t, _)| t.pos)
            .collect();
        for pos in turrets {
            if stranded(pos) {
                orphans.push(NavOrphan {
                    kind: "sentry gun",
                    label: "turret".into(),
                    pos,
                });
            }
        }
        let doors: Vec<Vec3> = self
            .ecs
            .world()
            .query::<(&crate::ecs::Transform, &crate::ecs::Door)>()
            .iter()
            .map(|(t, _)| t.pos)
            .collect();
        for pos in doors {
            if stranded(pos) {
                orphans.push(NavOrphan {
                    kind: "door",
                    label: "door".into(),
                    pos,
                });
            }
        }

        // The pads G will drop, by the runtime's own rule (snapped first, exactly as
        // `prepare_spawn` snaps them, or a pad a hair off the floor reads as an island).
        let snapped: Vec<spawn::SpawnPad> = pads
            .iter()
            .map(|p| spawn::SpawnPad {
                pos: nav
                    .nearest_standable(p.pos.x, p.pos.y + 0.1, p.pos.z, 16)
                    .unwrap_or(p.pos),
                yaw: p.yaw,
            })
            .collect();
        let (_, dropped) = partition_pads(&snapped, nav);
        let excluded_pads = dropped.iter().map(|&i| (i, snapped[i].pos)).collect();

        // A door whose panel slips between cell centres marks nothing, and is then not a
        // door at all as far as pathing is concerned.
        let blind_doors: Vec<usize> = (0..nav.door_count())
            .filter(|&i| nav.door_cells(i) == 0)
            .collect();

        // ── Traversability ──
        let mut pinch = nav.pinch_points(PINCH_TIGHT_M);
        pinch.sort_by(|a, b| a.width.total_cmp(&b.width));
        let narrowest = pinch.first().map(|p| p.width).unwrap_or(f32::INFINITY);
        let pinch_cells = pinch.len();
        let pinches = cluster(&pinch, |p| p.pos, REPRESENTATIVES);

        let mut climbs = nav.overclimb_edges(crate::character::JUMP_APEX);
        let joining_climbs = climbs.iter().filter(|c| c.joins.is_some()).count();
        let climb_edges = climbs.len();
        // Reconnecting climbs first: those are the ones that are probably a defect.
        climbs.sort_by_key(|c| c.joins.is_none());
        let climbs = cluster(&climbs, |c| c.from, REPRESENTATIVES);

        NavIssues {
            calc_ms: 0.0,
            total_cells: total,
            components: sizes.len(),
            main,
            main_cells,
            islands,
            orphans,
            excluded_pads,
            blind_doors,
            door_count: nav.door_count(),
            thinnest_door: (0..nav.door_count()).map(|i| nav.door_cells(i)).min().unwrap_or(0),
            pinch_cells,
            pinches,
            narrowest,
            climb_edges,
            joining_climbs,
            climbs,
        }
    }

    /// The 3D overlay: one flat square per standable cell, coloured by walkable
    /// component, plus bright markers on the traversability findings.
    ///
    /// This is the part that actually fixes levels. A list of coordinates tells you
    /// there is a hole; seeing the far side of a stairwell come up in a different colour
    /// tells you *which* stairwell, in one glance, from across the room.
    fn push_nav_overlay(
        vertices: &mut Vec<ColorVertex>,
        indices: &mut Vec<u32>,
        nav: &NavWorld,
        issues: &NavIssues,
    ) {
        let main = nav.main_component();
        // Each island gets its own colour, in size order, so the panel's "island comp 4"
        // and the patch on screen are the same thing.
        let mut colors: std::collections::HashMap<u32, [f32; 3]> =
            std::collections::HashMap::new();
        for (rank, isl) in issues.islands.iter().enumerate() {
            colors.insert(isl.id, NAV_ISLAND_COLORS[rank % NAV_ISLAND_COLORS.len()]);
        }
        // Slightly inside the cell, so the cell grid stays legible rather than reading
        // as one flat sheet of colour.
        let half = nav::cell_size_m() * 0.45;
        for (pos, comp) in nav.standable_with_components() {
            let col = if Some(comp) == main {
                NAV_MAIN_COLOR
            } else {
                *colors.get(&comp).unwrap_or(&NAV_ORPHAN_COLOR)
            };
            push_colored_quad_y(vertices, indices, pos + Vec3::Y * 0.02, half, col);
        }
        // Traversability markers ride on top of the floor colour: a small red post at
        // each pinch, a yellow one at each player-only climb.
        for p in &issues.pinches {
            push_colored_box(
                vertices,
                indices,
                p.pos + Vec3::new(-0.08, 0.03, -0.08),
                p.pos + Vec3::new(0.08, 0.75, 0.08),
                NAV_PINCH_COLOR,
            );
        }
        for c in &issues.climbs {
            let col = if c.joins.is_some() { NAV_JOIN_COLOR } else { NAV_CLIMB_COLOR };
            push_colored_box(
                vertices,
                indices,
                c.from + Vec3::new(-0.08, 0.03, -0.08),
                c.from + Vec3::new(0.08, 0.4, 0.08),
                col,
            );
        }
    }
}

/// The main component — "the level".
const NAV_MAIN_COLOR: [f32; 3] = [0.13, 0.72, 0.26];
/// One colour per island, in size order.
const NAV_ISLAND_COLORS: [[f32; 3]; 6] = [
    [0.95, 0.25, 0.85], // magenta
    [0.20, 0.70, 1.00], // cyan
    [1.00, 0.55, 0.10], // orange
    [0.95, 0.90, 0.20], // yellow
    [0.60, 0.35, 1.00], // violet
    [1.00, 0.35, 0.35], // salmon
];
/// More islands than colours — the tail shares one rather than wrapping into "main".
const NAV_ORPHAN_COLOR: [f32; 3] = [0.55, 0.55, 0.58];
/// A corridor the body does not fit down.
const NAV_PINCH_COLOR: [f32; 3] = [1.0, 0.1, 0.1];
/// A player-only climb…
const NAV_CLIMB_COLOR: [f32; 3] = [0.9, 0.85, 0.2];
/// …and one that would reconnect a cut-off area, which is the interesting kind.
const NAV_JOIN_COLOR: [f32; 3] = [1.0, 0.45, 0.0];

impl NavIssues {
    /// A level with no nav grid at all (an empty world) — reported, not hidden.
    fn empty() -> Self {
        NavIssues {
            calc_ms: 0.0,
            total_cells: 0,
            components: 0,
            main: None,
            main_cells: 0,
            islands: Vec::new(),
            orphans: Vec::new(),
            excluded_pads: Vec::new(),
            blind_doors: Vec::new(),
            door_count: 0,
            thinnest_door: 0,
            pinch_cells: 0,
            pinches: Vec::new(),
            narrowest: f32::INFINITY,
            climb_edges: 0,
            joining_climbs: 0,
            climbs: Vec::new(),
        }
    }

    /// Whether the level is clean on connectivity — the verdict the panel leads with.
    pub fn is_connected(&self) -> bool {
        self.components == 1 && self.orphans.is_empty()
    }

    /// The whole report as lines. One function serves the panel and the log so the two
    /// can never say different things about the same level.
    pub fn lines(&self) -> Vec<NavLine> {
        use NavSeverity::*;
        let mut out = Vec::new();
        let mut line = |sev, text: String| out.push(NavLine { text, sev });

        if self.total_cells == 0 {
            line(Error, "no walkable floor at all — nothing baked".into());
            return out;
        }

        // ── Connectivity ──
        if self.components == 1 {
            line(
                Ok,
                format!(
                    "1 walkable component — all {} cells of floor connect",
                    self.total_cells
                ),
            );
        } else {
            line(
                Error,
                format!(
                    "{} walkable components — {:.0}% of the floor is cut off from the rest",
                    self.components,
                    (1.0 - self.main_cells as f32 / self.total_cells as f32) * 100.0
                ),
            );
            line(
                Info,
                format!(
                    "main: {} cells ({:.0}%)",
                    self.main_cells,
                    self.main_cells as f32 / self.total_cells.max(1) as f32 * 100.0
                ),
            );
        }
        for isl in self.islands.iter().take(8) {
            let what = match isl.gap {
                None => "no measurable gap".into(),
                Some(g) => {
                    // Whose side of the gap it is on. Naming the neighbour matters when
                    // it is another island: that is a chain, and the fix is the *other*
                    // island's line, one row down.
                    let near = match isl.gap_to {
                        Some(c) if Some(c) == self.main => "the level".to_string(),
                        Some(c) => format!("island comp {c}"),
                        None => "the rest".to_string(),
                    };
                    let side = if g.rise < 0.0 { "above" } else { "below" };
                    let step = nav::max_step_m() + nav::cell_size_m();
                    // Three readings, and they have three different fixes — which is the
                    // whole reason the gap is reported as flat + rise rather than as one
                    // distance. Saying "one step too tall" about a 3.5 m drop (which an
                    // earlier version of this did) sends the author to fix the wrong thing.
                    if g.flat > nav::cell_size_m() * 2.0 {
                        format!(
                            "{:.1} m from {near} ({:+.2} m rise) at ({:.1}, {:.1}, {:.1}) — \
                             a distance, not a step: it needs a bridge or a floor",
                            g.flat, g.rise, g.from.x, g.from.y, g.from.z
                        )
                    } else if g.rise.abs() <= step + 1e-3 {
                        format!(
                            "{:.2} m {side} {near} at ({:.1}, {:.1}, {:.1}) — one step too \
                             tall for a hunter",
                            g.rise.abs(),
                            g.from.x,
                            g.from.y,
                            g.from.z
                        )
                    } else if g.rise.abs() <= crate::character::JUMP_APEX + 1e-3 {
                        format!(
                            "{:.2} m {side} {near} at ({:.1}, {:.1}, {:.1}) — you jump this, \
                             hunters cannot",
                            g.rise.abs(),
                            g.from.x,
                            g.from.y,
                            g.from.z
                        )
                    } else {
                        format!(
                            "{:.2} m {side} {near} at ({:.1}, {:.1}, {:.1}) — a drop, not a \
                             step: it needs stairs",
                            g.rise.abs(),
                            g.from.x,
                            g.from.y,
                            g.from.z
                        )
                    }
                }
            };
            line(
                Error,
                format!(
                    "island comp {}: {} cells ({:.1}%) — {what}",
                    isl.id, isl.cells, isl.pct
                ),
            );
        }
        if self.islands.len() > 8 {
            line(Info, format!("… and {} more islands", self.islands.len() - 8));
        }

        // ── Orphans ──
        if self.orphans.is_empty() {
            line(Ok, "0 orphaned objects — everything authored is reachable".into());
        } else {
            line(
                Error,
                format!("{} authored object(s) nobody can walk to:", self.orphans.len()),
            );
            for o in self.orphans.iter().take(10) {
                line(
                    Error,
                    format!(
                        "  {} {} at ({:.1}, {:.1}, {:.1})",
                        o.kind, o.label, o.pos.x, o.pos.y, o.pos.z
                    ),
                );
            }
        }
        // The exclusion is deliberate and it overrules the author, so it is stated even
        // though it is not itself a fault.
        for (i, pos) in &self.excluded_pads {
            line(
                Warn,
                format!(
                    "G will IGNORE pad {i} at ({:.1}, {:.1}, {:.1}) — on an island, so \
                     anyone spawning there is stranded",
                    pos.x, pos.y, pos.z
                ),
            );
        }

        if self.door_count > 0 && self.blind_doors.is_empty() {
            line(
                Ok,
                format!(
                    "{} door(s) on the grid, thinnest marks {} cell(s) — all visible to pathing",
                    self.door_count, self.thinnest_door
                ),
            );
        }
        if !self.blind_doors.is_empty() {
            line(
                Error,
                format!(
                    "{} door(s) mark NO nav cells and are invisible to pathing — hunters                      walk through them: {:?}",
                    self.blind_doors.len(),
                    self.blind_doors
                ),
            );
        }

        // ── Traversability ──
        if self.pinch_cells == 0 {
            line(Ok, format!("no corridor narrower than {PINCH_TIGHT_M:.2} m"));
        } else {
            let sev = if self.narrowest < PINCH_BLOCK_M { Error } else { Warn };
            line(
                sev,
                format!(
                    "{} cell(s) in corridors under {PINCH_TIGHT_M:.2} m — narrowest {:.2} m \
                     (a hunter's body is {:.2} m across)",
                    self.pinch_cells,
                    self.narrowest,
                    2.0 * ENEMY_RADIUS
                ),
            );
            for p in &self.pinches {
                line(
                    sev,
                    format!(
                        "  {:.2} m wide at ({:.1}, {:.1}, {:.1})",
                        p.width, p.pos.x, p.pos.y, p.pos.z
                    ),
                );
            }
        }
        if self.climb_edges > 0 {
            line(
                Info,
                format!(
                    "{} player-only climb(s) {:.2}–{:.2} m: the player walks or jumps \
                     these, hunters cannot",
                    self.climb_edges,
                    nav::max_step_m() + nav::cell_size_m(),
                    crate::character::JUMP_APEX
                ),
            );
            if self.joining_climbs > 0 {
                line(
                    Error,
                    format!(
                        "{} of them would RECONNECT a cut-off area — these are the ones to fix",
                        self.joining_climbs
                    ),
                );
            }
            for c in self.climbs.iter().filter(|c| c.joins.is_some()) {
                line(
                    Error,
                    format!(
                        "  {:.2} m step at ({:.1}, {:.1}, {:.1}) → joins comp {}",
                        c.rise,
                        c.from.x,
                        c.from.y,
                        c.from.z,
                        c.joins.map(|(_, b)| b).unwrap_or(0)
                    ),
                );
            }
        }

        line(Info, format!("calculated in {:.0} ms", self.calc_ms));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::tools::spawn_point::tests::place_pad;

    /// A 40 m room with a slab whose top sits **two cells (0.5 m)** above the floor —
    /// walkable, and unreachable, which is the shipping level's actual defect.
    ///
    /// Built as a free-standing platform box (the `structure_solid_boxes` channel) rather
    /// than CSG so the ledge is exactly two cells up with no quantization argument.
    fn room_with_a_severed_ledge() -> World {
        let mut world = crate::world::tools::spawn_point::tests::big_room(40.0);
        // A grounded platform whose top surface is 2 WT (0.5 m) up, spanning WT
        // x/z 40..80 → metres 10..20. Grounded, so it is solid to the floor and the
        // cells underneath are not walkable either.
        world.platforms.push(Platform {
            id: 1,
            x: 40.0,
            y: 2.0,
            z: 40.0,
            size_x: 40.0,
            size_z: 40.0,
            thickness: 2.0,
            grounded: true,
            railings: false,
        });
        world
    }

    /// **The headline number is the height of the step, not the fact of the island.**
    /// "0.50 m above the level" is a fix; "4 components" is only a symptom.
    #[test]
    fn a_severed_ledge_is_reported_with_the_height_that_severed_it() {
        let mut world = room_with_a_severed_ledge();
        world.calculate_nav_issues();
        let issues = world.nav_issues().expect("calculated");

        assert_eq!(issues.components, 2, "the floor and the ledge");
        assert!(!issues.is_connected());
        let isl = issues.islands.first().expect("one island");
        let gap = isl.gap.expect("the ledge is right beside the floor");
        assert!(
            (gap.rise.abs() - 0.5).abs() < 1e-3,
            "the step is 0.5 m, got {:+.3}",
            gap.rise
        );
        assert!(gap.flat <= 0.26, "and it is one cell across, got {:.2}", gap.flat);

        // The same defect from the other side: the climb that would close it.
        assert!(
            issues.joining_climbs > 0,
            "the step should be listed as reconnecting the island"
        );

        // And it says so in words, with the number in them.
        let text = issues
            .lines()
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("0.50 m above the level"), "got:\n{text}");
    }

    /// **Islands chain, and measuring every one against the main component lies.**
    ///
    /// A tier stacked on a tier: the upper one is nowhere near the floor, but it is one
    /// short step from the lower one, which is one short step from the floor. Reported
    /// against main it reads as a hopeless drop across the room; reported against its
    /// actual neighbour it is two small steps. This is not hypothetical — the shipping
    /// level has exactly this shape (a 1,380-cell area 3.5 m below the level and 0.5 m
    /// from a 32-cell sliver), and the first version of this report got it wrong.
    #[test]
    fn a_chained_island_is_measured_against_its_neighbour_not_the_main_component() {
        let mut world = crate::world::tools::spawn_point::tests::big_room(10.0);
        // Lower tier: top 2 WT (0.5 m) up, WT 8..32. Upper tier: top 5 WT (1.25 m),
        // WT 14..26 — sitting inside the lower one, so the floor is far away in XZ.
        world.platforms.push(Platform {
            id: 1,
            x: 8.0,
            y: 2.0,
            z: 8.0,
            size_x: 24.0,
            size_z: 24.0,
            thickness: 2.0,
            grounded: true,
            railings: false,
        });
        world.platforms.push(Platform {
            id: 2,
            x: 14.0,
            y: 5.0,
            z: 14.0,
            size_x: 12.0,
            size_z: 12.0,
            thickness: 5.0,
            grounded: true,
            railings: false,
        });
        world.calculate_nav_issues();
        let issues = world.nav_issues().expect("calculated");
        assert_eq!(issues.components, 3, "floor + two tiers");

        // Islands come largest-first, so [0] is the lower tier and [1] the upper.
        let lower = &issues.islands[0];
        let upper = &issues.islands[1];
        assert_eq!(
            lower.gap_to, issues.main,
            "the lower tier's nearest neighbour is the floor"
        );
        assert_eq!(
            upper.gap_to,
            Some(lower.id),
            "the upper tier's nearest neighbour is the LOWER TIER, not the floor"
        );
        let g = upper.gap.expect("a gap");
        assert!(
            (g.rise + 0.75).abs() < 1e-3,
            "and the step down to it is 0.75 m, got {:+.3}",
            g.rise
        );

        // Said in words, naming the island rather than "the level" — the author has to
        // be able to follow the chain.
        let text = issues
            .lines()
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("above island comp"), "got:\n{text}");
    }

    /// **A clean level says so out loud.** A panel that goes quiet is indistinguishable
    /// from a panel that is broken, which was open decision 3 in the handoff.
    #[test]
    fn a_clean_level_states_its_verdict() {
        let mut world = crate::world::tools::spawn_point::tests::big_room(40.0);
        world.calculate_nav_issues();
        let issues = world.nav_issues().expect("calculated");
        assert_eq!(issues.components, 1);
        assert!(issues.is_connected());
        assert!(issues.orphans.is_empty());

        let lines = issues.lines();
        assert!(
            lines.iter().any(|l| l.sev == NavSeverity::Ok && l.text.contains("1 walkable")),
            "the clean verdict must be an explicit line, got {:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(
            !lines.iter().any(|l| l.sev == NavSeverity::Error),
            "an open room has no errors"
        );
    }

    /// A pad on the ledge is **both** findings at once: an orphan (nothing can reach it)
    /// and an exclusion (G will drop it). Reporting only the second would read as the
    /// tool being fussy; only the first would hide that the level silently loses a pad.
    #[test]
    fn a_pad_on_an_island_is_reported_as_an_orphan_and_as_an_exclusion() {
        let mut world = room_with_a_severed_ledge();
        // Three on the floor, one up on the ledge (its top is at y = 0.5 m).
        place_pad(&mut world, Vec3::new(4.0, 0.0, 4.0), 0.0);
        place_pad(&mut world, Vec3::new(36.0, 0.0, 4.0), 0.0);
        place_pad(&mut world, Vec3::new(4.0, 0.0, 36.0), 0.0);
        place_pad(&mut world, Vec3::new(15.0, 0.5, 15.0), 0.0);

        world.calculate_nav_issues();
        let issues = world.nav_issues().expect("calculated");
        assert_eq!(
            issues.orphans.len(),
            1,
            "exactly the ledge pad is unreachable, got {:?}",
            issues.orphans.iter().map(|o| o.pos).collect::<Vec<_>>()
        );
        assert_eq!(issues.orphans[0].kind, "spawn pad");
        assert_eq!(
            issues.excluded_pads.len(),
            1,
            "and G drops exactly that one from the pool"
        );
    }

    /// The overlay is off until asked for, and then it covers the floor. Guards the
    /// upload path: the app only re-uploads on a revision change, so a toggle that
    /// forgets to bump the revision shows a stale (or invisible) overlay.
    #[test]
    fn the_overlay_is_opt_in_and_revisioned() {
        let mut world = room_with_a_severed_ledge();
        assert!(world.nav_overlay_mesh().is_none(), "nothing before Calculate");

        let rev0 = world.nav_overlay_rev();
        world.calculate_nav_issues();
        assert!(world.nav_overlay_rev() > rev0, "Calculate bumps the revision");
        assert!(
            world.nav_overlay_mesh().is_none(),
            "…but the overlay is still off until toggled"
        );

        let rev1 = world.nav_overlay_rev();
        world.toggle_nav_overlay();
        assert!(world.nav_overlay_rev() > rev1, "the toggle bumps it too");
        let mesh = world.nav_overlay_mesh().expect("an overlay to draw");
        assert!(
            mesh.indices.len() > 1000,
            "a 40 m room's worth of cells, got {} indices",
            mesh.indices.len()
        );
    }
}
