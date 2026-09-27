//! The PD adapter: a multiplayer stage's collision tiles, pads, waypoint graph and
//! spawn list, read from the decomp's JSON exports (no ROM parsing).
//!
//! * `src/assets/ntsc-final/tiles/<stage>.json` → [`LevelGeom`]. Only the GEOFLAGs
//!   an editor level could also supply survive the conversion: floor
//!   (`GEOFLAG_FLOOR1 | GEOFLAG_FLOOR2`), wall (`GEOFLAG_WALL`), `GEOFLAG_BLOCK_SIGHT`,
//!   `GEOFLAG_BLOCK_SHOOT`, `GEOFLAG_LADDER`, `GEOFLAG_AIBOTCROUCH` and
//!   `GEOFLAG_AIBOTDUCK` (`constants.h:1189-1201`), plus the room.
//! * `src/assets/ntsc-final/pads/<stage>.json` → [`PdPads`]: pads, PD's hand-placed
//!   waypoints and waygroups (the baseline graph), cover points.
//! * `src/setups/mp_setup<stage>.c` → the `spawn()` pads in `intro[]`.
//!
//! The decomp is gitignored, so it is read in place from `reference/pd-decomp`
//! (override with `PD_DECOMP_DIR`); nothing PD-derived is copied into the repo.

use std::path::PathBuf;

use glam::Vec3;
use serde_json::Value;

use super::level_geom::{GeomPoly, LevelGeom};

/// `WPSEGFLAG_OUTWARDSONLY` (`padhalllv.c:44`): the link may only be used leaving
/// this node ("eg. top of ledge").
pub const WPSEGFLAG_OUTWARDSONLY: i32 = 0x4000;
/// `WPSEGFLAG_INWARDSONLY` (`padhalllv.c:45`): only arriving ("eg. bottom of ledge").
pub const WPSEGFLAG_INWARDSONLY: i32 = 0x8000;

/// `WPSEG_GET_ID(seg)` (`padhalllv.c:51`).
pub fn wpseg_get_id(seg: i32) -> usize {
    (seg & (0xffff & !(WPSEGFLAG_OUTWARDSONLY | WPSEGFLAG_INWARDSONLY))) as usize
}

/// Root of the decomp checkout.
pub fn decomp_dir() -> PathBuf {
    std::env::var_os("PD_DECOMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../reference/pd-decomp"))
}

/// `struct pad`, the fields the spike uses. `pos` is where PD put the pad, which is
/// **not** ground height: on Complex most pads sit 52–53 cm above their floor.
#[derive(Clone, Debug)]
pub struct Pad {
    pub pos: Vec3,
    /// `pad->look`.
    pub look: Vec3,
    /// The `PADFLAG_AI*` bits the go-to code reads.
    pub flags: super::pd_nav::PadFlags,
}

impl Pad {
    /// The look direction as a PD yaw (0 = +Z, increasing towards +X), 0..2π.
    pub fn look_angle(&self) -> f32 {
        let a = self.look.x.atan2(self.look.z);
        if a < 0.0 {
            a + std::f32::consts::TAU
        } else {
            a
        }
    }
}

/// `struct waypoint` (`types.h`): `neighbours` holds PD's encoded segments
/// (neighbour id | `WPSEGFLAG_*`), without the `-1` terminator.
#[derive(Clone, Debug)]
pub struct Waypoint {
    pub padnum: usize,
    pub neighbours: Vec<i32>,
    pub groupnum: usize,
}

/// `struct waygroup`: `neighbours` encoded like [`Waypoint::neighbours`];
/// `waypoints` lists the member waypoints in index order.
#[derive(Clone, Debug)]
pub struct Waygroup {
    pub neighbours: Vec<i32>,
    pub waypoints: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Cover {
    pub pos: Vec3,
    pub dir: Vec3,
}

#[derive(Clone, Debug, Default)]
pub struct PdPads {
    pub pads: Vec<Pad>,
    pub waypoints: Vec<Waypoint>,
    pub waygroups: Vec<Waygroup>,
    pub cover: Vec<Cover>,
}

/// Everything the spike loads for one PD multiplayer stage.
#[derive(Clone, Debug)]
pub struct PdStage {
    pub name: &'static str,
    pub geom: LevelGeom,
    pub pads: PdPads,
    /// Pad numbers of the `spawn()` entries in `intro[]`, in file order.
    pub spawn_pads: Vec<usize>,
}

impl PdStage {
    /// Complex: stage `ref` (`STAGE_MP_COMPLEX`, `stagetable.c:22`).
    pub fn complex() -> Result<Self, String> {
        Self::load("ref")
    }

