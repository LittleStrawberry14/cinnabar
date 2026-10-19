//! Recording a frame's actor geometry: planning, the skinning dispatches and the draws.

use std::collections::HashMap;
use std::sync::Arc;

use wgpu::{CommandEncoder, Device, Queue, TextureView};

use super::ActorSources;
use super::skin::{self, SKIN_PARAMS_BYTES, SKIN_WORKGROUP_SIZE, SkinJob, SkinParams};
use super::view::{Dispatch, NeutralTextures, Slot, ViewActors};
use crate::shaderpack::draws::DrawKind;
use crate::shaderpack::frame_geometry::open_kind;
use crate::shaderpack::geometry::{
    CONSTANT_VERTEX_BUFFER, GEOMETRY_VERTEX_BUFFER, IRIS_VERTEX_STRIDE,
};
use crate::shaderpack::passes::ViewState;
use crate::shaderpack::targets::{
    AtlasViews, draw_reads, draws_into_geometry_depth, exchanged_by, pass_bind_groups,
};
use crate::shaderpack::uniforms::patch_block;

/// Compute dispatch dimension limit.
pub(super) const MAX_WORKGROUPS: u32 = 65_535;

/// Plans, uploads and skins this frame's actor geometry for `state`'s view, and updates the
/// draws' uniforms and bind groups. Draw uniforms must have been written for this frame.
pub(crate) fn prepare(
    device: &Device,
    queue: &Queue,
    encoder: &mut CommandEncoder,
    state: &mut ViewState,
    sources: &ActorSources<'_>,
) {
    let pack = Arc::clone(&state.pack);
    let ViewState {
        actors,
        targets,
        bindings,
        ..
    } = state;
    actors.begin();
    let Some(kernels) = sources.kernels else {
        return;
    };
    let drawn = |kind: DrawKind| pack.draws.iter().any(|d| d.kind == kind);
    if !DrawKind::ALL
        .into_iter()
        .any(|kind| kind.actor() && drawn(kind))
    {
        return;
    }
    let mut output = 0u32;
    if let Some(source) = sources.actors {
        actors.plan_rig(source, false, sources, &drawn, &mut output);
    }
    if let Some(source) = sources.hand {
        actors.plan_rig(source, true, sources, &drawn, &mut output);
    }
    if let Some(source) = &sources.block_entities {
        actors.plan_block_entities(source, &drawn, &mut output);
    }
    let frame = actors.frame;
    actors.views.retain(|_, (_, used)| *used == frame);
    if output == 0 {
        actors.draws.clear();
        return;
    }
    skin(device, queue, encoder, actors, sources, kernels, output);
    let neutral = actors
        .neutral
        .get_or_insert_with(|| NeutralTextures::new(device, queue));
    let mut next_slot: HashMap<DrawKind, u32> = HashMap::new();
    for draw in &actors.draws {
        let Some(index) = pack.draws.iter().position(|d| d.kind == draw.kind) else {
            actors.slot_of.push(u32::MAX);
            continue;
        };
        let geometry = &pack.draws[index];
        let program = &pack.programs[geometry.program];
        let slot_index = next_slot.entry(draw.kind).or_default();
        let slots = actors.slots.entry(draw.kind).or_default();
        if slots.len() <= *slot_index as usize {
            let size = program
                .reflection
                .uniforms
                .as_ref()
                .map_or(16, |block| u64::from(block.size));
            slots.push(Slot {
                uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("shader pack {} actor uniforms", program.name)),
                    size: size.max(16).next_multiple_of(16),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                texture: None,
                groups: Default::default(),
            });
        }
        let slot = &mut slots[*slot_index as usize];
        actors.slot_of.push(*slot_index);
        *slot_index += 1;
        if let Some(block) = &program.reflection.uniforms {
            actors.scratch.clone_from(&bindings.draws[index].base);
            actors.scratch.resize(block.size as usize, 0);
            patch_block(block, &draw.values.uniforms(), &mut actors.scratch);
            queue.write_buffer(&slot.uniforms, 0, &actors.scratch);
        }
        if slot.texture != Some(draw.texture) {
            let Some((view, _)) = actors.views.get(&draw.texture) else {
                continue;
            };
            let atlas = AtlasViews {
                color: view,
                normals: &neutral.normals,
                specular: &neutral.specular,
            };
            slot.groups = targets.per_parity(&exchanged_by(&pack, geometry.program), |targets| {
                pass_bind_groups(
                    device,
                    &pack,
                    targets,
                    Some(&slot.uniforms),
                    geometry.program,
                    draw_reads(&pack, geometry),
                    Some(atlas),
                    draws_into_geometry_depth(geometry.kind),
                )
            });
            slot.texture = Some(draw.texture);
        }
    }
}

