//! Prop placement tool (the object palette): arm a prop for placement, track a
//! floor ghost under the crosshair, and drop it as an authored ECS entity. Mirrors
//! the additive placement tool ([`super::super::World::arm_place`]) but authors an
//! entity instead of a brush. Catalog: [`crate::props`]; entity model: [`crate::ecs`].

use super::super::*;
use crate::ecs::{ComponentData, Destroyed, EntityData, Health, MeshId, Renderable, Transform};

/// How dark a *living* destructible prop tints at zero health, just before it blows —
/// a near (but not fully) black so the darkening reads as "taking damage." Full health
/// = no tint (white); the shade lerps between the two by the health fraction.
const PROP_DARKEN_FLOOR: f32 = 0.25;

/// The shade of a **destroyed** prop's charred husk (GoldenEye leaves the darkened
/// remains in place). Deep near-black so a blown prop reads unmistakably as spent,
/// well below the living damage floor.
const PROP_DESTROYED_SHADE: f32 = 0.05;

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
        self.clear_draw_state();
        self.selected = None;
        self.light_tool = false;
        self.light_preview_pos = None;
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
    ///
    /// A **ceiling-mounted** prop ([`crate::props::ceiling_mounted`]) anchors at its
    /// model origin instead. Deriving its anchor from bounds would be wrong twice
    /// over: the mount is not at the centre of a turret that cantilevers its barrel
    /// forward, and it is the rig — not the bounding box — that knows which point
    /// actually touches the ceiling.
    pub(crate) fn prop_anchor(&self, mesh: MeshId) -> Vec3 {
        if crate::props::ceiling_mounted(mesh) {
            return Vec3::ZERO;
        }
        match self.prop_bounds.get(&mesh) {
            Some((min, max)) => Vec3::new((min.x + max.x) * 0.5, min.y, (min.z + max.z) * 0.5),
            None => Vec3::ZERO,
        }
    }

    /// World model matrix for a placed prop: put its anchor at `pos`, then apply the
    /// authored rotation + scale. Shared by the in-world draw and (future) pick.
    pub(crate) fn prop_model_matrix(&self, mesh: MeshId, pos: Vec3, rot: Quat, scale: Vec3) -> Mat4 {
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
        // Surface pick along the cursor ray, gated to the horizontal face this prop
        // mounts on: the floor (up-facing, `Side::Min`) for everything that stands, the
        // ceiling (down-facing, `Side::Max`) for a prop that hangs.
        let ceiling = crate::props::ceiling_mounted(mesh);
        let want = if ceiling { Side::Max } else { Side::Min };
        let (sel, hit_wt) = self.pick_face_hit_from(origin, dir)?;
        if sel.axis != Axis::Y || sel.side != want {
            self.prop_preview_pos = None;
            return None;
        }
        // Metric contact point (WT → metres) — the floor it rests on, or the ceiling
        // it bolts to.
        let pos = hit_wt * WORLD_SCALE;
        self.prop_preview_pos = Some(pos);
        let s = WORLD_SCALE;

        // A hanging prop's ghost is its assembled bounds taken straight off the mount
        // point, not a box centred under the cursor: its anchor is the model origin,
        // so `min`/`max` are already relative to where it will hang.
        if ceiling {
            let (min, max) = self.prop_bounds.get(&mesh).copied()?;
            let scale = crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0);
            let (lo, hi) = (pos + min * scale, pos + max * scale);
            return Some(boxes_mesh(&[[
                lo.x / s,
                lo.y / s,
                lo.z / s,
                (hi.x - lo.x) / s,
                (hi.y - lo.y) / s,
                (hi.z - lo.z) / s,
            ]]));
        }

        // A door aimed near a doorway snaps into it, fitted to the hole. The ghost
        // shows the fit — one box per leaf — so a double opening visibly previews as
        // two panels before you commit.
        if let Some(way) = self.door_snap_for(mesh, pos) {
            let boxes: Vec<[f32; 6]> = (0..way.leaves)
                .filter_map(|leaf| self.door_leaf_box_wt(mesh, &way, leaf))
                .collect();
            if !boxes.is_empty() {
                return Some(boxes_mesh(&boxes));
            }
        }

        // Ghost footprint = the prop's world AABB at this placement, in WT for
        // `boxes_mesh`, base at the floor and centred on the cursor.
        let (min, max) = self.prop_bounds.get(&mesh).copied()?;
        let scale = crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0);
        let (w, h, d) = (
            (max.x - min.x) * scale,
            (max.y - min.y) * scale,
            (max.z - min.z) * scale,
        );
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

        // A door dropped at a doorway fits itself into the hole — and a double-width
        // opening authors **both leaves** in one click, mirrored so they meet in the
        // middle. Two ordinary door entities rather than one two-panel entity: each
        // leaf then swings on its own hinge and is separately selectable, editable and
        // deletable, with no new component or draw path.
        if let Some(way) = self.door_snap_for(mesh, pos) {
            self.record_undo();
            // A doorway holds exactly one door. Clear whatever is already in it first,
            // so a second click (the tool stays armed, so this is easy to do by
            // accident) **replaces** rather than stacking a second panel in the same
            // hole — and so swapping a door for a different model is just placing it.
            for e in self.doorway_occupants(&way) {
                self.ecs.despawn_authored(e);
                if self.selected_prop == Some(e) {
                    self.selected_prop = None;
                }
            }
            let mut placed = 0;
            for leaf in 0..way.leaves {
                let Some((lpos, lrot, lscale)) = self.door_fit_transform(mesh, &way, leaf) else {
                    continue;
                };
                let mut door = crate::ecs::Door::new(super::door::door_opening_type(mesh));
                // Leaves mirror: the left one hinges left and the right one hinges
                // right (with a reversed swing) so a double opens outward as a pair.
                if way.leaves == 2 && leaf == 1 {
                    door.hinge = crate::ecs::HingeSide::Right;
                    door.flip = true;
                    // Mirror the artwork too, so the pair reads as matched leaves
                    // meeting in the middle rather than two copies of one door.
                    door.mirrored = true;
                }
                let id = self.ecs.alloc_id();
                self.ecs.spawn_authored(&EntityData {
                    id,
                    components: vec![
                        ComponentData::Transform {
                            pos: lpos.to_array(),
                            rot: lrot.to_array(),
                            scale: lscale.to_array(),
                        },
                        ComponentData::Renderable { mesh },
                        ComponentData::door(door.opening_type),
                    ],
                });
                if let Some(e) = self.ecs.resolve(id) {
                    let _ = self.ecs.world_mut().insert_one(e, door);
                }
                placed += 1;
            }
            if placed > 0 {
                log::info!(
                    "fitted {mesh:?} into a {}-leaf doorway at {:?}",
                    way.leaves,
                    way.center
                );
                return true;
            }
        }

        let scale = crate::props::def(mesh).map(|d| d.scale).unwrap_or(1.0);
        self.record_undo();
        let id = self.ecs.alloc_id();
        let mut components = vec![
            ComponentData::Transform {
                pos: pos.to_array(),
                rot: Quat::IDENTITY.to_array(),
                scale: [scale, scale, scale],
            },
            ComponentData::Renderable { mesh },
        ];
        // A pickup carries the panel's draft settings (which weapon, how much ammo,
        // how long until it returns) onto the placed entity. Attached here so every
        // pickup — whatever placed it — gets its component from one funnel.
        if let Some(p) = self.pickup_for_placement(mesh) {
            components.push(ComponentData::Pickup {
                kind: p.kind,
                weapon: p.weapon.to_string(),
                mags: p.mags,
                respawn: p.respawn,
            });
        }
        self.ecs.spawn_authored(&EntityData { id, components });
        log::info!("placed prop {mesh:?} at {pos:?}");
        true
    }

    /// This frame's prop draw list for the renderer: `(catalog key, view_proj·world,
    /// tint)` per placed prop. A destructible prop that has taken hits darkens toward
    /// [`PROP_DARKEN_FLOOR`] by its health fraction (the shoot-feedback); a
    /// [`Destroyed`] prop stays drawn as a charred husk ([`PROP_DESTROYED_SHADE`], the
    /// GoldenEye "darkened remains"). Non-prop `Renderable`s (e.g. a door) are skipped.
    /// Per-prop `(key, model→world, tint)`. The renderer combines each world matrix
    /// with the shared camera clip matrix (and uses the world matrix for lighting), so
    /// this returns the raw world transform — `aspect` is no longer needed but is kept
    /// for call-site symmetry with the other draw-list getters.
    pub fn prop_draws(&self, _aspect: f32) -> Vec<(&'static str, Mat4, [f32; 4])> {
        let mut out = Vec::new();
        for (t, r, hp, destroyed, door, baked, turret, pickup) in self
            .ecs
            .world()
            .query::<(
                &Transform,
                &Renderable,
                Option<&Health>,
                Option<&Destroyed>,
                Option<&crate::ecs::Door>,
                Option<&crate::ecs::DoorGeom>,
                Option<&crate::ecs::Turret>,
                Option<&crate::ecs::Pickup>,
            )>()
            .iter()
        {
            // A collected pickup leaves the floor until it respawns — the whole
            // point of the respawn clock being visible.
            if pickup.is_some_and(|p| p.taken()) {
                continue;
            }
            let Some(def) = crate::props::def(r.mesh) else {
                // A weapon pickup has no catalog row on purpose: it draws from the
                // world-space weapon library instead (`weapon_pickup_draws`), keyed
                // by the gun's name rather than by a prop mesh.
                continue;
            };
            // An articulated prop is several rigid pieces on one entity, so it emits
            // one draw per piece — each the placement matrix with that piece's rig
            // matrix composed in. The draw list is already `(key, matrix, tint)` with
            // no cap on entries per key, so this needs nothing from the renderer.
            //
            // A turret in BUILD has no `Turret` component (it is HUNT-only runtime
            // state), which is exactly right: it draws parked at rest while authoring.
            if r.mesh == MeshId::SentryGun {
                let (yaw, pitch, spin) = turret
                    .map(|g| (g.yaw, g.pitch, g.spin))
                    .unwrap_or((0.0, 0.0, 0.0));
                let place = self.prop_model_matrix(r.mesh, t.pos, t.rot, t.scale);
                for part in &crate::turret::PARTS {
                    out.push((
                        part.key,
                        place * crate::turret::part_matrix(part, yaw, pitch, spin),
                        [1.0, 1.0, 1.0, 1.0],
                    ));
                }
                continue;
            }
            // A door draws through the door matrix — its placement matrix with the
            // mirror and any swing/slide composed in; everything else is a plain prop.
            //
            // The panel geometry is normally baked at BUILD→HUNT, but a door being
            // *authored* has none yet, so it is derived on the spot here. That is what
            // makes hinge and mirror edits visible in the editor: without it the
            // editor drew the plain prop matrix while HUNT drew the door matrix, so a
            // mirrored door looked unchanged until you started the hunt.
            let model = match door {
                Some(d) => {
                    let geom = baked
                        .copied()
                        .or_else(|| self.derive_door_geom(r.mesh, t.scale, d.hinge));
                    match geom {
                        Some(g) => super::door::door_world_matrix(t, &g, d),
                        None => self.prop_model_matrix(r.mesh, t.pos, t.rot, t.scale),
                    }
                }
                None => self.prop_model_matrix(r.mesh, t.pos, t.rot, t.scale),
            };
            let tint = if destroyed.is_some() {
                // Blown — the darkened husk stays in place.
                [PROP_DESTROYED_SHADE, PROP_DESTROYED_SHADE, PROP_DESTROYED_SHADE, 1.0]
            } else {
                match hp {
                    // Full health → white (no-op); as hp drops the shade lerps toward
                    // the near-black floor, so a battered crate darkens before it blows.
                    Some(h) if h.max > 0.0 => {
                        let frac = (h.hp / h.max).clamp(0.0, 1.0);
                        let s = PROP_DARKEN_FLOOR + (1.0 - PROP_DARKEN_FLOOR) * frac;
                        [s, s, s, 1.0]
                    }
                    _ => [1.0, 1.0, 1.0, 1.0],
                }
            };
            out.push((def.key, model, tint));
        }
        out
    }

    /// Bake a static collider for every authored **destructible** prop and attach its
    /// transient [`Health`] (Milestone 3), called at BUILD→HUNT. The collider makes the
    /// prop solid to player shots + movement; the handle→entity map lets a hitscan hit
    /// route damage to the prop. Health is HUNT-only combat state (from the catalog),
    /// stripped again by [`Self::clear_prop_colliders`] so the authored prop returns
    /// intact in BUILD. Furniture (no `destructible`) is left as a pure visual.
    pub(crate) fn spawn_prop_colliders(&mut self) {
        // Collect (entity, health) first so the query borrow is released before we
        // mutate the world (add colliders + insert Health).
        let mut targets: Vec<(hecs::Entity, f32)> = Vec::new();
        for (e, r) in self.ecs.world().query::<(hecs::Entity, &Renderable)>().iter() {
            if let Some(d) = crate::props::def(r.mesh).and_then(|d| d.destructible) {
                targets.push((e, d.health));
            }
        }
        for (e, health) in targets {
            let Some((min, max)) = self.prop_world_aabb(e) else {
                continue; // no registered bounds (headless) — skip
            };
            let handle = self.physics.add_prop_collider(min, max);
            self.prop_colliders.insert(handle, e);
            let _ = self.ecs.world_mut().insert_one(e, Health::full(health));
        }
    }

    /// Solid boxes (WT `[x, y, z, w, h, d]`, the nav voxelizer's units) for every
    /// placed prop, so the HUNT nav bake blocks their footprint and the grid-navving
    /// hunters path **around** them. Enemies move on the nav grid and ignore physics
    /// colliders (see the nav-vs-physics split), so prop collision for enemies lives
    /// here, not in the collider — fed alongside `structure_solid_boxes` into
    /// `nav::bake`. A destroyed husk keeps its footprint (its remains still block,
    /// consistent with its kept collider). Empty when bounds weren't registered
    /// (headless levelgen has no GLBs) → props simply don't affect that nav.
    pub(crate) fn prop_solid_boxes(&self) -> Vec<[f32; 6]> {
        let s = WORLD_SCALE;
        // Snapshot the prop entities first (query borrow) before calling the
        // `&self`-borrowing `prop_world_aabb` per entity.
        let entities: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            // Doors are deliberately excluded: this list is voxelized into the frozen
            // nav grid and can never change afterwards, which is right for a crate and
            // fatal for a door — one baked solid here could never be opened for a
            // hunter. They go to the live `nav::set_doors` overlay instead (see
            // `World::door_solid_boxes`).
            // Pickups are excluded for a different reason than doors: a pickup is
            // something you walk *through* — it has no player collider either — so
            // baking its footprint solid would have hunters detouring around an ammo
            // crate the player walks straight over. (A weapon pickup is excluded
            // anyway, having no catalog row; this is what makes the two crates agree
            // with it rather than behaving like scenery that happens to be lootable.)
            .filter(|(_, r)| {
                crate::props::def(r.mesh).is_some()
                    && crate::props::door_def(r.mesh).is_none()
                    && crate::props::pickup_kind(r.mesh).is_none()
            })
            .map(|(e, _)| e)
            .collect();
        let mut out = Vec::new();
        for e in entities {
            if let Some((min, max)) = self.prop_world_aabb(e) {
                out.push([
                    min.x / s,
                    min.y / s,
                    min.z / s,
                    (max.x - min.x) / s,
                    (max.y - min.y) / s,
                    (max.z - min.z) / s,
                ]);
            }
        }
        out
    }

    /// Tear down all prop colliders + strip the transient HUNT combat state
    /// ([`Health`] + [`Destroyed`]) from every authored prop, called when leaving
    /// HUNT. After this the authored props are back to their pristine, save-clean form
    /// (a crate blown up in HUNT reappears in BUILD).
    pub(crate) fn clear_prop_colliders(&mut self) {
        self.physics.clear_prop_colliders();
        self.prop_colliders.clear();
        let props: Vec<hecs::Entity> = self
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .map(|(e, _)| e)
            .collect();
        for e in props {
            let _ = self.ecs.world_mut().remove_one::<Health>(e);
            let _ = self.ecs.world_mut().remove_one::<Destroyed>(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Author a prop entity at `pos` with a unit-cube model bounds registered, via the
    /// real placement path. Leaves the tool disarmed.
    fn place_prop(world: &mut World, mesh: MeshId, pos: Vec3) {
        world.register_prop_bounds(mesh, Vec3::new(-0.5, 0.0, -0.5), Vec3::new(0.5, 1.0, 0.5));
        world.prop_tool = Some(mesh);
        world.prop_preview_pos = Some(pos);
        assert!(world.confirm_prop_placement(), "prop should place");
        world.cancel_prop_placement();
    }

    /// Count authored entities carrying a given component.
    fn count<T: hecs::Component>(world: &World) -> usize {
        world.ecs.world().query::<&T>().iter().count()
    }

    /// The bake gives a collider + full Health to a destructible prop and nothing to
    /// furniture; leaving HUNT strips that transient state back off.
    #[test]
    fn bake_arms_destructibles_only_and_teardown_restores_them() {
        let mut world = World::new();
        place_prop(&mut world, MeshId::WoodenCrate, Vec3::new(0.0, 0.0, 0.0));
        place_prop(&mut world, MeshId::HeavyWoodenTable, Vec3::new(5.0, 0.0, 0.0));

        world.spawn_prop_colliders();
        // Only the crate is destructible → exactly one collider + one Health.
        assert_eq!(world.prop_colliders.len(), 1, "one prop collider (the crate)");
        assert_eq!(count::<Health>(&world), 1, "only the crate carries Health");

        world.clear_prop_colliders();
        assert!(world.prop_colliders.is_empty(), "colliders torn down");
        assert_eq!(count::<Health>(&world), 0, "transient Health stripped");
        assert_eq!(count::<Destroyed>(&world), 0, "no lingering Destroyed marker");
    }

    /// A damaged (but not dead) crate darkens in the draw list; a destroyed one stays
    /// drawn as a charred husk (GoldenEye leaves the remains in place).
    #[test]
    fn damaged_crate_darkens_and_destroyed_crate_remains_as_husk() {
        let mut world = World::new();
        place_prop(&mut world, MeshId::WoodenCrate, Vec3::new(0.0, 0.0, 0.0));
        world.spawn_prop_colliders();
        let e = *world.prop_colliders.values().next().unwrap();

        // Full health → white.
        let full = world.prop_draws(1.0);
        assert_eq!(full.len(), 1);
        assert_eq!(full[0].2, [1.0, 1.0, 1.0, 1.0], "undamaged crate is untinted");

        // Half health → darkened (but not below the floor).
        world
            .ecs
            .world_mut()
            .query_one_mut::<&mut Health>(e)
            .unwrap()
            .hp = 30.0; // crate max is 60
        let hurt = world.prop_draws(1.0);
        let shade = hurt[0].2[0];
        assert!(shade < 1.0 && shade >= PROP_DARKEN_FLOOR, "darkened, got {shade}");

        // Destroyed → still drawn, but as the charred husk shade.
        world.ecs.world_mut().insert_one(e, Destroyed).unwrap();
        let dead = world.prop_draws(1.0);
        assert_eq!(dead.len(), 1, "destroyed crate remains in the draw list");
        assert_eq!(
            dead[0].2,
            [PROP_DESTROYED_SHADE, PROP_DESTROYED_SHADE, PROP_DESTROYED_SHADE, 1.0],
            "husk uses the destroyed shade"
        );
    }

    /// Deleting the selected prop despawns it (and clears the selection), and undo
    /// brings it back.
    #[test]
    fn delete_removes_selected_prop_and_undo_restores_it() {
        let mut world = World::new();
        place_prop(&mut world, MeshId::WoodenCrate, Vec3::new(0.0, 0.0, 0.0));
        place_prop(&mut world, MeshId::HeavyWoodenTable, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(count::<Renderable>(&world), 2);

        // Select one and delete it.
        let e = world
            .ecs
            .world()
            .query::<(hecs::Entity, &Renderable)>()
            .iter()
            .next()
            .map(|(e, _)| e)
            .unwrap();
        world.selected_prop = Some(e);
        world.delete_selected_prop();
        assert_eq!(count::<Renderable>(&world), 1, "the selected prop is gone");
        assert!(world.selected_prop().is_none(), "selection cleared after delete");

        world.undo();
        assert_eq!(count::<Renderable>(&world), 2, "undo restores the deleted prop");
    }

    /// A placed prop's footprint is baked solid into the nav grid, so grid-navving
    /// hunters treat it as an obstacle and route around it (they ignore the physics
    /// collider entirely).
    #[test]
    fn placed_prop_blocks_the_nav_grid() {
        let mut world = World::new(); // 24×16×24 WT cavity, floor at y=0 (6 m across)
        world.initial_meshes();
        // Drop a crate on the room floor at metres (3, 0, 3) — near the room centre.
        place_prop(&mut world, MeshId::WoodenCrate, Vec3::new(3.0, 0.0, 3.0));

        let props = world.prop_solid_boxes();
        assert!(!props.is_empty(), "the crate produced a solid box for nav");

        let mut regions = std::mem::take(&mut world.regions);
        // Just above the floor at the crate's spot is open air without the prop…
        let open = nav::bake(&mut regions, &[], &[]).expect("bake");
        assert!(
            !open.is_solid_meters(3.0, 0.1, 3.0),
            "the spot is walkable air before the prop blocks it"
        );
        // …and feeding the prop footprint marks that cell solid (enemies path around).
        let blocked = nav::bake(&mut regions, &props, &[]).expect("bake with prop solids");
        assert!(
            blocked.is_solid_meters(3.0, 0.1, 3.0),
            "the crate blocks its nav cell"
        );
        world.regions = regions;
    }
}
