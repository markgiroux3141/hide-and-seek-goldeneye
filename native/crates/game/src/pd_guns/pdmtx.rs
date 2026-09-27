//! Perfect Dark's matrix helpers (`lib/mtx.c`, `lib/mtx_c.c`) on glam's `Mat4`.
//!
//! PD's `Mtxf` is `f32 m[4][4]` with `m[3]` the translation column — the same
//! column-major layout as glam, so `m[i]` is `col(i)`. `mtx00015be4(a, b, dst)` and
//! `mtx4_mult_mtx4(a, b, dst)` are both `dst = a · b` (checked against the C: the
//! former just forces the bottom row to (0,0,0,1)). The anonymous `mtx000xxxxx`
//! scale helpers are named here by what they do, with the address kept in the doc.

use glam::{Mat4, Vec3, Vec4};

/// `mtx4_load_rotation` (`mtx.c:187`): `Rz(z) · Ry(y) · Rx(x)`.
pub fn load_rotation(rot: Vec3) -> Mat4 {
    let (xs, xc) = rot.x.sin_cos();
    let (ys, yc) = rot.y.sin_cos();
    let (zs, zc) = rot.z.sin_cos();
    let a = xs * zs;
    let b = xc * zs;
    let c = xs * zc;
    let d = xc * zc;
    Mat4::from_cols(
        Vec4::new(yc * zc, yc * zs, -ys, 0.0),
        Vec4::new(c * ys - xc * zs, a * ys + xc * zc, xs * yc, 0.0),
        Vec4::new(d * ys + xs * zs, b * ys - xs * zc, xc * yc, 0.0),
        Vec4::W,
    )
}

/// `mtx4_load_rotation_and_translation`.
pub fn load_rotation_translation(pos: Vec3, rot: Vec3) -> Mat4 {
    let mut m = load_rotation(rot);
    m.w_axis = pos.extend(1.0);
    m
}

/// `mtx4_load_x_rotation` — identical to glam's.
pub fn load_x_rotation(a: f32) -> Mat4 {
    Mat4::from_rotation_x(a)
}

pub fn load_y_rotation(a: f32) -> Mat4 {
    Mat4::from_rotation_y(a)
}

pub fn load_z_rotation(a: f32) -> Mat4 {
    Mat4::from_rotation_z(a)
}

/// `mtx4_set_translation`.
pub fn set_translation(m: &mut Mat4, pos: Vec3) {
    m.w_axis = Vec4::new(pos.x, pos.y, pos.z, m.w_axis.w);
}

/// `mtx00015be4(a, b, dst)`: `dst = a · b` with the affine bottom row.
pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = *a * *b;
    r.x_axis.w = 0.0;
    r.y_axis.w = 0.0;
    r.z_axis.w = 0.0;
    r.w_axis.w = 1.0;
    r
}

/// `mtx00015f04(mult, m)`: scale the three basis columns (incl. their w).
pub fn scale3(m: &mut Mat4, s: f32) {
    m.x_axis *= s;
    m.y_axis *= s;
    m.z_axis *= s;
}

/// `mtx00015e24(mult, m)`: scale column 0's xyz (the DUALFLIP mirror).
pub fn scale_col0_xyz(m: &mut Mat4, s: f32) {
    m.x_axis.x *= s;
    m.x_axis.y *= s;
    m.x_axis.z *= s;
}

/// `mtx00015df0`: column 0 incl. w.
pub fn scale_col0(m: &mut Mat4, s: f32) {
    m.x_axis *= s;
}

/// `mtx00015e4c`: column 1 incl. w.
pub fn scale_col1(m: &mut Mat4, s: f32) {
    m.y_axis *= s;
}

/// `mtx00015ea8`: column 2 incl. w.
pub fn scale_col2(m: &mut Mat4, s: f32) {
    m.z_axis *= s;
}

