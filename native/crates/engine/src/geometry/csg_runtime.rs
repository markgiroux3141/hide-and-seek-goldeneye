//! Runtime CSG subsystem — the thing this engine is fundamentally *for*
//! (ENGINE_PORT_PLAN "Engine ↔ Game boundary"). Brushes are authored at runtime
//! during the BUILD phase; each edit re-evaluates the affected region into a mesh
//! and (upstream) a collider.
//!
//! Ports three JS/spike oracles verbatim in behavior:
//! - the brush model (`src/core/BrushDef.js`) — an AABB in world-tile units,
//! - the region model (`src/core/csg/CSGRegion.js`) — a shell auto-fit to the
//!   subtractive brushes plus a 1-WT pad,
//! - the evaluation fold (`spike/.../csg-wasm/src/lib.rs::evaluate`) — shell
//!   then union/subtract in order, with disjoint-AABB early-reject and a
//!   consecutive-subtract pre-merge. Those two optimizations are what keep
//!   re-bake cheap enough to feel instant.
//!
//! Coordinate spaces: brush fields are in **world tiles (WT)**; geometry is
//! emitted in **meters** (WT × [`WORLD_SCALE`]). Matches the JS convention so
//! behavior diffs 1:1 against the reference build.

use csg::{csg_subtract, csg_union, polygons_to_mesh, Polygon};
use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::geometry::geom;
use crate::geometry::structures::{ColumnInputs, Platform};
use crate::render::mesh::{CpuMesh, TexturedMesh};
use crate::render::textures::{self, default_scheme};
use crate::render::uv_zones::{self, BrushInfo, ZonedBuilder};

/// Meters per world tile. Mirrors `src/core/constants.js` `WORLD_SCALE`.
pub const WORLD_SCALE: f32 = 0.25;

/// Wall thickness in WT — the fundamental unit. Mirrors `src/core/constants.js`
/// `WALL_THICKNESS`. A doorframe / protoroom carve is one WT deep.
pub const WALL_THICKNESS: f32 = 1.0;

/// A brush is either additive (contributes solid) or subtractive (carves).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Add,
    Subtract,
}

/// The three axes a brush face can face along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// Array index of this axis into an `[x, y, z]` triple (X→0, Y→1, Z→2).
    #[inline]
    pub fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }

    /// The positive unit normal along this axis.
    #[inline]
    pub fn normal(self) -> [f32; 3] {
        match self {
            Axis::X => [1.0, 0.0, 0.0],
            Axis::Y => [0.0, 1.0, 0.0],
            Axis::Z => [0.0, 0.0, 1.0],
        }
    }

    /// The component of a vector along this axis.
    #[inline]
    pub fn component(self, v: Vec3) -> f32 {
        match self {
            Axis::X => v.x,
            Axis::Y => v.y,
            Axis::Z => v.z,
        }
    }

    /// The two axes orthogonal to this one as (U, V), matching the JS oracle's
    /// `getFaceUVInfo` convention. Crucially, for both vertical walls (X- and
    /// Z-facing) **V is the world-up axis Y**, so a door/opening keeps its width
    /// horizontal and height vertical regardless of which wall it's cut into.
    /// Y-facing faces (floor/ceiling) use (X, Z).
    #[inline]
    pub fn orthogonals(self) -> (Axis, Axis) {
        match self {
            Axis::X => (Axis::Z, Axis::Y),
            Axis::Y => (Axis::X, Axis::Z),
            Axis::Z => (Axis::X, Axis::Y),
        }
    }

    /// The dominant axis of a surface normal — the axis whose (absolute)
    /// component is largest, i.e. which face plane the normal points out of.
    #[inline]
    pub fn dominant(normal: Vec3) -> Axis {
        let n = normal.abs();
        if n.x >= n.y && n.x >= n.z {
            Axis::X
        } else if n.y >= n.z {
            Axis::Y
        } else {
            Axis::Z
        }
    }
}

/// Which end of an axis a face sits on: `Min` (the `x`/`y`/`z` corner) or `Max`
/// (corner + dimension).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Min,
    Max,
}

impl Side {
    /// The opposite end of the axis.
    #[inline]
    pub fn flipped(self) -> Side {
        match self {
            Side::Min => Side::Max,
            Side::Max => Side::Min,
        }
    }
}

/// A brush has six faces; this is the slot one occupies in
/// [`Brush::face_tex`]. `axis * 2 + side`, so X-min is 0 and Z-max is 5.
#[inline]
pub fn face_slot(axis: Axis, side: Side) -> usize {
    axis.index() * 2 + side as usize
}

/// Which slot in the six-face array a slot index refers to — the inverse of
/// [`face_slot`], for the UI that has to name a stored override.
#[inline]
pub fn face_slot_parts(slot: usize) -> (Axis, Side) {
    let axis = match slot / 2 {
        0 => Axis::X,
        1 => Axis::Y,
        _ => Axis::Z,
    };
    let side = if slot % 2 == 0 { Side::Min } else { Side::Max };
    (axis, side)
}

/// A **per-face texture override**: this face of this brush renders with `scheme`
/// (and optionally a forced zone slot) instead of whatever the classifier would
/// have derived.
///
/// It exists for two jobs that turn out to be the same job. The first is repair:
/// [`crate::render::uv_zones`] recovers a triangle's theme by *guessing* which brush
/// owns it (`face_owner`), and a guess can be wrong — an override is the manual
/// answer that always wins. The second is authoring: an accent wall, a signage
/// panel, one deliberately mismatched floor, none of which the per-room flood-fill
/// retexture can express.
///
/// **Keyed by face, not by triangle.** A triangle has no persistent identity — the
/// CSG fold regenerates the soup on every edit, splits it at frames and at the
/// wall band, then sorts it by (scheme, zone) — so a triangle index means nothing
/// after the next keystroke. `(brush, axis, side)` survives all of that, and it is
/// what the classifier itself keys on, so an override is guaranteed to reach the
/// triangles it was aimed at. It rides on [`Brush`] rather than on the region so it
/// survives reclustering, undo and save/load with no plumbing of its own.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceTex {
    /// The theme this face renders with. Persisted by *name*, exactly as
    /// [`Brush::scheme`] is, and for the same reason.
    #[serde(serialize_with = "ser_scheme", deserialize_with = "de_scheme")]
    pub scheme: usize,
    /// Force every triangle of the face into this zone slot, or `None` to keep the
    /// zone the classifier derived.
    ///
    /// `None` is the useful default: a wall keeps its lower/upper band split and a
    /// floor stays a floor, they just draw from a different theme. Forcing a zone is
    /// how you put a specific *texture* on a face — a ceiling tile on a wall — at the
    /// cost of flattening any band split the face had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<u8>,
}

/// Default (and empty) value for [`Brush::face_tex`].
fn no_face_tex() -> [Option<FaceTex>; 6] {
    [None; 6]
}

/// Whether a face-override array carries nothing, so serde can leave it out of the
/// file entirely — which is the case for all but a handful of brushes.
fn face_tex_is_empty(a: &[Option<FaceTex>; 6]) -> bool {
    a.iter().all(Option::is_none)
}

/// A single CSG brush: an axis-aligned box in WT units plus its operation.
///
/// Position `(x, y, z)` is the **min corner**; `(w, h, d)` are the dimensions —
/// matching `BrushDef` (`maxX = x + w`, etc.). Taper / scheme flags from the JS
/// `BrushDef` are deliberately omitted until a later phase needs them.
///
/// `door` marks the doorframe carve (JS `BrushDef.isDoorframe`): at the BUILD→HUNT
/// bake, `World::build_doors` scans for these to place a breakable panel + a nav
/// overlay over the opening they cut. It carries no CSG meaning (a doorframe is a
/// plain subtract).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Brush {
    pub id: u32,
    pub op: Op,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
    pub h: f32,
    pub d: f32,
    #[serde(default)]
    pub door: bool,
    /// Marks a door/hole **opening frame** carve (JS `isDoorframe`/`isHoleFrame`):
    /// its interior reveals texture as the tunnel zones (5/6) instead of room
    /// walls. Set on both door and hole frames in `World::cut_opening`; `door`
    /// distinguishes the two (doorframe floor → 6, hole-frame floor → 5).
    #[serde(default)]
    pub frame: bool,
    /// Marks a **vent duct** carve — the crawlspace the player enters crouched.
    ///
    /// Two jobs, and neither is texturing. The duct's surfaces take their look from this
    /// brush's own `scheme` like any other carve (`face_owner` hands a triangle to the
    /// smallest brush whose face it lies on, which for a duct interior is the duct), so
    /// no zone or classifier change is needed to make a vent look like a vent.
    ///
    /// What the flag is for is everything downstream that has to know a hole in the wall
    /// is a *duct*: the nav bake, which turns its mouths into portals rather than letting
    /// hunters snap goals into the bore, and the NAV report, which has to tell a
    /// deliberately hunter-proof duct apart from an accidental island.
    #[serde(default)]
    pub vent: bool,
    /// WT-space floor anchor for this brush's wall texture (JS `BrushDef.floorY`,
    /// recovered per-triangle via `uv_zones` face-map). Defaults to `y`; a room's
    /// walls anchor to its floor, a stair pit's walls to the pit floor, so a
    /// down-stair no longer shifts the whole level's wall texture.
    #[serde(default)]
    pub floor_y: f32,
    /// Texture theme, as an index into [`textures::schemes`] (JS
    /// `BrushDef.schemeKey`), set per room by the number-key flood-fill retexture.
    /// Defaults to [`textures::default_scheme`].
    ///
    /// The index is a **runtime** handle only — it is a position in whatever
    /// `themes.json` currently lists. On disk this field is the theme's stable
    /// *name* (see [`ser_scheme`] / [`de_scheme`]), so themes can be added,
    /// reordered or removed without retexturing saved levels.
    #[serde(
        default = "default_scheme",
        serialize_with = "ser_scheme",
        deserialize_with = "de_scheme"
    )]
    pub scheme: usize,
    /// Ties together the brushes that one authored shape decomposed into. The
    /// freeform draw tool emits an arbitrary 90°-snapped polygon as N rectangles
    /// (fan-triangulation is convex-only, so a concave footprint *must* be split),
    /// and this is what lets the author's single drawn shape still be recognised as
    /// one thing afterwards. `0` = ungrouped, which is every brush the other tools
    /// make.
    ///
    /// A group takes the id of its first brush. Brush ids are unique and start at 1,
    /// so that's collision-free against both `0` and every other group without an
    /// allocator of its own to thread through snapshot/save/load.
    ///
    /// Carries **no** CSG meaning — the fold, the classifier and nav never read it,
    /// and it is deliberately left out of `region_hash` so a regroup can't
    /// invalidate a memoized bake.
    #[serde(default)]
    pub group: u32,
    /// Per-face texture overrides, indexed by [`face_slot`] (`axis * 2 + side`).
    ///
    /// Empty for almost every brush, and skipped entirely when it is, so the level
    /// file only grows where an author actually painted. See [`FaceTex`].
    #[serde(
        default = "no_face_tex",
        skip_serializing_if = "face_tex_is_empty"
    )]
    pub face_tex: [Option<FaceTex>; 6],
}

