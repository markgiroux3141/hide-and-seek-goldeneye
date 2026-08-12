//! Prop edit gizmo (object mode): select a placed prop, then move or rotate it with
//! a mouse-driven gizmo. Unlike the platform gizmo (look-relative mouse deltas while
//! grabbed), this is driven by the absolute mouse ray each frame — the free-cursor
//! object-mode workflow — by projecting the cursor onto the handle's axis (translate)
//! or rotation plane (rotate). Translate = 3 axis arrows; Rotate = a Y ring (yaw).
//! One gizmo shows at a time, cycled with Tab. Reuses the platform gizmo's render
//! channel (`ColoredMesh` via `set_gizmo_mesh`) and `ray_aabb` picking.

use engine::geometry::geom::ray_aabb;
use engine::render::mesh::ColorVertex;
use glam::{EulerRot, Quat};

use super::super::*;

/// Metric gizmo sizing (world metres — props are ~1 m, unlike the WT-scaled platform
/// gizmo). Arrows stick out from the object; the ring encircles it.
const ARROW_LEN: f32 = 1.1;
const ARROW_HALF: f32 = 0.05;
const RING_RADIUS: f32 = 0.95;
/// Tube (minor) radius of the rotation torus.
const RING_TUBE: f32 = 0.045;
/// Torus tessellation: segments around the ring × around the tube.
const RING_MAJOR_SEGS: usize = 40;
const RING_MINOR_SEGS: usize = 8;
/// Annulus half-width (fraction of `RING_RADIUS`) the rotate pick accepts.
const RING_PICK_BAND: f32 = 0.3;

/// Snap increments when Ctrl is held during a drag: 0.5 m translate, 15° rotate.
const TRANSLATE_SNAP: f32 = 0.5;
const ROTATE_SNAP: f32 = std::f32::consts::FRAC_PI_2 / 6.0; // 15°

const RED: [f32; 3] = [0.93, 0.20, 0.20];
const GREEN: [f32; 3] = [0.20, 0.93, 0.20];
const BLUE: [f32; 3] = [0.20, 0.20, 0.93];
const YELLOW: [f32; 3] = [0.95, 0.85, 0.20];

fn axis_vec(a: PropAxis) -> Vec3 {
    match a {
        PropAxis::X => Vec3::X,
        PropAxis::Y => Vec3::Y,
        PropAxis::Z => Vec3::Z,
    }
}

fn yaw_of(q: Quat) -> f32 {
    q.to_euler(EulerRot::YXZ).0
}

/// Param `t` of the point on the infinite line `p0 + t·a` (`a` unit) closest to the
/// ray `o + s·d`. `None` when the ray is parallel to the axis.
fn closest_t_on_axis(p0: Vec3, a: Vec3, o: Vec3, d: Vec3) -> Option<f32> {
    let w0 = p0 - o;
    let b = a.dot(d);
    let c = d.dot(d);
    let dcoef = a.dot(w0);
    let e = d.dot(w0);
    let denom = c - b * b; // a·a = 1
    (denom.abs() > 1e-6).then(|| (b * e - c * dcoef) / denom)
}

/// Intersect the ray with the horizontal plane `y = pivot.y`. `None` if parallel or
/// behind the origin.
fn ray_plane_xz(pivot: Vec3, o: Vec3, d: Vec3) -> Option<Vec3> {
    if d.y.abs() < 1e-6 {
        return None;
    }
    let t = (pivot.y - o.y) / d.y;
    (t >= 0.0).then(|| o + d * t)
}

impl World {
    /// The prop currently selected for editing, if any.
    pub fn selected_prop(&self) -> Option<hecs::Entity> {
        self.selected_prop
    }

    /// Whether a prop gizmo drag is in progress (the app keeps driving it each frame
    /// from the cursor ray until the button releases).
    pub fn is_prop_gizmo_dragging(&self) -> bool {
        self.prop_gizmo_drag.is_some()
    }

