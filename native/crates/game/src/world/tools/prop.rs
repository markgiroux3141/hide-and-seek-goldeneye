//! Prop placement tool (the object palette): arm a prop for placement, track a
//! floor ghost under the crosshair, and drop it as an authored ECS entity. Mirrors
//! the additive placement tool ([`super::super::World::arm_place`]) but authors an
//! entity instead of a brush. Catalog: [`crate::props`]; entity model: [`crate::ecs`].

use super::super::*;
use crate::ecs::{ComponentData, EntityData, MeshId};

impl World {
    /// Whether the prop-placement tool is armed (the app draws its ghost + routes a
    /// left-click confirm while this is true).
    pub fn is_placing_prop(&self) -> bool {
        self.prop_tool.is_some()
    }

    /// Arm/toggle prop placement for `mesh`, BUILD only. Re-arming the same prop
    /// disarms; a different prop switches. Cancels any other armed tool/selection so
    /// the authoring tools stay mutually exclusive.
    pub fn arm_prop_placement(&mut self, mesh: MeshId) {
        if self.mode != Mode::Build {
            return;
        }
        if self.prop_tool == Some(mesh) {
            self.prop_tool = None;
            self.prop_preview_pos = None;
            return;
        }
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.selected = None;
        self.prop_tool = Some(mesh);
        self.prop_preview_pos = None;
    }

    /// Disarm prop placement (Esc / pointer release / panel close).
    pub fn cancel_prop_placement(&mut self) {
        self.prop_tool = None;
        self.prop_preview_pos = None;
    }

    /// Register a prop mesh's model-space AABB `(min, max)`, called by the app once
    /// the GLB loads. Drives the ground/centre anchor + the ghost footprint.
    pub fn register_prop_bounds(&mut self, mesh: MeshId, min: Vec3, max: Vec3) {
        self.prop_bounds.insert(mesh, (min, max));
    }

    /// A prop's model-space anchor: horizontal centre + vertical base. The render +
    /// ghost matrices place this point at the prop's translation, so a prop authored
    /// on the floor rests its base there, centred. Zero if bounds weren't registered.
    fn prop_anchor(&self, mesh: MeshId) -> Vec3 {
        match self.prop_bounds.get(&mesh) {
            Some((min, max)) => Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5),
            None => Vec3::ZERO,
        }
    }

    /// World model matrix for a placed prop: put its anchor at `pos`, then apply the
    /// authored rotation + scale. Shared by the in-world draw and (future) pick.
    fn prop_model_matrix(&self, mesh: MeshId, pos: Vec3, rot: Quat, scale: Vec3) -> Mat4 {
        let anchor = self.prop_anchor(mesh);
        Mat4::from_translation(pos)
            * Mat4::from_quat(rot)
            * Mat4::from_scale(scale)
            * Mat4::from_translation(-anchor)
    }

    /// Recompute the floor ghost under the mouse-cursor ray `(origin, dir)` (each
    /// frame while armed) and return its box mesh, or `None` if the ray isn't on a
    /// floor. Stores the grounded metric point a confirm will place the prop at. The
    /// ray comes from the app unprojecting the free cursor (the object panel frees
    /// the cursor, so placement is mouse-picked, not crosshair-aimed).
    pub fn update_prop_preview(&mut self, origin: Vec3, dir: Vec3) -> Option<CpuMesh> {
        let mesh = self.prop_tool?;
        // Floor pick along the cursor ray, gated to an up-facing floor face.
        let (sel, hit_wt) = self.pick_face_hit_from(origin, dir)?;
        if sel.axis != Axis::Y || sel.side != Side::Min {
            self.prop_preview_pos = None;
            return None;
        }
        // Grounded metric contact point (WT → metres).
        let pos = hit_wt * WORLD_SCALE;
        self.prop_preview_pos = Some(pos);
        // Ghost footprint = the prop's world AABB at this placement, in WT for
        // `boxes_mesh`, base at the floor and centred on the cursor.
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        let scale = crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0);
        let (w, h, d) = (
            (max.x - min.x) * scale,
            (max.y - min.y) * scale,
            (max.z - min.z) * scale,
        );
        let s = WORLD_SCALE;
        let box_wt = [
            (pos.x - w * 0.5) / s,
            pos.y / s,
            (pos.z - d * 0.5) / s,
            w / s,
            h / s,
            d / s,
        ];
        Some(boxes_mesh(&[box_wt]))
    }

    /// Confirm placement (left-click): author a prop entity at the ghost point.
    /// Returns `true` if a prop was placed. Records an undo checkpoint. The tool
    /// stays armed so several props can be dropped in a row; disarm via the panel
    /// (re-click / close) or Esc.
    pub fn confirm_prop_placement(&mut self) -> bool {
        let (Some(mesh), Some(pos)) = (self.prop_tool, self.prop_preview_pos) else {
            return false;
        };
        let scale = crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0);
        self.record_undo();
        let id = self.ecs.alloc_id();
        self.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::IDENTITY.to_array(),
                    scale: [scale, scale, scale],
                },
                ComponentData::Renderable { mesh },
            ],
        });
        log::info!("placed prop {mesh:?} at {pos:?}");
        true
    }

    /// This frame's prop draw list for the renderer: `(catalog key, view_proj·world,
    /// tint)` per placed prop. Tint is white in Milestone 1 (the darken-on-hit uses
    /// it in Milestone 3). Non-prop `Renderable`s (e.g. a door) are skipped.
    pub fn prop_draws(&self, aspect: f32) -> Vec<(&'static str, Mat4, [f32; 4])> {
        let vp = self.view_proj(aspect);
        let mut out = Vec::new();
        for (t, r) in self
            .ecs
            .world()
            .query::<(&crate::ecs::Transform, &crate::ecs::Renderable)>()
            .iter()
        {
            let Some(def) = crate::props::def(r.mesh) else {
                continue;
            };
            let model = self.prop_model_matrix(r.mesh, t.pos, t.rot, t.scale);
            out.push((def.key, vp * model, [1.0, 1.0, 1.0, 1.0]));
        }
        out
    }
}