/// Serialize a theme index as its stable *name*.
///
/// Indices are positions in the runtime-loaded `themes.json`, so writing one to
/// disk would silently retexture every saved level the moment a theme is inserted
/// or removed. The name has no such coupling.
fn ser_scheme<S: serde::Serializer>(index: &usize, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(textures::scheme_name(*index))
}

/// A theme reference as it may appear on disk: the current form (a name) or the
/// legacy form (a bare index, written by level format v3 and earlier).
#[derive(Deserialize)]
#[serde(untagged)]
enum SchemeRef {
    Name(String),
    Index(usize),
}

/// Resolve an on-disk theme reference to a runtime index.
///
/// Accepts both forms, which is what lets pre-v4 levels load with no migration
/// pass: a legacy index is a position in the original hard-coded 10-scheme order,
/// which `themes.json` preserves in its first ten entries for exactly this reason.
/// An unknown name or an out-of-range index degrades to the default theme with a
/// warning rather than failing the load — one mystery-textured room is a far
/// better outcome than an unopenable level.
fn de_scheme<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    let fallback = |what: String| {
        log::warn!("level references unknown texture theme {what}; using the default");
        default_scheme()
    };
    Ok(match SchemeRef::deserialize(d)? {
        SchemeRef::Name(n) => {
            textures::scheme_index(&n).unwrap_or_else(|| fallback(format!("'{n}'")))
        }
        SchemeRef::Index(i) => {
            if i < textures::schemes().len() {
                i
            } else {
                fallback(format!("index {i}"))
            }
        }
    })
}

impl Brush {
    pub fn new(id: u32, op: Op, x: f32, y: f32, z: f32, w: f32, h: f32, d: f32) -> Self {
        Brush {
            id, op, x, y, z, w, h, d,
            door: false,
            frame: false,
            vent: false,
            floor_y: y,
            scheme: default_scheme(),
            group: 0,
            face_tex: no_face_tex(),
        }
    }

    /// Size along an axis (`w`/`h`/`d`).
    #[inline]
    pub fn dim(&self, axis: Axis) -> f32 {
        match axis {
            Axis::X => self.w,
            Axis::Y => self.h,
            Axis::Z => self.d,
        }
    }

    /// Min-corner coordinate along an axis (`x`/`y`/`z`).
    #[inline]
    pub fn min(&self, axis: Axis) -> f32 {
        match axis {
            Axis::X => self.x,
            Axis::Y => self.y,
            Axis::Z => self.z,
        }
    }

    /// Whether a WT point is inside this brush's AABB (half-open, taper ignored
    /// — coarse nav is fine). Mirrors JS `pointInBrush`.
    #[inline]
    pub fn contains(&self, x: f32, y: f32, z: f32) -> bool {
        geom::point_in_box(&[self.x, self.y, self.z, self.w, self.h, self.d], x, y, z)
    }

    /// The WT coordinate of the plane of the given face.
    #[inline]
    pub fn face_pos(&self, axis: Axis, side: Side) -> f32 {
        match side {
            Side::Min => self.min(axis),
            Side::Max => self.min(axis) + self.dim(axis),
        }
    }

    /// This face's texture override, if one has been painted. See [`FaceTex`].
    #[inline]
    pub fn face_tex(&self, axis: Axis, side: Side) -> Option<FaceTex> {
        self.face_tex[face_slot(axis, side)]
    }

    /// Paint (or with `None`, clear) this face's texture override.
    #[inline]
    pub fn set_face_tex(&mut self, axis: Axis, side: Side, tex: Option<FaceTex>) {
        self.face_tex[face_slot(axis, side)] = tex;
    }

    /// How many of this brush's six faces carry an override.
    #[inline]
    pub fn face_tex_count(&self) -> usize {
        self.face_tex.iter().filter(|f| f.is_some()).count()
    }

    /// Grow this brush's face outward by `step` WT (JS `applyFullFacePush`): a
    /// `Max` face extends the dimension; a `Min` face moves the corner back and
    /// extends the dimension so the opposite face stays put.
    pub fn push_face(&mut self, axis: Axis, side: Side, step: f32) {
        match side {
            Side::Max => self.set_dim(axis, self.dim(axis) + step),
            Side::Min => {
                self.set_min(axis, self.min(axis) - step);
                self.set_dim(axis, self.dim(axis) + step);
            }
        }
        // A moved floor re-anchors the wall texture (JS `applyFullFacePush`).
        if axis == Axis::Y && side == Side::Min {
            self.floor_y = self.y;
        }
    }

    /// Shrink this brush's face inward by `step` WT (JS `applyFullFacePull`).
    /// Returns `false` (no-op) if the brush is too thin along `axis` to absorb it.
    pub fn pull_face(&mut self, axis: Axis, side: Side, step: f32) -> bool {
        if self.dim(axis) <= step {
            return false;
        }
        match side {
            Side::Max => self.set_dim(axis, self.dim(axis) - step),
            Side::Min => {
                self.set_min(axis, self.min(axis) + step);
                self.set_dim(axis, self.dim(axis) - step);
            }
        }
        if axis == Axis::Y && side == Side::Min {
            self.floor_y = self.y;
        }
        true
    }

    #[inline]
    fn set_min(&mut self, axis: Axis, v: f32) {
        match axis {
            Axis::X => self.x = v,
            Axis::Y => self.y = v,
            Axis::Z => self.z = v,
        }
    }

    #[inline]
    fn set_dim(&mut self, axis: Axis, v: f32) {
        match axis {
            Axis::X => self.w = v,
            Axis::Y => self.h = v,
            Axis::Z => self.d = v,
        }
    }
}

// ─── Stairs ──────────────────────────────────────────────────────────
//
// A confirmed CSG stair, split three ways (JS `csgActions.confirmStairOp` +
// `csgStairGeometry` + `navWorld.stairSolidBoxes`):
//   1. Two `subtract` void brushes carve the stairwell tunnel + far corridor
//      into the region (they live in `Region::brushes`, like any subtract).
//   2. This descriptor drives the visible tread/riser/side mesh, which
//      [`Region::evaluate`] appends straight into the region mesh — so treads
//      render with the wall shader AND land in the region's trimesh collider
//      (the player walks/autosteps them for free; no separate physics path).
//   3. [`StairDesc::solid_boxes`] reconstructs the solid step blocks for the
//      nav voxelizer (the mesh isn't visible to grid nav, which reads CSG
//      membership) — the `collectExtraSolids` port.

/// What a flight is *made of* — the authoring choice shared by every stair tool
/// (`\u{2191}`/`\u{2193}` CSG stairs, `K` free-standing flights, `C` connects).
///
/// A **render** choice: the walking surface, the collider and the nav solids are the
/// same either way. Steps draw treads and risers over the flight's slope; a ramp draws
/// the slope itself. The collider has always been the smooth ramp
/// ([`StairDesc::append_ramp_collision`]) — a ramp just stops drawing steps over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StairShell {
    #[default]
    Steps,
    Ramp,
}

/// Default horizontal run of one step, in WT — one tile out per tile down, the 45°
/// flight every CSG stair was before the slope became configurable.
fn default_run_per_step() -> f32 {
    1.0
}

