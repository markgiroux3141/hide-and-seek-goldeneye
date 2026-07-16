//! HUNT-phase runtime on `World`: breakable-door build/breach and the
//! per-frame enemy/door render meshes.

use super::*;

impl World {
    /// A box mesh for the hunter at its current position (meters), for the
    /// renderer's entity pass. `None` when no hunter is active.
    pub fn enemy_mesh(&self) -> Option<CpuMesh> {
        let e = self.enemy.as_ref()?;
        // Capsule-sized box: 0.5 × 1.5 × 0.5 m, centered above the feet.
        let c = e.pos + Vec3::new(0.0, 0.75, 0.0);
        let polys = csg::box_polygons([c.x, c.y, c.z], [0.25, 0.75, 0.25]);
        let (p, n, i) = csg::polygons_to_mesh(&polys);
        Some(CpuMesh::from_csg(&p, &n, &i))
    }

    /// Arm breakable doors for the hunt (JS `door.js` `buildDoors`): scan every
    /// region for `door`-marked brushes, and for each add a panel collider (blocks
    /// the player) + a `World`-side [`Door`] record, then hand the doorframe AABBs
    /// to the nav overlay (index-aligned). Called once at G→HUNT.
    pub(crate) fn build_doors(&mut self, nav: &mut NavWorld) {
        let door_brushes: Vec<Brush> = self
            .regions
            .iter()
            .flat_map(|r| r.brushes.iter().copied().filter(|b| b.door))
            .collect();

        self.doors.clear();
        for b in &door_brushes {
            let min = Vec3::new(b.x, b.y, b.z) * WORLD_SCALE;
            let max = Vec3::new(b.x + b.w, b.y + b.h, b.z + b.d) * WORLD_SCALE;
            let panel = self.physics.add_door_collider(min, max);
            self.doors.push(Door {
                aabb: *b,
                hp: DOOR_HP,
                broken: false,
                panel,
            });
        }

        nav.set_doors(&door_brushes);
        if !door_brushes.is_empty() {
            log::info!("{} door(s) armed for the hunt", door_brushes.len());
        }
    }

    /// Drain a breaching door's hp; on break, remove its panel collider and flip
    /// the live nav flag. **This is the thesis in code:** a built element is
    /// destroyed and both collision and nav react instantly — one collider gone,
    /// one bool flipped — with **no re-voxelization and no CSG re-eval**.
    pub(crate) fn breach_tick(&mut self, di: usize, dt: f32) {
        let broke = {
            let Some(door) = self.doors.get_mut(di) else { return };
            if door.broken {
                return;
            }
            door.hp -= dt;
            if door.hp <= 0.0 {
                door.broken = true;
                Some(door.panel)
            } else {
                None
            }
        };
        if let Some(panel) = broke {
            self.physics.remove_door_collider(panel);
            if let Some(nav) = self.nav.as_mut() {
                nav.break_door(di);
            }
            log::info!(
                "DOOR {di} BREACHED — panel collider removed + nav flag flipped, no re-bake"
            );
        }
    }

    /// A combined mesh of every intact door panel (meters), for the renderer's
    /// door pass. `None` when no intact doors remain — so a breached door simply
    /// vanishes. Cheap to regenerate (a handful of boxes).
    pub fn door_mesh(&self) -> Option<CpuMesh> {
        let mut positions: Vec<f32> = Vec::new();
        let mut normals: Vec<f32> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for door in self.doors.iter().filter(|d| !d.broken) {
            let b = &door.aabb;
            let c = [
                (b.x + b.w * 0.5) * WORLD_SCALE,
                (b.y + b.h * 0.5) * WORLD_SCALE,
                (b.z + b.d * 0.5) * WORLD_SCALE,
            ];
            let half = [
                b.w * 0.5 * WORLD_SCALE,
                b.h * 0.5 * WORLD_SCALE,
                b.d * 0.5 * WORLD_SCALE,
            ];
            let polys = csg::box_polygons(c, half);
            let (p, n, i) = csg::polygons_to_mesh(&polys);
            let base = (positions.len() / 3) as u32;
            positions.extend_from_slice(&p);
            normals.extend_from_slice(&n);
            indices.extend(i.iter().map(|idx| idx + base));
        }
        if indices.is_empty() {
            return None;
        }
        Some(CpuMesh::from_csg(&positions, &normals, &indices))
    }
}