    /// Push this frame's mouse world ray (from the app's cursor unprojection).
    pub fn set_mouse_ray(&mut self, origin: Vec3, dir: Vec3) {
        self.mouse_ray = Some((origin, dir));
    }

    /// Set whether gizmo drags snap this frame (Ctrl held) — 0.5 m / 15° increments;
    /// otherwise continuous.
    pub fn set_gizmo_snap(&mut self, snap: bool) {
        self.gizmo_snap = snap;
    }

    /// Duplicate the selected prop (Shift+D): a full-component copy placed **exactly
    /// on top** of the original, which becomes the new selection so it can be
    /// dragged/rotated off into place.
    pub fn duplicate_selected_prop(&mut self) {
        let Some(src) = self.selected_prop else {
            return;
        };
        self.record_undo();
        if let Some(new_e) = self.ecs.duplicate_authored(src) {
            self.selected_prop = Some(new_e);
        }
    }

    /// Panel label for the active prop gizmo.
    pub fn prop_gizmo_label(&self) -> &'static str {
        match self.prop_gizmo_mode {
            PropGizmoMode::Translate => "Move",
            PropGizmoMode::Rotate => "Rotate",
        }
    }

    /// Cycle the active prop gizmo (T key): Translate ↔ Rotate.
    pub fn cycle_prop_gizmo(&mut self) {
        self.prop_gizmo_mode = match self.prop_gizmo_mode {
            PropGizmoMode::Translate => PropGizmoMode::Rotate,
            PropGizmoMode::Rotate => PropGizmoMode::Translate,
        };
    }

    /// Deselect the current prop (clears any drag too).
    pub fn deselect_prop(&mut self) {
        self.selected_prop = None;
        self.prop_gizmo_drag = None;
    }

    /// Delete the selected prop (panel Delete button / Del key): despawn its authored
    /// entity and clear the selection. Records an undo checkpoint, so the delete is
    /// undoable (the snapshot restores the removed entity). No-op if nothing is
    /// selected. BUILD-only in practice (object mode is a BUILD panel).
    pub fn delete_selected_prop(&mut self) {
        let Some(e) = self.selected_prop else {
            return;
        };
        self.record_undo();
        self.ecs.despawn_authored(e);
        self.selected_prop = None;
        self.prop_gizmo_drag = None;
    }

    fn prop_transform(&self, e: hecs::Entity) -> Option<crate::ecs::Transform> {
        self.ecs.world().entity(e).ok()?.get::<&crate::ecs::Transform>().map(|r| *r)
    }

    fn prop_mesh_id(&self, e: hecs::Entity) -> Option<crate::ecs::MeshId> {
        self.ecs.world().entity(e).ok()?.get::<&crate::ecs::Renderable>().map(|r| r.mesh)
    }

    /// World AABB (yaw ignored — good enough for click-selection + the HUNT collider
    /// bake) of a placed prop, from its registered model bounds + transform (base at
    /// `pos`, centred).
    pub(crate) fn prop_world_aabb(&self, e: hecs::Entity) -> Option<(Vec3, Vec3)> {
        let t = self.prop_transform(e)?;
        let mesh = self.prop_mesh_id(e)?;
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        let anchor = Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5);
        let lo = t.pos + (min - anchor) * t.scale;
        let hi = t.pos + (max - anchor) * t.scale;
        Some((lo.min(hi), lo.max(hi)))
    }

    /// The gizmo origin for a prop — its mid-height above the base, so arrows/ring
    /// sit around the object rather than at the floor. `None` without bounds. A
    /// light has no mesh bounds: its gizmo sits at the light position itself.
    fn prop_gizmo_origin(&self, e: hecs::Entity) -> Option<Vec3> {
        if self.entity_is_light(e) {
            return self.prop_transform(e).map(|t| t.pos);
        }
        let t = self.prop_transform(e)?;
        let mesh = self.prop_mesh_id(e)?;
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        let half_h = (max.y - min.y) * 0.5 * t.scale.y;
        Some(t.pos + Vec3::new(0.0, half_h, 0.0))
    }

    /// The click-pick AABB for a selectable authored entity: a prop's world bounds,
    /// or a small synthetic box around a light (which has no mesh). `None` if the
    /// entity is neither (or a prop without registered bounds).
    fn select_aabb(&self, e: hecs::Entity) -> Option<(Vec3, Vec3)> {
        if self.entity_is_light(e) {
            let p = self.prop_transform(e)?.pos;
            let h = Vec3::splat(super::light::LIGHT_MARKER_HALF * 1.7);
            return Some((p - h, p + h));
        }
        self.prop_world_aabb(e)
    }

    /// The gizmo mode to actually drive for `e`: lights are translate-only (no
    /// rotate/scale), so they ignore the cycled `prop_gizmo_mode`.
    fn effective_gizmo_mode(&self, e: hecs::Entity) -> PropGizmoMode {
        if self.entity_is_light(e) {
            PropGizmoMode::Translate
        } else {
            self.prop_gizmo_mode
        }
    }

    /// Translate-arrow AABBs `(axis, min, max)` around `origin` (metres).
    fn translate_parts(origin: Vec3) -> [(PropAxis, Vec3, Vec3); 3] {
        let h = ARROW_HALF;
        [
            (
                PropAxis::X,
                origin - Vec3::new(0.0, h, h),
                origin + Vec3::new(ARROW_LEN, h, h),
            ),
            (
                PropAxis::Y,
                origin - Vec3::new(h, 0.0, h),
                origin + Vec3::new(h, ARROW_LEN, h),
            ),
            (
                PropAxis::Z,
                origin - Vec3::new(h, h, 0.0),
                origin + Vec3::new(h, h, ARROW_LEN),
            ),
        ]
    }

    /// Which translate axis the mouse ray hits, if any (nearest).
    fn translate_pick(&self, origin: Vec3, o: Vec3, d: Vec3) -> Option<PropAxis> {
        let pad = Vec3::splat(0.03);
        let mut best: Option<(f32, PropAxis)> = None;
        for (ax, min, max) in Self::translate_parts(origin) {
            if let Some(t) = ray_aabb(o, d, min - pad, max + pad) {
                if best.map(|(bt, _)| t < bt).unwrap_or(true) {
                    best = Some((t, ax));
                }
            }
        }
        best.map(|(_, ax)| ax)
    }

    /// Whether the mouse ray hits the rotate ring (annulus around `origin`).
    fn rotate_pick(&self, origin: Vec3, o: Vec3, d: Vec3) -> bool {
        let Some(p) = ray_plane_xz(origin, o, d) else {
            return false;
        };
        let r = ((p.x - origin.x).powi(2) + (p.z - origin.z).powi(2)).sqrt();
        (r - RING_RADIUS).abs() <= RING_RADIUS * RING_PICK_BAND
    }

    /// Select the prop under the mouse ray (nearest AABB), else deselect. Returns
    /// `true` if a prop got selected. BUILD only.
    pub fn select_prop_at(&mut self, origin: Vec3, dir: Vec3) -> bool {
        if self.mode != Mode::Build {
            return false;
        }
        // Every authored entity with a Transform is a selection candidate; the pick
        // box distinguishes props (mesh bounds) from lights (a synthetic box).
        let ents: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &crate::ecs::Transform)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        let mut best: Option<(f32, hecs::Entity)> = None;
        for e in ents {
            if let Some((min, max)) = self.select_aabb(e) {
                if let Some(t) = ray_aabb(origin, dir, min, max) {
                    if best.map(|(bt, _)| t < bt).unwrap_or(true) {
                        best = Some((t, e));
                    }
                }
            }
        }
        self.prop_gizmo_drag = None;
        self.selected_prop = best.map(|(_, e)| e);
        self.selected_prop.is_some()
    }

    /// Try to start a gizmo drag under the mouse ray on the selected prop. Returns
    /// `true` if a handle was grabbed (so the caller shouldn't also re-select).
    pub fn start_prop_gizmo_drag(&mut self, o: Vec3, d: Vec3) -> bool {
        let Some(e) = self.selected_prop else {
            return false;
        };
        let (Some(t), Some(origin)) = (self.prop_transform(e), self.prop_gizmo_origin(e)) else {
            return false;
        };
        match self.effective_gizmo_mode(e) {
            PropGizmoMode::Translate => {
                let Some(axis) = self.translate_pick(origin, o, d) else {
                    return false;
                };
                let Some(t0) = closest_t_on_axis(origin, axis_vec(axis), o, d) else {
                    return false;
                };
                self.record_undo();
                self.prop_gizmo_drag = Some(PropGizmoDrag {
                    mode: PropGizmoMode::Translate,
                    axis,
                    entity: e,
                    pivot: origin,
                    start_pos: t.pos,
                    start_yaw: yaw_of(t.rot),
                    grab_ref: t0,
                });
                true
            }
            PropGizmoMode::Rotate => {
                if !self.rotate_pick(origin, o, d) {
                    return false;
                }
                let Some(p) = ray_plane_xz(origin, o, d) else {
                    return false;
                };
                let ang0 = (p.z - origin.z).atan2(p.x - origin.x);
                self.record_undo();
                self.prop_gizmo_drag = Some(PropGizmoDrag {
                    mode: PropGizmoMode::Rotate,
                    axis: PropAxis::Y,
                    entity: e,
                    pivot: origin,
                    start_pos: t.pos,
                    start_yaw: yaw_of(t.rot),
                    grab_ref: ang0,
                });
                true
            }
        }
    }

    /// Advance the active gizmo drag from this frame's mouse ray. No-op if no drag.
    pub fn update_prop_gizmo_drag(&mut self) {
        let Some(drag) = self.prop_gizmo_drag else {
            return;
        };
        let Some((o, d)) = self.mouse_ray else {
            return;
        };
        let snap = self.gizmo_snap;
        let (pos, rot) = match drag.mode {
            PropGizmoMode::Translate => {
                let a = axis_vec(drag.axis);
                let Some(t) = closest_t_on_axis(drag.pivot, a, o, d) else {
                    return;
                };
                let mut new = drag.start_pos + a * (t - drag.grab_ref);
                if snap {
                    // Snap the moved axis's absolute coordinate to the grid.
                    let comp = new.dot(a);
                    let snapped = (comp / TRANSLATE_SNAP).round() * TRANSLATE_SNAP;
                    new += a * (snapped - comp);
                }
                (new, None)
            }
            PropGizmoMode::Rotate => {
                let Some(p) = ray_plane_xz(drag.pivot, o, d) else {
                    return;
                };
                let ang = (p.z - drag.pivot.z).atan2(p.x - drag.pivot.x);
                let mut yaw = drag.start_yaw - (ang - drag.grab_ref);
                if snap {
                    yaw = (yaw / ROTATE_SNAP).round() * ROTATE_SNAP;
                }
                (drag.start_pos, Some(Quat::from_rotation_y(yaw)))
            }
        };
        if let Ok(t) = self
            .ecs
            .world_mut()
            .query_one_mut::<&mut crate::ecs::Transform>(drag.entity)
        {
            t.pos = pos;
            if let Some(r) = rot {
                t.rot = r;
            }
        }
    }

    /// End the active gizmo drag (mouse release).
    pub fn end_prop_gizmo_drag(&mut self) {
        self.prop_gizmo_drag = None;
    }

    /// Snap the selected prop straight down onto the floor beneath it (the panel's
    /// "Ground" button): raycast down from just above the base, set the base there.
    pub fn ground_selected_prop(&mut self) {
        let Some(e) = self.selected_prop else {
            return;
        };
        let Some(t) = self.prop_transform(e) else {
            return;
        };
        // Cast down from a little above the current base to find the floor.
        let from = t.pos + Vec3::new(0.0, 0.2, 0.0);
        let Some(hit) = self.physics.raycast(from, -Vec3::Y, 200.0) else {
            return;
        };
        self.record_undo();
        if let Ok(tr) = self
            .ecs
            .world_mut()
            .query_one_mut::<&mut crate::ecs::Transform>(e)
        {
            tr.pos.y = hit.point.y;
        }
    }

    /// The selected prop's gizmo overlay mesh (colored handles), or `None`. The
    /// hovered/dragged handle is brightened. Mode-dependent: arrows or the Y ring.
    pub(crate) fn prop_gizmo_mesh(&self) -> Option<ColoredMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let e = self.selected_prop?;
        let origin = self.prop_gizmo_origin(e)?;
        let ray = self.mouse_ray;
        let mut verts = Vec::new();
        let mut idx = Vec::new();
        let bright = |c: [f32; 3]| [(c[0] * 1.5).min(1.0), (c[1] * 1.5).min(1.0), (c[2] * 1.5).min(1.0)];

        match self.effective_gizmo_mode(e) {
            PropGizmoMode::Translate => {
                let hover = self
                    .prop_gizmo_drag
                    .map(|d| d.axis)
                    .or_else(|| ray.and_then(|(o, d)| self.translate_pick(origin, o, d)));
                for (ax, min, max) in Self::translate_parts(origin) {
                    let base = match ax {
                        PropAxis::X => RED,
                        PropAxis::Y => GREEN,
                        PropAxis::Z => BLUE,
                    };
                    let col = if Some(ax) == hover { bright(base) } else { base };
                    push_colored_box(&mut verts, &mut idx, min, max, col);
                }
            }
            PropGizmoMode::Rotate => {
                let hot = self.prop_gizmo_drag.is_some()
                    || ray.map(|(o, d)| self.rotate_pick(origin, o, d)).unwrap_or(false);
                let col = if hot { bright(YELLOW) } else { YELLOW };
                // A proper torus lying flat in XZ (encircles the Y/yaw axis).
                push_torus(&mut verts, &mut idx, origin, RING_RADIUS, RING_TUBE, col);
            }
        }
        Some(ColoredMesh { vertices: verts, indices: idx })
    }
}