/// Which way a staircase runs from the selected wall face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StairDir {
    /// Steps descend below the floor into a lower corridor (JS `'down'`).
    Down,
    /// Steps rise above the floor into a raised corridor (JS `'up'`).
    Up,
}

/// A confirmed stair's parameters, in WT. Mirrors the JS `state.csg.csgStairs[]`
/// descriptor: `axis`/`side`/`face_pos` fix the anchoring wall face, `(u0, u1)`
/// the horizontal span along the in-plane `u_axis`, `floor`/`ceil` the vertical
/// extent, and `direction`/`step_count` the run. Enough to rebuild both the tread
/// mesh and the nav solid boxes deterministically (matches the JS oracles).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct StairDesc {
    pub direction: StairDir,
    pub step_count: u32,
    /// Wall-normal axis the stair steps along (X or Z; never Y).
    pub axis: Axis,
    pub side: Side,
    /// Wall-plane WT coord on `axis` (the face the stair starts flush with).
    pub face_pos: f32,
    /// The horizontal in-plane axis (Z for an X-wall, X for a Z-wall).
    pub u_axis: Axis,
    /// Horizontal span [u0, u1) along `u_axis`.
    pub u0: f32,
    pub u1: f32,
    /// Face bottom (vMin) and the stairwell ceiling H, in WT Y. `ceil` is the top of the
    /// *selection*, which is the doorway height — not necessarily the wall's own top.
    pub floor: f32,
    pub ceil: f32,
    /// The anchoring wall face's own top (its `v_max`), in WT Y — how high the wall this
    /// was cut into actually goes. Only needed to size the lintel over an up-stair; see
    /// [`Self::lintel`]. `None` in a level saved before it was recorded, which reads as
    /// "assume the selection reached the top" and emits no lintel, exactly as those levels
    /// looked.
    #[serde(default)]
    pub face_top: Option<f32>,
    /// Wall-texture floor anchor in WT (JS descriptor `floorY` = the pit/dest
    /// floor). Used to anchor the stair side-wall UVs so they don't shift.
    pub floor_y: f32,
    /// Texture theme inherited from the wall the stair was cut into, and updated
    /// when its room is retextured. Persisted by name, exactly as
    /// [`Brush::scheme`] is.
    #[serde(
        default = "default_scheme",
        serialize_with = "ser_scheme",
        deserialize_with = "de_scheme"
    )]
    pub scheme: usize,
    /// The two void-brush ids this stair carved (JS `voidBrushIds`), so a room
    /// retexture flood-fill can find and re-scheme the matching tread mesh.
    pub void_ids: [u32; 2],
    /// Treads and risers, or the bare slope. `#[serde(default)]` = [`StairShell::Steps`],
    /// so a level saved before ramps existed loads as stairs.
    #[serde(default)]
    pub shell: StairShell,
    /// Horizontal run of one step, in WT — the slope dial. 1.0 is the original 45°;
    /// larger is shallower. **Steeper than 1.0 is not offered**: the player capsule's
    /// `max_slope_climb_angle` is 50°, and the walking surface is the slope in both
    /// shells, so a steeper flight is one nobody can walk up.
    #[serde(default = "default_run_per_step")]
    pub run_per_step: f32,
}

impl StairDesc {
    /// A WT point mapped into world space by the wall axis (JS `csgStairGeometry`
    /// `tw`): `n` runs along the wall normal, `y` is world-up, `u` runs along the
    /// horizontal in-plane axis.
    #[inline]
    fn tw(&self, n: f32, y: f32, u: f32) -> [f32; 3] {
        let mut p = [0.0f32; 3];
        p[self.axis.index()] = n;
        p[1] = y;
        p[self.u_axis.index()] = u;
        p
    }

    /// Which way along the wall normal the flight advances: +1 off a `Max` face.
    #[inline]
    fn dir(&self) -> f32 {
        if self.side == Side::Max {
            1.0
        } else {
            -1.0
        }
    }

    /// Horizontal run of one step, in WT. Guarded so a level carrying a zero (or a
    /// negative) never collapses the flight into the wall.
    #[inline]
    fn run(&self) -> f32 {
        if self.run_per_step > 0.0 {
            self.run_per_step
        } else {
            default_run_per_step()
        }
    }

    /// Total horizontal run of the whole flight, in WT — how far out from the wall it
    /// reaches. The slope dial makes this **longer** than `step_count`, never shorter.
    pub fn total_run(&self) -> f32 {
        self.step_count as f32 * self.run()
    }

    /// The destination floor: `step_count` WT below the anchor floor going down, above
    /// it going up. One step is always 1 WT of rise; only the run varies.
    #[inline]
    fn dest_y(&self) -> f32 {
        match self.direction {
            StairDir::Down => self.floor - self.step_count as f32,
            StairDir::Up => self.floor + self.step_count as f32,
        }
    }

    /// The walking surface as a WT quad: the flight's true slope, from the nosing at the
    /// wall face to the destination floor. `None` for a zero-step op.
    ///
    /// This is the one source for the slope — the collider, the ramp shell's visible
    /// surface, the sloped ceiling (which is this plus the headroom) and the nav overlay
    /// all read it, so none of them can disagree about where the flight actually is.
    pub fn ramp_quad(&self) -> Option<[[f32; 3]; 4]> {
        if self.step_count == 0 {
            return None;
        }
        Some(self.sloped_quad(0.0))
    }

    /// The vertical strip of wall an **up**-stair's carve takes from *above* its doorway,
    /// as `(y_lo, y_hi)` in WT — `None` when there is none to close.
    ///
    /// `confirm_stairs` carves the stairwell from the floor to `ceil + step_count`, one
    /// box, because a box is all a subtract brush is. When the selection reached the top of
    /// the wall that overshoot lands in the solid above the room and nothing shows. When the
    /// author scaled the selection *down* to make a doorway, it eats real wall above the
    /// lintel — and the flat ceiling used to hide that (the opening simply read as a tall
    /// stairwell) where the sloped soffit, which starts at the doorway head, does not.
    ///
    /// So this is closed with a panel rather than by carving less: the void has to stay
    /// tall enough to contain the flight, and one AABB cannot have a sloped top. Clamped to
    /// [`Self::face_top`] so the panel fills the hole and never overlaps the CSG wall face
    /// above it, which would z-fight.
    fn lintel(&self) -> Option<(f32, f32)> {
        if self.direction != StairDir::Up || self.step_count == 0 {
            return None;
        }
        let top = (self.ceil + self.step_count as f32).min(self.face_top?);
        (top > self.ceil).then_some((self.ceil, top))
    }

    /// The slope raised by `lift` WT — the walking surface at 0, the sloped ceiling at
    /// the flight's headroom.
    fn sloped_quad(&self, lift: f32) -> [[f32; 3]; 4] {
        let n0 = self.face_pos;
        let n1 = self.face_pos + self.dir() * self.total_run();
        let (y0, y1) = (self.floor + lift, self.dest_y() + lift);
        [
            self.tw(n0, y0, self.u0),
            self.tw(n1, y1, self.u0),
            self.tw(n1, y1, self.u1),
            self.tw(n0, y0, self.u1),
        ]
    }

