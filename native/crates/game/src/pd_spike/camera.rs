//! Orbit camera for watching the arena: right-drag orbits, middle-drag (or
//! shift + right-drag) pans, wheel zooms, WASD slides the focus across the floor.
//! Metres, render space.

use glam::{Mat4, Vec2, Vec3, Vec4};

pub struct OrbitCam {
    /// The point orbited around.
    pub target: Vec3,
    /// Heading of the camera around the target (radians, PD convention: 0 = the
    /// camera sits on -Z looking towards +Z).
    pub yaw: f32,
    /// Elevation above the horizon (radians).
    pub pitch: f32,
    pub dist: f32,
    pub fov_y: f32,
}

impl Default for OrbitCam {
    fn default() -> Self {
        OrbitCam { target: Vec3::new(0.0, 0.5, 0.0), yaw: 0.6, pitch: 0.85, dist: 17.0, fov_y: 55f32.to_radians() }
    }
}

impl OrbitCam {
    pub fn eye(&self) -> Vec3 {
        let back = Vec3::new(-self.yaw.sin() * self.pitch.cos(), self.pitch.sin(), -self.yaw.cos() * self.pitch.cos());
        self.target + back * self.dist
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = Mat4::perspective_rh(self.fov_y, aspect, 0.05, 500.0);
        let view = Mat4::look_at_rh(self.eye(), self.target, Vec3::Y);
        proj * view
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.006;
        self.pitch = (self.pitch + dy * 0.006).clamp(0.05, 1.55);
    }

    pub fn zoom(&mut self, steps: f32) {
        self.dist = (self.dist * 0.88f32.powf(steps)).clamp(1.0, 150.0);
    }

    /// Pan in the camera's screen plane by pixel deltas.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let fwd = (self.target - self.eye()).normalize();
        let right = fwd.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(fwd);
        let k = self.dist * 0.0015;
        self.target += (-right * dx + up * dy) * k;
    }

    /// Slide the focus across the floor plane, relative to where the camera faces.
    pub fn slide(&mut self, forward: f32, strafe: f32, dt: f32) {
        let f = Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos());
        let r = Vec3::new(-f.z, 0.0, f.x);
        let speed = self.dist.max(4.0) * 0.8;
        self.target += (f * forward + r * strafe) * speed * dt;
    }

    /// World-space ray through a pixel (origin, unit direction).
    pub fn ray(&self, px: Vec2, size: Vec2) -> (Vec3, Vec3) {
        let inv = self.view_proj(size.x / size.y.max(1.0)).inverse();
        let ndc = Vec2::new(px.x / size.x * 2.0 - 1.0, 1.0 - px.y / size.y * 2.0);
        let near = inv * Vec4::new(ndc.x, ndc.y, 0.0, 1.0);
        let far = inv * Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
        let near = near.truncate() / near.w;
        let far = far.truncate() / far.w;
        (near, (far - near).normalize())
    }

    /// Pixel position of a world point, or `None` behind the camera.
    pub fn to_screen(&self, p: Vec3, size: Vec2) -> Option<Vec2> {
        let clip = self.view_proj(size.x / size.y.max(1.0)) * p.extend(1.0);
        if clip.w <= 0.01 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some(Vec2::new((ndc.x + 1.0) * 0.5 * size.x, (1.0 - ndc.y) * 0.5 * size.y))
    }
}
