//! Room plan tool: draft a room's **footprint** on a top-down drafting plane, nudge
//! its corners until the layout is right, then extrude it into the subtract brushes
//! that *are* the room.
//!
//! **Why this exists next to the freeform draw tool.** [`super::draw`] is a *surface*
//! tool — every corner it takes is projected onto a room face you are already standing
//! in, which makes it excellent for detailing a room you can see and useless for
//! deciding where the next room should go. Laying out a base with it means flying
//! outside the level to judge the arrangement, memorising a position, flying back in
//! and carving blind. This tool inverts that: the level is drawn as a plan, footprints
//! are placed in world coordinates against the rooms that already exist, and the
//! result is a free-standing room you connect afterwards with the door/hole tools.
//!
//! **What it builds is not a new kind of geometry.** The world is implicitly solid —
//! the starting level is one `Op::Subtract` box — so a room *is* a subtract brush and
//! nothing downstream has to learn anything. A footprint drawn away from every
//! existing room clusters into a region of its own ([`super::super::regions`]); one
//! drawn overlapping an existing room merges with it. Both fall out of the recluster.
//!
//! **The 90° machinery is [`super::draw`]'s, reused verbatim.** Vertices are integer
//! WT — here world `(x, z)` rather than face-UV `(u, v)` — which is what keeps the
//! self-intersection test and [`rect_decompose`] exact and epsilon-free, and the
//! decomposition into rectangles is required for the same reason it is there:
//! `polygons_to_mesh` fan-triangulates, so a concave (L, U, T) footprint pushed
//! through as one prism would be garbage.
//!
//! **The drafting views are the other half of the feature.** A plan you cannot see
//! squarely is not a plan, so arming this tool swaps the fly camera for an
//! [`OrthoCamera`] and frees the mouse. The six axis views + perspective are on the
//! numpad, Blender-style. This is the only tool that does that — every other one
//! aims with the camera crosshair, which an orthographic view does not have.

use engine::render::camera::{OrthoCamera, ViewAxis};

use super::super::*;
use super::draw::{axis_lock, overlap, rect_decompose, segment_self_intersects, Overlap};

/// Default height of a fresh room, in WT. Matches the height of the level the editor
/// boots into, which is the only "known good" room dimension in the codebase.
const ROOM_DEFAULT_HEIGHT: f32 = 8.0;

/// Clamp on the extrude, in WT, in either direction — a runaway-scroll guard rather
/// than a design limit (mirrors `draw::DRAW_DEPTH_MAX`).
const ROOM_HEIGHT_MAX: f32 = 64.0;

/// Clamp on the drafting plane's height and on a corner's distance from the origin,
/// in WT. Nothing stops a click in an ortho view from landing a mile away, and a
/// footprint out there would allocate a bounding box to match.
const ROOM_EXTENT_MAX: f32 = 512.0;

/// How wide a drawn line is, as a fraction of the view's vertical half-extent. Scaling
/// with zoom is what keeps the plan readable at every scale — a fixed world width
/// vanishes when you zoom out to place a room and swamps the grid when you zoom in.
const LINE_FRAC: f32 = 0.0035;

/// Corner handle half-size, in the same units as [`LINE_FRAC`]. Also the pick radius
/// for grabbing one, so a handle is exactly as easy to hit as it looks.
const HANDLE_FRAC: f32 = 0.012;

/// Colours (linear RGB, matching the gizmo channel's other markers).
const C_SKETCH: [f32; 3] = [1.0, 0.85, 0.15];
const C_PENDING: [f32; 3] = [0.65, 0.55, 0.12];
const C_HANDLE: [f32; 3] = [1.0, 0.45, 0.10];
const C_CLOSE: [f32; 3] = [0.25, 1.0, 0.35];
const C_GHOST: [f32; 3] = [0.30, 0.80, 1.00];
const C_ON_SLICE: [f32; 3] = [0.45, 0.55, 0.70];
const C_OFF_SLICE: [f32; 3] = [0.20, 0.22, 0.28];
const C_GRID: [f32; 3] = [0.16, 0.17, 0.20];
const C_GRID_MAJOR: [f32; 3] = [0.28, 0.30, 0.36];

/// Grid line spacing in WT, coarsest first — [`grid_step`] picks the finest one that
/// doesn't flood the view.
const GRID_STEPS: [i32; 4] = [64, 16, 4, 1];
/// Max grid lines drawn per axis. Past this the step coarsens.
const GRID_MAX_LINES: i32 = 60;

// ─── State ───────────────────────────────────────────────────────────────────

/// The room tool's phase. `None` on [`World`] = the tool is off. Esc walks back down
/// this ladder one rung (and one corner) at a time, exactly as the draw tool's does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RoomPhase {
    /// Placing corners. An empty vertex list is the idle state — the cursor previews
    /// where the first corner would land.
    Drawing,
    /// The loop is closed. The wheel slides the whole footprint up and down the Y
    /// axis; corners can be dragged.
    Base,
    /// The wheel sets the signed extrude (up from the plane, or down from it) and a
    /// click commits. Corners can still be dragged.
    Height,
}

impl World {
    // ─── Arming / teardown ───────────────────────────────────────────────────

    /// Whether the room tool is armed, so the app frees the cursor, routes clicks and
    /// scroll here, and reads the numpad as view keys.
    pub fn is_room_tool(&self) -> bool {
        self.room_phase.is_some()
    }

    /// Whether the footprint's corners can be dragged right now (the loop is closed).
    pub fn is_room_editing(&self) -> bool {
        matches!(self.room_phase, Some(RoomPhase::Base | RoomPhase::Height))
    }

    /// Arm / disarm the room tool. Radial-only — there is no key left to give it, and
    /// it does not want one: the tool takes the camera and the cursor over, which is
    /// not something to trip into with a stray keypress.
    pub fn room_tool_key(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.room_phase.is_some() {
            self.clear_room_state();
            log::info!("room tool off");
            return;
        }
        // The crosshair tools are mutually exclusive; this one additionally owns the
        // camera, so it has to be the only thing armed.
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.clear_draw_state();
        self.selected = None;

        self.room_phase = Some(RoomPhase::Drawing);
        self.room_verts.clear();
        self.room_cursor = None;
        self.room_rects.clear();
        self.room_drag = None;
        self.room_height = ROOM_DEFAULT_HEIGHT;
        // Start the drafting plane on the floor of whatever the camera is nearest, so
        // the first room lands level with the level rather than at an arbitrary Y.
        self.room_base = self.nearest_floor_y();
        self.enter_room_view(ViewAxis::Top);
        log::info!(
            "room tool armed — top view, base y={} WT. Click corners, click the first to close. Numpad: 7 top / 1 front / 3 right (+Ctrl for the opposite), 5 perspective",
            self.room_base
        );
    }

    /// Disarm (Esc at the top of the ladder, a mode switch, the cursor being released).
    pub fn cancel_room(&mut self) {
        if self.room_phase.is_some() {
            self.clear_room_state();
        }
    }

    /// Clear every scrap of room-tool state, including the drafting camera.
    pub(crate) fn clear_room_state(&mut self) {
        self.room_phase = None;
        self.room_verts.clear();
        self.room_cursor = None;
        self.room_rects.clear();
        self.room_drag = None;
        self.room_height = ROOM_DEFAULT_HEIGHT;
        self.ortho = None;
    }

    /// Esc while armed: one rung back down the ladder — height → base, base → reopen
    /// the outline, outline → drop the last corner, empty outline → `false`, which
    /// lets the app disarm the tool wholesale.
    pub fn room_escape(&mut self) -> bool {
        match self.room_phase {
            Some(RoomPhase::Height) => {
                self.room_phase = Some(RoomPhase::Base);
                log::info!("room: back to the base height — scroll to slide the plan up/down");
                true
            }
            Some(RoomPhase::Base) => {
                // Reopen the loop. The closing click never pushed a duplicate of the
                // first corner, so re-closing is one click.
                self.room_rects.clear();
                self.room_drag = None;
                self.room_phase = Some(RoomPhase::Drawing);
                log::info!("room: outline reopened");
                true
            }
            Some(RoomPhase::Drawing) => {
                if self.room_verts.is_empty() {
                    return false;
                }
                self.room_verts.pop();
                true
            }
            None => false,
        }
    }

    // ─── The drafting camera ─────────────────────────────────────────────────

    /// Which orthographic view is active, or `None` in the perspective fly view.
    pub fn room_view(&self) -> Option<ViewAxis> {
        self.ortho.as_ref().map(|o| o.axis)
    }

