//! Openable doors — the geometry derivation, the HUNT bake/teardown, and the player's
//! "use" action.
//!
//! A door is an ordinary placed prop ([`super::prop`]) that the catalog marks as a door
//! ([`crate::props::door_def`]), so it inherits the entire object pipeline for free:
//! palette, ghost placement, translate/rotate gizmos, snap, duplicate, undo and
//! persistence. What this module adds is the behaviour on top.
//!
//! **Where the pivot comes from.** The GoldenEye door assets give us nothing to hang a
//! hinge on — each is a single unnamed mesh with no frame and no authored pivot. So
//! [`door_geom`] derives it from the model-space AABB: the *width axis* is whichever of
//! local X/Z is wider (measured to be X on `metal_door`/`grey_door`/`wooden_door` and Z
//! on the other ten — assuming either one would make several doors swing about their own
//! face), and the hinge is the AABB edge on the authored side, taken from min/max rather
//! than the origin because four of the models are not centred on their width axis.
//!
//! **Opening is manual**, on the use key — GoldenEye and Perfect Dark open doors with B,
//! they do not swing open as you approach.

use super::super::*;
use crate::ecs::{
    Door, DoorGeom, DoorState, HingeSide, MeshId, OpeningType, Renderable, Transform,
};
use crate::props::DoorMotion;

/// Volume the door cues play at, before distance falloff.
pub(crate) const DOOR_VOL: f32 = 0.8;

/// Distance in metres past which a door is inaudible. Doors are *information* in a
/// hide-and-seek game — hearing one open tells you a hunter found your wing — so they
/// must fall off with range rather than play flat across the whole level, which is what
/// [`engine::audio::AudioManager::play`] does on its own.
pub(crate) const DOOR_AUDIBLE_RANGE: f32 = 22.0;

/// Linear-falloff volume for a world-space sound heard from `listener`.
pub(crate) fn falloff_volume(base: f32, at: Vec3, listener: Vec3, range: f32) -> f32 {
    let d = at.distance(listener);
    base * (1.0 - (d / range)).clamp(0.0, 1.0)
}

/// Resolve a door's panel geometry from its mesh bounds, prop scale and authored
/// settings. `bounds` is the model-space AABB registered from the GLB at startup.
///
/// Pure and standalone (no `World`) so it is directly testable and so the HUNT bake can
/// call it per entity without re-borrowing the world.
pub(crate) fn door_geom(bounds: (Vec3, Vec3), scale: Vec3, hinge_side: HingeSide) -> DoorGeom {
    let (min, max) = bounds;
    let size = max - min;
    let center = (min + max) * 0.5;

    // Width axis = the wider horizontal extent. Measured to differ per model, so it is
    // chosen here rather than assumed.
    let width_is_x = size.x >= size.z;
    let width_axis = if width_is_x { Vec3::X } else { Vec3::Z };
    let width_model = if width_is_x { size.x } else { size.z };

    // Hinge sits on one end of the width axis, at the panel's base. Taken from the AABB
    // edge, not the origin — four door models are off-centre on this axis.
    let (lo, hi) = if width_is_x { (min.x, max.x) } else { (min.z, max.z) };
    let edge = match hinge_side {
        HingeSide::Left => lo,
        HingeSide::Right => hi,
    };
    let hinge = if width_is_x {
        Vec3::new(edge, min.y, center.z)
    } else {
        Vec3::new(center.x, min.y, edge)
    };

    // Scale is per-axis: a door fitted into a doorway is squashed on width only, so the
    // panel's world width must come from the scale along *its* width axis, not a single
    // uniform factor.
    let width_scale = if width_is_x { scale.x } else { scale.z };
    DoorGeom {
        hinge,
        width_axis,
        width: width_model * width_scale,
        height: size.y * scale.y,
        center,
        anchor: Vec3::new(center.x, min.y, center.z),
        half: size * scale * 0.5,
        collider: None,
        nav_index: None,
    }
}

/// The door's full model→world matrix at its current `open_frac`: the ordinary prop
/// placement matrix with the open transform composed on the model-space side.
///
/// This is the single source of truth for where a door *is* — the draw list, the
/// collider pose and any picking all go through it, so the panel you see, the panel you
/// bump into and the panel that blocks sight can never drift apart.
pub(crate) fn door_world_matrix(t: &Transform, geom: &DoorGeom, door: &Door) -> Mat4 {
    Mat4::from_translation(t.pos)
        * Mat4::from_quat(t.rot)
        * Mat4::from_scale(t.scale)
        * Mat4::from_translation(-geom.anchor)
        * open_transform(door, geom, t.scale)
        * mirror_transform(door, geom)
}

/// Reflect the panel's artwork across its own width, about the panel centre.
///
/// Innermost in [`door_world_matrix`] — it reflects the *mesh*, then the swing pivots
/// the reflected panel. Reflecting about the centre leaves the panel's bounding box
/// exactly where it was, so the hinge (derived from that box) and the collider need no
/// adjustment at all: only which side the handle appears on changes.
///
/// Safe because the prop pipeline is `cull_mode: None` — a negative scale flips triangle
/// winding, which would turn a back-face-culled model inside out.
pub(crate) fn mirror_transform(door: &Door, geom: &DoorGeom) -> Mat4 {
    if !door.mirrored {
        return Mat4::IDENTITY;
    }
    let s = if geom.width_axis == Vec3::X {
        Vec3::new(-1.0, 1.0, 1.0)
    } else {
        Vec3::new(1.0, 1.0, -1.0)
    };
    Mat4::from_translation(geom.center) * Mat4::from_scale(s) * Mat4::from_translation(-geom.center)
}

/// How far a sliding door travels, in world metres — the authored distance, or the
/// panel's own width (sideways) / height (shutter) when left at the `0.0` auto default.
fn slide_travel(door: &Door, geom: &DoorGeom) -> f32 {
    if door.slide_distance > 0.0 {
        return door.slide_distance;
    }
    match door.opening_type {
        OpeningType::Shutter => geom.height,
        _ => geom.width,
    }
}

/// The door's **model-space** open transform: the motion applied to the panel before
/// the prop's own placement matrix. Identity when shut, so a door at `open_frac == 0`
/// draws exactly where an ordinary prop would.
///
/// Model space (not world) is the natural frame here: the hinge and the width axis are
/// both model-space quantities, and composing on this side means the authored rotation
/// and scale carry the motion automatically — a door rotated 90° by the gizmo swings
/// about its own edge without any extra bookkeeping.
pub(crate) fn open_transform(door: &Door, geom: &DoorGeom, scale: Vec3) -> Mat4 {
    if door.open_frac <= 0.0 {
        return Mat4::IDENTITY;
    }
    let dir = if door.flip { -1.0 } else { 1.0 };
    match door.opening_type {
        OpeningType::Swing => {
            let theta = door.open_angle * door.open_frac * dir;
            // Pivot about the hinge: translate the hinge to the origin, turn, put it back.
            Mat4::from_translation(geom.hinge)
                * Mat4::from_rotation_y(theta)
                * Mat4::from_translation(-geom.hinge)
        }
        OpeningType::Slide => {
            // The travel is in world metres but this transform sits *inside* the prop's
            // scale, so convert back to model units or a scaled-down door would slide
            // proportionally too far.
            // Divide by the scale along the axis actually being travelled.
            let sx = if geom.width_axis == Vec3::X { scale.x } else { scale.z };
            let d = slide_travel(door, geom) * door.open_frac * dir / sx.abs().max(1e-6);
            Mat4::from_translation(geom.width_axis * d)
        }
        OpeningType::Shutter => {
            let d = slide_travel(door, geom) * door.open_frac * dir / scale.y.abs().max(1e-6);
            Mat4::from_translation(Vec3::Y * d)
        }
    }
}

/// The panel's world pose (collider centre + rotation) at the current `open_frac`,
/// given the prop's placement matrix. The collider box's local axes are the model axes,
/// so its half-extents are constant and only the pose moves.
pub(crate) fn panel_pose(
    door: &Door,
    geom: &DoorGeom,
    model: Mat4,
    prop_rot: Quat,
) -> (Vec3, Quat) {
    let center = model.transform_point3(geom.center);
    let rot = match door.opening_type {
        OpeningType::Swing => {
            let dir = if door.flip { -1.0 } else { 1.0 };
            prop_rot * Quat::from_rotation_y(door.open_angle * door.open_frac * dir)
        }
        // A sliding panel translates without turning.
        _ => prop_rot,
    };
    (center, rot)
}

