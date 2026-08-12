//! Point-light placement + the level's lighting queries.
//!
//! A light is an authored ECS entity — [`Transform`](crate::ecs::Transform) +
//! [`PointLight`](crate::ecs::PointLight), and deliberately **no**
//! [`Renderable`](crate::ecs::Renderable) — so it persists alongside props (see
//! `world::persist`) yet is naturally skipped by every prop path (draw list,
//! colliders, nav) that queries `Renderable`. Placement mirrors [`super::prop`];
//! selection + the (translate-only) move gizmo are shared with the prop gizmo
//! (see `world::tools::prop_gizmo`), which treats a `PointLight` entity specially.

use super::super::*;
use crate::ecs::{AmbientSettings, ComponentData, EntityData, PointLight, Transform};

/// How far a light clears whatever surface it's placed on (floor / wall / ceiling) —
/// just enough for the marker to sit right above the surface rather than embedded in
/// it, so hovering across an edge barely shifts the light. Drag it out with the gizmo
/// afterwards if you want it further into the room.
const LIGHT_SURFACE_OFFSET: f32 = 0.2;

/// Half-extent (metres) of the small marker cube drawn for a light + its placement
/// ghost. Also the synthetic pick box the gizmo selects against (lights have no
/// mesh bounds).
pub(crate) const LIGHT_MARKER_HALF: f32 = 0.13;

/// Max lights that cast shadows at once — **must match the renderer's
/// `MAX_SHADOW_LIGHTS`**. The most influential (intensity × range) lights win the
/// slots; the rest still light without shadows.
pub const SHADOW_CAP: usize = 4;

impl World {
    /// Whether the point-light placement tool is armed.
    pub fn is_placing_light(&self) -> bool {
        self.light_tool
    }

    /// Arm/toggle point-light placement, BUILD only. Re-arming disarms; cancels any
    /// other armed tool/selection so the authoring tools stay mutually exclusive.
    pub fn arm_light_placement(&mut self) {
        if self.mode != Mode::Build {
            return;
        }
        if self.light_tool {
            self.light_tool = false;
            self.light_preview_pos = None;
            return;
        }
        self.opening_tool = None;
        self.opening_preview = None;
        self.place_tool = None;
        self.clear_platform_state();
        self.selected = None;
        self.prop_tool = None;
        self.prop_preview_pos = None;
        self.light_tool = true;
        self.light_preview_pos = None;
    }

    /// Disarm point-light placement (Esc / Q / panel close).
    pub fn cancel_light_placement(&mut self) {
        self.light_tool = false;
        self.light_preview_pos = None;
    }

    /// Recompute the placement point under the cursor ray each frame while armed,
    /// returning a marker ghost mesh, or `None` when the ray hits no surface. Works
    /// on **any** face — floor, wall, or ceiling: the picked face is axis-aligned, so
    /// the "into the room" direction is along its axis, signed toward the camera
    /// (always on the open side of the face it hit). The light is offset off the
    /// surface so it sits a bit away like a fixture. Stores the point a confirm places.
    pub fn update_light_preview(&mut self, origin: Vec3, dir: Vec3) -> Option<CpuMesh> {
        if !self.light_tool {
            return None;
        }
        let (sel, hit_wt) = self.pick_face_hit_from(origin, dir)?;
        let hit = hit_wt * WORLD_SCALE;
        let axis = match sel.axis {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        };
        // Sign of the camera's offset along the axis = the open-space (inward) side.
        let sign = (origin - hit).dot(axis).signum();
        let pos = hit + axis * (sign * LIGHT_SURFACE_OFFSET);
        self.light_preview_pos = Some(pos);
        Some(light_marker_box(pos))
    }

    /// Confirm placement (left-click): author a light entity at the preview point
    /// with default colour / intensity / range, select it, and **disarm** the tool —
    /// unlike props, lights place one at a time so the very next click grabs the move
    /// gizmo to reposition it (no deselect/reselect dance). Records an undo checkpoint.
    pub fn confirm_light_placement(&mut self) -> bool {
        if !self.light_tool {
            return false;
        }
        let Some(pos) = self.light_preview_pos else {
            return false;
        };
        let l = PointLight::default();
        self.record_undo();
        let id = self.ecs.alloc_id();
        let e = self.ecs.spawn_authored(&EntityData {
            id,
            components: vec![
                ComponentData::Transform {
                    pos: pos.to_array(),
                    rot: Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                },
                ComponentData::PointLight {
                    color: l.color.to_array(),
                    intensity: l.intensity,
                    range: l.range,
                },
            ],
        });
        // Select the fresh light so its gizmo + panel controls appear at once, and
        // disarm placement so the next click repositions this light instead of
        // dropping another.
        self.selected_prop = Some(e);
        self.prop_gizmo_drag = None;
        self.light_tool = false;
        self.light_preview_pos = None;
        log::info!("placed light at {pos:?}");
        true
    }

    /// All authored lights in ECS query order, as `(pos, colour, intensity, range)`.
    /// The order is stable for a given world, so the two consumers below
    /// ([`Self::light_draws`] + [`Self::shadow_casters`]) agree on shadow-slot indexing.
    fn raw_lights(&self) -> Vec<(Vec3, [f32; 3], f32, f32)> {
        self.ecs
            .world()
            .query::<(&Transform, &PointLight)>()
            .iter()
            .map(|(t, l)| (t.pos, l.color.to_array(), l.intensity, l.range))
            .collect()
    }