    /// Snap to an orthographic view, framing the level the first time and keeping the
    /// current focus + zoom on every switch after (so cycling views doesn't lose your
    /// place).
    pub fn enter_room_view(&mut self, axis: ViewAxis) {
        self.ortho_last = axis;
        match self.ortho.as_mut() {
            // A view change keeps the focus point and the zoom, so cycling views
            // orbits what you are already looking at rather than re-framing the whole
            // level out from under you.
            Some(o) => o.axis = axis,
            None => {
                let mut o = OrthoCamera::new(axis, self.level_center_m(), 8.0);
                let (min, max) = self.level_bounds_m();
                o.frame(min, max);
                self.ortho = Some(o);
            }
        }
        log::info!("room: {} view", axis.label());
    }

    /// Leave the orthographic views for the perspective fly camera, without disarming
    /// the tool. The sketch stays live and stays drawable — the drafting plane has a
    /// known Y, so a perspective ray hits it just as well as an overhead one.
    pub fn leave_room_view(&mut self) {
        if self.ortho.is_some() {
            self.ortho = None;
            log::info!("room: perspective view");
        }
    }

    /// Numpad `5`: flip between the last orthographic view and perspective.
    pub fn toggle_room_view(&mut self) {
        if self.ortho.is_some() {
            self.leave_room_view();
        } else {
            self.enter_room_view(self.ortho_last);
        }
    }

    /// Drag the orthographic view by a cursor delta in physical pixels (RMB pan).
    pub fn pan_room_view(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        if let Some(o) = self.ortho.as_mut() {
            o.pan(dx, dy, viewport_h);
        }
    }

    /// Zoom the orthographic view by wheel notches (positive = in).
    pub fn zoom_room_view(&mut self, steps: f32) {
        if let Some(o) = self.ortho.as_mut() {
            o.zoom(steps);
        }
    }

    /// The wheel's job in the current phase: zoom while the outline is open, then the
    /// base height, then the extrude. `zoom_override` (Ctrl) forces the zoom in every
    /// phase, which is the only way to reach it once the loop closes.
    pub fn room_scroll(&mut self, steps: f32, zoom_override: bool) {
        if zoom_override {
            self.zoom_room_view(steps);
            return;
        }
        match self.room_phase {
            Some(RoomPhase::Drawing) | None => self.zoom_room_view(steps),
            Some(RoomPhase::Base) => {
                self.room_base = (self.room_base + steps).clamp(-ROOM_EXTENT_MAX, ROOM_EXTENT_MAX);
            }
            Some(RoomPhase::Height) => {
                // Signed, and it runs *through* zero into a downward extrude — the
                // same convention as the draw tool's depth, so "scroll down to go the
                // other way" means one thing in both tools.
                self.room_height =
                    (self.room_height + steps).clamp(-ROOM_HEIGHT_MAX, ROOM_HEIGHT_MAX);
            }
        }
    }

    // ─── Pointer → drafting plane ────────────────────────────────────────────

    /// Intersect a world-space ray (metres) with the drafting plane and snap the hit
    /// to the WT grid.
    ///
    /// `None` when the ray runs along the plane — which is exactly the four side
    /// views. Drawing is therefore top/bottom/perspective only, and that is a property
    /// of the geometry rather than a rule the tool imposes: a side view genuinely
    /// cannot say where along the depth axis a click meant.
    pub(crate) fn room_plane_hit(&self, origin: Vec3, dir: Vec3) -> Option<(i32, i32)> {
        if dir.y.abs() < 1.0e-4 {
            return None;
        }
        let plane_y = self.room_base * WORLD_SCALE;
        let t = (plane_y - origin.y) / dir.y;
        if t <= 0.0 || !t.is_finite() {
            return None;
        }
        let p = origin + dir * t;
        let x = (p.x / WORLD_SCALE).round();
        let z = (p.z / WORLD_SCALE).round();
        if !x.is_finite() || !z.is_finite() {
            return None;
        }
        Some((
            x.clamp(-ROOM_EXTENT_MAX, ROOM_EXTENT_MAX) as i32,
            z.clamp(-ROOM_EXTENT_MAX, ROOM_EXTENT_MAX) as i32,
        ))
    }

    /// Per-move pointer update: refresh the previewed corner, or advance a corner drag.
    /// Returns whether anything moved (the app only needs this to skip redundant work).
    pub fn room_hover(&mut self, origin: Vec3, dir: Vec3) -> bool {
        if self.room_phase.is_none() {
            return false;
        }
        let Some(hit) = self.room_plane_hit(origin, dir) else {
            return false;
        };
        if let Some(i) = self.room_drag {
            return self.drag_room_corner(i, hit);
        }
        let cand = match self.room_verts.last() {
            // Every segment is 90°-snapped, so the previewed corner is the click point
            // locked onto whichever axis it travelled furthest along.
            Some(&last) if self.room_phase == Some(RoomPhase::Drawing) => axis_lock(last, hit),
            _ => hit,
        };
        let changed = self.room_cursor != Some(cand);
        self.room_cursor = Some(cand);
        changed
    }

    // ─── Corner dragging ─────────────────────────────────────────────────────