/// The persisted opening type for a catalog door — the component-side mirror of the
/// catalog's [`DoorMotion`]. Swing for anything without its own motion.
pub(crate) fn door_opening_type(mesh: MeshId) -> OpeningType {
    match crate::props::door_def(mesh).map(|d| d.motion) {
        Some(DoorMotion::SlideSideways) => OpeningType::Slide,
        Some(DoorMotion::SlideUp) => OpeningType::Shutter,
        _ => OpeningType::Swing,
    }
}

/// A doorway found in the level geometry: the rectangular hole a door should fill.
///
/// Recovered from the `door`-marked frame carve the door tool leaves behind
/// (`opening.rs` sets `frame.door = true`), so the level's own geometry tells us where
/// the doorways are — nothing extra has to be authored or stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Doorway {
    /// Centre of the opening, in metres (mid-wall on the normal axis, mid-span
    /// horizontally, mid-height vertically).
    pub center: Vec3,
    /// Which horizontal axis the opening's *width* runs along.
    pub width_axis: Axis,
    /// Opening width in metres.
    pub width: f32,
    /// Opening height in metres.
    pub height: f32,
    /// Floor of the opening, in metres — where a door panel's base sits.
    pub base_y: f32,
    /// How many leaves this opening wants: 2 for a double-width carve, else 1.
    pub leaves: u32,
}

impl World {
    /// Every doorway in the level, recovered from the `door`-marked frame carves.
    ///
    /// A doorframe carve is one wall thick along the wall's normal, so the thin
    /// horizontal dimension identifies the normal and the other identifies the width —
    /// the same "thinnest axis is the normal" reading used for the door meshes
    /// themselves, applied to the hole instead of the panel.
    pub(crate) fn doorways(&self) -> Vec<Doorway> {
        let s = WORLD_SCALE;
        let mut out = Vec::new();
        for region in &self.regions {
            for b in region.brushes.iter().filter(|b| b.door) {
                // Thin horizontal dimension = the wall normal; the other = the span.
                let (width_axis, width_wt) = if (b.w - WALL_THICKNESS).abs() < 0.01 {
                    (Axis::Z, b.d)
                } else if (b.d - WALL_THICKNESS).abs() < 0.01 {
                    (Axis::X, b.w)
                } else {
                    continue; // not a recognisable doorframe
                };
                out.push(Doorway {
                    center: Vec3::new(
                        (b.x + b.w * 0.5) * s,
                        (b.y + b.h * 0.5) * s,
                        (b.z + b.d * 0.5) * s,
                    ),
                    width_axis,
                    width: width_wt * s,
                    height: b.h * s,
                    base_y: b.y * s,
                    // A double carve is exactly two singles wide; anything at or past
                    // 1.5 singles is unambiguously the double.
                    leaves: if width_wt >= DOOR_WIDTH * 1.5 { 2 } else { 1 },
                });
            }
        }
        out
    }