/// Append a torus (major radius `r`, tube radius `tube`) centred at `center`, lying
/// flat in the XZ plane so it encircles the vertical (Y/yaw) axis. Uniform `color`.
fn push_torus(v: &mut Vec<ColorVertex>, idx: &mut Vec<u32>, center: Vec3, r: f32, tube: f32, color: [f32; 3]) {
    use std::f32::consts::TAU;
    let base = v.len() as u32;
    let (maj, min) = (RING_MAJOR_SEGS, RING_MINOR_SEGS);
    for i in 0..maj {
        let u = (i as f32 / maj as f32) * TAU;
        let (cu, su) = (u.cos(), u.sin());
        for j in 0..min {
            let w = (j as f32 / min as f32) * TAU;
            let (cw, sw) = (w.cos(), w.sin());
            let p = center
                + Vec3::new((r + tube * cw) * cu, tube * sw, (r + tube * cw) * su);
            v.push(ColorVertex { pos: p.to_array(), color });
        }
    }
    for i in 0..maj {
        let i2 = (i + 1) % maj;
        for j in 0..min {
            let j2 = (j + 1) % min;
            let a = base + (i * min + j) as u32;
            let b = base + (i2 * min + j) as u32;
            let c = base + (i2 * min + j2) as u32;
            let d = base + (i * min + j2) as u32;
            idx.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
}