    /// Try to grab the corner under the pointer. Returns whether one was grabbed, so
    /// the caller can fall through to whatever a click otherwise means.
    pub fn start_room_drag(&mut self, origin: Vec3, dir: Vec3) -> bool {
        if !self.is_room_editing() {
            return false;
        }
        let Some(hit) = self.room_plane_hit(origin, dir) else {
            return false;
        };
        // Pick radius in WT, matched to the drawn handle so a handle is as easy to
        // grab as it looks. In perspective there is no single scale, so use a fixed
        // couple of cells.
        let r = match self.ortho.as_ref() {
            Some(o) => ((o.half_h * HANDLE_FRAC * 2.0) / WORLD_SCALE).max(1.0),
            None => 1.5,
        };
        let mut best: Option<(usize, f32)> = None;
        for (i, v) in self.room_verts.iter().enumerate() {
            let d = (((v.0 - hit.0) as f32).powi(2) + ((v.1 - hit.1) as f32).powi(2)).sqrt();
            if d <= r && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        let Some((i, _)) = best else { return false };
        self.room_drag = Some(i);
        true
    }

    /// Release a corner drag.
    pub fn end_room_drag(&mut self) {
        if self.room_drag.take().is_some() {
            log::info!(
                "room: footprint is {} corner(s) → {} rect(s)",
                self.room_verts.len(),
                self.room_rects.len()
            );
        }
    }

    /// Move corner `i` to `to`, dragging its two neighbours' shared coordinates along
    /// so every edge stays axis-aligned.
    ///
    /// **A rectilinear corner cannot move alone.** It joins one horizontal and one
    /// vertical edge; moving it without its neighbours would tilt both. So the
    /// neighbour that shares this corner's `x` keeps sharing it, and likewise for `z`
    /// — which from the author's side reads exactly as "drag this corner", because the
    /// two edges meeting there follow it.
    ///
    /// A drag that would make the outline non-simple is **refused** rather than
    /// clamped: the footprint simply stops following the pointer at the illegal
    /// position and resumes when it comes back, which needs no explanation on screen.
    fn drag_room_corner(&mut self, i: usize, to: (i32, i32)) -> bool {
        let n = self.room_verts.len();
        if i >= n || n < 4 || self.room_verts[i] == to {
            return false;
        }
        let mut next = self.room_verts.clone();
        let (prev_i, next_i) = ((i + n - 1) % n, (i + 1) % n);
        for j in [prev_i, next_i] {
            if next[j].0 == next[i].0 {
                next[j].0 = to.0;
            } else if next[j].1 == next[i].1 {
                next[j].1 = to.1;
            }
        }
        next[i] = to;
        if !polygon_is_simple(&next) {
            return false;
        }
        let rects = rect_decompose(&next);
        if rects.is_empty() {
            return false;
        }
        self.room_verts = next;
        self.room_rects = rects;
        true
    }

    // ─── Clicks ──────────────────────────────────────────────────────────────

    /// Handle a left-click, dispatching on the phase. Returns **every** rebuilt region
    /// mesh for the app to upload — a committed room reclusters the level, so this is
    /// a `Vec` for the same reason [`World::draw_click`] is one.
    pub fn room_click(&mut self, origin: Vec3, dir: Vec3) -> Vec<RegionMesh> {
        match self.room_phase {
            Some(RoomPhase::Drawing) => {
                self.room_place_corner(origin, dir);
                Vec::new()
            }
            Some(RoomPhase::Base) => {
                // Confirming the base height moves on to the extrude. Corner drags are
                // grabbed before this ever runs (see `start_room_drag`).
                self.room_phase = Some(RoomPhase::Height);
                log::info!(
                    "room: base y={} WT — scroll to set the height (down extrudes below the plane), click to build",
                    self.room_base
                );
                Vec::new()
            }
            Some(RoomPhase::Height) => self.confirm_room(),
            None => Vec::new(),
        }
    }

    /// Drop a corner, or close the loop when the click lands back on the first one.
    fn room_place_corner(&mut self, origin: Vec3, dir: Vec3) {
        let Some(hit) = self.room_plane_hit(origin, dir) else {
            log::info!("room: a side view can't place a corner — numpad 7 for top, 5 for perspective");
            return;
        };
        let Some(&last) = self.room_verts.last() else {
            self.room_verts.push(hit);
            self.room_cursor = Some(hit);
            log::info!("room: corner at ({}, {}) WT", hit.0, hit.1);
            return;
        };
        let cand = axis_lock(last, hit);
        if cand == last {
            return; // the pointer hasn't left the last corner
        }
        // A rectilinear loop needs at least 4 corners, so anything shorter landing on
        // the first one is a stray click, not a close.
        let closing = self.room_verts.len() >= 4 && cand == self.room_verts[0];
        if segment_self_intersects(&self.room_verts, cand, closing) {
            log::info!("room: that segment would cross the outline — pick another corner");
            return;
        }
        if !closing {
            self.room_verts.push(cand);
            return;
        }
        let rects = rect_decompose(&self.room_verts);
        if rects.is_empty() {
            log::info!("room: that outline encloses no area — nothing to build");
            return;
        }
        self.room_rects = rects;
        self.room_phase = Some(RoomPhase::Base);
        self.room_cursor = None;
        log::info!(
            "room: footprint closed ({} corners → {} rect(s)) — drag a corner to adjust, scroll to set the floor height, click to go on",
            self.room_verts.len(),
            self.room_rects.len()
        );
    }

    // ─── Commit ──────────────────────────────────────────────────────────────

    /// Build the room: add its decomposed subtract brushes and re-bake.
    pub(crate) fn confirm_room(&mut self) -> Vec<RegionMesh> {
        let mut brushes = self.room_brushes();
        if brushes.is_empty() {
            log::info!("room: height is 0 — scroll to set one");
            return Vec::new();
        }
        let group = self.next_brush_id;
        for b in brushes.iter_mut() {
            b.id = self.next_brush_id;
            b.group = group;
            self.next_brush_id += 1;
        }
        let count = brushes.len();
        let (floor, height) = (brushes[0].y, brushes[0].h);

        // A footprint drawn in open space belongs to no existing region, and
        // `assign_brush_to_region` can only place a brush that is already inside one —
        // so give it a region of its own up front and let the recluster decide what it
        // really is. That decision is why this takes the **full** recluster rather than
        // `rebuild_affected_regions`: a room drawn over an existing one has to merge
        // with it, and merging is exactly the case the incremental path bails on. The
        // memo cache keeps the untouched regions from re-folding, so the cost is the
        // upload, not the CSG.
        let rid = self.next_region_id;
        self.next_region_id += 1;
        let mut region = Region::new(rid);
        region.brushes = brushes;
        region.refresh_shell();
        self.regions.push(region);

        log::info!(
            "room: built {count} brush(es) as group {group} — floor y={floor} WT, {height} WT tall. It is sealed in solid rock until you cut a door or a hole into it."
        );

        // Stay armed and go back to a clean outline: laying out a base means drawing
        // several rooms in a row, and having to re-arm between each would fight that.
        self.room_verts.clear();
        self.room_rects.clear();
        self.room_cursor = None;
        self.room_drag = None;
        self.room_phase = Some(RoomPhase::Drawing);
        self.recluster_all()
    }

    /// The brushes the current footprint + height would build — one per decomposed
    /// rectangle, `id`/`group` unassigned.
    ///
    /// The single source for both the ghost and the commit, so the preview cannot
    /// drift from what lands. Empty at height 0.
    fn room_brushes(&self) -> Vec<Brush> {
        if self.room_height == 0.0 || self.room_rects.is_empty() {
            return Vec::new();
        }
        // A downward extrude hangs the room *below* the drafting plane, so the plane is
        // its ceiling rather than its floor.
        let h = self.room_height.abs();
        let y0 = if self.room_height > 0.0 {
            self.room_base
        } else {
            self.room_base - h
        };
        self.room_rects
            .iter()
            .map(|&(x, z, w, d)| {
                let mut b =
                    Brush::new(0, Op::Subtract, x as f32, y0, z as f32, w as f32, h, d as f32);
                b.scheme = self.room_scheme;
                // **One** wall-texture anchor for the whole room, not one per brush.
                // `uv_zones::face_owner` reads the owning brush's `floor_y` as the wall
                // UV origin, so leaving these at the per-brush default would make an
                // L-shaped room's two halves band against each other along an internal
                // decomposition boundary the author never drew. Same rule, same reason,
                // as `draw::draw_brushes`.
                b.floor_y = y0;
                b
            })
            .collect()
    }

    /// The theme new rooms are built with (`1`–`9` while the tool is armed). Held here
    /// rather than read off the crosshair because an orthographic view has no
    /// crosshair to read.
    pub fn set_room_scheme(&mut self, scheme: usize) {
        self.room_scheme = scheme;
        log::info!("room: new rooms use theme '{}'", textures::scheme_name(scheme));
    }

    // ─── Read-out ────────────────────────────────────────────────────────────

    /// One line describing what the tool is waiting for, for the HUD / logs.
    pub fn room_status(&self) -> Option<String> {
        let phase = self.room_phase?;
        let view = match self.room_view() {
            Some(a) => a.label(),
            None => "persp",
        };
        Some(match phase {
            RoomPhase::Drawing if self.room_verts.is_empty() => {
                format!("ROOM [{view}]  y={}  click the first corner", self.room_base)
            }
            RoomPhase::Drawing => format!(
                "ROOM [{view}]  y={}  {} corner(s) — click the first to close",
                self.room_base,
                self.room_verts.len()
            ),
            RoomPhase::Base => format!(
                "ROOM [{view}]  floor y={}  scroll to move, drag a corner, click to go on",
                self.room_base
            ),
            RoomPhase::Height => format!(
                "ROOM [{view}]  y={} h={}  scroll to size, click to build",
                self.room_base, self.room_height
            ),
        })
    }

    // ─── The plan overlay ────────────────────────────────────────────────────

    /// The room tool's schematic, drawn into the gizmo channel (depth-Always, so it
    /// reads through solid rock — which is the whole point when every room is a void
    /// inside solid rock).
    ///
    /// Four layers: the WT grid on the drafting plane, every existing room's footprint
    /// (bright if it straddles the plane, dim if it is on another storey — that Y slice
    /// is what stops a multi-storey base from drawing as mush), the sketch itself, and
    /// the extrude ghost.
    pub(crate) fn room_overlay_mesh(&self) -> Option<ColoredMesh> {
        self.room_phase?;
        let mut v = Vec::new();
        let mut idx = Vec::new();
        // Line width and handle size track the zoom so the plan reads the same at
        // every scale. In perspective there is no single scale, so pick something
        // sane for a room-sized subject.
        let (t, hs) = match self.ortho.as_ref() {
            Some(o) => (o.half_h * LINE_FRAC, o.half_h * HANDLE_FRAC),
            None => (0.02, 0.07),
        };
        let plane = self.room_base * WORLD_SCALE;

        // The grid and the schematic footprints belong to the drafting views only. In
        // the perspective view the rooms are simply *there* to be looked at, and an
        // x-ray outline of each one painted over the geometry you can already see is
        // noise rather than information.
        if let Some(o) = self.ortho.as_ref() {
            self.push_plan_grid(&mut v, &mut idx, o, plane, t);
            self.push_room_footprints(&mut v, &mut idx, t);
        }
        self.push_sketch(&mut v, &mut idx, plane, t, hs);
        self.push_extrude_ghost(&mut v, &mut idx, t);

        (!v.is_empty()).then_some(ColoredMesh {
            vertices: v,
            indices: idx,
        })
    }

    /// The WT grid, on the drafting plane, bounded to what the view can see.
    fn push_plan_grid(
        &self,
        v: &mut Vec<ColorVertex>,
        idx: &mut Vec<u32>,
        o: &OrthoCamera,
        plane: f32,
        t: f32,
    ) {
        // Generous horizontal margin — the aspect ratio isn't known here, and drawing a
        // little off-screen is free next to computing it.
        let half_wt = (o.half_h / WORLD_SCALE).max(1.0);
        let cx = o.center.x / WORLD_SCALE;
        let cz = o.center.z / WORLD_SCALE;
        let (x0, x1) = (cx - half_wt * 2.5, cx + half_wt * 2.5);
        let (z0, z1) = (cz - half_wt * 2.5, cz + half_wt * 2.5);
        let step = grid_step(x1 - x0);
        let major = step * 4;
        let clamp = |a: f32| a.clamp(-ROOM_EXTENT_MAX, ROOM_EXTENT_MAX);
        let (gx0, gx1) = (
            (clamp(x0) as i32).div_euclid(step) * step,
            (clamp(x1) as i32).div_euclid(step) * step,
        );
        let (gz0, gz1) = (
            (clamp(z0) as i32).div_euclid(step) * step,
            (clamp(z1) as i32).div_euclid(step) * step,
        );
        let mut x = gx0;
        while x <= gx1 {
            let c = if x % major == 0 { C_GRID_MAJOR } else { C_GRID };
            push_line(v, idx, wt(x as f32, 0.0, gz0 as f32).with_y(plane), wt(x as f32, 0.0, gz1 as f32).with_y(plane), t * 0.6, c);
            x += step;
        }
        let mut z = gz0;
        while z <= gz1 {
            let c = if z % major == 0 { C_GRID_MAJOR } else { C_GRID };
            push_line(v, idx, wt(gx0 as f32, 0.0, z as f32).with_y(plane), wt(gx1 as f32, 0.0, z as f32).with_y(plane), t * 0.6, c);
            z += step;
        }
    }

    /// Every existing room, as the outline of its footprint drawn at its own floor
    /// height. In a top view they read as a plan; in a side view the storeys separate
    /// out on their own.
    fn push_room_footprints(&self, v: &mut Vec<ColorVertex>, idx: &mut Vec<u32>, t: f32) {
        for r in &self.regions {
            for b in &r.brushes {
                if b.op != Op::Subtract {
                    continue;
                }
                let on_slice = self.room_base >= b.y - 0.01 && self.room_base <= b.y + b.h + 0.01;
                let c = if on_slice { C_ON_SLICE } else { C_OFF_SLICE };
                let w = if on_slice { t * 0.9 } else { t * 0.6 };
                push_rect_outline(v, idx, b.x, b.y, b.z, b.x + b.w, b.z + b.d, w, c);
            }
        }
    }

    /// The footprint being drawn: its committed edges, the corner handles, and the
    /// axis-locked segment the next click would commit.
    fn push_sketch(
        &self,
        v: &mut Vec<ColorVertex>,
        idx: &mut Vec<u32>,
        plane: f32,
        t: f32,
        hs: f32,
    ) {
        let n = self.room_verts.len();
        if n == 0 {
            if let Some(c) = self.room_cursor {
                push_handle(v, idx, c, plane, hs, C_HANDLE);
            }
            return;
        }
        let closed = self.room_phase != Some(RoomPhase::Drawing);
        let segs = if closed { n } else { n - 1 };
        for i in 0..segs {
            let a = self.room_verts[i];
            let b = self.room_verts[(i + 1) % n];
            push_line(v, idx, wt_at(a, plane), wt_at(b, plane), t, C_SKETCH);
        }
        for (i, &p) in self.room_verts.iter().enumerate() {
            // While drawing, the first corner is the close target and says so.
            let c = if !closed && i == 0 && n >= 4 { C_CLOSE } else { C_HANDLE };
            let s = if Some(i) == self.room_drag { hs * 1.6 } else { hs };
            push_handle(v, idx, p, plane, s, c);
        }
        if !closed {
            if let (Some(cur), Some(&last)) = (self.room_cursor, self.room_verts.last()) {
                if cur != last {
                    push_line(v, idx, wt_at(last, plane), wt_at(cur, plane), t * 0.8, C_PENDING);
                    push_handle(v, idx, cur, plane, hs * 0.7, C_PENDING);
                }
            }
        }
    }

    /// The room the current height would build, as a wireframe box per decomposed
    /// rectangle. Driven off [`room_brushes`](Self::room_brushes) so the ghost is
    /// literally the thing that would be committed.
    fn push_extrude_ghost(&self, v: &mut Vec<ColorVertex>, idx: &mut Vec<u32>, t: f32) {
        if self.room_phase != Some(RoomPhase::Height) {
            return;
        }
        for b in self.room_brushes() {
            let (x0, y0, z0) = (b.x, b.y, b.z);
            let (x1, y1, z1) = (b.x + b.w, b.y + b.h, b.z + b.d);
            push_rect_outline(v, idx, x0, y0, z0, x1, z1, t, C_GHOST);
            push_rect_outline(v, idx, x0, y1, z0, x1, z1, t, C_GHOST);
            for (cx, cz) in [(x0, z0), (x1, z0), (x0, z1), (x1, z1)] {
                push_line(v, idx, wt(cx, y0, cz), wt(cx, y1, cz), t, C_GHOST);
            }
        }
    }

    // ─── Framing helpers ─────────────────────────────────────────────────────

    /// World-space AABB of every subtractive brush, in metres. Falls back to a small
    /// box at the origin for an empty level.
    fn level_bounds_m(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for r in &self.regions {
            for b in r.brushes.iter().filter(|b| b.op == Op::Subtract) {
                min = min.min(wt(b.x, b.y, b.z));
                max = max.max(wt(b.x + b.w, b.y + b.h, b.z + b.d));
            }
        }
        if !min.is_finite() || !max.is_finite() || min.x > max.x {
            return (Vec3::splat(-4.0), Vec3::splat(4.0));
        }
        (min, max)
    }

    fn level_center_m(&self) -> Vec3 {
        let (a, b) = self.level_bounds_m();
        (a + b) * 0.5
    }

    /// The floor height (WT) of whichever room the camera is nearest, so a fresh
    /// drafting plane starts level with the level instead of at an arbitrary Y.
    fn nearest_floor_y(&self) -> f32 {
        let cam = self.camera.pos;
        let mut best: Option<(f32, f32)> = None;
        for r in &self.regions {
            for b in r.brushes.iter().filter(|b| b.op == Op::Subtract) {
                let c = wt(b.x + b.w * 0.5, b.y + b.h * 0.5, b.z + b.d * 0.5);
                let d = c.distance_squared(cam);
                if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best = Some((d, b.y));
                }
            }
        }
        best.map(|(_, y)| y).unwrap_or(0.0)
    }
}