/// `mtx00015edc`: column 2 xyz.
pub fn scale_col2_xyz(m: &mut Mat4, s: f32) {
    m.z_axis.x *= s;
    m.z_axis.y *= s;
    m.z_axis.z *= s;
}

/// `mtx00016710(mult, m)`: scale ROW 2 (every column's z), i.e. `diag(1,1,s,1) · m`.
pub fn scale_row2(m: &mut Mat4, s: f32) {
    m.x_axis.z *= s;
    m.y_axis.z *= s;
    m.z_axis.z *= s;
    m.w_axis.z *= s;
}

/// `mtx4_transform_vec`.
pub fn transform(m: &Mat4, v: Vec3) -> Vec3 {
    m.transform_point3(v)
}

/// `mtx4_rotate_vec`.
pub fn rotate(m: &Mat4, v: Vec3) -> Vec3 {
    m.transform_vector3(v)
}

/// `mtx00016b58` (`mtx.c:354`): a camera-style basis from a look direction and
/// an up vector, with `pos` as the translation. Column 2 is the NEGATED,
/// normalised look; column 0 is `up × look'`, column 1 re-orthogonalised up.
pub fn look_basis(pos: Vec3, look: Vec3, up: Vec3) -> Mat4 {
    let mut look = look;
    let tmp = -1.0 / look.length();
    look *= tmp;
    let mut a = up.y * look.z - up.z * look.y;
    let mut b = up.z * look.x - up.x * look.z;
    let mut c = up.x * look.y - up.y * look.x;
    let tmp = 1.0 / (a * a + b * b + c * c).sqrt();
    a *= tmp;
    b *= tmp;
    c *= tmp;
    let mut ux = look.y * c - look.z * b;
    let mut uy = look.z * a - look.x * c;
    let mut uz = look.x * b - look.y * a;
    let tmp = 1.0 / (ux * ux + uy * uy + uz * uz).sqrt();
    ux *= tmp;
    uy *= tmp;
    uz *= tmp;
    Mat4::from_cols(
        Vec4::new(a, b, c, 0.0),
        Vec4::new(ux, uy, uz, 0.0),
        Vec4::new(look.x, look.y, look.z, 0.0),
        Vec4::new(pos.x, pos.y, pos.z, 1.0),
    )
}

/// `mtx00016d58`: `look_basis` with the look given as a target point.
pub fn look_at_basis(pos: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    look_basis(pos, target - pos, up)
}

/// `mtx00016874` (`mtx.c:285`): the *inverse* of `look_basis` — a world-to-camera
/// view matrix (rows are the basis, translation is `-basis · pos`).
pub fn view_matrix(pos: Vec3, look: Vec3, up: Vec3) -> Mat4 {
    let basis = look_basis(Vec3::ZERO, look, up);
    let a = basis.x_axis.truncate();
    let u = basis.y_axis.truncate();
    let l = basis.z_axis.truncate();
    Mat4::from_cols(
        Vec4::new(a.x, u.x, l.x, 0.0),
        Vec4::new(a.y, u.y, l.y, 0.0),
        Vec4::new(a.z, u.z, l.z, 0.0),
        Vec4::new(-pos.dot(a), -pos.dot(u), -pos.dot(l), 1.0),
    )
}

/// `guAlignF` (`ultra/gu/align.c`) — `angle` in degrees, as `mtx4_align` passes
/// `RTOD2(angle)`.
pub fn align(angle_deg: f32, x: f32, y: f32, z: f32) -> Mat4 {
    let len = (x * x + y * y + z * z).sqrt();
    let (x, y, z) = if len > 0.0 { (x / len, y / len, z / len) } else { (x, y, z) };
    let a = angle_deg * (3.141_592_6 / 180.0);
    let (s, c) = a.sin_cos();
    let h = (x * x + z * z).sqrt();
    if h == 0.0 {
        return Mat4::IDENTITY;
    }
    let hinv = 1.0 / h;
    Mat4::from_cols(
        Vec4::new((-z * c - s * y * x) * hinv, s * h, (c * x - s * y * z) * hinv, 0.0),
        Vec4::new((z * s - c * y * x) * hinv, c * h, (-s * x - c * y * z) * hinv, 0.0),
        Vec4::new(-x, -y, -z, 0.0),
        Vec4::W,
    )
}