/// Uploads the jobs and records the skinning and block-entity dispatches into the output.
fn skin(
    device: &Device,
    queue: &Queue,
    encoder: &mut CommandEncoder,
    actors: &mut ViewActors,
    sources: &ActorSources<'_>,
    kernels: skin::SkinKernels<'_>,
    output: u32,
) {
    let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
    let output_buffer = actors
        .output
        .ensure(
            device,
            "shader-pack actor vertices",
            u64::from(output) * IRIS_VERTEX_STRIDE,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
        )
        .clone();
    let jobs = actors
        .job_buffer
        .ensure(
            device,
            "shader-pack actor skinning jobs",
            (actors.jobs.len().max(1) * std::mem::size_of::<SkinJob>()) as u64,
            storage,
        )
        .clone();
    queue.write_buffer(&jobs, 0, bytemuck::cast_slice(&actors.jobs));
    let stride =
        u64::from(device.limits().min_uniform_buffer_offset_alignment).max(SKIN_PARAMS_BYTES);
    let params = actors
        .params
        .ensure(
            device,
            "shader-pack actor skinning parameters",
            stride * actors.dispatches.len() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        )
        .clone();
    let camera = sources.camera.extend(0.0).to_array();
    let block_lists = sources
        .block_entities
        .map(|source| [source.solid, source.overlay, source.additive]);
    let mut groups = Vec::with_capacity(actors.dispatches.len());
    for (index, dispatch) in actors.dispatches.iter().enumerate() {
        let (counts, source_group) = match *dispatch {
            Dispatch::Rig {
                hand, first, count, ..
            } => {
                let source = if hand { sources.hand } else { sources.actors };
                let Some(source) = source else {
                    groups.push(None);
                    continue;
                };
                let buffers = source.buffers();
                let entries: Vec<_> = [
                    buffers.instances,
                    buffers.vertices,
                    buffers.spans,
                    buffers.previous_bones,
                    buffers.current_bones,
                ]
                .iter()
                .enumerate()
                .map(|(binding, buffer)| wgpu::BindGroupEntry {
                    binding: binding as u32,
                    resource: buffer.as_entire_binding(),
                })
                .collect();
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shader-pack actor skinning sources"),
                    layout: kernels.rig_layout,
                    entries: &entries,
                });
                ([count, first, 0, 0], group)
            }
            Dispatch::BlockEntities {
                list,
                first_output,
                count,
            } => {
                let Some(list) = block_lists.and_then(|lists| lists[list]) else {
                    groups.push(None);
                    continue;
                };
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("shader-pack block-entity sources"),
                    layout: kernels.block_entity_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 5,
                        resource: list.buffer.as_entire_binding(),
                    }],
                });
                ([count, first_output, (-1i32).cast_unsigned(), 0], group)
            }
        };
        let offset = stride * index as u64;
        queue.write_buffer(
            &params,
            offset,
            bytemuck::bytes_of(&SkinParams { camera, counts }),
        );
        let output_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("shader-pack actor skinning output"),
            layout: kernels.output_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: jobs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &params,
                        offset,
                        size: std::num::NonZeroU64::new(SKIN_PARAMS_BYTES),
                    }),
                },
            ],
        });
        groups.push(Some((source_group, output_group)));
    }
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some("shader-pack actor skinning"),
        timestamp_writes: None,
    });
    for (dispatch, groups) in actors.dispatches.iter().zip(&groups) {
        let Some((source_group, output_group)) = groups else {
            continue;
        };
        let (pipeline, [x, y, z]) = match *dispatch {
            Dispatch::Rig {
                count,
                max_vertices,
                ..
            } => (kernels.rig, rig_workgroups(count, max_vertices)),
            Dispatch::BlockEntities { count, .. } => {
                (kernels.block_entities, linear_workgroups(count))
            }
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, source_group, &[]);
        pass.set_bind_group(1, output_group, &[]);
        pass.dispatch_workgroups(x, y, z);
    }
}

/// Workgroups of a rig dispatch: rig vertices in x, jobs in y, continuing in z.
pub(crate) fn rig_workgroups(jobs: u32, max_vertices: u32) -> [u32; 3] {
    let x = max_vertices.div_ceil(SKIN_WORKGROUP_SIZE).max(1);
    let y = jobs.clamp(1, MAX_WORKGROUPS);
    [x, y, jobs.div_ceil(y).max(1)]
}

/// Workgroups covering `count` invocations, continuing in y past the x limit.
pub(crate) fn linear_workgroups(count: u32) -> [u32; 3] {
    let groups = count.div_ceil(SKIN_WORKGROUP_SIZE).max(1);
    let x = groups.min(MAX_WORKGROUPS);
    [x, groups.div_ceil(x), 1]
}

/// Records the pack's draw of `kind` over every actor draw of that kind.
pub(crate) fn record(
    encoder: &mut CommandEncoder,
    state: &mut ViewState,
    kind: DrawKind,
    write_alt: u32,
    depth: Option<(&TextureView, [u32; 2])>,
) {
    if !state.actors.draws.iter().any(|draw| draw.kind == kind) {
        return;
    }
    let Some(output) = state.actors.output.buffer.clone() else {
        return;
    };
    let constant = state.pack.host.constant_stream.clone();
    let Some((mut render, _)) = open_kind(encoder, state, kind, write_alt, depth) else {
        return;
    };
    render.set_vertex_buffer(GEOMETRY_VERTEX_BUFFER, output.slice(..));
    render.set_vertex_buffer(CONSTANT_VERTEX_BUFFER, constant.slice(..));
    let actors = &state.actors;
    let parity = state.targets.parity;
    let Some(slots) = actors.slots.get(&kind) else {
        return;
    };
    for (draw, slot) in actors.draws.iter().zip(&actors.slot_of) {
        if draw.kind != kind {
            continue;
        }
        let Some(slot) = slots
            .get(*slot as usize)
            .filter(|slot| slot.texture == Some(draw.texture))
        else {
            continue;
        };
        for (index, group) in slot.groups[parity].iter().enumerate() {
            if let Some(group) = group {
                render.set_bind_group(index as u32, group, &[]);
            }
        }
        render.draw(draw.vertices.clone(), 0..1);
    }
}