// ─── Free functions ──────────────────────────────────────────────────────────

/// WT triple → world metres.
fn wt(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z) * WORLD_SCALE
}

/// An integer plan corner at a given world-metres height.
fn wt_at(p: (i32, i32), y_m: f32) -> Vec3 {
    Vec3::new(p.0 as f32 * WORLD_SCALE, y_m, p.1 as f32 * WORLD_SCALE)
}

/// The finest [`GRID_STEPS`] entry that keeps the line count under
/// [`GRID_MAX_LINES`] across `span` WT.
fn grid_step(span_wt: f32) -> i32 {
    for &s in GRID_STEPS.iter().rev() {
        if span_wt / s as f32 <= GRID_MAX_LINES as f32 {
            return s;
        }
    }
    GRID_STEPS[0]
}

/// An axis-aligned line between two world-metres points, as a thin box. Every line
/// this tool draws is axis-aligned, so a box is exact rather than an approximation.
fn push_line(
    v: &mut Vec<ColorVertex>,
    idx: &mut Vec<u32>,
    a: Vec3,
    b: Vec3,
    t: f32,
    rgb: [f32; 3],
) {
    let half = Vec3::splat(t.max(1.0e-4));
    let min = a.min(b) - half;
    let max = a.max(b) + half;
    push_colored_box(v, idx, min, max, rgb);
}

/// A corner handle: a small cube centred on a plan corner.
fn push_handle(
    v: &mut Vec<ColorVertex>,
    idx: &mut Vec<u32>,
    p: (i32, i32),
    y_m: f32,
    s: f32,
    rgb: [f32; 3],
) {
    let c = wt_at(p, y_m);
    push_colored_box(v, idx, c - Vec3::splat(s), c + Vec3::splat(s), rgb);
}

/// The outline of an XZ rectangle (WT) at height `y` (WT), as four thin boxes.
#[allow(clippy::too_many_arguments)]
fn push_rect_outline(
    v: &mut Vec<ColorVertex>,
    idx: &mut Vec<u32>,
    x0: f32,
    y: f32,
    z0: f32,
    x1: f32,
    z1: f32,
    t: f32,
    rgb: [f32; 3],
) {
    let c = [
        (wt(x0, y, z0), wt(x1, y, z0)),
        (wt(x1, y, z0), wt(x1, y, z1)),
        (wt(x1, y, z1), wt(x0, y, z1)),
        (wt(x0, y, z1), wt(x0, y, z0)),
    ];
    for (a, b) in c {
        push_line(v, idx, a, b, t, rgb);
    }
}

