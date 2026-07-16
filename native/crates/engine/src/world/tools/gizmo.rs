//! Platform move/scale gizmo: handle parts, ray-pick, drag start/step, and
//! the colored handle mesh.

use super::super::*;

impl World {
    // ─── Platform gizmo: move arrows + scale handles (JS `gizmo.js`) ─────

    /// Whether a gizmo drag is in progress (the app routes mouse motion to the
    /// drag instead of the camera while this is true).
    pub fn is_gizmo_dragging(&self) -> bool {
        self.gizmo_drag.is_some()
    }

    /// The gizmo's parts for the selected platform as `(handle, min_m, max_m, rgb)`
    /// AABBs in **meters** — shared by picking and the mesh build. Empty unless a
    /// platform is selected under the platform tool.
    pub(crate) fn gizmo_parts(&self) -> Vec<(GizmoHandle, Vec3, Vec3, [f32; 3])> {
        if self.platform_phase.is_none() {
            return Vec::new();
        }
        let Some(pid) = self.selected_platform else {
            return Vec::new();
        };
        let Some(p) = self.platform_by_id(pid) else {
            return Vec::new();
        };
        const RED: [f32; 3] = [0.93, 0.20, 0.20];
        const GREEN: [f32; 3] = [0.20, 0.93, 0.20];
        const BLUE: [f32; 3] = [0.20, 0.20, 0.93];
        let s = WORLD_SCALE;
        // WT AABB → meters AABB.
        let m = |x0: f32, y0: f32, z0: f32, x1: f32, y1: f32, z1: f32| {
            (Vec3::new(x0 * s, y0 * s, z0 * s), Vec3::new(x1 * s, y1 * s, z1 * s))
        };
        let (cx, cy, cz) = (p.center_x(), p.y, p.center_z());
        let sh = GIZMO_SHAFT_HALF;
        let al = GIZMO_ARROW_LENGTH;
        let hh = GIZMO_HANDLE_SIZE * 0.5;
        let hy = cy + hh; // scale cubes sit just above the top surface

        let mut parts = Vec::new();
        // Move arrows: thin boxes from centre outward along +axis.
        let (a, b) = m(cx, cy - sh, cz - sh, cx + al, cy + sh, cz + sh);
        parts.push((GizmoHandle::MoveX, a, b, RED));
        let (a, b) = m(cx - sh, cy, cz - sh, cx + sh, cy + al, cz + sh);
        parts.push((GizmoHandle::MoveY, a, b, GREEN));
        let (a, b) = m(cx - sh, cy - sh, cz, cx + sh, cy + sh, cz + al);
        parts.push((GizmoHandle::MoveZ, a, b, BLUE));
        // Scale handle cubes at edge midpoints.
        let mut cube = |gx: f32, gz: f32, handle: GizmoHandle, rgb: [f32; 3]| {
            let (a, b) = m(gx - hh, hy - hh, gz - hh, gx + hh, hy + hh, gz + hh);
            parts.push((handle, a, b, rgb));
        };
        cube(p.max_x(), cz, GizmoHandle::ScaleXMax, RED);
        cube(p.x, cz, GizmoHandle::ScaleXMin, RED);
        cube(cx, p.max_z(), GizmoHandle::ScaleZMax, BLUE);
        cube(cx, p.z, GizmoHandle::ScaleZMin, BLUE);
        parts
    }

    /// The gizmo handle under the crosshair, if any (ray vs each part's AABB).
    pub(crate) fn gizmo_pick(&self) -> Option<GizmoHandle> {
        let origin = self.camera.pos;
        let dir = self.camera.forward();
        let pad = Vec3::splat(0.02); // easier aim on the thin arrows
        let mut best: Option<(f32, GizmoHandle)> = None;
        for (h, min, max, _c) in self.gizmo_parts() {
            if let Some(t) = crate::geom::ray_aabb(origin, dir, min - pad, max + pad) {
                if best.map(|(bt, _)| t < bt).unwrap_or(true) {
                    best = Some((t, h));
                }
            }
        }
        best.map(|(_, h)| h)
    }