    /// Append this stair's tread/riser/side/fill geometry (in meters) to a mesh
    /// buffer. Port of `buildCsgStairGeometry`, but every quad is emitted
    /// **double-sided** — so backface culling is a non-issue and the JS `flip`
    /// bookkeeping is unnecessary (the visible winding always renders, with its
    /// normal toward the viewer). The extra reversed triangles are harmless in
    /// the region's trimesh collider.
    fn append_geometry(&self, pos: &mut Vec<f32>, norm: &mut Vec<f32>, idx: &mut Vec<u32>, ws: f32) {
        let dir = self.dir();
        let (u0, u1) = (self.u0, self.u1);
        let floor = self.floor;
        let sc = self.step_count as i32;
        let run = self.run();
        if sc <= 0 {
            return;
        }

        let mut quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| {
            geom::push_quad_double(pos, norm, idx, a, b, c, d, ws);
        };

        match self.shell {
            StairShell::Steps => {
                for k in 0..sc {
                    let kf = k as f32;
                    // Normal-axis span for this step (`run_per_step` WT deep).
                    let (n_lo, n_hi) = if dir > 0.0 {
                        (self.face_pos + kf * run, self.face_pos + (kf + 1.0) * run)
                    } else {
                        (self.face_pos - (kf + 1.0) * run, self.face_pos - kf * run)
                    };
                    // Vertical span for this step.
                    let (step_floor, step_top) = match self.direction {
                        StairDir::Down => (floor - (kf + 1.0), floor - kf),
                        StairDir::Up => (floor + kf, floor + kf + 1.0),
                    };

                    // Tread (top surface).
                    quad(
                        self.tw(n_lo, step_top, u0),
                        self.tw(n_hi, step_top, u0),
                        self.tw(n_hi, step_top, u1),
                        self.tw(n_lo, step_top, u1),
                    );

                    // Riser (vertical front face of the step).
                    let riser_pos = match self.direction {
                        StairDir::Down => if dir > 0.0 { n_hi } else { n_lo },
                        StairDir::Up => if dir > 0.0 { n_lo } else { n_hi },
                    };
                    quad(
                        self.tw(riser_pos, step_floor, u0),
                        self.tw(riser_pos, step_floor, u1),
                        self.tw(riser_pos, step_top, u1),
                        self.tw(riser_pos, step_top, u0),
                    );

                    // Left/right side walls.
                    quad(
                        self.tw(n_lo, step_floor, u0),
                        self.tw(n_hi, step_floor, u0),
                        self.tw(n_hi, step_top, u0),
                        self.tw(n_lo, step_top, u0),
                    );
                    quad(
                        self.tw(n_hi, step_floor, u1),
                        self.tw(n_lo, step_floor, u1),
                        self.tw(n_lo, step_top, u1),
                        self.tw(n_hi, step_top, u1),
                    );
                }
                // Fill the stepped floor underneath an up-stair.
                if self.direction == StairDir::Up {
                    for k in 0..(sc - 1) {
                        let kf = k as f32;
                        let (fill_lo, fill_hi) = if dir > 0.0 {
                            (self.face_pos + kf * run, self.face_pos + (kf + 1.0) * run)
                        } else {
                            (self.face_pos - (kf + 1.0) * run, self.face_pos - kf * run)
                        };
                        let fill_y = floor + (kf + 1.0);
                        quad(
                            self.tw(fill_lo, fill_y, u0),
                            self.tw(fill_hi, fill_y, u0),
                            self.tw(fill_hi, fill_y, u1),
                            self.tw(fill_lo, fill_y, u1),
                        );
                    }
                }
            }
            StairShell::Ramp => {
                let q = self.sloped_quad(0.0);
                quad(q[0], q[1], q[2], q[3]);
                let base_y = floor.min(self.dest_y());
                for (near, far) in [(q[0], q[1]), (q[3], q[2])] {
                    quad(
                        [near[0], base_y, near[2]],
                        near,
                        far,
                        [far[0], base_y, far[2]],
                    );
                }
            }
        }

        // The lintel over an up-stair's doorway, then the sloped ceiling — see
        // `append_zoned` for why neither needs a CSG change.
        if let Some((lo, hi)) = self.lintel() {
            quad(
                self.tw(self.face_pos, lo, u0),
                self.tw(self.face_pos, lo, u1),
                self.tw(self.face_pos, hi, u1),
                self.tw(self.face_pos, hi, u0),
            );
        }
        let headroom = self.ceil - self.floor;
        if headroom > 0.0 {
            let c = self.sloped_quad(headroom);
            quad(c[0], c[1], c[2], c[3]);
        }
    }

    /// Append a **smooth ramp** walking surface for this stair to a collider mesh
    /// buffer (meters): a single double-sided sloped quad from the base nosing to
    /// the top nosing, in place of the stepped tread/riser geometry. Used only by
    /// the collider path ([`Region::evaluate`]); the visible mesh keeps its
    /// discrete steps via [`append_zoned`](Self::append_zoned), so the player sees
    /// stairs but walks a ramp — no per-riser auto-step pop. The ramp reproduces
    /// the stair's true slope (here always 45°, since each CSG step is 1×1 WT).
    fn append_ramp_collision(
        &self,
        pos: &mut Vec<f32>,
        norm: &mut Vec<f32>,
        idx: &mut Vec<u32>,
        ws: f32,
    ) {
        let Some(q) = self.ramp_quad() else {
            return;
        };
        geom::push_quad_double(pos, norm, idx, q[0], q[1], q[2], q[3], ws);
    }

    /// The tread/riser/side geometry as a standalone mesh (meters), for the ghost
    /// preview drawn while a stair op is pending.
    pub fn mesh(&self) -> CpuMesh {
        let mut pos = Vec::new();
        let mut norm = Vec::new();
        let mut idx = Vec::new();
        self.append_geometry(&mut pos, &mut norm, &mut idx, WORLD_SCALE);
        CpuMesh::from_csg(&pos, &norm, &idx)
    }

    /// Emit this stair's tread/riser/side/fill geometry into a [`ZonedBuilder`]
    /// with **explicit texture zones + UVs**, matching JS `buildCsgStairGeometry`:
    /// tread → 0 (floor), riser → 5 (stair_gradient), and **everything else
    /// (side walls, far ceiling panel, ceiling-drop wall, up-fill) → 3 (upper
    /// wall / brown)** — not the gradient. Per-quad UVs so the gradient riser maps
    /// 0..1 vertically per step. Single-winding (rendered with culling off).
    fn append_zoned(&self, b: &mut ZonedBuilder) {
        let dir = self.dir();
        let (u0, u1) = (self.u0, self.u1);
        let floor = self.floor;
        let sc = self.step_count as i32;
        let step_width = u1 - u0;
        let run = self.run();
        let sch = self.scheme;
        const TREAD: u8 = 0;
        const CEIL: u8 = 1;
        const RISER: u8 = 5;
        const SIDE: u8 = 3;
        if sc <= 0 {
            return;
        }
        // Texture length along the slope, so both shells tile at their true scale
        // instead of being stretched by the horizontal projection.
        let slope_len = (self.total_run().powi(2) + (sc as f32).powi(2)).sqrt();

        match self.shell {
            StairShell::Steps => {
                for k in 0..sc {
                    let kf = k as f32;
                    let (n_lo, n_hi) = if dir > 0.0 {
                        (self.face_pos + kf * run, self.face_pos + (kf + 1.0) * run)
                    } else {
                        (self.face_pos - (kf + 1.0) * run, self.face_pos - kf * run)
                    };
                    let (step_floor, step_top) = match self.direction {
                        StairDir::Down => (floor - (kf + 1.0), floor - kf),
                        StairDir::Up => (floor + kf, floor + kf + 1.0),
                    };
                    let riser_h = step_top - step_floor;

                    // Tread (top surface): U across the tread depth, V across the width.
                    b.emit_quad_uv(
                        [
                            self.tw(n_lo, step_top, u0),
                            self.tw(n_hi, step_top, u0),
                            self.tw(n_hi, step_top, u1),
                            self.tw(n_lo, step_top, u1),
                        ],
                        [[0.0, 0.0], [run, 0.0], [run, step_width], [0.0, step_width]],
                        sch,
                        TREAD,
                    );

                    // Riser (front face): the gradient maps 0..1 top-to-bottom per step.
                    let riser_pos = match self.direction {
                        StairDir::Down => if dir > 0.0 { n_hi } else { n_lo },
                        StairDir::Up => if dir > 0.0 { n_lo } else { n_hi },
                    };
                    let riser_u = step_width / riser_h;
                    b.emit_quad_uv(
                        [
                            self.tw(riser_pos, step_floor, u0),
                            self.tw(riser_pos, step_floor, u1),
                            self.tw(riser_pos, step_top, u1),
                            self.tw(riser_pos, step_top, u0),
                        ],
                        [[0.0, 0.0], [riser_u, 0.0], [riser_u, 1.0], [0.0, 1.0]],
                        sch,
                        RISER,
                    );

                    // Left/right side walls → upper-wall zone.
                    b.emit_quad_uv(
                        [
                            self.tw(n_lo, step_floor, u0),
                            self.tw(n_hi, step_floor, u0),
                            self.tw(n_hi, step_top, u0),
                            self.tw(n_lo, step_top, u0),
                        ],
                        [[0.0, 0.0], [run, 0.0], [run, riser_h], [0.0, riser_h]],
                        sch,
                        SIDE,
                    );
                    b.emit_quad_uv(
                        [
                            self.tw(n_hi, step_floor, u1),
                            self.tw(n_lo, step_floor, u1),
                            self.tw(n_lo, step_top, u1),
                            self.tw(n_hi, step_top, u1),
                        ],
                        [[0.0, 0.0], [run, 0.0], [run, riser_h], [0.0, riser_h]],
                        sch,
                        SIDE,
                    );
                }
                // Fill the stepped floor underneath an up-stair.
                if self.direction == StairDir::Up {
                    for k in 0..(sc - 1) {
                        let kf = k as f32;
                        let (fill_lo, fill_hi) = if dir > 0.0 {
                            (self.face_pos + kf * run, self.face_pos + (kf + 1.0) * run)
                        } else {
                            (self.face_pos - (kf + 1.0) * run, self.face_pos - kf * run)
                        };
                        let fill_y = floor + (kf + 1.0);
                        b.emit_quad_uv(
                            [
                                self.tw(fill_lo, fill_y, u0),
                                self.tw(fill_hi, fill_y, u0),
                                self.tw(fill_hi, fill_y, u1),
                                self.tw(fill_lo, fill_y, u1),
                            ],
                            [[0.0, 0.0], [run, 0.0], [run, step_width], [0.0, step_width]],
                            sch,
                            SIDE,
                        );
                    }
                }
            }
            StairShell::Ramp => {
                // The walking surface itself — the quad the collider has always had.
                b.emit_quad_uv(
                    self.sloped_quad(0.0),
                    [
                        [0.0, 0.0],
                        [slope_len, 0.0],
                        [slope_len, step_width],
                        [0.0, step_width],
                    ],
                    sch,
                    TREAD,
                );
                // Close the wedge underneath: from the slope down to the lower of the two
                // floors — the pit floor going down, the room floor going up. Without
                // these you see straight through the flight from the side, where the
                // stepped shell's per-step side walls did that job.
                let base_y = floor.min(self.dest_y());
                let q = self.sloped_quad(0.0);
                // `sloped_quad` runs near→far down the u0 edge (corners 0,1) and
                // near→far down the u1 edge (corners 3,2).
                for (near, far) in [(q[0], q[1]), (q[3], q[2])] {
                    b.emit_quad_uv(
                        [
                            [near[0], base_y, near[2]],
                            near,
                            far,
                            [far[0], base_y, far[2]],
                        ],
                        [
                            [0.0, 0.0],
                            [0.0, near[1] - base_y],
                            [slope_len, far[1] - base_y],
                            [slope_len, 0.0],
                        ],
                        sch,
                        SIDE,
                    );
                }
            }
        }

        // Close the wall the carve took from above the doorway (up-stairs only).
        if let Some((lo, hi)) = self.lintel() {
            b.emit_quad_uv(
                [
                    self.tw(self.face_pos, lo, u0),
                    self.tw(self.face_pos, lo, u1),
                    self.tw(self.face_pos, hi, u1),
                    self.tw(self.face_pos, hi, u0),
                ],
                [
                    [0.0, 0.0],
                    [step_width, 0.0],
                    [step_width, hi - lo],
                    [0.0, hi - lo],
                ],
                sch,
                SIDE,
            );
        }

        // ── The sloped ceiling, both shells.
        //
        // The flight's own slope lifted by its headroom, so the tunnel keeps the height
        // of the room it leaves all the way along instead of the ceiling staying flat and
        // then falling off a cliff at the far end. It needs no change to the CSG: the
        // void brushes are already carved to the *taller* of the two ends, so this panel
        // always sits at or below what was cut, and seals flush against the destination
        // corridor's ceiling. The dead space above it is enclosed and never seen.
        let headroom = self.ceil - self.floor;
        if headroom > 0.0 {
            b.emit_quad_uv(
                self.sloped_quad(headroom),
                [
                    [0.0, 0.0],
                    [slope_len, 0.0],
                    [slope_len, step_width],
                    [0.0, step_width],
                ],
                sch,
                CEIL,
            );
        }
    }