/// Whether a closed rectilinear outline is simple — every edge axis-aligned and
/// non-degenerate, and no two edges meeting anywhere but at a shared corner.
///
/// The incremental [`segment_self_intersects`] answers "may I add this segment?",
/// which is the right question while drawing and the wrong one after a corner drag:
/// a drag moves three corners at once and can push an edge across one on the far side
/// of the loop. So this re-checks the whole polygon. `n` is a hand-drawn corner count,
/// so O(n²) over exact integers is nothing.
pub(crate) fn polygon_is_simple(v: &[(i32, i32)]) -> bool {
    let n = v.len();
    if n < 4 {
        return false;
    }
    for i in 0..n {
        let (a, b) = (v[i], v[(i + 1) % n]);
        if a == b || (a.0 != b.0 && a.1 != b.1) {
            return false; // zero-length or diagonal
        }
    }
    for i in 0..n {
        let (a1, a2) = (v[i], v[(i + 1) % n]);
        for j in (i + 1)..n {
            let (b1, b2) = (v[j], v[(j + 1) % n]);
            let adjacent = j == i + 1 || (i == 0 && j == n - 1);
            match overlap(a1, a2, b1, b2) {
                Overlap::None => {}
                Overlap::Point(p) => {
                    // Neighbours are allowed to meet, but only at the corner they
                    // actually share — an edge clipping a neighbour's far end is a
                    // fold-back, not a join.
                    let shared = if j == i + 1 { a2 } else { a1 };
                    if !adjacent || p != shared {
                        return false;
                    }
                }
                Overlap::Segment => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ─── Fixtures ────────────────────────────────────────────────────────────

    /// A ray straight down onto the drafting plane from `(x, z)` WT — how a top view
    /// clicks, and the one the tool is actually driven with in every test below. Goes
    /// through the real `room_plane_hit`, so the snapping and clamping are under test
    /// rather than stubbed.
    fn down_at(x: f32, z: f32) -> (Vec3, Vec3) {
        (
            Vec3::new(x * WORLD_SCALE, 64.0 * WORLD_SCALE, z * WORLD_SCALE),
            Vec3::NEG_Y,
        )
    }

    /// Arm the tool on a booted world.
    fn armed() -> World {
        let mut w = World::new();
        w.initial_meshes();
        w.room_tool_key();
        assert!(w.is_room_tool(), "tool armed");
        w
    }

    /// Click a plan corner from directly above it.
    fn click(w: &mut World, x: i32, z: i32) -> Vec<RegionMesh> {
        let (o, d) = down_at(x as f32, z as f32);
        w.with_undo_many(|w| w.room_click(o, d))
    }

    /// Walk a closed footprint: every corner, then the first one again to close.
    fn footprint(w: &mut World, corners: &[(i32, i32)]) {
        for &(x, z) in corners {
            click(w, x, z);
        }
        let (fx, fz) = corners[0];
        click(w, fx, fz);
        assert_eq!(
            w.room_phase,
            Some(RoomPhase::Base),
            "clicking the first corner again closed the loop"
        );
    }

    /// Every brush in the world with an id at or past `since`.
    fn added(w: &World, since: u32) -> Vec<Brush> {
        w.regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied())
            .filter(|b| b.id >= since)
            .collect()
    }

    /// Drive the whole tool once: footprint, base height, room height, commit.
    fn build_room(w: &mut World, corners: &[(i32, i32)], base: f32, height: f32) -> Vec<Brush> {
        let first = w.next_brush_id;
        footprint(w, corners);
        w.room_base = base;
        click(w, corners[0].0, corners[0].1); // Base → Height
        assert_eq!(w.room_phase, Some(RoomPhase::Height));
        w.room_height = height;
        click(w, corners[0].0, corners[0].1); // commit
        added(w, first)
    }

    // ─── Outline validity ────────────────────────────────────────────────────

    /// The whole-polygon check exists because the incremental one can't see a corner
    /// drag: a drag moves three corners at once and can push an edge across one on the
    /// far side of the loop, which `segment_self_intersects` is never asked about.
    #[test]
    fn polygon_simplicity_accepts_rectilinear_loops_and_rejects_the_broken_ones() {
        assert!(polygon_is_simple(&[(0, 0), (6, 0), (6, 4), (0, 4)]), "a rectangle");
        assert!(
            polygon_is_simple(&[(0, 0), (6, 0), (6, 2), (2, 2), (2, 6), (0, 6)]),
            "an L is simple even though it is concave"
        );
        assert!(
            !polygon_is_simple(&[(0, 0), (6, 0), (6, 4)]),
            "three corners cannot close a rectilinear loop"
        );
        assert!(
            !polygon_is_simple(&[(0, 0), (6, 3), (6, 6), (0, 6)]),
            "a diagonal edge is rejected outright — it would kill the decomposition"
        );
        assert!(
            !polygon_is_simple(&[(0, 0), (0, 0), (6, 0), (6, 4), (0, 4)]),
            "a zero-length edge (a duplicated corner) is degenerate"
        );
        // A figure-eight: the two lobes share the middle edge's line and cross.
        assert!(
            !polygon_is_simple(&[(0, 0), (4, 0), (4, 4), (2, 4), (2, -2), (0, -2)]),
            "an outline that folds back through itself is not simple"
        );
    }

    // ─── Ray → drafting plane ────────────────────────────────────────────────

    /// Snapping, clamping, and the refusal that makes the four side views
    /// non-drawable. That last one is the load-bearing case: a side view genuinely
    /// cannot say where along its depth axis a click meant, so the tool must decline
    /// rather than guess.
    #[test]
    fn the_plane_hit_snaps_clamps_and_refuses_a_ray_that_runs_along_it() {
        let mut w = armed();
        w.room_base = 4.0;
        let s = WORLD_SCALE;

        let (o, d) = down_at(10.4, 15.6);
        assert_eq!(w.room_plane_hit(o, d), Some((10, 16)), "snapped to the WT grid");

        // A ray parallel to the plane — which is exactly what a front/side view casts.
        assert!(
            w.room_plane_hit(Vec3::new(0.0, 4.0 * s, 0.0), Vec3::X).is_none(),
            "a ray along the plane never meets it"
        );
        // The plane is behind the ray's origin.
        assert!(
            w.room_plane_hit(Vec3::new(0.0, 0.0, 0.0), Vec3::NEG_Y).is_none(),
            "the plane is behind the pointer"
        );
        // Absurdly far out — clamped rather than allowed to size a bounding box.
        let (o, d) = down_at(9_000.0, -9_000.0);
        assert_eq!(
            w.room_plane_hit(o, d),
            Some((512, -512)),
            "clamped to the world extent"
        );
    }

    /// The plane rides the base height, so raising it moves where a click lands in
    /// world space — that is what makes "slide the plan up a storey" mean anything.
    #[test]
    fn the_drafting_plane_follows_the_base_height() {
        let mut w = armed();
        w.room_base = 0.0;
        let origin = Vec3::new(0.0, 10.0 * WORLD_SCALE, 0.0);
        let slanted = Vec3::new(1.0, -1.0, 0.0).normalize();
        let low = w.room_plane_hit(origin, slanted).expect("hits at y=0");
        w.room_base = 5.0;
        let high = w.room_plane_hit(origin, slanted).expect("hits at y=5");
        assert_eq!(low, (10, 0), "a 45° ray from 10 WT up meets y=0 ten WT along");
        assert_eq!(high, (5, 0), "and meets y=5 only five WT along");
    }

    // ─── Building a room ─────────────────────────────────────────────────────

    /// The headline case. A rectangle drawn in open space becomes a sealed room: one
    /// subtract brush, in a **region of its own**, at the drawn footprint and height.
    #[test]
    fn a_footprint_drawn_in_open_space_becomes_its_own_region() {
        let mut w = armed();
        let before = w.regions.len();
        let brushes = build_room(&mut w, &[(40, 40), (52, 40), (52, 50), (40, 50)], 0.0, 8.0);

        assert_eq!(brushes.len(), 1, "a rectangle needs no decomposition");
        let b = brushes[0];
        assert_eq!(b.op, Op::Subtract, "a room is a void carved out of solid rock");
        assert_eq!((b.x, b.z, b.w, b.d), (40.0, 40.0, 12.0, 10.0), "the drawn footprint");
        assert_eq!((b.y, b.h), (0.0, 8.0), "extruded up from the drafting plane");
        assert_eq!(
            w.regions.len(),
            before + 1,
            "it touches nothing, so it clusters into a region of its own"
        );
    }

    /// A concave footprint has to be decomposed (fan-triangulation is convex-only),
    /// and the pieces must then read as **one** authored room: one group id, and — the
    /// trap `draw` had to learn — one shared wall-texture anchor, or the two halves
    /// band against each other along a boundary the author never drew.
    #[test]
    fn an_l_shaped_room_decomposes_but_stays_one_group_with_one_anchor() {
        let mut w = armed();
        let brushes = build_room(
            &mut w,
            &[(40, 40), (52, 40), (52, 44), (44, 44), (44, 52), (40, 52)],
            6.0,
            8.0,
        );
        assert!(brushes.len() >= 2, "an L cannot be one box (got {})", brushes.len());

        let groups: HashSet<u32> = brushes.iter().map(|b| b.group).collect();
        assert_eq!(groups.len(), 1, "every piece carries the one group id");
        assert_ne!(*groups.iter().next().unwrap(), 0, "and it is a real id, not 'ungrouped'");

        let anchors: HashSet<u32> = brushes.iter().map(|b| b.floor_y.to_bits()).collect();
        assert_eq!(anchors.len(), 1, "one wall-texture anchor for the whole room");
        assert_eq!(brushes[0].floor_y, 6.0, "and it is the room's own floor");
        assert!(brushes.iter().all(|b| b.op == Op::Subtract && b.y == 6.0 && b.h == 8.0));
    }

    /// Scrolling the height past zero hangs the room **below** the drafting plane —
    /// the plane becomes its ceiling. Without this, "extrude down" would build the
    /// room above the line you drew it on.
    #[test]
    fn a_negative_height_hangs_the_room_below_the_plane() {
        let mut w = armed();
        let brushes = build_room(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)], 12.0, -5.0);
        let b = brushes[0];
        assert_eq!((b.y, b.h), (7.0, 5.0), "floor 5 WT below the plane, plane is the ceiling");
        assert_eq!(b.floor_y, 7.0, "and the wall anchor follows the real floor");
    }

    /// Zero height is a live state on the way from an upward extrude to a downward
    /// one, so the click that lands on it must build nothing rather than a degenerate
    /// brush — and must not spend an id or leave an undo step.
    #[test]
    fn a_zero_height_commit_builds_nothing() {
        let mut w = armed();
        let before_brushes = w.regions.iter().map(|r| r.brushes.len()).sum::<usize>();
        let before_id = w.next_brush_id;
        footprint(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)]);
        click(&mut w, 40, 40); // Base → Height
        w.room_height = 0.0;
        click(&mut w, 40, 40); // commit
        assert_eq!(w.regions.iter().map(|r| r.brushes.len()).sum::<usize>(), before_brushes);
        assert_eq!(w.next_brush_id, before_id, "no id was spent");
        assert!(w.undo().is_none(), "and no dead undo step was recorded");
    }

    /// A footprint drawn over an existing room must **merge** with it rather than sit
    /// beside it as a second region — which is the case the incremental rebuild path
    /// bails on, and the reason the commit takes the full recluster.
    #[test]
    fn a_footprint_drawn_over_an_existing_room_merges_with_it() {
        let mut w = armed();
        let before = w.regions.len();
        // The booted level is one 24×16×24 subtract at the origin; overlap it.
        build_room(&mut w, &[(12, 12), (36, 12), (36, 24), (12, 24)], 0.0, 8.0);
        assert_eq!(w.regions.len(), before, "it joined the room it overlaps");
        assert_eq!(w.brush_to_region.len(), w.regions.iter().map(|r| r.brushes.len()).sum::<usize>(),
            "and every brush is mapped to the region that actually holds it");
    }

    /// One undo takes the whole room away, however many rectangles it decomposed into,
    /// and a save/load round trip changes nothing about it. The round trip is not
    /// ceremony: `draw` shipped a bug where a reclustered fold differed from the
    /// authored one, and it only showed on reload.
    #[test]
    fn a_room_undoes_as_one_and_survives_a_save_load_round_trip() {
        let mut w = armed();
        let before = w.regions.iter().map(|r| r.brushes.len()).sum::<usize>();
        let first = w.next_brush_id;
        let brushes = build_room(
            &mut w,
            &[(40, 40), (52, 40), (52, 44), (44, 44), (44, 52), (40, 52)],
            0.0,
            8.0,
        );
        let n = brushes.len();
        assert!(w.undo().is_some(), "the commit left exactly one undo step");
        assert_eq!(
            w.regions.iter().map(|r| r.brushes.len()).sum::<usize>(),
            before,
            "one undo removes the whole room, not one rectangle of it"
        );
        assert!(w.redo().is_some());
        assert_eq!(w.regions.iter().map(|r| r.brushes.len()).sum::<usize>(), before + n);

        let path = std::env::temp_dir().join("bah_room_roundtrip.json");
        w.save_level(&path).expect("save");
        let mut loaded = World::new();
        loaded.load_level(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        let mut want: Vec<(u32, f32, f32, f32, f32, f32, f32)> = brushes
            .iter()
            .map(|b| (b.group, b.x, b.y, b.z, b.w, b.h, b.d))
            .collect();
        let mut got: Vec<(u32, f32, f32, f32, f32, f32, f32)> = added(&loaded, first)
            .iter()
            .map(|b| (b.group, b.x, b.y, b.z, b.w, b.h, b.d))
            .collect();
        want.sort_by(|a, b| a.partial_cmp(b).unwrap());
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(got, want, "the room round-trips brush for brush, group included");
    }

    // ─── Corner dragging ─────────────────────────────────────────────────────

    /// The rule that makes "drag a corner" expressible at all: a rectilinear corner
    /// joins one horizontal and one vertical edge, so it cannot move alone without
    /// tilting both. Its two neighbours' shared coordinates come with it.
    #[test]
    fn dragging_a_corner_brings_its_neighbours_and_keeps_every_edge_square() {
        let mut w = armed();
        footprint(&mut w, &[(40, 40), (52, 40), (52, 50), (40, 50)]);
        let before = w.room_rects.clone();

        // Grab the (52, 40) corner and pull it out to (58, 36).
        let (o, d) = down_at(52.0, 40.0);
        assert!(w.start_room_drag(o, d), "the corner handle was grabbed");
        let (o, d) = down_at(58.0, 36.0);
        assert!(w.room_hover(o, d), "the drag moved the footprint");

        assert!(w.room_verts.contains(&(58, 36)), "the dragged corner is where it was put");
        let n = w.room_verts.len();
        for i in 0..n {
            let (a, b) = (w.room_verts[i], w.room_verts[(i + 1) % n]);
            assert!(
                a.0 == b.0 || a.1 == b.1,
                "every edge is still axis-aligned ({a:?} → {b:?})"
            );
        }
        assert_ne!(w.room_rects, before, "and the decomposition was refreshed");
        // The corner ends up as the extreme in both directions it was pulled.
        assert_eq!(w.room_verts.iter().map(|p| p.0).max(), Some(58));
        assert_eq!(w.room_verts.iter().map(|p| p.1).min(), Some(36));
    }

    /// A drag that would make the outline non-simple is refused, not clamped: the
    /// footprint just stops following the pointer, which needs no explanation on
    /// screen and cannot leave a self-crossing shape to be committed.
    #[test]
    fn a_drag_that_would_cross_the_outline_is_refused() {
        let mut w = armed();
        // A U, so there is a slot for a corner to be dragged across.
        footprint(
            &mut w,
            &[
                (40, 40),
                (52, 40),
                (52, 52),
                (48, 52),
                (48, 44),
                (44, 44),
                (44, 52),
                (40, 52),
            ],
        );
        let before = w.room_verts.clone();
        let (o, d) = down_at(48.0, 44.0);
        assert!(w.start_room_drag(o, d), "grabbed the inner corner");
        // Drag it past the far side of the outline entirely.
        let (o, d) = down_at(20.0, 20.0);
        w.room_hover(o, d);
        assert_eq!(w.room_verts, before, "the illegal position was refused outright");
    }

    /// Corner handles are only live once the loop is closed. While drawing, a click
    /// near an existing corner has to mean "close the loop" / "drop a corner" — a
    /// grab there would make closing impossible.
    #[test]
    fn corners_are_only_draggable_once_the_loop_is_closed() {
        let mut w = armed();
        click(&mut w, 40, 40);
        click(&mut w, 52, 40);
        let (o, d) = down_at(40.0, 40.0);
        assert!(!w.start_room_drag(o, d), "no grab while the outline is open");
        assert!(!w.is_room_editing());
        click(&mut w, 52, 50);
        click(&mut w, 40, 50);
        click(&mut w, 40, 40);
        assert!(w.is_room_editing(), "closing the loop turns the handles on");
        assert!(w.start_room_drag(o, d), "and now the corner grabs");
    }

    // ─── Phases + the escape ladder ──────────────────────────────────────────

    /// The wheel does the current phase's job. Getting this wrong is invisible until
    /// you scroll: the same gesture has to zoom, then move a storey, then size a room.
    #[test]
    fn the_wheel_does_the_current_phases_job() {
        let mut w = armed();
        let zoom0 = w.ortho.unwrap().half_h;
        w.room_scroll(1.0, false);
        assert!(w.ortho.unwrap().half_h < zoom0, "while drawing, the wheel zooms");

        footprint(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)]);
        let base0 = w.room_base;
        w.room_scroll(3.0, false);
        assert_eq!(w.room_base, base0 + 3.0, "with the loop closed it moves the plane");

        // Ctrl is the only way back to the zoom once the wheel has another job.
        let zoom1 = w.ortho.unwrap().half_h;
        w.room_scroll(1.0, true);
        assert!(w.ortho.unwrap().half_h < zoom1, "Ctrl forces the zoom in every phase");
        assert_eq!(w.room_base, base0 + 3.0, "and leaves the plane where it was");

        click(&mut w, 40, 40); // → Height
        let h0 = w.room_height;
        w.room_scroll(-2.0, false);
        assert_eq!(w.room_height, h0 - 2.0, "at the height step it sizes the room");
    }

    /// Esc walks back one rung at a time rather than throwing the drawing away, and
    /// only reports "not handled" at the very bottom — which is what lets the app fall
    /// through to releasing the cursor and disarming.
    #[test]
    fn the_escape_ladder_backs_out_one_rung_at_a_time() {
        let mut w = armed();
        footprint(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)]);
        click(&mut w, 40, 40);
        assert_eq!(w.room_phase, Some(RoomPhase::Height));

        assert!(w.room_escape());
        assert_eq!(w.room_phase, Some(RoomPhase::Base), "height → base");
        assert!(w.room_escape());
        assert_eq!(w.room_phase, Some(RoomPhase::Drawing), "base → the outline reopens");
        assert_eq!(w.room_verts.len(), 4, "the corners are kept, so re-closing is one click");
        assert!(w.room_rects.is_empty(), "but the decomposition is dropped");

        for want in [3, 2, 1, 0] {
            assert!(w.room_escape());
            assert_eq!(w.room_verts.len(), want, "one corner per press");
        }
        assert!(
            !w.room_escape(),
            "an empty outline is the bottom — the app disarms the tool from here"
        );
        assert!(w.is_room_tool(), "and Esc itself never disarms it");
    }

    /// A closing click needs four corners: anything shorter landing on the first one
    /// is a stray click, not a loop.
    #[test]
    fn three_corners_cannot_close_a_loop() {
        let mut w = armed();
        click(&mut w, 40, 40);
        click(&mut w, 48, 40);
        click(&mut w, 48, 48);
        click(&mut w, 40, 40);
        assert_eq!(w.room_phase, Some(RoomPhase::Drawing), "still drawing");
    }

    /// Every segment is 90°-snapped, so a click off the axis is locked onto whichever
    /// axis it travelled furthest along — a diagonal would kill the decomposition.
    #[test]
    fn a_click_off_the_axis_is_locked_square() {
        let mut w = armed();
        click(&mut w, 40, 40);
        click(&mut w, 52, 43); // 12 across, 3 down → locks to the X move
        assert_eq!(w.room_verts, vec![(40, 40), (52, 40)]);
        click(&mut w, 55, 60); // 3 across, 20 down → locks to the Z move
        assert_eq!(w.room_verts, vec![(40, 40), (52, 40), (52, 60)]);
    }

    /// The tool stays armed after a commit, on a clean outline: laying out a base
    /// means drawing several rooms in a row.
    #[test]
    fn committing_leaves_the_tool_armed_and_ready_for_the_next_room() {
        let mut w = armed();
        build_room(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)], 0.0, 8.0);
        assert!(w.is_room_tool(), "still armed");
        assert_eq!(w.room_phase, Some(RoomPhase::Drawing));
        assert!(w.room_verts.is_empty() && w.room_rects.is_empty(), "on a clean outline");
        // And the second room really does build.
        let brushes = build_room(&mut w, &[(60, 40), (68, 40), (68, 48), (60, 48)], 0.0, 8.0);
        assert_eq!(brushes.len(), 1);
    }

    // ─── Arming, disarming, and the camera ───────────────────────────────────

    /// The tool takes the camera over, so arming it must be exclusive and disarming it
    /// must give the camera back — a stranded orthographic view with no tool to drive
    /// it would leave the editor unusable with no way out.
    #[test]
    fn arming_is_exclusive_and_disarming_returns_the_camera() {
        let mut w = World::new();
        w.initial_meshes();
        w.camera.pitch = -1.4;
        w.draw_tool_key();
        assert!(w.is_draw_tool());

        w.room_tool_key();
        assert!(w.is_room_tool() && !w.is_draw_tool(), "arming abandons the draw tool");
        assert!(w.selected.is_none(), "and drops any face selection");
        assert_eq!(w.room_view(), Some(ViewAxis::Top), "and opens on the plan view");

        w.room_tool_key();
        assert!(!w.is_room_tool());
        assert!(w.ortho.is_none(), "the drafting camera goes with it");
    }

    /// A mode switch into HUNT must not leave the drafting camera on — the player
    /// would spawn looking at an orthographic projection of the level.
    #[test]
    fn entering_hunt_clears_the_drafting_view() {
        let mut w = armed();
        assert!(w.ortho.is_some());
        w.toggle_mode();
        assert!(!w.is_room_tool() && w.ortho.is_none(), "BUILD-only, camera and all");
    }

    /// The perspective view keeps the tool armed and keeps it drawable — the drafting
    /// plane has a known height, so a perspective ray meets it as well as an overhead
    /// one does. Only the side views can't, and that is geometry, not policy.
    #[test]
    fn perspective_keeps_the_tool_live_and_the_side_views_are_the_only_undrawable_ones() {
        let mut w = armed();
        w.leave_room_view();
        assert!(w.is_room_tool() && w.room_view().is_none());
        let slanted = Vec3::new(1.0, -1.0, 0.0).normalize();
        assert!(
            w.room_plane_hit(Vec3::new(0.0, 8.0 * WORLD_SCALE, 0.0), slanted).is_some(),
            "a perspective ray still meets the plane"
        );
        w.toggle_room_view();
        assert_eq!(w.room_view(), Some(ViewAxis::Top), "5 flips back to the last view");

        w.enter_room_view(ViewAxis::Front);
        assert!(
            w.room_plane_hit(Vec3::new(0.0, 0.0, 9.0), ViewAxis::Front.forward()).is_none(),
            "a front view's ray runs along the plane"
        );
    }

    /// The plan view's handedness, asserted through the matrix rather than trusted:
    /// looking down, +X must go right and +Z must go down the screen, or every
    /// footprint is mirrored against the level it is being placed into.
    #[test]
    fn the_top_view_lays_the_plan_out_like_a_map() {
        let cam = OrthoCamera::new(ViewAxis::Top, Vec3::ZERO, 10.0);
        let vp = cam.view_proj(1.0);
        let ndc = |p: Vec3| {
            let c = vp * p.extend(1.0);
            (c.x / c.w, c.y / c.w, c.z / c.w)
        };
        let (x, _, _) = ndc(Vec3::new(5.0, 0.0, 0.0));
        assert!(x > 0.0, "+X is to the right (got {x})");
        let (_, y, _) = ndc(Vec3::new(0.0, 0.0, 5.0));
        assert!(y < 0.0, "+Z is down the screen (got {y})");
        // Orthographic: no perspective divide, so the same offset is the same distance
        // wherever it sits along the view axis.
        let near = ndc(Vec3::new(5.0, 20.0, 0.0)).0;
        let far = ndc(Vec3::new(5.0, -20.0, 0.0)).0;
        assert!((near - far).abs() < 1e-5, "depth doesn't change the scale");
        let (_, _, z) = ndc(Vec3::ZERO);
        assert!((0.0..=1.0).contains(&z), "and the depth lands in wgpu's 0..1 clip range");
    }

    /// Zoom is geometric and clamped, and pan moves the focus opposite the drag so the
    /// content follows the cursor.
    #[test]
    fn the_drafting_camera_zooms_and_pans() {
        let mut cam = OrthoCamera::new(ViewAxis::Top, Vec3::ZERO, 10.0);
        cam.zoom(1.0);
        assert!(cam.half_h < 10.0, "scrolling up zooms in");
        cam.zoom(-1.0);
        assert!((cam.half_h - 10.0).abs() < 1e-4, "and back out symmetrically");
        cam.zoom(500.0);
        assert!(cam.half_h >= 0.25, "clamped rather than collapsing to zero");

        let mut cam = OrthoCamera::new(ViewAxis::Top, Vec3::ZERO, 10.0);
        cam.pan(100.0, 0.0, 1000.0);
        assert!(cam.center.x < 0.0, "dragging right walks the focus left, so content follows");
        let mut cam = OrthoCamera::new(ViewAxis::Front, Vec3::ZERO, 10.0);
        cam.pan(0.0, 100.0, 1000.0);
        assert!(cam.center.y > 0.0, "dragging down walks the focus up");
    }

    /// The overlay is the tool's only feedback, so it must actually be produced — and
    /// only while armed, since it renders through the shared gizmo channel.
    #[test]
    fn the_plan_overlay_is_drawn_only_while_the_tool_is_armed() {
        let mut w = World::new();
        w.initial_meshes();
        assert!(w.room_overlay_mesh().is_none(), "nothing to draw when disarmed");
        w.room_tool_key();
        let m = w.room_overlay_mesh().expect("the grid + the level's footprints");
        assert!(!m.vertices.is_empty() && !m.indices.is_empty());
        footprint(&mut w, &[(40, 40), (48, 40), (48, 48), (40, 48)]);
        click(&mut w, 40, 40); // → Height, which adds the extrude ghost
        let with_ghost = w.room_overlay_mesh().expect("mesh").vertices.len();
        w.room_escape(); // back to Base, ghost gone
        let without = w.room_overlay_mesh().expect("mesh").vertices.len();
        assert!(with_ghost > without, "the height step adds the extrude ghost");
    }

    /// The grid coarsens instead of flooding the view — a 1 WT grid across a
    /// several-hundred-WT base is both unreadable and thousands of boxes.
    #[test]
    fn the_grid_coarsens_as_the_view_widens() {
        assert_eq!(grid_step(20.0), 1, "close in, every cell");
        assert!(grid_step(500.0) > grid_step(20.0));
        assert!(grid_step(5000.0) >= grid_step(500.0));
        for span in [1.0, 50.0, 500.0, 5000.0, 50_000.0] {
            assert!(
                span / grid_step(span) as f32 <= GRID_MAX_LINES as f32 || grid_step(span) == 64,
                "span {span} would draw too many lines"
            );
        }
    }
}


