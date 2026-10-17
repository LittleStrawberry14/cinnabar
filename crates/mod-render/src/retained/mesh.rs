//! Camera-independent line expansion; the host shader resolves width and near-plane clipping.
use super::*;
use bytemuck::{Pod, Zeroable};

/// Every field is a complete WGSL vec4; layout is checked against the host shader in render.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GpuVertex {
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub c: [f32; 4],
    pub color_a: [f32; 4],
    pub color_b: [f32; 4],
    pub uv: [f32; 4],
    pub data: [f32; 4],
    pub shape: [f32; 4],
}
/// Fixed host tessellation for a round join or semicircular end cap.
pub const ROUND_SEGMENTS: usize = 8;

/// Bounds every selectable line style before allocating GPU geometry.
pub fn expanded_vertices(geometry: &Geometry) -> usize {
    if !geometry.polyline {
        return geometry.indices.len();
    }
    let segments = geometry.vertices.len() - usize::from(!geometry.closed);
    segments * 6
        + segments * ROUND_SEGMENTS * 3
        + if geometry.closed {
            0
        } else {
            ROUND_SEGMENTS * 6
        }
}

/// Builds a single immutable GPU mesh shared by all placements and line styles.
pub fn build(geometry: &Geometry) -> Vec<GpuVertex> {
    let mut output = Vec::with_capacity(expanded_vertices(geometry));
    if !geometry.polyline {
        for &index in &geometry.indices {
            let vertex = geometry.vertices[index as usize];
            output.push(GpuVertex {
                a: point(vertex.position, 0.),
                color_a: vertex.color,
                uv: [vertex.uv[0], vertex.uv[1], 0., 0.],
                data: vertex.data,
                ..Default::default()
            });
        }
        return output;
    }
    let vertices = &geometry.vertices;
    let segments = vertices.len() - usize::from(!geometry.closed);
    let mut distance = 0.;
    for index in 0..segments {
        let next = (index + 1) % vertices.len();
        let a = vertices[index];
        let b = vertices[next];
        let length = a
            .position
            .iter()
            .zip(b.position)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        if length <= f32::EPSILON {
            continue;
        }
        let base = GpuVertex {
            a: point(a.position, distance),
            b: point(b.position, distance + length),
            color_a: a.color,
            color_b: b.color,
            uv: [a.uv[0], a.uv[1], b.uv[0], b.uv[1]],
            data: a.data,
            ..Default::default()
        };
        for (t, side) in [
            (0., -1.),
            (1., -1.),
            (1., 1.),
            (0., -1.),
            (1., 1.),
            (0., 1.),
        ] {
            output.push(GpuVertex {
                shape: [1., t, side, 0.],
                ..base
            });
        }
        // Full fans are immutable. The instance selects bevel, bounded miter or round joins.
        if geometry.closed || next + 1 < vertices.len() {
            let c = vertices[(next + 1) % vertices.len()];
            let joint = GpuVertex {
                c: point(c.position, 0.),
                ..base
            };
            fan(&mut output, joint, 3., 1.);
        }
        if !geometry.closed && index == 0 {
            fan(&mut output, base, 2., 0.);
        }
        if !geometry.closed && next + 1 == vertices.len() {
            fan(&mut output, base, 2., 1.);
        }
        distance += length;
    }
    output
}

/// Emits a bounded triangle fan; negative radial phase names the fan's center.
fn fan(output: &mut Vec<GpuVertex>, base: GpuVertex, kind: f32, endpoint: f32) {
    for index in 0..ROUND_SEGMENTS {
        for phase in [
            -1.,
            index as f32 / ROUND_SEGMENTS as f32,
            (index + 1) as f32 / ROUND_SEGMENTS as f32,
        ] {
            output.push(GpuVertex {
                shape: [kind, endpoint, phase, 0.],
                ..base
            });
        }
    }
}
/// Packs position and cumulative line distance into the shared shader layout.
fn point(position: [f32; 3], distance: f32) -> [f32; 4] {
    [position[0], position[1], position[2], distance]
}