    /// Begin a gizmo drag on the given handle (records the platform's pre-drag
    /// transform for cancel).
    pub(crate) fn gizmo_start(&mut self, handle: GizmoHandle) {
        if let Some(pid) = self.selected_platform {
            if let Some(orig) = self.platform_by_id(pid) {
                self.gizmo_drag = Some(GizmoDrag {
                    handle,
                    platform_id: pid,
                    orig,
                    accumulated: 0.0,
                });
                log::info!("gizmo drag started ({handle:?})");
            }
        }
    }

    /// Feed a mouse delta into the active gizmo drag: project the handle's world
    /// axis onto the screen, accumulate distance-scaled motion, and apply whole-WT
    /// steps (JS `gizmo.processDrag`). Returns the rebuilt mesh when it changed.
    pub fn gizmo_drag_delta(&mut self, dx: f32, dy: f32) -> Option<RegionMesh> {
        let mut drag = self.gizmo_drag?;
        let p = self.platform_by_id(drag.platform_id)?;

        let world_axis = match drag.handle {
            GizmoHandle::MoveX | GizmoHandle::ScaleXMax => Vec3::X,
            GizmoHandle::ScaleXMin => -Vec3::X,
            GizmoHandle::MoveY => Vec3::Y,
            GizmoHandle::MoveZ | GizmoHandle::ScaleZMax => Vec3::Z,
            GizmoHandle::ScaleZMin => -Vec3::Z,
        };
        let fwd = self.camera.forward();
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd).normalize_or_zero();
        let center_m = Vec3::new(p.center_x(), p.y, p.center_z()) * WORLD_SCALE;
        let dist = self.camera.pos.distance(center_m).max(0.5);
        let sens = dist * GIZMO_DRAG_SENSITIVITY;

        drag.accumulated += (dx * world_axis.dot(right) - dy * world_axis.dot(up)) * sens;
        let wt = drag.accumulated.round();
        drag.accumulated -= wt;
        self.gizmo_drag = Some(drag);
        if wt == 0.0 {
            return None;
        }

        let plat = self.platforms.iter_mut().find(|p| p.id == drag.platform_id)?;
        let mut changed = false;
        match drag.handle {
            GizmoHandle::MoveX => {
                plat.x += wt;
                changed = true;
            }
            GizmoHandle::MoveY => {
                plat.y += wt;
                changed = true;
            }
            GizmoHandle::MoveZ => {
                plat.z += wt;
                changed = true;
            }
            GizmoHandle::ScaleXMax => {
                let ns = (plat.size_x + wt).max(1.0);
                changed = ns != plat.size_x;
                plat.size_x = ns;
            }
            GizmoHandle::ScaleXMin => {
                let ns = (plat.size_x + wt).max(1.0);
                if ns != plat.size_x {
                    plat.x -= ns - plat.size_x;
                    plat.size_x = ns;
                    changed = true;
                }
            }
            GizmoHandle::ScaleZMax => {
                let ns = (plat.size_z + wt).max(1.0);
                changed = ns != plat.size_z;
                plat.size_z = ns;
            }
            GizmoHandle::ScaleZMin => {
                let ns = (plat.size_z + wt).max(1.0);
                if ns != plat.size_z {
                    plat.z -= ns - plat.size_z;
                    plat.size_z = ns;
                    changed = true;
                }
            }
        }
        if changed {
            Some(self.rebuild_structures())
        } else {
            None
        }
    }

    /// The gizmo overlay mesh (colored handles) for the selected platform, or
    /// `None`. The hovered handle (or the one being dragged) is brightened.
    pub fn gizmo_mesh(&self) -> Option<ColoredMesh> {
        if self.mode != Mode::Build {
            return None;
        }
        let parts = self.gizmo_parts();
        if parts.is_empty() {
            return None;
        }
        let active = self.gizmo_drag.map(|d| d.handle).or_else(|| self.gizmo_pick());
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (h, min, max, rgb) in parts {
            let col = if Some(h) == active {
                [(rgb[0] * 1.5).min(1.0), (rgb[1] * 1.5).min(1.0), (rgb[2] * 1.5).min(1.0)]
            } else {
                rgb
            };
            push_colored_box(&mut vertices, &mut indices, min, max, col);
        }
        Some(ColoredMesh { vertices, indices })
    }
}