/// Regression tests for the **mesh-loss** defect the room plan tool exposed.
///
/// Rooms drawn apart are separate regions, so connecting two of them is the first
/// edit in the editor's life that routinely *merges* regions. A merge reclusters the
/// level into fresh ids, and the result the tool hands back is one mesh per surviving
/// region **plus an empty mesh for every id that just stopped existing** — the empties
/// being how the renderer is told to drop the old geometry.
///
/// Every tool used to narrow that to `Option<RegionMesh>` with
/// `rebuild_affected_regions(..).into_iter().next()`, which is lossless only while the
/// edit stays inside one region. Cutting a door between two rooms therefore left both
/// pre-cut rooms on screen (no opening visible from either side) with the merged
/// region painted over them in the gap.
#[cfg(test)]
mod merge_tests {
    use super::*;
    use std::collections::HashSet;

    /// Two rooms with a 2 WT wall between them: the booted 24×16×24 room, and one
    /// drawn past its +Z wall by the room tool. They do not touch, so they cluster
    /// into **separate regions** — which is the precondition for everything below.
    fn two_rooms_apart() -> World {
        let mut w = World::new();
        w.initial_meshes();
        w.room_tool_key();
        let ray = |x: i32, z: i32| {
            (
                Vec3::new(x as f32 * WORLD_SCALE, 64.0 * WORLD_SCALE, z as f32 * WORLD_SCALE),
                Vec3::NEG_Y,
            )
        };
        // The booted room spans z 0..24; this one starts at 26.
        for (x, z) in [(4, 26), (20, 26), (20, 38), (4, 38), (4, 26)] {
            let (o, d) = ray(x, z);
            w.with_undo_many(|w| w.room_click(o, d));
        }
        w.room_base = 0.0;
        let (o, d) = ray(4, 26);
        w.with_undo_many(|w| w.room_click(o, d)); // base → height
        w.room_height = 16.0;
        w.with_undo_many(|w| w.room_click(o, d)); // build
        w.cancel_room();
        assert_eq!(w.regions.len(), 2, "two rooms with a gap are two regions");
        w
    }