    /// Reconstruct the solid step blocks (WT AABBs `[x, y, z, w, h, d]`) — one per
    /// step, from the void floor up to that step's tread. Direct port of
    /// `navWorld.stairSolidBoxes`; fed to the nav voxelizer so grid nav sees the
    /// treads as walkable ground (the mesh isn't visible to grid nav).
    pub fn solid_boxes(&self) -> Vec<[f32; 6]> {
        let dir = self.dir();
        let sc = self.step_count as f32;
        let run = self.run();
        let void_floor = match self.direction {
            StairDir::Down => self.floor - sc,
            StairDir::Up => self.floor,
        };
        let (u0, u1) = (self.u0, self.u1);
        let mut boxes = Vec::new();
        for k in 0..self.step_count as i32 {
            let kf = k as f32;
            let n_lo = if dir > 0.0 {
                self.face_pos + kf * run
            } else {
                self.face_pos - (kf + 1.0) * run
            };
            let step_top = match self.direction {
                StairDir::Down => self.floor - kf,
                StairDir::Up => self.floor + (kf + 1.0),
            };
            let h = step_top - void_floor;
            if h <= 0.0 {
                continue;
            }
            match self.axis {
                Axis::X => boxes.push([n_lo, void_floor, u0, run, h, u1 - u0]),
                _ => boxes.push([u0, void_floor, n_lo, u1 - u0, h, run]),
            }
        }
        boxes
    }
}

// ─── Brush → polygons ───────────────────────────────────────────────
//
// Port of `brush_to_polygons` (spike lib.rs): convert a WT-space box to 6 quad
// polygons in meters, CCW-from-outside so `Plane::from_points` yields outward
// normals. Taper is omitted (Phase 1 boxes have none).

fn brush_to_polygons(b: &Brush, ws: f32) -> Vec<Polygon> {
    let ws64 = ws as f64;
    let x0 = (b.x as f64 * ws64) as f32;
    let x1 = ((b.x + b.w) as f64 * ws64) as f32;
    let y0 = (b.y as f64 * ws64) as f32;
    let y1 = ((b.y + b.h) as f64 * ws64) as f32;
    let z0 = (b.z as f64 * ws64) as f32;
    let z1 = ((b.z + b.d) as f64 * ws64) as f32;

    // 8 corners: index bits are (x1?, y1?, z1?).
    let c: [[f32; 3]; 8] = [
        [x0, y0, z0], // 0: ---
        [x1, y0, z0], // 1: +--
        [x0, y1, z0], // 2: -+-
        [x1, y1, z0], // 3: ++-
        [x0, y0, z1], // 4: --+
        [x1, y0, z1], // 5: +-+
        [x0, y1, z1], // 6: -++
        [x1, y1, z1], // 7: +++
    ];

    // 6 faces, CCW winding seen from outside (identical vertex order to spike).
    const FACES: [[usize; 4]; 6] = [
        [0, 4, 6, 2], // x-min
        [1, 3, 7, 5], // x-max
        [0, 1, 5, 4], // y-min
        [2, 6, 7, 3], // y-max
        [0, 2, 3, 1], // z-min
        [4, 5, 7, 6], // z-max
    ];

    FACES
        .iter()
        .filter_map(|vi| Polygon::new(vec![c[vi[0]], c[vi[1]], c[vi[2]], c[vi[3]]]))
        .collect()
}

// ─── AABB (meters) for the evaluate() early-reject ──────────────────

#[derive(Clone, Copy)]
struct Aabb {
    min: [f32; 3],
    max: [f32; 3],
}

impl Aabb {
    fn from_brush(b: &Brush, ws: f32) -> Self {
        let ws64 = ws as f64;
        Aabb {
            min: [
                (b.x as f64 * ws64) as f32,
                (b.y as f64 * ws64) as f32,
                (b.z as f64 * ws64) as f32,
            ],
            max: [
                ((b.x + b.w) as f64 * ws64) as f32,
                ((b.y + b.h) as f64 * ws64) as f32,
                ((b.z + b.d) as f64 * ws64) as f32,
            ],
        }
    }

    fn intersects(&self, o: &Aabb) -> bool {
        self.min[0] <= o.max[0]
            && self.max[0] >= o.min[0]
            && self.min[1] <= o.max[1]
            && self.max[1] >= o.min[1]
            && self.min[2] <= o.max[2]
            && self.max[2] >= o.min[2]
    }

    fn union(&self, o: &Aabb) -> Aabb {
        Aabb {
            min: [
                self.min[0].min(o.min[0]),
                self.min[1].min(o.min[1]),
                self.min[2].min(o.min[2]),
            ],
            max: [
                self.max[0].max(o.max[0]),
                self.max[1].max(o.max[1]),
                self.max[2].max(o.max[2]),
            ],
        }
    }
}

// ─── Localized boolean ops ──────────────────────────────────────────
//
// A brush's boolean op can only affect polygons its *volume* overlaps — a
// subtract carves nothing outside its box, a union adds nothing outside its box.
// So instead of rebuilding a BSP over the whole accumulated soup for every brush
// (O(total polygons) per op), we split the soup into the polygons whose AABB
// meets the brush (`near`) and the rest (`far`), run the actual boolean op on
// `near` only, and pass `far` through untouched. The surface is identical; the
// cost drops to O(local), which is what lets a huge level re-bake as fast as a
// small one. AABBs are conservative, so a polygon the brush truly touches always
// lands in `near` — no false negatives.

/// AABB (meters) of a polygon's vertices.
fn poly_aabb(p: &Polygon) -> Aabb {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &p.vertices {
        for k in 0..3 {
            min[k] = min[k].min(v[k]);
            max[k] = max[k].max(v[k]);
        }
    }
    Aabb { min, max }
}

/// Split `soup` into (near, far) by whether each polygon's AABB meets `aabb`.
fn partition_near_far(soup: Vec<Polygon>, aabb: &Aabb) -> (Vec<Polygon>, Vec<Polygon>) {
    let mut near = Vec::new();
    let mut far = Vec::new();
    for p in soup {
        if poly_aabb(&p).intersects(aabb) {
            near.push(p);
        } else {
            far.push(p);
        }
    }
    (near, far)
}

/// [`csg_subtract`] restricted to the polygons the brush AABB actually reaches.
///
/// If the brush's AABB meets no existing polygon (`near` empty) we can't tell
/// locally whether the brush sits inside solid (→ carve a cavity) or inside air
/// (→ no-op), so we fall back to the full op over the whole soup (`far` == soup
/// then), which resolves it correctly. That path only fires for a brush touching
/// no surface — e.g. the first room carved into the bare shell.
fn subtract_local(soup: Vec<Polygon>, b_polys: Vec<Polygon>, b_aabb: &Aabb) -> Vec<Polygon> {
    let (near, mut far) = partition_near_far(soup, b_aabb);
    if near.is_empty() {
        return csg_subtract(far, b_polys);
    }
    let mut clipped = csg_subtract(near, b_polys);
    far.append(&mut clipped);
    far
}