    /// Indices (into `raw_lights`) of the shadow-casting lights, most influential
    /// (intensity × range) first, capped at [`SHADOW_CAP`]. Position in this list =
    /// the light's shadow-cube slot. Stable sort keeps ties in query order.
    fn caster_indices(raw: &[(Vec3, [f32; 3], f32, f32)]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..raw.len()).collect();
        order.sort_by(|&a, &b| {
            let ia = raw[a].2 * raw[a].3;
            let ib = raw[b].2 * raw[b].3;
            ib.partial_cmp(&ia).unwrap_or(std::cmp::Ordering::Equal)
        });
        order.truncate(SHADOW_CAP);
        order
    }

    /// This frame's active point lights for the renderer: `(world_pos_m, colour_rgb,
    /// intensity, range_m, shadow_index)` per authored light. `shadow_index` is the
    /// light's shadow-cube slot (0..[`SHADOW_CAP`]) if it's one of the most-influential
    /// casters, else -1 (lit but no shadow). Matches [`Self::shadow_casters`] ordering.
    pub fn light_draws(&self) -> Vec<(Vec3, [f32; 3], f32, f32, i32)> {
        let raw = self.raw_lights();
        let casters = Self::caster_indices(&raw);
        let mut slot = vec![-1i32; raw.len()];
        for (s, &ri) in casters.iter().enumerate() {
            slot[ri] = s as i32;
        }
        raw.iter()
            .enumerate()
            .map(|(i, &(p, c, intensity, range))| (p, c, intensity, range, slot[i]))
            .collect()
    }

    /// The shadow casters this frame as `(world_pos_m, range_m)`, in shadow-cube-slot
    /// order (position = slot index), matching the indices in [`Self::light_draws`].
    pub fn shadow_casters(&self) -> Vec<(Vec3, f32)> {
        let raw = self.raw_lights();
        Self::caster_indices(&raw)
            .into_iter()
            .map(|ri| (raw[ri].0, raw[ri].3))
            .collect()
    }

    /// Whether the level has any authored point light (drives the game-mode
    /// real-vs-flat fallback: real lighting only when at least one light exists).
    pub fn has_lights(&self) -> bool {
        self.ecs.world().query::<&PointLight>().iter().next().is_some()
    }

    /// The level-wide ambient fill (colour + strength).
    pub fn ambient(&self) -> AmbientSettings {
        self.ambient
    }

    /// Set the level-wide ambient fill (panel edit).
    pub fn set_ambient(&mut self, ambient: AmbientSettings) {
        self.ambient = ambient;
    }

    /// The selected entity's light params `(colour_rgb, intensity, range)`, if the
    /// current object selection is a point light. Drives the left-panel editor +
    /// tells the shared gizmo to stay translate-only. `None` for a prop or nothing.
    pub fn selected_light(&self) -> Option<([f32; 3], f32, f32)> {
        let e = self.selected_prop?;
        let l = self.ecs.world().entity(e).ok()?.get::<&PointLight>().map(|r| *r)?;
        Some((l.color.to_array(), l.intensity, l.range))
    }

    /// Whether an authored entity is a point light (has a [`PointLight`]) — the
    /// prop gizmo uses this to force translate-only + a synthetic pick box.
    pub(crate) fn entity_is_light(&self, e: hecs::Entity) -> bool {
        self.ecs.world().entity(e).map(|r| r.has::<PointLight>()).unwrap_or(false)
    }

    /// Overwrite the selected light's params (panel edit). No-op if the selection
    /// isn't a light. The caller records undo on the drag as one gesture.
    pub fn set_selected_light(&mut self, color: [f32; 3], intensity: f32, range: f32) {
        let Some(e) = self.selected_prop else {
            return;
        };
        if let Ok(l) = self
            .ecs
            .world_mut()
            .query_one_mut::<&mut PointLight>(e)
        {
            l.color = Vec3::from(color);
            l.intensity = intensity;
            l.range = range;
        }
    }

    /// Append a small colour-tinted marker cube per authored light to the shared
    /// gizmo overlay (build-mode visibility + a selection target). The selected
    /// light's marker is enlarged + brightened. Called by `gizmo_mesh`.
    pub(crate) fn push_light_markers(
        &self,
        v: &mut Vec<engine::render::mesh::ColorVertex>,
        idx: &mut Vec<u32>,
    ) {
        let selected = self.selected_prop;
        for (e, t, l) in self
            .ecs
            .world()
            .query::<(hecs::Entity, &Transform, &PointLight)>()
            .iter()
        {
            let is_sel = Some(e) == selected;
            let h = if is_sel { LIGHT_MARKER_HALF * 1.35 } else { LIGHT_MARKER_HALF };
            let c = l.color.to_array();
            // Lift the marker off the light's colour so even a dim/black light stays
            // visible; the selected one is pushed brighter still.
            let lift = if is_sel { 0.35 } else { 0.18 };
            let gain = if is_sel { 1.6 } else { 1.0 };
            let col = [
                (c[0] * gain + lift).min(1.0),
                (c[1] * gain + lift).min(1.0),
                (c[2] * gain + lift).min(1.0),
            ];
            push_colored_box(v, idx, t.pos - Vec3::splat(h), t.pos + Vec3::splat(h), col);
        }
    }
}

/// A marker cube CpuMesh centred at metric `pos` (the placement ghost), built in
/// WT for [`boxes_mesh`] (which scales WT → metres), matching the prop ghost path.
fn light_marker_box(pos: Vec3) -> CpuMesh {
    let s = WORLD_SCALE;
    let h = LIGHT_MARKER_HALF;
    boxes_mesh(&[[
        (pos.x - h) / s,
        (pos.y - h) / s,
        (pos.z - h) / s,
        (2.0 * h) / s,
        (2.0 * h) / s,
        (2.0 * h) / s,
    ]])
}