    /// The doorway nearest `at`, within `SNAP_RANGE` metres. `None` when the cursor
    /// isn't near a doorway, which is what lets a door still be free-placed against a
    /// flat wall (a closet door, a decorative one) instead of being forced into a hole.
    pub(crate) fn nearest_doorway(&self, at: Vec3) -> Option<Doorway> {
        /// How close the placement point must be to a doorway to snap into it. A little
        /// over one room-height, so aiming at the floor in a doorway reliably catches it.
        const SNAP_RANGE: f32 = 2.5;
        self.doorways()
            .into_iter()
            .filter(|d| d.center.distance(at) <= SNAP_RANGE)
            .min_by(|a, b| {
                a.center
                    .distance(at)
                    .partial_cmp(&b.center.distance(at))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Fit a door mesh into a doorway: the transform (position, yaw, **non-uniform**
    /// scale) that makes the panel exactly fill its share of the opening.
    ///
    /// `leaf` is 0 for a single door and 0/1 for the two halves of a double.
    ///
    /// Non-uniform scale is the point. Fitting by height alone — what the catalog's
    /// `DOOR_FIT` does for a free-placed door — leaves `wooden_door` overhanging its
    /// carve by 23% and `elevator_door` short of it by 19%, because the models span
    /// aspect ratios 0.346–0.528 while the carve is a fixed 0.429. Squashing width
    /// independently is invisible on a door panel and makes every model fit its hole.
    pub(crate) fn door_fit_transform(
        &self,
        mesh: MeshId,
        way: &Doorway,
        leaf: u32,
    ) -> Option<(Vec3, Quat, Vec3)> {
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        let size = max - min;
        // The model's own width axis (measured to be X on three of the thirteen).
        let model_width_is_x = size.x >= size.z;
        let model_width = if model_width_is_x { size.x } else { size.z };
        if model_width <= 0.0 || size.y <= 0.0 {
            return None;
        }

        // Yaw so the panel's width axis lies along the opening's width axis.
        let opening_width_is_x = way.width_axis == Axis::X;
        let yaw = if model_width_is_x == opening_width_is_x {
            0.0
        } else {
            std::f32::consts::FRAC_PI_2
        };

        // Each leaf fills its share of the span.
        let leaf_width = way.width / way.leaves as f32;
        let width_scale = leaf_width / model_width;
        let height_scale = way.height / size.y;
        // Thickness rides the height scale — it isn't constrained by the opening, and
        // matching it to width would make a squashed panel look like card.
        let scale = if model_width_is_x {
            Vec3::new(width_scale, height_scale, height_scale)
        } else {
            Vec3::new(height_scale, height_scale, width_scale)
        };

        // Centre of this leaf's share of the opening, base on the opening floor.
        let offset = (leaf as f32 + 0.5) * leaf_width - way.width * 0.5;
        let mut pos = Vec3::new(way.center.x, way.base_y, way.center.z);
        if opening_width_is_x {
            pos.x += offset;
        } else {
            pos.z += offset;
        }
        Some((pos, Quat::from_rotation_y(yaw), scale))
    }

    /// The doorway a door prop being placed at `pos` should snap into, or `None` for a
    /// non-door prop or a spot away from any doorway (which free-places as before).
    pub(crate) fn door_snap_for(&self, mesh: MeshId, pos: Vec3) -> Option<Doorway> {
        crate::props::door_def(mesh)?;
        self.nearest_doorway(pos)
    }

    /// One fitted leaf's world AABB as a WT box, for the placement ghost.
    pub(crate) fn door_leaf_box_wt(
        &self,
        mesh: MeshId,
        way: &Doorway,
        leaf: u32,
    ) -> Option<[f32; 6]> {
        let (pos, rot, scale) = self.door_fit_transform(mesh, way, leaf)?;
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        // Scale in model space, then the yaw (a multiple of 90°) just swaps X and Z.
        let sized = (max - min) * scale;
        let yawed = if (rot.to_euler(glam::EulerRot::YXZ).0).abs() > 0.1 {
            Vec3::new(sized.z, sized.y, sized.x)
        } else {
            sized
        };
        let s = WORLD_SCALE;
        Some([
            (pos.x - yawed.x * 0.5) / s,
            pos.y / s,
            (pos.z - yawed.z * 0.5) / s,
            yawed.x / s,
            yawed.y / s,
            yawed.z / s,
        ])
    }

    /// Register every placed door with the nav **door overlay**, after the grid bake.
    ///
    /// This is the mechanism the whole hunter side turns on, and it is why doors are
    /// deliberately kept out of `prop_solid_boxes`: that list is voxelized into the grid
    /// and frozen, so a door baked there could never open. The overlay instead rides the
    /// frozen grid and is read *live* by A\* — a door swinging open reroutes hunters on
    /// the next query with no re-bake at all.
    ///
    /// Each door's authored [`DoorAccess`] becomes the overlay's `passable` flag, so a
    /// door hunters may never open reads as solid to their pathing rather than merely
    /// expensive. The nav index is cached back onto the door's [`DoorGeom`] so the door
    /// system can flip the live flag as the panel moves.
    pub(crate) fn register_doors_with_nav(&mut self) {
        let doors: Vec<(hecs::Entity, MeshId)> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .filter(|(_, r)| crate::props::door_def(r.mesh).is_some())
            .map(|(e, r)| (e, r.mesh))
            .collect();

        let s = WORLD_SCALE;
        let mut brushes = Vec::new();
        let mut owners = Vec::new();
        for (e, _mesh) in doors {
            let Some((min, max)) = self.prop_world_aabb(e) else { continue };
            let mut b = Brush::new(
                0,
                Op::Subtract,
                min.x / s,
                min.y / s,
                min.z / s,
                ((max.x - min.x) / s).max(0.01),
                ((max.y - min.y) / s).max(0.01),
                ((max.z - min.z) / s).max(0.01),
            );
            b.door = true;
            brushes.push(b);
            owners.push(e);
        }
        if brushes.is_empty() {
            return;
        }
        let Some(nav) = self.nav.as_mut() else { return };
        nav.set_doors(&brushes);
        for (i, e) in owners.iter().enumerate() {
            // Authored access decides whether this door is a wall to hunters.
            let can_open = self
                .ecs
                .world()
                .get::<&Door>(*e)
                .map(|d| d.opens_for(true))
                .unwrap_or(true);
            nav.set_door_passable(i, can_open);
        }
        // Cache the index on each door so the tick can drive the overlay.
        for (i, e) in owners.into_iter().enumerate() {
            if let Ok(mut g) = self.ecs.world_mut().get::<&mut DoorGeom>(e) {
                g.nav_index = Some(i);
            }
        }
        log::info!("nav door overlay: {} door(s) registered", brushes.len());
    }

    /// A hunter has reached a door on its path and wants it open. Returns `true` if the
    /// door started opening — `false` if it is not theirs to open, so the caller can
    /// stop waiting on it.
    ///
    /// The sound is what makes this matter: it is played at the *door*, attenuated by
    /// the player's distance, so hearing one open tells you where a hunter just went.
    pub(crate) fn hunter_opens_door(&mut self, nav_index: usize) -> bool {
        let target = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Door, &DoorGeom)>()
            .iter()
            .find(|(_, _, g)| g.nav_index == Some(nav_index))
            .map(|(e, d, _)| (e, *d));
        let Some((e, door)) = target else { return false };
        if !door.opens_for(true) {
            return false;
        }
        if door.state != DoorState::Closed {
            return true; // already on its way — keep waiting
        }
        let (pos, mesh) = {
            let w = self.ecs.world();
            (
                w.get::<&Transform>(e).map(|t| t.pos).unwrap_or_default(),
                w.get::<&Renderable>(e).map(|r| r.mesh).ok(),
            )
        };
        if let Ok(mut d) = self.ecs.world_mut().get::<&mut Door>(e) {
            d.state = DoorState::Opening;
            d.timer = 0.0;
        }
        let listener = self.character.as_ref().map(|c| c.pos).unwrap_or(pos);
        let vol = falloff_volume(DOOR_VOL, pos, listener, DOOR_AUDIBLE_RANGE);
        if let (Some(audio), Some(name)) = (
            self.audio.as_mut(),
            mesh.and_then(|m| crate::props::door_def(m).map(|d| d.open_sound)),
        ) {
            audio.play(name, vol);
        }
        true
    }

    /// Panel geometry for a door that hasn't been baked yet, derived from its
    /// registered mesh bounds. `None` headless (no GLB → no bounds).
    ///
    /// The HUNT bake caches this on the entity as a [`DoorGeom`] because it also holds
    /// the live collider; in BUILD there is no collider, so the draw path just derives
    /// it per frame — it is a handful of arithmetic on numbers already in memory.
    pub(crate) fn derive_door_geom(
        &self,
        mesh: MeshId,
        scale: Vec3,
        hinge: HingeSide,
    ) -> Option<DoorGeom> {
        crate::props::door_def(mesh)?;
        let bounds = self.prop_bounds.get(&mesh).copied()?;
        Some(door_geom(bounds, scale, hinge))
    }

    /// The door entities already filling `way` — anything a fresh placement there would
    /// stack on top of. Matched by position rather than by a stored link, so a door
    /// dragged into a doorway with the gizmo counts too.
    pub(crate) fn doorway_occupants(&self, way: &Doorway) -> Vec<hecs::Entity> {
        let mut out = Vec::new();
        for (e, t, r) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &Renderable)>()
            .iter()
        {
            if crate::props::door_def(r.mesh).is_none() {
                continue;
            }
            // Inside the opening's span horizontally and standing on its floor. Half a
            // leaf's slack, so both leaves of a double are caught but the door in the
            // next doorway along is not.
            let horiz = Vec3::new(t.pos.x - way.center.x, 0.0, t.pos.z - way.center.z).length();
            if horiz <= way.width * 0.6 && (t.pos.y - way.base_y).abs() < 0.5 {
                out.push(e);
            }
        }
        out
    }

    /// The selected prop's authored door settings, if it is a door. Drives the
    /// SELECTED DOOR inspector; `None` for a non-door prop or an empty selection.
    ///
    /// A door placed **outside** a doorway has no `Door` component yet (it only gets
    /// one at the HUNT bake), so this synthesises the catalog default rather than
    /// hiding the editor — you can set the hinge before ever entering HUNT.
    pub fn selected_door(&self) -> Option<Door> {
        let e = self.selected_prop?;
        let w = self.ecs.world();
        let mesh = w.get::<&Renderable>(e).ok()?.mesh;
        crate::props::door_def(mesh)?;
        Some(
            w.get::<&Door>(e)
                .map(|d| *d)
                .unwrap_or_else(|_| Door::new(door_opening_type(mesh))),
        )
    }

    /// Write back edited door settings from the inspector, preserving the runtime
    /// open/close state so an edit mid-hunt doesn't teleport the panel. Re-derives the
    /// baked geometry when the hinge or mirror changed, so the swing updates live.
    pub fn set_selected_door(&mut self, edited: Door) {
        let Some(e) = self.selected_prop else { return };
        self.record_undo();
        let (hinge_changed, state) = {
            let w = self.ecs.world();
            match w.get::<&Door>(e) {
                Ok(d) => (d.hinge != edited.hinge, Some((d.state, d.open_frac, d.timer))),
                Err(_) => (true, None),
            }
        };
        let mut next = edited;
        if let Some((st, frac, timer)) = state {
            next.state = st;
            next.open_frac = frac;
            next.timer = timer;
        }
        let _ = self.ecs.world_mut().insert_one(e, next);

        // The hinge is baked into `DoorGeom`, so a hinge change has to re-derive it or
        // the door would keep swinging about its old edge.
        if hinge_changed {
            let bounds = self
                .ecs
                .world()
                .get::<&Renderable>(e)
                .ok()
                .and_then(|r| self.prop_bounds.get(&r.mesh).copied());
            let scale = self.ecs.world().get::<&Transform>(e).ok().map(|t| t.scale);
            if let (Some(bounds), Some(scale)) = (bounds, scale) {
                let old = self.ecs.world().get::<&DoorGeom>(e).ok().map(|g| *g);
                if let Some(old) = old {
                    let mut geom = door_geom(bounds, scale, next.hinge);
                    geom.collider = old.collider; // keep the live panel
                    let _ = self.ecs.world_mut().insert_one(e, geom);
                }
            }
        }
    }

    /// Attach the live door state to every authored door prop, at BUILD→HUNT.
    ///
    /// Adds the [`Door`] component (from the catalog, unless the level already authored
    /// one) plus the transient [`DoorGeom`] carrying the resolved pivot and the moving
    /// collider. Skips doors whose mesh bounds were never registered — headless callers
    /// load no GLBs, so there they stay inert scenery rather than panicking.
    pub(crate) fn spawn_doors(&mut self) {
        let mut targets: Vec<(hecs::Entity, MeshId, Option<Door>)> = Vec::new();
        for (e, r, existing) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable, Option<&Door>)>()
            .iter()
        {
            if crate::props::door_def(r.mesh).is_some() {
                targets.push((e, r.mesh, existing.copied()));
            }
        }
        for (e, mesh, existing) in targets {
            let Some(def) = crate::props::door_def(mesh) else { continue };
            // An authored door keeps its settings; one placed before doors had settings
            // (or with no Door component at all) picks up the catalog defaults.
            let door = existing.unwrap_or_else(|| Door::new(door_opening_type(mesh)));
            let _ = def;
            let Some(bounds) = self.prop_bounds.get(&mesh).copied() else {
                continue; // headless — no GLB bounds, so no panel
            };
            // The *entity's* scale, not the catalog's: a door fitted into a doorway
            // carries a non-uniform scale, and its collider has to match what is drawn.
            let t = self.ecs.world().get::<&Transform>(e).ok().map(|t| *t);
            let scale = t.map(|t| t.scale).unwrap_or(Vec3::splat(
                crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0),
            ));
            let mut geom = door_geom(bounds, scale, door.hinge);

            // Bake the collider at the shut pose.
            if let Some(t) = t {
                let model = self.prop_model_matrix(mesh, t.pos, t.rot, t.scale);
                let (center, rot) = panel_pose(&door, &geom, model, t.rot);
                let handle = self.physics.add_door_panel(center, geom.half, rot);
                geom.collider = Some(handle);
                self.door_entities.insert(handle, e);
            }
            let _ = self.ecs.world_mut().insert(e, (door, geom));
        }
    }

