//! Immediate-mode debug lines, rebuilt every frame.
//!
//! The engine has no line pipeline — every pass is a triangle list — so a "line"
//! here is a thin quad turned to face the camera, pushed into a [`ColoredMesh`]
//! that the renderer draws unlit. Everything is in **metres** (render space).

use glam::Vec3;

use engine::render::mesh::{ColorVertex, ColoredMesh};

pub type Rgb = [f32; 3];

pub const WHITE: Rgb = [1.0, 1.0, 1.0];
pub const GREY: Rgb = [0.45, 0.45, 0.5];
pub const RED: Rgb = [1.0, 0.25, 0.2];
pub const GREEN: Rgb = [0.3, 1.0, 0.35];
pub const BLUE: Rgb = [0.3, 0.6, 1.0];
pub const CYAN: Rgb = [0.2, 0.95, 0.95];
pub const YELLOW: Rgb = [1.0, 0.9, 0.2];
pub const ORANGE: Rgb = [1.0, 0.55, 0.1];
pub const MAGENTA: Rgb = [1.0, 0.3, 0.9];

pub struct DebugDraw {
    pub mesh: ColoredMesh,
    eye: Vec3,
}

impl DebugDraw {
    pub fn new(eye: Vec3) -> Self {
        DebugDraw { mesh: ColoredMesh::default(), eye }
    }

    pub fn is_empty(&self) -> bool {
        self.mesh.indices.is_empty()
    }

    /// A segment `width` metres thick, facing the camera.
    pub fn line(&mut self, a: Vec3, b: Vec3, color: Rgb, width: f32) {
        let d = b - a;
        if d.length_squared() < 1e-10 {
            return;
        }
        let mid = (a + b) * 0.5;
        let mut side = d.cross(self.eye - mid);
        if side.length_squared() < 1e-10 {
            side = d.any_orthonormal_vector();
        }
        let side = side.normalize() * (width * 0.5);
        let base = self.mesh.vertices.len() as u32;
        for p in [a - side, a + side, b + side, b - side] {
            self.mesh.vertices.push(ColorVertex { pos: p.into(), color });
        }
        self.mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// A line with a small V head at `b`.
    pub fn arrow(&mut self, a: Vec3, b: Vec3, color: Rgb, width: f32) {
        self.line(a, b, color, width);
        let d = b - a;
        let len = d.length();
        if len < 1e-4 {
            return;
        }
        let dir = d / len;
        let head = (len * 0.25).min(0.25);
        let side = dir.cross(Vec3::Y).normalize_or_zero() * head * 0.5;
        self.line(b, b - dir * head + side, color, width);
        self.line(b, b - dir * head - side, color, width);
    }

    /// A horizontal circle.
    pub fn circle(&mut self, centre: Vec3, radius: f32, color: Rgb, width: f32) {
        self.arc(centre, 0.0, std::f32::consts::PI, radius, color, width, false);
    }

    /// A horizontal arc of `half_angle` either side of `yaw`, using PD's angle
    /// convention (0 = +Z, increasing towards +X). `spokes` draws the two radii,
    /// which turns the arc into a cone outline.
    pub fn arc(
        &mut self,
        centre: Vec3,
        yaw: f32,
        half_angle: f32,
        radius: f32,
        color: Rgb,
        width: f32,
        spokes: bool,
    ) {
        let segs = ((half_angle * 2.0 * radius.max(0.5) * 6.0) as usize).clamp(8, 96);
        let at = |a: f32| centre + Vec3::new(a.sin(), 0.0, a.cos()) * radius;
        let start = yaw - half_angle;
        let step = half_angle * 2.0 / segs as f32;
        let mut prev = at(start);
        for i in 1..=segs {
            let p = at(start + step * i as f32);
            self.line(prev, p, color, width);
            prev = p;
        }
        if spokes {
            self.line(centre, at(yaw - half_angle), color, width);
            self.line(centre, at(yaw + half_angle), color, width);
        }
    }

    /// A small 3-axis cross, for points.
    pub fn cross(&mut self, p: Vec3, size: f32, color: Rgb, width: f32) {
        self.line(p - Vec3::X * size, p + Vec3::X * size, color, width);
        self.line(p - Vec3::Y * size, p + Vec3::Y * size, color, width);
        self.line(p - Vec3::Z * size, p + Vec3::Z * size, color, width);
    }
}