/// [`csg_union`] restricted to the polygons the brush AABB actually reaches.
/// Same empty-`near` fallback as [`subtract_local`].
fn union_local(soup: Vec<Polygon>, b_polys: Vec<Polygon>, b_aabb: &Aabb) -> Vec<Polygon> {
    let (near, mut far) = partition_near_far(soup, b_aabb);
    if near.is_empty() {
        return csg_union(far, b_polys);
    }
    let mut merged = csg_union(near, b_polys);
    far.append(&mut merged);
    far
}

// ─── The fold ───────────────────────────────────────────────────────

/// Evaluate `shell ± brushes` into a polygon soup, in meters. Ported from the
/// spike `evaluate()` — shell, then each brush in order, with a disjoint-AABB
/// early-reject and a consecutive-subtract pre-merge — but every boolean op is
/// **localized** ([`subtract_local`]/[`union_local`]): it only re-clips the
/// polygons within the brush's AABB, so per-edit cost scales with the edited
/// region's local complexity, not the whole level's.
fn evaluate(shell: &Brush, brushes: &[Brush], ws: f32) -> Vec<Polygon> {
    let mut result = brush_to_polygons(shell, ws);
    // Grows with unions; subtracts never grow it, so it stays a correct upper
    // bound for early-rejecting non-overlapping brushes.
    let mut acc_aabb = Aabb::from_brush(shell, ws);

    let mut i = 0;
    while i < brushes.len() {
        let is_subtract = brushes[i].op == Op::Subtract;
        let brush_aabb = Aabb::from_brush(&brushes[i], ws);

        if is_subtract {
            // Disjoint subtract is a no-op — skip the BSP build entirely.
            if !brush_aabb.intersects(&acc_aabb) {
                i += 1;
                continue;
            }

            // Consecutive-subtract run: union the overlapping ones, subtract once.
            let mut run_end = i + 1;
            while run_end < brushes.len() && brushes[run_end].op == Op::Subtract {
                run_end += 1;
            }
            if run_end - i >= 3 {
                let mut merged: Vec<Polygon> = Vec::new();
                let mut merged_aabb: Option<Aabb> = None;
                let mut started = false;
                for j in i..run_end {
                    let j_aabb = Aabb::from_brush(&brushes[j], ws);
                    if !j_aabb.intersects(&acc_aabb) {
                        continue;
                    }
                    let polys = brush_to_polygons(&brushes[j], ws);
                    merged_aabb = Some(match merged_aabb {
                        Some(a) => a.union(&j_aabb),
                        None => j_aabb,
                    });
                    if !started {
                        merged = polys;
                        started = true;
                    } else {
                        // The merged void is itself local to the run's AABBs.
                        merged = union_local(merged, polys, &j_aabb);
                    }
                }
                if started {
                    result = subtract_local(result, merged, &merged_aabb.unwrap());
                }
                i = run_end;
                continue;
            }
        }

        let polys = brush_to_polygons(&brushes[i], ws);
        if is_subtract {
            result = subtract_local(result, polys, &brush_aabb);
        } else if !brush_aabb.intersects(&acc_aabb) {
            // Disjoint union — concatenate; no BSP needed.
            result.extend(polys);
            acc_aabb = acc_aabb.union(&brush_aabb);
        } else {
            result = union_local(result, polys, &brush_aabb);
            acc_aabb = acc_aabb.union(&brush_aabb);
        }
        i += 1;
    }

    result
}

// ─── Region clustering (JS `src/core/csg/regions.js`) ────────────────
//
// Brushes are grouped into connected regions so each re-bakes independently.
// Two brushes share a region if their AABBs overlap or merely touch — more
// permissive than the face-adjacency `brushes_touching` used for room retexture,
// because an additive brush sitting *inside* a room overlaps the room's subtract
// and must fold together with it. A doorway cut that spans two rooms' walls
// overlaps both, so it bridges them into one region without special-casing frames
// (JS `clusterBrushes` comment). This is why a fully-connected base is a single
// region: the win is for *disconnected* geometry (separate wings) and for
// bounding the worst case, not for splitting a connected blob.

/// Whether two brushes' AABBs overlap or touch (inclusive of edge contact), in
/// WT. Port of JS `brushesOverlapOrTouch`.
pub fn brushes_overlap_or_touch(a: &Brush, b: &Brush) -> bool {
    let span = |br: &Brush, i: usize| match i {
        0 => (br.x, br.x + br.w),
        1 => (br.y, br.y + br.h),
        _ => (br.z, br.z + br.d),
    };
    for i in 0..3 {
        let (a0, a1) = span(a, i);
        let (b0, b1) = span(b, i);
        // Strict `<` so shared faces (a1 == b0) still count as touching.
        if a1 < b0 || b1 < a0 {
            return false;
        }
    }
    true
}

/// Partition `brushes` into connected components by [`brushes_overlap_or_touch`]
/// (JS `clusterBrushes`). Returns groups of indices into `brushes`; order within
/// a group is arbitrary. An empty input yields no groups.
pub fn cluster_brush_indices(brushes: &[Brush]) -> Vec<Vec<usize>> {
    let n = brushes.len();
    let mut visited = vec![false; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut group = Vec::new();
        let mut stack = vec![start];
        visited[start] = true;
        while let Some(cur) = stack.pop() {
            group.push(cur);
            for (other, seen) in visited.iter_mut().enumerate() {
                if *seen {
                    continue;
                }
                if brushes_overlap_or_touch(&brushes[cur], &brushes[other]) {
                    *seen = true;
                    stack.push(other);
                }
            }
        }
        groups.push(group);
    }
    groups
}

// ─── Region ─────────────────────────────────────────────────────────

/// One cluster of brushes plus its auto-resized shell — the unit of re-bake and
/// (upstream) the unit of collision (per-region colliders, per the plan). Ports
/// `CSGRegion`: the shell is an additive box fit to the subtractive brushes plus
/// a 1-WT pad so the carved cavities always sit inside solid.
pub struct Region {
    pub id: u32,
    pub brushes: Vec<Brush>,
    /// Confirmed stairs in this region (JS `state.csg.csgStairs`, scoped per
    /// region). Their void brushes live in `brushes`; these descriptors drive the
    /// tread mesh (folded into [`evaluate`](Self::evaluate)) and the nav solids.
    pub stairs: Vec<StairDesc>,
    shell: Brush,
}

/// Shell padding around the subtractive brushes, in WT (JS `WALL_THICKNESS`-ish
/// 1-tile margin so walls have thickness).
const SHELL_PAD: f32 = 1.0;

impl Region {
    pub fn new(id: u32) -> Self {
        // Placeholder shell; update_shell() resizes it before every evaluate.
        Region {
            id,
            brushes: Vec::new(),
            stairs: Vec::new(),
            shell: Brush::new(u32::MAX, Op::Add, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0),
        }
    }