    /// Aim from inside the booted room at its +Z wall, where the other room is.
    fn aim_at_the_shared_wall(w: &mut World) {
        w.camera.pos = Vec3::new(12.0, 8.0, 12.0) * WORLD_SCALE;
        w.camera.yaw = std::f32::consts::PI; // +Z
        w.camera.pitch = 0.0;
    }

    /// **The invariant.** Given the region ids that existed before an edit and the
    /// meshes it handed back, every region that exists now must have been uploaded and
    /// every id that vanished must have been cleared. Anything missing is geometry the
    /// renderer is still drawing from before the edit.
    fn assert_meshes_account_for_every_region(
        w: &World,
        before: &[u32],
        meshes: &[RegionMesh],
    ) {
        let sent: HashSet<u32> = meshes.iter().map(|m| m.id).collect();
        let live: HashSet<u32> = w.regions.iter().map(|r| r.id).collect();
        for id in &live {
            assert!(
                sent.contains(id),
                "region {id} exists but was never uploaded — it renders as whatever \
                 was in that slot before (sent {sent:?}, live {live:?})"
            );
        }
        for id in before {
            assert!(
                live.contains(id) || sent.contains(id),
                "region {id} stopped existing and was never cleared — the renderer \
                 still holds its pre-edit mesh (sent {sent:?}, live {live:?})"
            );
        }
    }

