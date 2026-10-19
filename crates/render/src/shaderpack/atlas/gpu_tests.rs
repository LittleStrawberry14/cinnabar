use super::animate::{
    AtlasAnimations, BLEND_INSTANCE_ATTRIBUTES, BLEND_SOURCE_ENTRIES, BlendInstance, BlendPipelines,
};
use super::labpbr::labpbr_normal;
use super::pixels::{MipImages, build_images};
use super::plan::{AuthoredPbr, plan_atlas};
use super::tests::{FRAME_TICKS, expected, fixture, page, slot_rect};
use super::upload::{COLOR_STORAGE_FORMAT, PBR_FORMAT, upload_atlas};
use super::*;
use assets::texture::{PBR_REF_LABPBR, PBR_REF_NORMAL};

/// Reads one level of a colour atlas back as encoded bytes.
fn read_level(gpu: &crate::gpu_snapshot::Gpu, texture: &wgpu::Texture, level: u32) -> MipImages {
    let (width, height) = (texture.width() >> level, texture.height() >> level);
    let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let pixels = mapped
        .chunks_exact(row as usize)
        .flat_map(|line| &line[..width as usize * 4])
        .copied()
        .collect();
    let mut levels = vec![Vec::new(); level as usize];
    levels.push(pixels);
    MipImages {
        width: texture.width(),
        height: texture.height(),
        levels,
    }
}

fn decode(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode(value: f32) -> u8 {
    let value = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The chunk shaders' frame mix: sampled (decoded) colour and linear alpha.
fn blended(current: &[u8], next: &[u8], weight: f32) -> Vec<u8> {
    current
        .as_chunks::<4>()
        .0
        .iter()
        .zip(next.as_chunks::<4>().0)
        .flat_map(|(a, b)| {
            let mut texel = [0; 4];
            for channel in 0..3 {
                let mixed = decode(a[channel]) * (1.0 - weight) + decode(b[channel]) * weight;
                texel[channel] = encode(mixed);
            }
            texel[3] = (f32::from(a[3]) * (1.0 - weight) + f32::from(b[3]) * weight).round() as u8;
            texel
        })
        .collect()
}

/// The LabPBR frame mix: linear channels.
fn blended_linear(current: &[u8], next: &[u8], weight: f32) -> Vec<u8> {
    current
        .iter()
        .zip(next)
        .map(|(&a, &b)| (f32::from(a) * (1.0 - weight) + f32::from(b) * weight).round() as u8)
        .collect()
}

/// Blends with the colour-only pipeline when the frame store has no LabPBR frames, and with the
/// colour + normal + specular pipeline (one pass per mip) when the animation frames author normals.
#[test]
fn animated_display_sprites_show_the_clock_frame_after_an_update() {
    let Some(gpu) = crate::gpu_snapshot::Gpu::for_fixture("shader-pack atlas animation update")
    else {
        return;
    };
    let layout = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &BLEND_SOURCE_ENTRIES,
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shader-pack frame blend"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blend.wgsl").into()),
        });
    let pipeline = |entry_point: &str, formats: &[wgpu::TextureFormat]| {
        let targets = formats
            .iter()
            .map(|&format| Some(format.into()))
            .collect::<Vec<_>>();
        gpu.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: None,
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<BlendInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &BLEND_INSTANCE_ATTRIBUTES,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            })
    };
    let colour = pipeline("fragment_colour", &[COLOR_STORAGE_FORMAT]);
    let layers = pipeline(
        "fragment_layers",
        &[COLOR_STORAGE_FORMAT, PBR_FORMAT, PBR_FORMAT],
    );
    let pipelines = BlendPipelines {
        colour: &colour,
        layers: &layers,
    };
    let fixture = fixture();
    let normals = page(7, 16, 6);
    let flags = PBR_REF_NORMAL | PBR_REF_LABPBR;
    let mut refs = vec![u32::MAX; STATIC_SPRITE_SLOTS as usize];
    for layer in 3..=5 {
        refs[(static_sprite_slot(0, layer) - ATLAS_HEADER_SLOTS) as usize] = flags | layer;
    }
    for authored in [false, true] {
        let pbr = authored.then(|| AuthoredPbr {
            normal_pages: [&normals, &normals],
            mer_pages: [&normals, &normals],
            refs: &refs,
        });
        let (plan, images) = fixture.with_inputs(pbr, |inputs| {
            let plan = plan_atlas(inputs, gpu.device.limits().max_texture_dimension_2d).unwrap();
            let images = build_images(inputs, &plan);
            (plan, images)
        });
        let uploaded = upload_atlas(&gpu.device, &gpu.queue, &plan, &images);
        let mut animations = AtlasAnimations::new(
            &uploaded,
            &fixture.animations,
            &fixture.frames,
            plan.animations.clone(),
            plan.mip_levels,
            [plan.atlas.width, plan.atlas.height],
        );
        animations.bind_blend_sources(&gpu.device, &layout);
        let clock = ChunkAnimationClock::from_parts(FRAME_TICKS + 1, 0.5);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        assert_eq!(
            animations.encode(
                &gpu.device,
                &gpu.queue,
                &mut encoder,
                clock,
                Some(pipelines)
            ),
            2
        );
        gpu.queue.submit([encoder.finish()]);
        let plain = slot_rect(&plan, animation_sprite_slot(0));
        let blending = slot_rect(&plan, animation_sprite_slot(1));
        for level in 0..plan.mip_levels {
            let atlas = read_level(&gpu, &uploaded.atlas.color, level);
            let side = plain.side >> level;
            assert_eq!(
                atlas.read(level, plain),
                expected(0, 4, level, side),
                "authored {authored}, mip {level}"
            );
            let wanted = blended(
                &expected(0, 4, level, side),
                &expected(0, 5, level, side),
                0.75,
            );
            for (got, want) in atlas.read(level, blending).iter().zip(&wanted) {
                assert!(
                    got.abs_diff(*want) <= 1,
                    "authored {authored}, mip {level}: {got} vs {want}"
                );
            }
            if !authored {
                continue;
            }
            let normal = |layer: u32| {
                expected(7, layer, level, side)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|&texel| labpbr_normal(texel, flags | layer))
                    .collect::<Vec<_>>()
            };
            let atlas = read_level(&gpu, &uploaded.atlas.pbr.as_ref().unwrap()[0], level);
            assert_eq!(atlas.read(level, plain), normal(4), "normal mip {level}");
            let wanted = blended_linear(&normal(4), &normal(5), 0.75);
            for (got, want) in atlas.read(level, blending).iter().zip(&wanted) {
                assert!(
                    got.abs_diff(*want) <= 1,
                    "normal mip {level}: {got} vs {want}"
                );
            }
        }
    }
}
