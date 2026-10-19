//! Particle quads from Cinnabar's camera-facing instances (`ParticleGpuFrame`), expanded as
//! `particles.wesl` does: corners `centre ± axis_x ± axis_y`, texture coordinates into the
//! particle atlas with `v` running down the quad, the instance's gamma tint, and its packed
//! block/sky light (full-bright when the particle is unlit).

use super::vertices::{FULL_BRIGHT, Mesh, Vertex};
use bevy::math::{Vec3, Vec4Swizzles};
use particles::ParticleInstance;

/// The corners of one quad in winding order.
const CORNERS: [[f32; 2]; 4] = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];

/// Java's raw lightmap coordinates of an instance: `center_light.w` packs block light in its
/// low nibble and sky light in the next, read when `axis_y.w` marks the particle lit.
pub(super) fn instance_light(instance: &ParticleInstance) -> [f32; 2] {
    if instance.axis_y[3] <= 0.5 {
        return FULL_BRIGHT;
    }
    let packed = instance.center_light[3].max(0.0) as u32;
    [
        (packed & 15) as f32 * 16.0,
        ((packed >> 4) & 15) as f32 * 16.0,
    ]
}

/// Appends `instances` as quads relative to `camera`.
pub(super) fn particle_quads(mesh: &mut Mesh, instances: &[ParticleInstance], camera: Vec3) {
    for instance in instances {
        let centre = bevy::math::Vec4::from_array(instance.center_light).xyz() - camera;
        let x = Vec3::from_slice(&instance.axis_x[..3]);
        let y = Vec3::from_slice(&instance.axis_y[..3]);
        let [u0, v0, du, dv] = instance.uv;
        let light = instance_light(instance);
        let normal = x.cross(y);
        let mid_uv = [u0 + du * 0.5, v0 + dv * 0.5];
        mesh.quad(CORNERS.map(|[cx, cy]| Vertex {
            position: centre + x * cx + y * cy,
            colour: instance.color,
            normal,
            uv: [u0 + (cx * 0.5 + 0.5) * du, v0 + (0.5 - cy * 0.5) * dv],
            mid_uv,
            light,
        }));
    }
}