    /// Tear down every door panel and strip the transient door state, on return to
    /// BUILD. The authored [`Door`] settings survive (they persist); only the runtime
    /// open/close state and the geometry cache go, so a door left open in HUNT is shut
    /// again in BUILD — the same HUNT-transient rule the destructible props follow.
    pub(crate) fn clear_doors(&mut self) {
        self.physics.clear_door_panels();
        self.door_entities.clear();
        let doors: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &DoorGeom)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for e in doors {
            let _ = self.ecs.world_mut().remove_one::<DoorGeom>(e);
            if let Ok(mut d) = self.ecs.world_mut().get::<&mut Door>(e) {
                d.state = DoorState::Closed;
                d.open_frac = 0.0;
                d.timer = 0.0;
            }
        }
    }

    /// The nearest door the player is close enough to work, with its distance.
    /// `use_radius` is measured to the panel's world centre.
    fn door_within_reach(&self, from: Vec3) -> Option<(hecs::Entity, f32)> {
        let mut best: Option<(hecs::Entity, f32)> = None;
        for (e, t, r, door) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &Renderable, &Door)>()
            .iter()
        {
            if crate::props::door_def(r.mesh).is_none() {
                continue;
            }
            let d = t.pos.distance(from);
            if d <= door.use_radius && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((e, d));
            }
            let _ = r;
        }
        best
    }

    /// The player pressed **use** (B). Opens the nearest shut door in reach, or shuts
    /// the nearest open one — GoldenEye's context-sensitive use button. Returns `true`
    /// if a door took the input, so the caller can fall through to the key's other
    /// meaning (reload) when there is no door to work.
    ///
    /// A door mid-animation ignores the press, so mashing B doesn't stutter the panel.
    pub fn use_door(&mut self) -> bool {
        if self.mode != Mode::Hunt {
            return false;
        }
        let Some(feet) = self.character.as_ref().map(|c| c.pos) else {
            return false;
        };
        let Some((e, _)) = self.door_within_reach(feet) else {
            return false;
        };
        let Some((mesh, action)) = ({
            let w = self.ecs.world();
            let door = w.get::<&Door>(e).ok().map(|d| *d);
            let mesh = w.get::<&Renderable>(e).ok().map(|r| r.mesh);
            match (door, mesh) {
                (Some(d), Some(m)) => match d.state {
                    DoorState::Closed if d.opens_for(false) => Some((m, DoorState::Opening)),
                    DoorState::Open if d.opens_for(false) => Some((m, DoorState::Closing)),
                    // Locked (or not the player's to open): rattle, don't move.
                    DoorState::Closed | DoorState::Open => Some((m, DoorState::Closed)),
                    // Mid-swing — ignore the press entirely.
                    _ => None,
                },
                _ => None,
            }
        }) else {
            return false;
        };

        let pos = self
            .ecs
            .world()
            .get::<&Transform>(e)
            .ok()
            .map(|t| t.pos)
            .unwrap_or(feet);
        let vol = falloff_volume(DOOR_VOL, pos, feet, DOOR_AUDIBLE_RANGE);
        let sound = match action {
            DoorState::Opening => crate::props::door_def(mesh).map(|d| d.open_sound),
            // A *sliding* panel is heard while it travels, so its close starts now. A
            // hinged one is heard when it latches, which is the door system's job at the
            // end of the swing — playing it here made a door you had just pushed sound
            // like it had already shut.
            DoorState::Closing => crate::props::door_def(mesh)
                .filter(|d| d.motion != DoorMotion::Swing)
                .map(|d| d.close_sound),
            _ => Some(crate::props::DOOR_LOCKED_SOUND),
        };
        if action != DoorState::Closed {
            if let Ok(mut d) = self.ecs.world_mut().get::<&mut Door>(e) {
                d.state = action;
                d.timer = 0.0;
            }
        }
        if let (Some(audio), Some(name)) = (self.audio.as_mut(), sound) {
            audio.play(name, vol);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecs::{ComponentData, DoorAccess, EntityData};

    /// Author a door prop at `pos` with an explicit model-space AABB, via the real
    /// placement path. `bounds` lets a test choose which axis the panel is thin on —
    /// the thing the real assets disagree about.
    fn place_door(world: &mut World, mesh: MeshId, pos: Vec3, bounds: (Vec3, Vec3)) {
        world.register_prop_bounds(mesh, bounds.0, bounds.1);
        world.prop_tool = Some(mesh);
        world.prop_preview_pos = Some(pos);
        assert!(world.confirm_prop_placement(), "door should place");
        world.cancel_prop_placement();
    }

    /// A panel 1 m wide on X, 2 m tall, 0.1 m thin on Z — the common door shape.
    fn thin_z() -> (Vec3, Vec3) {
        (Vec3::new(-0.5, 0.0, -0.05), Vec3::new(0.5, 2.0, 0.05))
    }

    /// The mirror case: thin on **X**, wide on Z. Three of the thirteen real door
    /// models are built this way, so the derivation must not assume otherwise.
    fn thin_x() -> (Vec3, Vec3) {
        (Vec3::new(-0.05, 0.0, -0.5), Vec3::new(0.05, 2.0, 0.5))
    }

    /// The hinge is taken from the panel's *wider* horizontal axis, whichever that is,
    /// and sits on the authored edge at the panel's base. Getting this wrong on the
    /// thin-X models would swing them about their own face.
    #[test]
    fn hinge_follows_the_panels_wider_axis_not_a_fixed_one() {
        let z = door_geom(thin_z(), Vec3::ONE, HingeSide::Left);
        assert_eq!(z.width_axis, Vec3::X, "wide on X gives an X width axis");
        assert_eq!(z.hinge.x, -0.5, "hinge on the left edge of X");
        assert_eq!(z.hinge.y, 0.0, "hinge at the panel base");

        let x = door_geom(thin_x(), Vec3::ONE, HingeSide::Left);
        assert_eq!(x.width_axis, Vec3::Z, "wide on Z gives a Z width axis");
        assert_eq!(x.hinge.z, -0.5, "hinge on the left edge of Z");

        // Right hinge takes the other edge of the same axis.
        let right = door_geom(thin_z(), Vec3::ONE, HingeSide::Right);
        assert_eq!(right.hinge.x, 0.5);
    }

    /// The hinge comes from the AABB edge, not the model origin — four of the real
    /// door GLBs are not centred on their width axis, so an origin-based pivot would
    /// sit off the panel entirely.
    #[test]
    fn hinge_is_taken_from_the_aabb_not_the_origin() {
        // Panel spanning x in [0.2, 1.2]: its centre is 0.7, nowhere near the origin.
        let offset = (Vec3::new(0.2, 0.0, -0.05), Vec3::new(1.2, 2.0, 0.05));
        let g = door_geom(offset, Vec3::ONE, HingeSide::Left);
        assert_eq!(g.hinge.x, 0.2, "hinge on the panel's actual left edge");
        assert_eq!(g.width, 1.0, "width from the extent, not the origin distance");
    }

    /// A swing pivots about the hinge: the hinge edge stays put while the free edge
    /// sweeps. This is the property that distinguishes a hinge from a spin about the
    /// centre, which is what a naive rotation on the model would give.
    #[test]
    fn a_swinging_panel_turns_about_its_hinge_edge() {
        let geom = door_geom(thin_z(), Vec3::ONE, HingeSide::Left);
        let mut door = Door::new(OpeningType::Swing);
        door.open_frac = 1.0; // fully open, 90 degrees

        let m = open_transform(&door, &geom, Vec3::ONE);
        let hinge_after = m.transform_point3(geom.hinge);
        assert!(
            hinge_after.distance(geom.hinge) < 1e-5,
            "the hinge edge is the fixed point of the swing"
        );

        // The free edge swings a quarter turn away — it must actually move, and it must
        // stay the same distance from the hinge (a rotation, not a shear).
        let free = Vec3::new(0.5, 0.0, 0.0);
        let free_after = m.transform_point3(free);
        assert!(free_after.distance(free) > 0.5, "the free edge swept clear");
        assert!(
            ((free_after - geom.hinge).length() - (free - geom.hinge).length()).abs() < 1e-5,
            "the panel is rigid — the free edge keeps its radius"
        );
    }

    /// `flip` mirrors the swing, which together with the hinge side gives all four
    /// configurations of a real doorway.
    #[test]
    fn flip_reverses_the_swing() {
        let geom = door_geom(thin_z(), Vec3::ONE, HingeSide::Left);
        let free = Vec3::new(0.5, 0.0, 0.0);
        let mut door = Door::new(OpeningType::Swing);
        door.open_frac = 1.0;
        let a = open_transform(&door, &geom, Vec3::ONE).transform_point3(free);
        door.flip = true;
        let b = open_transform(&door, &geom, Vec3::ONE).transform_point3(free);
        assert!(a.z * b.z < 0.0, "the free edge swings to opposite sides");
    }

    /// A sliding door travels its own width by default, and a **scaled** door still
    /// travels exactly that far in world metres — the open transform sits inside the
    /// prop's scale, so the model-space offset has to be divided back out.
    #[test]
    fn a_slide_travels_its_own_width_in_world_metres_at_any_scale() {
        let scale = 0.7;
        // `door_geom` reports width in world metres (model width times scale).
        let geom = door_geom(thin_z(), Vec3::splat(scale), HingeSide::Left);
        assert!((geom.width - 0.7).abs() < 1e-6, "a 1 m panel at 0.7 is 0.7 m wide");

        let mut door = Door::new(OpeningType::Slide);
        door.open_frac = 1.0;
        let m = open_transform(&door, &geom, Vec3::splat(scale));
        // Model-space offset, then through the prop scale, gives the world travel.
        let travelled = m.transform_point3(Vec3::ZERO).x * scale;
        assert!(
            (travelled - geom.width).abs() < 1e-5,
            "slid {travelled} m, expected its own {} m width",
            geom.width
        );
    }

    /// A shutter rises instead of sliding sideways.
    #[test]
    fn a_shutter_travels_vertically() {
        let geom = door_geom(thin_z(), Vec3::ONE, HingeSide::Left);
        let mut door = Door::new(OpeningType::Shutter);
        door.open_frac = 1.0;
        let d = open_transform(&door, &geom, Vec3::ONE).transform_point3(Vec3::ZERO);
        assert!(d.y > 1.9 && d.x.abs() < 1e-6, "rose by its height, not sideways");
    }

    /// A shut door draws exactly where a plain prop would, so nothing shifts the moment
    /// doors become live.
    #[test]
    fn a_shut_door_is_geometrically_a_plain_prop() {
        let geom = door_geom(thin_z(), Vec3::ONE, HingeSide::Left);
        let door = Door::new(OpeningType::Swing);
        assert_eq!(open_transform(&door, &geom, Vec3::ONE), Mat4::IDENTITY);
    }

    /// The HUNT bake gives every door prop a live panel (component + collider) and
    /// leaves ordinary furniture alone; returning to BUILD strips the transient state
    /// and shuts the door again, so the authored level is never saved mid-swing.
    #[test]
    fn bake_arms_doors_only_and_teardown_shuts_them() {
        let mut world = World::new();
        place_door(&mut world, MeshId::WoodenDoor, Vec3::new(2.0, 0.0, 2.0), thin_z());
        place_door(&mut world, MeshId::WoodenCrate, Vec3::new(4.0, 0.0, 2.0), thin_z());

        world.spawn_doors();
        let geoms = world.ecs.world().query::<&DoorGeom>().iter().count();
        assert_eq!(geoms, 1, "only the door got panel geometry, not the crate");
        assert_eq!(world.door_entities.len(), 1, "one panel collider, mapped back");

        // Open it, then leave HUNT.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &DoorGeom)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Open;
            d.open_frac = 1.0;
        }
        world.clear_doors();

        assert_eq!(world.ecs.world().query::<&DoorGeom>().iter().count(), 0);
        assert!(world.door_entities.is_empty(), "panels torn down");
        let d = *world.ecs.world().get::<&Door>(e).unwrap();
        assert_eq!(d.state, DoorState::Closed, "a door left open returns shut");
        assert_eq!(d.open_frac, 0.0);
    }

    /// Doors are kept out of the nav bake on purpose: that list is frozen at BUILD to
    /// HUNT, so a door voxelized into it could never be opened for a hunter. A crate in
    /// the same level still blocks.
    #[test]
    fn doors_are_excluded_from_the_frozen_nav_solids() {
        let mut world = World::new();
        place_door(&mut world, MeshId::WoodenCrate, Vec3::new(3.0, 0.0, 3.0), thin_z());
        assert_eq!(world.prop_solid_boxes().len(), 1, "the crate is baked solid");

        place_door(&mut world, MeshId::WoodenDoor, Vec3::new(2.0, 0.0, 2.0), thin_z());
        assert_eq!(
            world.prop_solid_boxes().len(),
            1,
            "the door adds nothing to the frozen solids"
        );
    }

    /// A level written before doors had authoring options still loads — every new field
    /// falls back to the same default a freshly placed door gets. This is what let the
    /// level format stay at v3 instead of needing a bump.
    #[test]
    fn legacy_doors_load_with_the_catalog_defaults() {
        let legacy = r#"{"type":"Door","opening_type":"Swing"}"#;
        let c: ComponentData = serde_json::from_str(legacy).expect("legacy Door loads");
        let ComponentData::Door { open_angle, use_radius, access, hinge, .. } = c else {
            panic!("wrong variant");
        };
        assert_eq!(open_angle, crate::ecs::DOOR_OPEN_ANGLE);
        assert_eq!(use_radius, crate::ecs::DOOR_USE_RADIUS);
        assert_eq!(access, DoorAccess::Both);
        assert_eq!(hinge, HingeSide::Left);
    }

    /// A fully authored door survives a save/load round-trip, while its runtime
    /// open/close state deliberately does not.
    #[test]
    fn authored_door_options_round_trip() {
        let mut world = World::new();
        let id = world.ecs.alloc_id();
        world.ecs.spawn_authored(&EntityData {
            id,
            components: vec![ComponentData::Renderable { mesh: MeshId::BrownSlidingDoor }],
        });
        let e = world.ecs.resolve(id).unwrap();
        let mut door = Door::new(OpeningType::Slide);
        door.flip = true;
        door.hinge = HingeSide::Right;
        door.auto_close = 0.0; // stays open
        door.access = DoorAccess::PlayerOnly;
        door.use_radius = 3.5;
        door.state = DoorState::Open; // runtime — must NOT survive
        door.open_frac = 1.0;
        let _ = world.ecs.world_mut().insert_one(e, door);

        let json = serde_json::to_string(&world.ecs.save_authored()).unwrap();
        let back: Vec<EntityData> = serde_json::from_str(&json).unwrap();
        let mut reloaded = World::new();
        reloaded.ecs.load_authored(&back);
        let d = *reloaded
            .ecs
            .world()
            .get::<&Door>(reloaded.ecs.resolve(id).unwrap())
            .unwrap();

        assert!(d.flip);
        assert_eq!(d.hinge, HingeSide::Right);
        assert_eq!(d.auto_close, 0.0);
        assert_eq!(d.access, DoorAccess::PlayerOnly);
        assert_eq!(d.use_radius, 3.5);
        assert_eq!(d.state, DoorState::Closed, "runtime state is never persisted");
        assert_eq!(d.open_frac, 0.0);
    }

    /// A locked door refuses everyone; the other modes are direction-aware. This is the
    /// authoring lever that makes a door a hiding advantage or a trap.
    #[test]
    fn access_gates_who_may_open_a_door() {
        let mut d = Door::new(OpeningType::Swing);
        d.access = DoorAccess::Locked;
        assert!(!d.opens_for(false) && !d.opens_for(true));
        d.access = DoorAccess::PlayerOnly;
        assert!(d.opens_for(false) && !d.opens_for(true));
        d.access = DoorAccess::HuntersOnly;
        assert!(!d.opens_for(false) && d.opens_for(true));
        d.access = DoorAccess::Both;
        assert!(d.opens_for(false) && d.opens_for(true));
    }

    /// Door audio falls off with distance. Flat playback would be actively misleading in
    /// a hide-and-seek game, where hearing a door is how you locate a hunter.
    #[test]
    fn door_sound_falls_off_with_distance() {
        let near = falloff_volume(1.0, Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 20.0);
        let far = falloff_volume(1.0, Vec3::ZERO, Vec3::new(15.0, 0.0, 0.0), 20.0);
        let beyond = falloff_volume(1.0, Vec3::ZERO, Vec3::new(50.0, 0.0, 0.0), 20.0);
        assert!(near > far && far > 0.0, "closer is louder");
        assert_eq!(beyond, 0.0, "out of range is silent, never negative");
    }

    /// Cut a real doorway with the door tool, aiming at the middle of a wall face.
    /// Returns the doorway the geometry now reports.
    fn cut_doorway(world: &mut World, double: bool) -> Doorway {
        world.door_tool_key(); // arm
        // Drive the real scroll entry point rather than setting `door_double`
        // directly. An earlier version of this test poked the flag, which is exactly
        // why it stayed green while the app was routing scroll only to the *hole*
        // tool and the door width never actually changed in game.
        if double {
            world.adjust_opening_size(1.0, 0.0);
        }
        // Aim the fly camera at the -X wall of the starting room from inside it.
        world.camera.pos = Vec3::new(3.0, 0.9, 3.0);
        world.camera.yaw = std::f32::consts::PI; // look toward -X
        world.camera.pitch = 0.0;
        let p = world
            .resolve_opening_placement()
            .expect("crosshair should resolve a wall opening");
        world.cut_opening(p).expect("the carve should rebuild a region");
        world.cancel_opening();
        let ways = world.doorways();
        assert_eq!(ways.len(), 1, "exactly one doorway was cut");
        ways[0]
    }

    /// The door tool's scroll toggles single vs double width, and a double carve is
    /// exactly two singles wide — so each leaf keeps the single door's aspect ratio.
    #[test]
    fn scroll_toggles_single_and_double_doorway_width() {
        let mut world = World::new();
        world.initial_meshes();
        let single = cut_doorway(&mut world, false);
        assert_eq!(single.leaves, 1);
        assert!(
            (single.width - DOOR_WIDTH * WORLD_SCALE).abs() < 1e-4,
            "single opening is {DOOR_WIDTH} WT wide"
        );

        let mut world = World::new();
        world.initial_meshes();
        let double = cut_doorway(&mut world, true);
        assert_eq!(double.leaves, 2, "a double carve asks for two leaves");
        assert!(
            (double.width - single.width * 2.0).abs() < 1e-4,
            "a double is exactly two singles wide, so each leaf matches a single"
        );
    }

    /// The armed door tool must report itself through `is_opening_arming`, because that
    /// is the predicate the app's scroll handler routes on. It is deliberately **not**
    /// `is_hole_arming` — routing on that shipped a door tool whose scroll did nothing.
    #[test]
    fn the_door_tool_routes_scroll_through_the_opening_predicate() {
        let mut world = World::new();
        world.door_tool_key();
        assert!(world.is_door_arming());
        assert!(
            world.is_opening_arming(),
            "the app routes scroll on this predicate — it must cover the door tool"
        );
        assert!(
            !world.is_hole_arming(),
            "and NOT on this one, which excludes the door tool"
        );
    }

    /// The scroll handler only touches the door width while the *door* tool is armed —
    /// it must not stomp the hole tool's free sizing.
    #[test]
    fn door_scroll_does_not_disturb_the_hole_tool() {
        let mut world = World::new();
        world.hole_tool_key();
        let before = world.hole_w;
        world.adjust_opening_size(1.0, 0.0);
        assert_eq!(world.hole_w, before + 1.0, "the hole tool still free-sizes");
        assert!(!world.door_double, "and the door width was not touched");
    }

    /// A door placed at a doorway is **fitted** to the hole rather than dropped at its
    /// catalog scale. This is the fix for the visible defect: scaling by height alone
    /// left `wooden_door` — the widest model — overhanging its carve by ~23%.
    #[test]
    fn a_door_placed_at_a_doorway_is_fitted_to_it() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);

        // A deliberately over-wide panel: 1.4 m across for a 2 m opening height, which
        // is the `wooden_door` shape that overhangs.
        let bounds = (Vec3::new(-0.7, 0.0, -0.05), Vec3::new(0.7, 2.6, 0.05));
        world.register_prop_bounds(MeshId::WoodenDoor, bounds.0, bounds.1);

        let (_, _, scale) = world
            .door_fit_transform(MeshId::WoodenDoor, &way, 0)
            .expect("a registered door fits");
        // The panel's width axis is X here, so scale.x carries the width fit.
        let fitted_width = 1.4 * scale.x;
        let fitted_height = 2.6 * scale.y;
        assert!(
            (fitted_width - way.width).abs() < 1e-4,
            "fitted to {} m, the opening is {} m",
            fitted_width,
            way.width
        );
        assert!((fitted_height - way.height).abs() < 1e-4, "and fills its height");
        assert!(
            scale.x < scale.y,
            "the fit is non-uniform — an over-wide panel is squashed on width only"
        );
    }

    /// Placing into a double-width doorway authors **both leaves** in one click,
    /// mirrored so they meet in the middle, each filling half the opening.
    #[test]
    fn a_double_doorway_places_two_mirrored_leaves() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, true);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );

        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement(), "the pair should place");
        world.cancel_prop_placement();

        let doors: Vec<Door> = world
            .ecs
            .world()
            .query::<&Door>()
            .iter()
            .map(|d| *d)
            .collect();
        assert_eq!(doors.len(), 2, "one click authored both leaves");
        assert!(
            doors.iter().any(|d| d.hinge == HingeSide::Left)
                && doors.iter().any(|d| d.hinge == HingeSide::Right),
            "the leaves hinge on opposite edges"
        );
        assert!(
            doors.iter().any(|d| d.flip) && doors.iter().any(|d| !d.flip),
            "and swing opposite ways, so the pair opens outward together"
        );

        // Each leaf fills exactly half the span, and they don't overlap.
        let (a, _, _) = world.door_fit_transform(MeshId::WoodenDoor, &way, 0).unwrap();
        let (b, _, _) = world.door_fit_transform(MeshId::WoodenDoor, &way, 1).unwrap();
        let gap = (a - b).length();
        assert!(
            (gap - way.width / 2.0).abs() < 1e-4,
            "leaf centres sit half a leaf either side of the middle"
        );
    }

    /// A door dropped away from any doorway still free-places as an ordinary prop —
    /// snapping is an assist, not a restriction (closet doors, decoration, the two
    /// vehicle-scale shutters that no doorway carve would ever fit).
    #[test]
    fn a_door_away_from_a_doorway_still_free_places() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );

        // Well away from the carve.
        let far = way.center + Vec3::new(0.0, 0.0, 5.0);
        assert!(world.door_snap_for(MeshId::WoodenDoor, far).is_none());
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(far);
        assert!(world.confirm_prop_placement());
        assert_eq!(
            world.ecs.world().query::<&Renderable>().iter().count(),
            1,
            "a single free-placed door, not a fitted pair"
        );
    }

    /// A non-door prop is never snapped into a doorway, however close it is dropped.
    #[test]
    fn only_doors_snap_to_doorways() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        assert!(world.door_snap_for(MeshId::WoodenCrate, way.center).is_none());
    }

    /// A fitted door's collider is built from the **entity's** scale, not the catalog's,
    /// so what you bump into matches what you see. Reading the catalog scale here would
    /// give a squashed door a full-width collider.
    #[test]
    fn a_fitted_doors_panel_matches_its_authored_scale() {
        let bounds = (Vec3::new(-0.5, 0.0, -0.05), Vec3::new(0.5, 2.0, 0.05));
        // Squashed to 60% on width, 80% on height — a doorway fit.
        let squashed = door_geom(bounds, Vec3::new(0.6, 0.8, 0.8), HingeSide::Left);
        assert!((squashed.width - 0.6).abs() < 1e-6, "width follows scale.x");
        assert!((squashed.height - 1.6).abs() < 1e-6, "height follows scale.y");
        assert!((squashed.half.x - 0.3).abs() < 1e-6, "and so does the collider box");
    }

    /// Mirroring reflects the panel's artwork but leaves its footprint alone — the
    /// bounding box is unchanged, which is what lets the hinge and the collider stay
    /// untouched. Without that property, mirroring a leaf would move the door.
    #[test]
    fn mirroring_reflects_the_panel_without_moving_it() {
        let bounds = thin_z();
        let geom = door_geom(bounds, Vec3::ONE, HingeSide::Left);
        let mut door = Door::new(OpeningType::Swing);
        assert_eq!(mirror_transform(&door, &geom), Mat4::IDENTITY, "off by default");

        door.mirrored = true;
        let m = mirror_transform(&door, &geom);
        // A point on the left edge lands on the right edge, and vice versa.
        let left = Vec3::new(-0.5, 1.0, 0.0);
        let moved = m.transform_point3(left);
        assert!((moved.x - 0.5).abs() < 1e-5, "left edge reflects to the right edge");
        assert!((moved.y - 1.0).abs() < 1e-5, "height is untouched");
        // The pair of edges maps onto itself → same AABB → hinge and collider unmoved.
        let right_back = m.transform_point3(Vec3::new(0.5, 1.0, 0.0));
        assert!((right_back.x + 0.5).abs() < 1e-5);
    }

    /// The second leaf of a double door is mirrored, so the pair reads as matched
    /// leaves meeting in the middle rather than two copies of the same door.
    #[test]
    fn the_second_leaf_of_a_double_is_mirrored() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, true);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());

        let doors: Vec<Door> = world.ecs.world().query::<&Door>().iter().map(|d| *d).collect();
        assert_eq!(doors.len(), 2);
        assert_eq!(
            doors.iter().filter(|d| d.mirrored).count(),
            1,
            "exactly one leaf is mirrored — a mirrored pair, not two mirrored copies"
        );
    }

    /// The inspector reads a door that has never been baked (placed in BUILD, no
    /// `Door` component yet) as its catalog default, so hinge and mirror are editable
    /// before ever entering HUNT — and writing back changes the stored settings.
    #[test]
    fn the_inspector_reads_and_writes_an_unbaked_door() {
        let mut world = World::new();
        world.initial_meshes();
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(Vec3::new(3.0, 0.0, 3.0));
        assert!(world.confirm_prop_placement());
        world.cancel_prop_placement();
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.selected_prop = Some(e);

        let mut d = world.selected_door().expect("a door reports its settings");
        assert_eq!(d.hinge, HingeSide::Left, "catalog default");
        d.hinge = HingeSide::Right;
        d.mirrored = true;
        world.set_selected_door(d);

        let back = world.selected_door().unwrap();
        assert_eq!(back.hinge, HingeSide::Right);
        assert!(back.mirrored);
    }

    /// A non-door prop has no door settings, so the inspector section stays hidden.
    #[test]
    fn the_inspector_ignores_a_non_door_prop() {
        let mut world = World::new();
        world.register_prop_bounds(
            MeshId::WoodenCrate,
            Vec3::new(-0.5, 0.0, -0.5),
            Vec3::new(0.5, 1.0, 0.5),
        );
        world.prop_tool = Some(MeshId::WoodenCrate);
        world.prop_preview_pos = Some(Vec3::new(3.0, 0.0, 3.0));
        assert!(world.confirm_prop_placement());
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.selected_prop = Some(e);
        assert!(world.selected_door().is_none());
    }

    /// A doorway holds exactly one door. Clicking again with the tool still armed —
    /// which is easy to do by accident, since it stays armed to let you place several
    /// props in a row — **replaces** the door instead of stacking a second panel in the
    /// same hole.
    #[test]
    fn placing_into_an_occupied_doorway_replaces_rather_than_stacks() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, true);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);

        assert!(world.confirm_prop_placement());
        assert_eq!(world.ecs.world().query::<&Door>().iter().count(), 2, "a pair");

        // Click again on the same doorway — the tool is still armed.
        assert!(world.confirm_prop_placement());
        assert_eq!(
            world.ecs.world().query::<&Door>().iter().count(),
            2,
            "still a pair — the second click replaced, it did not stack"
        );

        // And a different door model swaps the pair out rather than adding to it.
        world.register_prop_bounds(
            MeshId::MetalDoor2,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::MetalDoor2);
        assert!(world.confirm_prop_placement());
        let meshes: Vec<MeshId> = world
            .ecs
            .world()
            .query::<&Renderable>()
            .iter()
            .map(|r| r.mesh)
            .collect();
        assert_eq!(meshes.len(), 2, "the doorway still holds exactly one door");
        assert!(
            meshes.iter().all(|m| *m == MeshId::MetalDoor2),
            "and it is the newly placed model"
        );
    }

    /// A door in a *different* doorway is untouched by a placement — the occupancy check
    /// must be local to the opening, not "any door anywhere".
    #[test]
    fn replacing_a_door_leaves_other_doorways_alone() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        // A free-placed door well away from the carve must not count as an occupant.
        let far = way.center + Vec3::new(0.0, 0.0, 6.0);
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(far);
        assert!(world.confirm_prop_placement());

        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());
        assert_eq!(
            world.ecs.world().query::<&Renderable>().iter().count(),
            2,
            "the distant door survived; only the doorway itself was filled"
        );
    }

    /// **The editor must draw a door exactly as the hunt does.** Panel geometry is only
    /// *baked* at BUILD→HUNT, so an earlier version fell back to the plain prop matrix
    /// while authoring — which silently ignored the mirror, so toggling Mirror appeared
    /// to do nothing until you started the hunt.
    #[test]
    fn mirroring_is_visible_in_the_editor_not_only_in_the_hunt() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());
        world.cancel_prop_placement();

        // Still BUILD: nothing has been baked, so there is no DoorGeom on the entity.
        assert_eq!(
            world.ecs.world().query::<&DoorGeom>().iter().count(),
            0,
            "precondition: the draw path has to derive the geometry itself"
        );

        let before = world.prop_draws(1.0);
        assert_eq!(before.len(), 1);

        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Door)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.selected_prop = Some(e);
        let mut d = world.selected_door().unwrap();
        d.mirrored = true;
        world.set_selected_door(d);

        let after = world.prop_draws(1.0);
        assert_ne!(
            before[0].1, after[0].1,
            "the editor's draw matrix changed when the door was mirrored"
        );

        // …and it changed by a *reflection*: the panel's two width edges swap, so the
        // door does not move, it only faces the other way.
        let (l, r) = (Vec3::new(-0.5, 1.0, 0.0), Vec3::new(0.5, 1.0, 0.0));
        let plain_l = before[0].1.transform_point3(l);
        let mirrored_r = after[0].1.transform_point3(r);
        assert!(
            plain_l.distance(mirrored_r) < 1e-4,
            "the left edge unmirrored lands where the right edge mirrored does"
        );
    }

    /// The same guarantee for the hinge: changing it in the editor is visible there.
    #[test]
    fn a_hinge_change_is_visible_in_the_editor() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());
        world.cancel_prop_placement();

        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Door)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        world.selected_prop = Some(e);
        let mut d = world.selected_door().unwrap();
        // A shut door sits in the same place whichever edge it hinges on, so compare
        // the derived pivot rather than the matrix.
        let geom_l = world
            .derive_door_geom(MeshId::WoodenDoor, Vec3::ONE, HingeSide::Left)
            .unwrap();
        d.hinge = HingeSide::Right;
        world.set_selected_door(d);
        let geom_r = world
            .derive_door_geom(MeshId::WoodenDoor, Vec3::ONE, HingeSide::Right)
            .unwrap();
        assert_ne!(geom_l.hinge, geom_r.hinge, "the pivot moved to the other edge");
        assert_eq!(world.selected_door().unwrap().hinge, HingeSide::Right);
    }

    /// Bring a level to the state BUILD→HUNT leaves it in, without the rest of the
    /// mode switch (player entry, wave spawn): bake the nav grid, then bring the doors
    /// up and attach them to the overlay.
    fn go_live(world: &mut World) {
        let mut regions = std::mem::take(&mut world.regions);
        let nav = nav::bake(&mut regions, &[], &[]).expect("nav bakes");
        world.regions = regions;
        world.nav = Some(nav);
        world.spawn_doors();
        world.register_doors_with_nav();
    }

    /// Run the ECS system list one step, as `fixed_step` does.
    fn tick(world: &mut World, dt: f32) -> Vec<(&'static str, Vec3)> {
        let mut ctx = crate::ecs::SystemCtx {
            dt,
            player_feet: Vec3::ZERO,
            nav: world.nav.as_mut(),
            physics: &mut world.physics,
            commands: Vec::new(),
            sounds: Vec::new(),
        };
        world.ecs.run_systems(&mut ctx);
        ctx.sounds
    }

    /// Place a door into the doorway `way` and bring the level live.
    fn door_in_doorway(world: &mut World, way: &Doorway) {
        world.register_prop_bounds(
            MeshId::WoodenDoor,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(MeshId::WoodenDoor);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());
        world.cancel_prop_placement();
    }

    /// Placed doors join the **live nav overlay**, not the frozen solid bake. This is
    /// the split the whole hunter side rests on: the bake is voxelized once and can
    /// never change, so a door in it could never open.
    #[test]
    fn doors_join_the_live_nav_overlay_not_the_frozen_bake() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        door_in_doorway(&mut world, &way);

        assert!(
            world.prop_solid_boxes().is_empty(),
            "the door contributes nothing to the frozen bake"
        );
        go_live(&mut world);
        assert_eq!(
            world.nav.as_ref().unwrap().door_count(),
            1,
            "and everything to the live overlay"
        );
        // The index is cached back on the door so the tick can drive the overlay.
        let g = world
            .ecs
            .world()
            .query::<&DoorGeom>()
            .iter()
            .next()
            .map(|g| *g)
            .expect("the door was baked");
        assert_eq!(g.nav_index, Some(0));
    }

    /// A door hunters may never open is registered **impassable**, so their A\* treats
    /// the shut panel as solid rather than merely expensive — an authored "player only"
    /// door is a real wall. One they may open stays passable.
    #[test]
    fn authored_access_becomes_the_navs_passable_flag() {
        for (access, passable) in [
            (DoorAccess::Both, true),
            (DoorAccess::HuntersOnly, true),
            (DoorAccess::PlayerOnly, false),
            (DoorAccess::Locked, false),
        ] {
            let mut world = World::new();
            world.initial_meshes();
            let way = cut_doorway(&mut world, false);
            door_in_doorway(&mut world, &way);
            let e = world
                .ecs
                .world()
                .query::<(hecs::Entity, &Door)>()
                .iter()
                .map(|(e, _)| e)
                .next()
                .unwrap();
            {
                let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
                d.access = access;
            }
            go_live(&mut world);

            // The overlay has no getter for `passable`, so probe it the way A* does:
            // a route straight through the shut doorway either exists or it does not.
            let nav = world.nav.as_ref().unwrap();
            let n = Vec3::new(way.center.x, way.base_y + 0.1, way.center.z);
            let from = n - Vec3::new(0.0, 0.0, 0.6);
            let to = n + Vec3::new(0.0, 0.0, 0.6);
            let routed = nav.find_path(from, to).is_some();
            assert_eq!(
                routed, passable,
                "{access:?} should leave the shut door {}",
                if passable { "routable" } else { "impassable" }
            );
        }
    }

    /// The door tick publishes the panel's blocking state to the overlay every step, so
    /// a hunter's path updates as the door swings — no nav re-bake anywhere.
    #[test]
    fn the_tick_publishes_door_state_to_the_nav_overlay_live() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        door_in_doorway(&mut world, &way);
        go_live(&mut world);
        assert!(!world.nav.as_ref().unwrap().door_is_open(0), "starts shut");

        // Open it and run the tick until the panel is meaningfully ajar.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Door)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Opening;
        }
        for _ in 0..240 {
            tick(&mut world, 1.0 / 120.0);
        }
        assert!(
            world.nav.as_ref().unwrap().door_is_open(0),
            "the overlay followed the panel open"
        );

        // And back again when it shuts.
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Closing;
        }
        for _ in 0..240 {
            tick(&mut world, 1.0 / 120.0);
        }
        assert!(
            !world.nav.as_ref().unwrap().door_is_open(0),
            "and followed it shut again"
        );
    }

    /// A hunter's request opens a door it is allowed to work, and is refused on one it
    /// is not — so `DoorAccess` holds on the AI side as well as in pathing.
    #[test]
    fn a_hunter_can_work_a_door_unless_access_forbids_it() {
        let mut world = World::new();
        world.initial_meshes();
        let way = cut_doorway(&mut world, false);
        door_in_doorway(&mut world, &way);
        go_live(&mut world);
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Door)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap();

        assert!(world.hunter_opens_door(0), "a shared door opens for a hunter");
        assert_eq!(
            world.ecs.world().get::<&Door>(e).unwrap().state,
            DoorState::Opening
        );

        // A player-only door refuses the same request and stays shut.
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Closed;
            d.open_frac = 0.0;
            d.access = DoorAccess::PlayerOnly;
        }
        assert!(!world.hunter_opens_door(0), "not theirs to open");
        assert_eq!(
            world.ecs.world().get::<&Door>(e).unwrap().state,
            DoorState::Closed
        );
    }

    /// An unknown door index is a no-op rather than a panic — the overlay index and the
    /// entity set are maintained separately, so a stale request must be survivable.
    #[test]
    fn a_stale_door_request_is_ignored() {
        let mut world = World::new();
        world.initial_meshes();
        assert!(!world.hunter_opens_door(7));
    }

    /// Set up one live door of `mesh` in a doorway and return its entity.
    fn live_door(world: &mut World, mesh: MeshId) -> hecs::Entity {
        world.initial_meshes();
        let way = cut_doorway(world, false);
        world.register_prop_bounds(
            mesh,
            Vec3::new(-0.5, 0.0, -0.05),
            Vec3::new(0.5, 2.0, 0.05),
        );
        world.prop_tool = Some(mesh);
        world.prop_preview_pos = Some(way.center);
        assert!(world.confirm_prop_placement());
        world.cancel_prop_placement();
        go_live(world);
        world
            .ecs
            .world()
            .query::<(hecs::Entity, &Door)>()
            .iter()
            .map(|(e, _)| e)
            .next()
            .unwrap()
    }

    /// Run the tick until the door settles, collecting every cue raised on the way.
    fn run_until_settled(world: &mut World) -> Vec<&'static str> {
        let mut cues = Vec::new();
        for _ in 0..1200 {
            for (n, _) in tick(world, 1.0 / 120.0) {
                cues.push(n);
            }
        }
        cues
    }

    /// A hinged door latches when it **arrives**, not when it is pushed. Playing the
    /// close on the way in made a door you had just shut sound closed while you were
    /// still watching it swing.
    #[test]
    fn a_swinging_door_is_heard_when_it_finishes_closing() {
        let mut world = World::new();
        let e = live_door(&mut world, MeshId::WoodenDoor);
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Open;
            d.open_frac = 1.0;
            d.auto_close = 0.0; // manual only
        }
        // Push it shut the way the use key does — silent at this moment.
        let before = world.prop_draws(1.0).len();
        assert_eq!(before, 1);
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Closing;
        }
        // The very first tick must not have latched yet…
        let first = tick(&mut world, 1.0 / 120.0);
        assert!(first.is_empty(), "no close cue while the panel is still swinging");

        // …and the cue lands exactly once, as it arrives.
        let cues = run_until_settled(&mut world);
        assert_eq!(
            cues.len(),
            1,
            "a hinged door is heard once, on the latch: {cues:?}"
        );
        assert!(cues[0].contains("close"));
        assert_eq!(
            world.ecs.world().get::<&Door>(e).unwrap().state,
            DoorState::Closed
        );
    }

    /// A door that shuts **itself** used to be completely silent — nothing on the
    /// auto-close path ever raised a cue. An auto-closing door is a free audio tell, so
    /// it has to be heard.
    #[test]
    fn a_door_that_shuts_itself_is_still_heard() {
        let mut world = World::new();
        let e = live_door(&mut world, MeshId::WoodenDoor);
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Open;
            d.open_frac = 1.0;
            d.timer = 0.05; // about to time out
            d.auto_close = 0.05;
        }
        let cues = run_until_settled(&mut world);
        assert_eq!(cues.len(), 1, "the self-closing door was heard: {cues:?}");
    }

    /// A sliding panel is heard for the whole journey, so its close belongs at the
    /// moment it starts moving — and it is the *same* clip as the open, because the
    /// sound is the door running in its track, not a latch.
    #[test]
    fn a_sliding_door_is_heard_as_it_starts_to_travel() {
        let def = crate::props::door_def(MeshId::BrownSlidingDoor).expect("a door");
        assert_eq!(
            def.open_sound, def.close_sound,
            "a slide sounds the same both ways"
        );

        let mut world = World::new();
        let e = live_door(&mut world, MeshId::BrownSlidingDoor);
        {
            let mut d = world.ecs.world_mut().get::<&mut Door>(e).unwrap();
            d.state = DoorState::Open;
            d.open_frac = 1.0;
            d.timer = 0.05;
            d.auto_close = 0.05;
        }
        // The cue lands while the panel is still open, not on arrival.
        let mut heard_at: Option<f32> = None;
        for _ in 0..1200 {
            let cues = tick(&mut world, 1.0 / 120.0);
            if !cues.is_empty() && heard_at.is_none() {
                heard_at = Some(world.ecs.world().get::<&Door>(e).unwrap().open_frac);
            }
        }
        let frac = heard_at.expect("the sliding door was heard");
        assert!(
            frac > 0.9,
            "heard as it set off (open_frac {frac}), not once it had arrived"
        );
    }
}