    pub fn load(stage: &'static str) -> Result<Self, String> {
        let root = decomp_dir();
        let assets = root.join("src/assets/ntsc-final");
        let tiles = read_json(&assets.join(format!("tiles/{stage}.json")))?;
        let pads = read_json(&assets.join(format!("pads/{stage}.json")))?;
        let setup_path = root.join(format!("src/setups/mp_setup{stage}.c"));
        let setup = std::fs::read_to_string(&setup_path).map_err(|e| format!("{}: {e}", setup_path.display()))?;
        let geom = parse_tiles(&tiles)?;
        let pads = parse_pads(&pads)?;
        let spawn_pads = parse_intro_spawns(&setup)?;
        if let Some(&bad) = spawn_pads.iter().find(|&&p| p >= pads.pads.len()) {
            return Err(format!("spawn pad {bad:#x} out of range"));
        }
        Ok(PdStage { name: stage, geom, pads, spawn_pads })
    }

    pub fn waypoint_pos(&self, w: usize) -> Vec3 {
        self.pads.pads[self.pads.waypoints[w].padnum].pos
    }
}

fn read_json(path: &std::path::Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: {e} (set PD_DECOMP_DIR to the pd-decomp checkout)", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The number in a generated constant name: `PAD_REF_001C` → 0x1c. The decomp's
/// asset tool numbers pads, waypoints, waygroups and rooms this way, in order.
fn id_suffix(name: &str) -> Result<usize, String> {
    let hex = name.rsplit('_').next().unwrap_or("");
    usize::from_str_radix(hex, 16).map_err(|_| format!("bad id {name:?}"))
}

fn vec3(v: &Value) -> Result<Vec3, String> {
    let f = |k: usize| v.get(k).and_then(Value::as_f64).map(|x| x as f32).ok_or_else(|| format!("bad vec3 {v}"));
    Ok(Vec3::new(f(0)?, f(1)?, f(2)?))
}

fn parse_tiles(json: &Value) -> Result<LevelGeom, String> {
    let rooms = json.get("rooms").and_then(Value::as_object).ok_or("tiles: no \"rooms\" object")?;
    let mut geom = LevelGeom::default();
    for (name, tiles) in rooms {
        let room = id_suffix(name)? as u16;
        geom.room_names.push((room, name.clone()));
        for t in tiles.as_array().ok_or("tiles: room is not a list")? {
            let flag = |k: &str| t.get(k).and_then(Value::as_bool).unwrap_or(false);
            let verts = t
                .get("vertices")
                .and_then(Value::as_array)
                .ok_or("tiles: tile has no vertices")?
                .iter()
                .map(|v| {
                    let c = |k: &str| v.get(k).and_then(Value::as_f64).map(|x| x as f32).ok_or("tiles: bad vertex");
                    Ok(Vec3::new(c("x")?, c("y")?, c("z")?))
                })
                .collect::<Result<Vec<_>, &str>>()?;
            if verts.len() < 3 {
                return Err(format!("tiles: {name} has a tile with {} vertices", verts.len()));
            }
            let mut poly = GeomPoly::new(
                verts,
                flag("flag0001") || flag("flag0002"), // GEOFLAG_FLOOR1 | GEOFLAG_FLOOR2
                flag("flag0004"),                     // GEOFLAG_WALL
                flag("flag0008"),                     // GEOFLAG_BLOCK_SIGHT
                flag("flag0010"),                     // GEOFLAG_BLOCK_SHOOT
                Some(room),
            );
            poly.ladder = flag("ladder"); // GEOFLAG_LADDER
            poly.crouch = flag("aibotcrouch"); // GEOFLAG_AIBOTCROUCH
            poly.duck = flag("aibotduck"); // GEOFLAG_AIBOTDUCK
            geom.polys.push(poly);
        }
    }
    geom.room_names.sort();
    Ok(geom)
}

/// The generated name → index, checked against its position in the list.
fn indexed(list: &[Value], what: &str) -> Result<(), String> {
    for (i, e) in list.iter().enumerate() {
        let id = e.get("id").and_then(Value::as_str).ok_or_else(|| format!("{what}: entry without id"))?;
        if id_suffix(id)? != i {
            return Err(format!("{what}: {id} is at index {i}"));
        }
    }
    Ok(())
}

/// A neighbour list → PD's encoded segments.
fn segments(list: Option<&Value>, key: &str) -> Result<Vec<i32>, String> {
    let Some(list) = list.and_then(Value::as_array) else { return Ok(Vec::new()) };
    list.iter()
        .map(|n| {
            let id = id_suffix(n.get(key).and_then(Value::as_str).ok_or("pads: neighbour without id")?)? as i32;
            let f = |k: &str| n.get(k).and_then(Value::as_bool).unwrap_or(false);
            Ok(id
                | if f("flag4000") { WPSEGFLAG_OUTWARDSONLY } else { 0 }
                | if f("flag8000") { WPSEGFLAG_INWARDSONLY } else { 0 })
        })
        .collect()
}

fn parse_pads(json: &Value) -> Result<PdPads, String> {
    let list = |k: &str| json.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    let (pads_j, wps_j, groups_j, cover_j) = (list("pads"), list("waypoints"), list("waygroups"), list("cover"));
    indexed(pads_j, "pads")?;
    indexed(wps_j, "waypoints")?;
    indexed(groups_j, "waygroups")?;
    let pads = pads_j
        .iter()
        .map(|p| {
            let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
            Ok(Pad {
                pos: vec3(p.get("pos").ok_or("pad without pos")?)?,
                look: vec3(p.get("dir").ok_or("pad without dir")?)?,
                flags: super::pd_nav::PadFlags { walkdirect: flag("aiwalkdirect"), crouch: flag("aicrouch"), duck: flag("aiduck") },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let waypoints = wps_j
        .iter()
        .map(|w| {
            let s = |k: &str| w.get(k).and_then(Value::as_str).ok_or_else(|| format!("waypoint without {k}"));
            Ok(Waypoint {
                padnum: id_suffix(s("pad")?)?,
                neighbours: segments(w.get("neighbours"), "waypoint")?,
                groupnum: id_suffix(s("waygroup")?)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut waygroups = groups_j
        .iter()
        .map(|g| Ok(Waygroup { neighbours: segments(g.get("neighbours"), "waygroup")?, waypoints: Vec::new() }))
        .collect::<Result<Vec<_>, String>>()?;
    for (i, w) in waypoints.iter().enumerate() {
        if w.padnum >= pads.len() {
            return Err(format!("waypoint {i:#x}: pad {:#x} out of range", w.padnum));
        }
        let g = waygroups.get_mut(w.groupnum).ok_or_else(|| format!("waypoint {i:#x}: bad group"))?;
        g.waypoints.push(i);
        if let Some(&n) = w.neighbours.iter().find(|&&n| wpseg_get_id(n) >= waypoints.len()) {
            return Err(format!("waypoint {i:#x}: neighbour {:#x} out of range", wpseg_get_id(n)));
        }
    }
    let cover = cover_j
        .iter()
        .map(|c| {
            Ok(Cover {
                pos: vec3(c.get("pos").ok_or("cover without pos")?)?,
                dir: vec3(c.get("dir").ok_or("cover without dir")?)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PdPads { pads, waypoints, waygroups, cover })
}

/// The pads named by `spawn(PAD_...)` inside `intro[] = { ... };`. `case_respawn(`
/// also contains `spawn(`, so a match must start a word.
fn parse_intro_spawns(src: &str) -> Result<Vec<usize>, String> {
    let start = src.find("intro[] = {").ok_or("setup: no intro[]")?;
    let body = &src[start..];
    let body = &body[..body.find("};").ok_or("setup: intro[] not closed")?];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(k) = rest.find("spawn(") {
        let word_start = rest[..k].chars().last().map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        let after = &rest[k + "spawn(".len()..];
        if word_start {
            let arg = &after[..after.find(')').ok_or("setup: unclosed spawn(")?];
            out.push(id_suffix(arg.trim())?);
        }
        rest = after;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intro_spawns_skip_case_respawn() {
        let src = "s32 intro[] = {\n\tspawn(PAD_REF_001C)\n\tcase_respawn(0, PAD_REF_0000)\n\tspawn(PAD_REF_002E)\n};";
        assert_eq!(parse_intro_spawns(src).unwrap(), vec![0x1c, 0x2e]);
    }

    #[test]
    fn segments_keep_pd_encoding() {
        assert_eq!(wpseg_get_id(0x8a | WPSEGFLAG_OUTWARDSONLY), 0x8a);
        assert_eq!(wpseg_get_id(0x13 | WPSEGFLAG_INWARDSONLY), 0x13);
    }

    /// Complex as the recon counted it (`SPIKE_PD_COMPLEX.md`, "What the decomp
    /// already gives us"). Reads the gitignored decomp in place.
    #[test]
    fn complex_loads_with_the_documented_counts() {
        let s = PdStage::complex().expect("reference/pd-decomp (or PD_DECOMP_DIR) must be present");
        let g = &s.geom;
        assert_eq!(g.polys.len(), 1208);
        assert_eq!(g.room_names.len(), 45);
        assert_eq!(g.polys.iter().filter(|p| p.floor).count(), 350);
        assert_eq!(g.polys.iter().filter(|p| p.wall).count(), 858);
        assert_eq!(g.polys.iter().filter(|p| p.wall && !p.blocks_sight).count(), 41);
        let (lo, hi) = g.bounds();
        assert_eq!((lo.x, lo.y, lo.z), (-5053.0, -276.0, -1956.0));
        assert_eq!((hi.x, hi.y, hi.z), (-643.0, 748.0, 1943.0));
        assert_eq!(s.pads.pads.len(), 226);
        assert_eq!(s.pads.waypoints.len(), 144);
        assert_eq!(s.pads.waygroups.len(), 20);
        assert_eq!(s.pads.cover.len(), 101);
        assert_eq!(s.spawn_pads, (0x1c..=0x2e).collect::<Vec<_>>());
        assert_eq!(s.pads.waygroups.iter().map(|g| g.waypoints.len()).sum::<usize>(), 144);
    }

    /// The overlay alignment check, headless: every waypoint pad and spawn pad has a
    /// floor tile under it, so pads and tiles share one coordinate frame. Spawn pads
    /// sit 52–63 cm above theirs (the offset stage 2 must drop a spawning chr by).
    #[test]
    fn every_waypoint_and_spawn_pad_stands_over_a_floor() {
        let s = PdStage::complex().unwrap();
        let under = |pad: usize| {
            let p = s.pads.pads[pad].pos;
            s.geom.floor_below(p.x, p.z, p.y + 10.0).map(|(y, _)| p.y - y)
        };
        // All 226, not just the waypoint pads: the viewer's red "no floor" spike
        // should never show on Complex.
        for pad in 0..s.pads.pads.len() {
            assert!(under(pad).is_some(), "pad {pad:#x} has no floor under it");
        }
        for &pad in &s.spawn_pads {
            let h = under(pad).unwrap_or_else(|| panic!("spawn pad {pad:#x} has no floor under it"));
            assert!((50.0..=65.0).contains(&h), "spawn pad {pad:#x} is {h} cm above its floor");
        }
    }

    #[test]
    fn the_height_clip_hides_the_upper_floors() {
        use super::super::greybox::{self, GreyboxOpts};
        let s = PdStage::complex().unwrap();
        let all = GreyboxOpts { clip_y: None, room_tint: false };
        let low = GreyboxOpts { clip_y: Some(200.0), room_tint: false };
        let shown = |o: &GreyboxOpts| s.geom.polys.iter().filter(|p| greybox::is_visible(p, o)).count();
        assert_eq!(shown(&all), 1208);
        assert!(shown(&low) < 1208);
        assert!(s.geom.polys.iter().filter(|p| greybox::is_visible(p, &low)).all(|p| p.min_y() <= 200.0));
        let upper_floors = s.geom.polys.iter().filter(|p| p.floor && p.min_y() > 200.0).count();
        assert!(upper_floors > 50, "{upper_floors}");
        assert!(greybox::build(&s.geom, &low).indices.len() < greybox::build(&s.geom, &all).indices.len());
    }
}