    /// The reported bug, end to end: a doorway cut between two separately-drawn rooms.
    #[test]
    fn a_doorway_between_two_rooms_hands_back_every_mesh_the_merge_produced() {
        let mut w = two_rooms_apart();
        aim_at_the_shared_wall(&mut w);
        let before: Vec<u32> = w.regions.iter().map(|r| r.id).collect();

        w.door_tool_key();
        w.update_door_preview();
        let meshes = w.with_undo_many(|w| w.confirm_door());

        assert!(!meshes.is_empty(), "the cut rebuilt something");
        assert_eq!(w.regions.len(), 1, "the doorway merged the two rooms into one region");
        assert!(
            !before.iter().all(|id| w.regions.iter().any(|r| r.id == *id)),
            "the merge really did retire the old ids — otherwise this test proves nothing"
        );
        assert_meshes_account_for_every_region(&w, &before, &meshes);
    }

    /// The doorway is real geometry, not just bookkeeping: the wall between the rooms
    /// is actually pierced. Guards against a "fix" that uploads the right ids while
    /// the carve lands somewhere useless.
    #[test]
    fn the_doorway_actually_pierces_the_wall_between_the_two_rooms() {
        let mut w = two_rooms_apart();
        aim_at_the_shared_wall(&mut w);
        w.door_tool_key();
        w.update_door_preview();
        w.with_undo_many(|w| w.confirm_door());

        let brushes: Vec<Brush> =
            w.regions.iter().flat_map(|r| r.brushes.iter().copied()).collect();
        // Frame carve at the wall (z 24..25), protoroom just past it (z 25..26): together
        // they span the whole 2 WT gap, which is what connects the two voids.
        assert!(
            brushes.iter().any(|b| b.frame && b.door && b.z <= 24.0 && b.z + b.d >= 25.0),
            "a door-marked frame pierces the z=24 wall (got {:?})",
            brushes.iter().map(|b| (b.id, b.z, b.d, b.frame)).collect::<Vec<_>>()
        );
        assert!(
            brushes
                .iter()
                .any(|b| b.op == Op::Subtract && !b.frame && b.z >= 25.0 && b.z + b.d <= 26.0),
            "and the protoroom carries the opening the rest of the way to the far room"
        );
    }

    /// A hole cut the same way merges just as hard — the door tool is not special, and
    /// `confirm_opening` is the shared path both go through.
    #[test]
    fn a_hole_between_two_rooms_hands_back_every_mesh_too() {
        let mut w = two_rooms_apart();
        aim_at_the_shared_wall(&mut w);
        let before: Vec<u32> = w.regions.iter().map(|r| r.id).collect();

        w.hole_tool_key();
        w.update_opening_preview();
        let meshes = w.with_undo_many(|w| w.confirm_opening());

        assert!(!meshes.is_empty(), "the cut rebuilt something");
        assert_eq!(w.regions.len(), 1, "the hole merged the two rooms");
        assert_meshes_account_for_every_region(&w, &before, &meshes);
    }

    /// Undo has to put both regions back — and hand back the meshes to redraw them
    /// with. The restore path reclusters too, so it has exactly the same exposure.
    #[test]
    fn undoing_the_doorway_restores_both_regions_and_reports_them_all() {
        let mut w = two_rooms_apart();
        aim_at_the_shared_wall(&mut w);
        w.door_tool_key();
        w.update_door_preview();
        w.with_undo_many(|w| w.confirm_door());
        assert_eq!(w.regions.len(), 1);

        let before: Vec<u32> = w.regions.iter().map(|r| r.id).collect();
        let meshes = w.undo().expect("the cut left an undo step");
        assert_eq!(w.regions.len(), 2, "undo splits the merged region back in two");
        assert_meshes_account_for_every_region(&w, &before, &meshes);
    }

    /// A doorway cut *inside* one region must not have regressed into a full
    /// recluster — the incremental path is what keeps editing a large base fast, and
    /// the fix must not have bought correctness by giving that up.
    #[test]
    fn a_doorway_inside_one_region_still_takes_the_incremental_path() {
        let mut w = World::new();
        w.initial_meshes();
        let before: Vec<u32> = w.regions.iter().map(|r| r.id).collect();
        w.door_tool_key();
        w.update_door_preview();
        let meshes = w.with_undo_many(|w| w.confirm_door());

        assert_eq!(meshes.len(), 1, "one region touched, one mesh — no recluster");
        assert_eq!(
            w.regions.iter().map(|r| r.id).collect::<Vec<_>>(),
            before,
            "and the region kept its id, which a recluster would not have"
        );
        assert_meshes_account_for_every_region(&w, &before, &meshes);
    }
}