    /// Resize the shell to enclose every subtractive brush, padded by [`SHELL_PAD`]
    /// on all sides. No subtractive brushes → shell left as-is (nothing to house).
    fn update_shell(&mut self) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for b in self.brushes.iter().filter(|b| b.op == Op::Subtract) {
            any = true;
            min[0] = min[0].min(b.x);
            min[1] = min[1].min(b.y);
            min[2] = min[2].min(b.z);
            max[0] = max[0].max(b.x + b.w);
            max[1] = max[1].max(b.y + b.h);
            max[2] = max[2].max(b.z + b.d);
        }
        if !any {
            return;
        }
        self.shell.x = min[0] - SHELL_PAD;
        self.shell.y = min[1] - SHELL_PAD;
        self.shell.z = min[2] - SHELL_PAD;
        self.shell.w = (max[0] - min[0]) + SHELL_PAD * 2.0;
        self.shell.h = (max[1] - min[1]) + SHELL_PAD * 2.0;
        self.shell.d = (max[2] - min[2]) + SHELL_PAD * 2.0;
    }

    /// Re-run CSG for this region and return the resulting **collider** mesh in
    /// meters. Any confirmed stairs then get a smooth **ramp** walking surface
    /// appended (not the stepped treads), so the player walks the slope without
    /// auto-stepping each riser — the discrete steps live only in the render mesh
    /// ([`evaluate_textured`](Self::evaluate_textured)).
    pub fn evaluate(&mut self) -> CpuMesh {
        self.update_shell();
        let polys = evaluate(&self.shell, &self.brushes, WORLD_SCALE);
        let (mut pos, mut norm, mut idx) = polygons_to_mesh(&polys);
        for s in &self.stairs {
            s.append_ramp_collision(&mut pos, &mut norm, &mut idx, WORLD_SCALE);
        }
        CpuMesh::from_csg(&pos, &norm, &idx)
    }

    /// This region's brushes as the classifier wants them (WT AABB + the owner
    /// attributes it recovers per triangle).
    ///
    /// Public because the editor's surface probe has to classify with *exactly* the
    /// same inputs as the bake — an explanation of why a triangle looks the way it
    /// does is worthless if it is computed from a slightly different brush list.
    pub fn brush_infos(&self) -> Vec<BrushInfo> {
        self.brushes
            .iter()
            .map(|b| BrushInfo {
                id: b.id,
                min: [b.x, b.y, b.z],
                max: [b.x + b.w, b.y + b.h, b.z + b.d],
                floor_y: b.floor_y,
                // A duct anchors its UVs to its own corner; everything else keeps the
                // world-space grid it has always had. Derived, not persisted — the
                // `vent` flag already says which brushes want it.
                origin_xz: if b.vent { [b.x, b.z] } else { [0.0, 0.0] },
                scheme: b.scheme,
                frame: b.frame,
                door: b.door,
                face_tex: b.face_tex,
            })
            .collect()
    }

    /// Re-run CSG and classify the result into a textured, per-zone-grouped mesh
    /// for rendering (port of `assignUVsAndZones` + the stair zoned emission). The
    /// collider still comes from [`evaluate`](Self::evaluate); this is render-only.
    pub fn evaluate_textured(&mut self, platforms: &[Platform]) -> TexturedMesh {
        self.update_shell();
        let polys = evaluate(&self.shell, &self.brushes, WORLD_SCALE);
        let (pos, _norm, idx) = polygons_to_mesh(&polys);

        // Per-brush attributes drive per-triangle scheme + wall-UV floor anchor
        // (the face-map recovers the owner inside `classify_soup`).
        let brush_infos = self.brush_infos();

        let mut b = ZonedBuilder::new();
        let cols = ColumnInputs::new(&self.brushes, platforms);
        let cornice = textures::cornice_table();
        uv_zones::classify_soup(&mut b, &pos, &idx, &brush_infos, default_scheme(), &cols, &cornice);
        for s in &self.stairs {
            s.append_zoned(&mut b);
        }
        b.finish()
    }

    /// Evaluate the region **once** and derive *both* the collider mesh and the
    /// textured render mesh from the same polygon soup. This is the per-edit path
    /// ([`World::rebuild_region`]); the standalone [`evaluate`](Self::evaluate) /
    /// [`evaluate_textured`](Self::evaluate_textured) each run the full CSG fold,
    /// so calling both (as the editor used to) folded the region twice per edit.
    /// The fold is the dominant cost, so sharing it roughly halves per-edit time.
    ///
    /// The base soup (`pos`/`norm`/`idx`) feeds the textured classify directly by
    /// reference; the collider clones it only to append the stair **ramp** surface
    /// (the render mesh keeps the stepped treads via [`append_zoned`]). The clone
    /// is a cheap memcpy next to the fold.
    pub fn evaluate_both(&mut self, platforms: &[Platform]) -> (CpuMesh, TexturedMesh) {
        self.update_shell();
        let polys = evaluate(&self.shell, &self.brushes, WORLD_SCALE);
        let (pos, norm, idx) = polygons_to_mesh(&polys);

        // Collider: base soup + smooth stair ramps.
        let collider = if self.stairs.is_empty() {
            CpuMesh::from_csg(&pos, &norm, &idx)
        } else {
            let mut cpos = pos.clone();
            let mut cnorm = norm.clone();
            let mut cidx = idx.clone();
            for s in &self.stairs {
                s.append_ramp_collision(&mut cpos, &mut cnorm, &mut cidx, WORLD_SCALE);
            }
            CpuMesh::from_csg(&cpos, &cnorm, &cidx)
        };

        // Textured render mesh: classify the same soup into per-zone groups, then
        // append the stepped stair geometry with explicit zones/UVs.
        let brush_infos = self.brush_infos();
        let mut zb = ZonedBuilder::new();
        let cols = ColumnInputs::new(&self.brushes, platforms);
        let cornice = textures::cornice_table();
        uv_zones::classify_soup(&mut zb, &pos, &idx, &brush_infos, default_scheme(), &cols, &cornice);
        for s in &self.stairs {
            s.append_zoned(&mut zb);
        }
        (collider, zb.finish())
    }

    /// Recompute the shell to fit the current brushes (call before querying
    /// [`shell`](Self::shell) or [`solid_at`](Self::solid_at) after edits).
    pub fn refresh_shell(&mut self) {
        self.update_shell();
    }

    /// The current shell box (WT). Only valid after [`refresh_shell`](Self::refresh_shell)
    /// or [`evaluate`](Self::evaluate).
    pub fn shell(&self) -> Brush {
        self.shell
    }

    /// Solidity at a WT point: replay CSG membership — inside the shell (solid),
    /// then each brush in order flips it (`add` → solid, `subtract` → air).
    /// Mirrors JS `regionSolidAt`; used by the nav voxelizer.
    pub fn solid_at(&self, x: f32, y: f32, z: f32) -> bool {
        if !self.shell.contains(x, y, z) {
            return false; // outside the shell — this region doesn't cover the point
        }
        let mut solid = true;
        for b in &self.brushes {
            if b.contains(x, y, z) {
                solid = b.op == Op::Add;
            }
        }
        solid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4-step down-stair off a `Max` X wall at x=8, in a room whose floor is y=0 and
    /// whose ceiling is y=8.
    fn csg_stair(shell: StairShell, run_per_step: f32) -> StairDesc {
        StairDesc {
            direction: StairDir::Down,
            step_count: 4,
            axis: Axis::X,
            side: Side::Max,
            face_pos: 8.0,
            u_axis: Axis::Z,
            u0: 2.0,
            u1: 6.0,
            floor: 0.0,
            ceil: 8.0,
            // The selection reached the top of the wall — the ordinary case, and the one
            // with no wall left above the doorway.
            face_top: Some(8.0),
            floor_y: -4.0,
            scheme: 0,
            void_ids: [0, 0],
            shell,
            run_per_step,
        }
    }

    /// The slope dial stretches the **run**, never the rise, and every consumer reads it
    /// off the same place. A flight whose carve, treads and nav boxes disagreed about how
    /// far out it reached would bury its last steps in the wall.
    #[test]
    fn the_slope_dial_stretches_the_run_across_every_consumer() {
        let steep = csg_stair(StairShell::Steps, 1.0);
        let shallow = csg_stair(StairShell::Steps, 3.0);
        assert_eq!(steep.total_run(), 4.0, "1x: four steps, four tiles out");
        assert_eq!(shallow.total_run(), 12.0, "3x: same drop, three times the run");

        // Nav solids: same count and same tops, three times as deep, reaching as far as
        // the flight says it does.
        let (a, b) = (steep.solid_boxes(), shallow.solid_boxes());
        assert_eq!(a.len(), b.len(), "the same four steps either way");
        for (sa, sb) in a.iter().zip(&b) {
            assert_eq!(sa[1], sb[1], "same floor");
            assert_eq!(sa[4], sb[4], "same height — the rise never changes");
            assert!((sb[3] - 3.0 * sa[3]).abs() < 1e-5, "three times the tread depth");
        }
        assert!(
            (b.last().unwrap()[0] + b.last().unwrap()[3] - (8.0 + 12.0)).abs() < 1e-5,
            "the last step ends where `total_run` says the flight does"
        );

        // And the walking surface agrees with both.
        let q = shallow.ramp_quad().expect("a ramp");
        assert!((q[0][0] - 8.0).abs() < 1e-5, "starts at the wall face");
        assert!((q[1][0] - 20.0).abs() < 1e-5, "and reaches 12 WT out");
        assert!((q[1][1] + 4.0).abs() < 1e-5, "landing 4 WT down, as always");
    }

    /// The ramp shell draws the flight's own slope and closes the wedge under it — the
    /// job the stepped shell's per-step side walls were doing. Nav is untouched: it still
    /// gets the stepped boxes, which is what makes a ramp climbable at all.
    #[test]
    fn the_ramp_shell_draws_the_slope_and_closes_under_it() {
        let steps = csg_stair(StairShell::Steps, 1.0);
        let ramp = csg_stair(StairShell::Ramp, 1.0);
        assert_eq!(
            format!("{:?}", steps.solid_boxes()),
            format!("{:?}", ramp.solid_boxes()),
            "the shell is a render choice — nav sees the same flight"
        );

        let mut b = ZonedBuilder::new();
        ramp.append_zoned(&mut b);
        let mesh = b.finish();
        // Walking surface + two side wedges + the sloped ceiling. Nothing per-step.
        assert_eq!(mesh.vertices.len(), 4 * 6, "four quads, whatever the step count");
        let mut steps_b = ZonedBuilder::new();
        steps.append_zoned(&mut steps_b);
        assert!(
            steps_b.finish().vertices.len() > mesh.vertices.len(),
            "the stepped shell is the one with per-step geometry"
        );

        // The drawn slope IS the collider's ramp — they cannot drift.
        let q = ramp.ramp_quad().expect("a ramp");
        for c in q {
            let m = [c[0] * WORLD_SCALE, c[1] * WORLD_SCALE, c[2] * WORLD_SCALE];
            assert!(
                mesh.vertices.iter().any(|v| (v.pos[0] - m[0]).abs() < 1e-3
                    && (v.pos[1] - m[1]).abs() < 1e-3
                    && (v.pos[2] - m[2]).abs() < 1e-3),
                "corner {c:?} of the collider ramp is drawn"
            );
        }
    }

    /// **The ceiling follows the stairs.** It used to stay flat the whole way down and
    /// then fall off a cliff at the far end; now it is the flight's own slope lifted by
    /// the headroom, so the tunnel keeps the height of the room it left. Both shells.
    ///
    /// It needs no CSG change because the void brushes are already carved to the taller
    /// of the two ends — this panel always sits at or below what was cut.
    #[test]
    fn the_stairwell_ceiling_follows_the_flight_in_both_shells() {
        for shell in [StairShell::Steps, StairShell::Ramp] {
            let s = csg_stair(shell, 2.0);
            let mut b = ZonedBuilder::new();
            s.append_zoned(&mut b);
            let mesh = b.finish();

            // The soffit is the one thing in the ceiling zone.
            let ceil_group = mesh
                .groups
                .iter()
                .find(|g| g.zone == 1)
                .unwrap_or_else(|| panic!("{shell:?}: a ceiling quad is emitted"));
            assert_eq!(ceil_group.count, 6, "{shell:?}: one quad (6 indices), no more");

            let headroom = s.ceil - s.floor;
            let floor_q = s.ramp_quad().expect("a ramp");
            for (i, c) in s.sloped_quad(headroom).iter().enumerate() {
                assert!(
                    (c[1] - (floor_q[i][1] + headroom)).abs() < 1e-5,
                    "{shell:?}: the ceiling is the floor slope plus the headroom"
                );
            }
            // It lands exactly on the destination corridor's ceiling, so the two meet
            // flush instead of leaving a step.
            let far = s.sloped_quad(headroom)[1];
            assert!(
                (far[1] - (s.ceil - s.step_count as f32)).abs() < 1e-5,
                "{shell:?}: the far end meets the corridor ceiling"
            );
            // And never above what the void carved, or it would poke into solid rock.
            assert!(
                s.sloped_quad(headroom).iter().all(|c| c[1] <= s.ceil + 1e-5),
                "{shell:?}: the soffit stays inside the carve"
            );
        }
    }

    /// **The hole the sloped ceiling exposed.** An up-stair's carve is one box reaching
    /// `step_count` WT above the doorway, because that is all a subtract brush is. With a
    /// full-height selection that overshoot lands in the solid above the room and nothing
    /// shows; scale the selection down to make a doorway and it eats real wall over the
    /// lintel. The old flat ceiling hid it — the opening just read as a tall stairwell —
    /// and the soffit, which starts at the doorway head, does not.
    #[test]
    fn an_up_stair_closes_the_wall_its_carve_takes_above_the_doorway() {
        // A 12 WT wall, doorway selected only to y=6, four steps up.
        let mut s = csg_stair(StairShell::Ramp, 1.0);
        s.direction = StairDir::Up;
        s.ceil = 6.0;
        s.face_top = Some(12.0);
        let (lo, hi) = s.lintel().expect("there is wall above the doorway to close");
        assert_eq!((lo, hi), (6.0, 10.0), "from the doorway head to the top of the carve");

        let mut b = ZonedBuilder::new();
        s.append_zoned(&mut b);
        let mesh = b.finish();
        let ws = WORLD_SCALE;
        for y in [lo, hi] {
            assert!(
                mesh.vertices
                    .iter()
                    .any(|v| (v.pos[0] - 8.0 * ws).abs() < 1e-4 && (v.pos[1] - y * ws).abs() < 1e-4),
                "the lintel panel sits in the wall plane at y={y}"
            );
        }

        // Clamped to the wall's own top, so the panel fills the hole and never overlaps
        // the CSG wall face above it — that overlap is what would z-fight.
        s.face_top = Some(8.0);
        assert_eq!(s.lintel(), Some((6.0, 8.0)), "clamped to the wall top");

        // A selection that reached the top has no wall above it to lose.
        s.ceil = 8.0;
        assert_eq!(s.lintel(), None);

        // And neither has a down-stair: its carve tops out at the doorway head already.
        s.direction = StairDir::Down;
        s.ceil = 6.0;
        s.face_top = Some(12.0);
        assert_eq!(s.lintel(), None, "a down-stair carves nothing above its doorway");
    }

    /// A level saved before ramps and the slope dial existed loads as the 45° staircase
    /// it was — both fields default, so the file format did not need a bump.
    #[test]
    fn csg_stairs_saved_before_ramps_load_as_stairs() {
        let s: StairDesc = serde_json::from_str(
            r#"{"direction":"Down","step_count":4,"axis":"X","side":"Max","face_pos":8,
                "u_axis":"Z","u0":2,"u1":6,"floor":0,"ceil":8,"floor_y":-4,
                "void_ids":[1,2]}"#,
        )
        .expect("a shell-less stair still parses");
        assert_eq!(s.shell, StairShell::Steps);
        assert_eq!(s.run_per_step, 1.0);
        assert_eq!(s.total_run(), 4.0, "the original 45° flight");

        let back: StairDesc =
            serde_json::from_str(&serde_json::to_string(&csg_stair(StairShell::Ramp, 2.0)).unwrap())
                .unwrap();
        assert_eq!(back.shell, StairShell::Ramp);
        assert_eq!(back.run_per_step, 2.0);
    }

    #[test]
    fn room_shell_is_nonempty_and_watertight_count() {
        // Editor's opening move: one subtract brush inside an auto-shell = a room.
        let mut region = Region::new(0);
        region
            .brushes
            .push(Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 12.0, 8.0, 12.0));
        let mesh = region.evaluate();
        assert!(!mesh.vertices.is_empty(), "room should produce geometry");
        assert!(mesh.indices.len() % 3 == 0 && !mesh.indices.is_empty());
    }

    #[test]
    fn disjoint_subtract_is_a_noop() {
        // Test the fold directly with a fixed shell (Region::update_shell would
        // otherwise grow the shell to enclose the far brush). A subtract whose
        // AABB misses the accumulator must leave the result byte-identical to
        // the shell alone.
        let shell = Brush::new(u32::MAX, Op::Add, 0.0, 0.0, 0.0, 12.0, 8.0, 12.0);
        let (_p, _n, base) = polygons_to_mesh(&evaluate(&shell, &[], WORLD_SCALE));

        let far = Brush::new(2, Op::Subtract, 500.0, 500.0, 500.0, 4.0, 4.0, 4.0);
        let (_p2, _n2, with_far) = polygons_to_mesh(&evaluate(&shell, &[far], WORLD_SCALE));
        assert_eq!(base.len(), with_far.len(), "disjoint subtract changed geometry");
    }

    #[test]
    fn push_pull_are_inverse_on_a_max_face() {
        let mut brush = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        brush.push_face(Axis::X, Side::Max, 4.0);
        assert_eq!(brush.w, 14.0);
        assert!(brush.pull_face(Axis::X, Side::Max, 4.0));
        assert_eq!(brush.w, 10.0);
    }

    #[test]
    fn pull_refuses_to_collapse_a_thin_brush() {
        let mut brush = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 3.0, 8.0, 10.0);
        assert!(!brush.pull_face(Axis::X, Side::Max, 4.0), "3 <= 4, must no-op");
        assert_eq!(brush.w, 3.0);
    }

    #[test]
    fn overlap_or_touch_covers_edge_contact_and_gap() {
        let a = Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 10.0, 10.0);
        // Shares the x=10 face → touches.
        let flush = Brush::new(2, Op::Subtract, 10.0, 0.0, 0.0, 10.0, 10.0, 10.0);
        assert!(brushes_overlap_or_touch(&a, &flush), "flush faces touch");
        // Overlapping volume → touches.
        let overlap = Brush::new(3, Op::Add, 5.0, 5.0, 5.0, 4.0, 4.0, 4.0);
        assert!(brushes_overlap_or_touch(&a, &overlap), "overlap counts");
        // 1-WT gap on x → disjoint.
        let gap = Brush::new(4, Op::Subtract, 11.0, 0.0, 0.0, 10.0, 10.0, 10.0);
        assert!(!brushes_overlap_or_touch(&a, &gap), "gap is disjoint");
    }

    #[test]
    fn cluster_splits_disjoint_and_joins_connected() {
        // Two connected (touching) + one far away → 2 groups of sizes {2, 1}.
        let brushes = vec![
            Brush::new(1, Op::Subtract, 0.0, 0.0, 0.0, 10.0, 10.0, 10.0),
            Brush::new(2, Op::Subtract, 10.0, 0.0, 0.0, 10.0, 10.0, 10.0), // touches #1
            Brush::new(3, Op::Subtract, 100.0, 0.0, 0.0, 10.0, 10.0, 10.0), // far
        ];
        let mut groups = cluster_brush_indices(&brushes);
        groups.sort_by_key(|g| g.len());
        assert_eq!(groups.len(), 2, "one connected pair + one island");
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 2);
    }

    #[test]
    fn min_face_push_holds_the_opposite_face() {
        let mut brush = Brush::new(1, Op::Subtract, 5.0, 0.0, 0.0, 10.0, 8.0, 10.0);
        let max_before = brush.face_pos(Axis::X, Side::Max);
        brush.push_face(Axis::X, Side::Min, 4.0);
        assert_eq!(brush.x, 1.0);
        assert_eq!(brush.face_pos(Axis::X, Side::Max), max_before, "max face fixed");
    }
}
