//! 3D Projection, Camera Rotation, and Depth Mathematics for Asynchronous Mouse Warp.

use std::f32::consts::PI;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
}

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3 {
    pub m: [[f32; 3]; 3],
}

impl Default for Mat3 {
    fn default() -> Self {
        Self::identity()
    }
}

impl Mat3 {
    pub const fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
        }
    }

    /// Constructs a 3D rotation matrix from camera Euler angles (yaw, pitch in radians).
    /// Yaw rotates around the Y-axis, pitch around the X-axis.
    pub fn from_yaw_pitch(yaw: f32, pitch: f32) -> Self {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();

        // R = R_yaw * R_pitch
        // R_yaw:   [[cy, 0, sy], [0, 1, 0], [-sy, 0, cy]]
        // R_pitch: [[1, 0, 0], [0, cp, -sp], [0, sp, cp]]
        Self {
            m: [
                [cy, sy * sp, sy * cp],
                [0.0, cp, -sp],
                [-sy, cy * sp, cy * cp],
            ],
        }
    }

    #[inline]
    pub fn transform_vec3(&self, v: Vec3) -> Vec3 {
        Vec3 {
            x: self.m[0][0] * v.x + self.m[0][1] * v.y + self.m[0][2] * v.z,
            y: self.m[1][0] * v.x + self.m[1][1] * v.y + self.m[1][2] * v.z,
            z: self.m[2][0] * v.x + self.m[2][1] * v.y + self.m[2][2] * v.z,
        }
    }

    /// Transpose is the exact inverse for orthogonal rotation matrices.
    pub fn inverse(&self) -> Self {
        Self {
            m: [
                [self.m[0][0], self.m[1][0], self.m[2][0]],
                [self.m[0][1], self.m[1][1], self.m[2][1]],
                [self.m[0][2], self.m[1][2], self.m[2][2]],
            ],
        }
    }
}

/// Linearizes raw depth from hardware depth buffer.
///
/// * `z_raw` - raw depth value [0.0..1.0] from depth buffer.
/// * `near`  - near clipping plane (typically 0.1 m).
/// * `far`   - far clipping plane (typically 1000.0 m).
/// * `is_reverse_z` - true for modern reverse-Z buffers (near=1.0, far=0.0).
#[inline]
pub fn linearize_depth(z_raw: f32, near: f32, far: f32, is_reverse_z: bool) -> f32 {
    let z = z_raw.clamp(0.0, 1.0);
    if is_reverse_z {
        if z < 1e-6 {
            far
        } else {
            near / z
        }
    } else {
        let denom = far - z * (far - near);
        if denom.abs() < 1e-6 {
            far
        } else {
            (near * far) / denom
        }
    }
}

/// Unprojects 2D screen UV coordinates + linear depth into 3D camera-space position.
///
/// * `u, v` - screen coordinates in [0.0..1.0].
/// * `z_linear` - linear depth in meters.
/// * `fov_rad` - horizontal FOV in radians.
/// * `aspect` - width / height aspect ratio.
#[inline]
pub fn unproject_uv(u: f32, v: f32, z_linear: f32, fov_rad: f32, aspect: f32) -> Vec3 {
    let half_fov_x = fov_rad * 0.5;
    let tan_half_x = half_fov_x.tan();
    let tan_half_y = tan_half_x / aspect.max(0.001);

    // Screen NDC: u in [0..1] -> [-1..1], v in [0..1] -> [1..-1]
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;

    Vec3 {
        x: ndc_x * tan_half_x * z_linear,
        y: ndc_y * tan_half_y * z_linear,
        z: z_linear,
    }
}

/// Reprojects a 3D camera-space point back onto 2D screen UV coordinates.
/// Returns None if the point is behind the camera ($Z \le 0$).
#[inline]
pub fn project_view_point(p: Vec3, fov_rad: f32, aspect: f32) -> Option<Vec2> {
    if p.z <= 1e-4 {
        return None;
    }

    let half_fov_x = fov_rad * 0.5;
    let tan_half_x = half_fov_x.tan();
    let tan_half_y = tan_half_x / aspect.max(0.001);

    let ndc_x = p.x / (p.z * tan_half_x);
    let ndc_y = p.y / (p.z * tan_half_y);

    let u = ndc_x * 0.5 + 0.5;
    let v = 0.5 - ndc_y * 0.5;

    Some(Vec2::new(u, v))
}

/// Converts degrees to radians.
#[inline]
pub fn deg_to_rad(deg: f32) -> f32 {
    deg * (PI / 180.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_matrix_roundtrip() {
        let ident = Mat3::identity();
        let p = Vec3::new(1.0, 2.0, 3.0);
        let rotated = ident.transform_vec3(p);
        assert_eq!(p, rotated);
    }

    #[test]
    fn test_unproject_project_roundtrip() {
        let fov = deg_to_rad(90.0);
        let aspect = 16.0 / 9.0;
        let u_orig = 0.75;
        let v_orig = 0.35;
        let z_orig = 10.0;

        let p_view = unproject_uv(u_orig, v_orig, z_orig, fov, aspect);
        assert!((p_view.z - z_orig).abs() < 1e-4);

        let uv_reproj = project_view_point(p_view, fov, aspect).expect("Must project in front");
        assert!((uv_reproj.x - u_orig).abs() < 1e-4);
        assert!((uv_reproj.y - v_orig).abs() < 1e-4);
    }

    #[test]
    fn test_rotation_matrix_inverse() {
        let rot = Mat3::from_yaw_pitch(0.15, -0.08);
        let inv = rot.inverse();
        let p = Vec3::new(5.0, -3.0, 12.0);
        let transformed = rot.transform_vec3(p);
        let restored = inv.transform_vec3(transformed);
        assert!((restored.x - p.x).abs() < 1e-4);
        assert!((restored.y - p.y).abs() < 1e-4);
        assert!((restored.z - p.z).abs() < 1e-4);
    }

    #[test]
    fn test_depth_linearization() {
        let near = 0.1f32;
        let far = 100.0f32;
        let z_lin_near = linearize_depth(0.0, near, far, false);
        let z_lin_far = linearize_depth(1.0, near, far, false);
        assert!((z_lin_near - near).abs() < 1e-2);
        assert!((z_lin_far - far).abs() < 1e-2);

        // Reverse-Z
        let z_rev_near = linearize_depth(1.0, near, far, true);
        let z_rev_far = linearize_depth(0.0, near, far, true);
        assert!((z_rev_near - near).abs() < 1e-2);
        assert!((z_rev_far - far).abs() < 1e-2);
    }
}