/// `mtx4_align(m, angle, x, y, z)` — radians in, via `RTOD2`.
pub fn mtx4_align(angle_rad: f32, x: f32, y: f32, z: f32) -> Mat4 {
    align(angle_rad * (180.0 / std::f32::consts::PI), x, y, z)
}

/// `mtx00016e98` (`mtx.c:444`): a basis whose −z is the normalised `(x,y,z)`,
/// rolled by `angle` — used by the muzzle-flare billboards.
pub fn mtx00016e98(angle: f32, x: f32, y: f32, z: f32) -> Mat4 {
    let len = (x * x + y * y + z * z).sqrt();
    let (x, y, z) = if len > 0.0 { (x / len, y / len, z / len) } else { (x, y, z) };
    let (sine, cosine) = angle.sin_cos();
    let norm = (x * x + z * z).sqrt();
    if norm == 0.0 {
        return Mat4::IDENTITY;
    }
    let cos_x = x * cosine;
    let sin_x = x * sine;
    let cos_z = z * cosine;
    let sin_z = z * sine;
    let invnorm = 1.0 / norm;
    Mat4::from_cols(
        Vec4::new((-cos_z - y * sin_x) * invnorm, (sin_z - y * cos_x) * invnorm, -x, 0.0),
        Vec4::new(sine * norm, cosine * norm, -y, 0.0),
        Vec4::new((cos_x - y * sin_z) * invnorm, (-sin_x - y * cos_z) * invnorm, -z, 0.0),
        Vec4::W,
    )
}

/// `model_tween_rot_axis` (`model.c:597`): shortest-way tween of one PD euler angle.
pub fn tween_rot_axis(curangle: f32, goalangle: f32, mult: f32) -> f32 {
    let full = crate::pd_spike::pdmath::baddtor(360.0);
    let mut cur = curangle;
    let mut diff = goalangle - curangle;
    if goalangle < curangle {
        diff += full;
    }
    if diff < std::f32::consts::PI {
        cur += diff * mult;
        if cur >= full {
            cur -= full;
        }
    } else {
        cur -= (full - diff) * mult;
        if cur < 0.0 {
            cur += full;
        }
    }
    cur
}

/// `model_tween_rot`.
pub fn tween_rot(cur: Vec3, goal: Vec3, mult: f32) -> Vec3 {
    Vec3::new(
        tween_rot_axis(cur.x, goal.x, mult),
        tween_rot_axis(cur.y, goal.y, mult),
        tween_rot_axis(cur.z, goal.z, mult),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_rotation_is_z_y_x() {
        let r = Vec3::new(0.3, -0.7, 1.1);
        let m = load_rotation(r);
        let g = Mat4::from_euler(glam::EulerRot::ZYX, r.z, r.y, r.x);
        assert!(m.abs_diff_eq(g, 1e-5), "{m:?}\n{g:?}");
    }

    #[test]
    fn view_matrix_inverts_look_basis() {
        let pos = Vec3::new(10.0, 2.0, -5.0);
        let look = Vec3::new(0.3, -0.2, 1.0);
        let up = Vec3::Y;
        let b = look_basis(pos, look, up);
        let v = view_matrix(pos, look, up);
        assert!((v * b).abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }

    #[test]
    fn tween_takes_the_short_way_round() {
        let full = crate::pd_spike::pdmath::baddtor(360.0);
        let a = tween_rot_axis(full - 0.1, 0.1, 0.5);
        assert!(a < 0.01 || a > full - 0.01, "{a}");
    }
}
