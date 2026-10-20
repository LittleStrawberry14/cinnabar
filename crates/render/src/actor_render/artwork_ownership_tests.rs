use super::*;
use std::{cell::Cell, rc::Rc};

/// Builds original solid-colour layers so readback can distinguish every appended suffix.
fn page(colors: &[[u8; 4]]) -> render_model::ActorTexturePage {
    render_model::ActorTexturePage {
        width: 8,
        height: 8,
        layers: colors.len() as u32,
        rgba8: colors
            .iter()
            .flat_map(|color| color.repeat(8 * 8))
            .collect(),
        color_mask: false,
        multitexture: false,
    }
}

/// Gives each immutable publication a distinct identity without installed game assets.
fn publication(marker: u8, pages: Vec<render_model::ActorTexturePage>) -> ActorArtworkPages {
    let mut artwork = ActorArtworkPages::default();
    artwork.entity_identity = [marker; 32];
    artwork.pages = pages.into();
    artwork
}

/// Samples a real array layer through its published view, including its actual GPU pixels.
fn color(native: &crate::gpu_snapshot::Gpu, page: &GpuArtworkPage, layer: u32) -> [u8; 4] {
    let image = native.render(
        &format!(
            r#"
@group(0) @binding(0) var artwork: texture_2d_array<f32>;
@vertex fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {{
    let p = array(vec2f(-1., -1.), vec2f(3., -1.), vec2f(-1., 3.))[index];
    return vec4f(p, 0.5, 1.);
}}
@fragment fn fragment() -> @location(0) vec4f {{
    return textureLoad(artwork, vec2i(0), {layer}, 0);
}}
"#
        ),
        "vertex",
        &[crate::gpu_snapshot::Draw {
            fragment: "fragment",
            vertices: 0..3,
            bindings: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&page.view),
            }],
            blend: None,
            write_depth: false,
        }],
    );
    image[..4].try_into().unwrap()
}

#[test]
fn divergent_shared_artwork_appends_preserve_each_pages_pixels() {
    let Some(native) = crate::gpu_snapshot::Gpu::for_fixture(
        "divergent_shared_artwork_appends_preserve_each_pages_pixels",
    ) else {
        return;
    };
    let device = RenderDevice::from(native.device.clone());
    let queue = RenderQueue::new(native.queue.clone());
    let prefix = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]];
    let extensions = [[255, 255, 0, 255], [0, 255, 255, 255]];
    for (split_publication, fail_append) in [(false, false), (true, false), (false, true)] {
        let mut gpu = GpuArtwork::default();
        let original = publication(1, vec![page(&prefix), page(&prefix)]);
        assert!(gpu.prepare(&original, &device, &queue));
        assert_eq!(
            gpu.page(0).unwrap()._texture.id(),
            gpu.page(1).unwrap()._texture.id()
        );
        let next = publication(
            3,
            extensions
                .iter()
                .map(|suffix| page(&[prefix.as_slice(), &[*suffix]].concat()))
                .collect(),
        );
        if split_publication {
            let middle = publication(2, vec![next.pages[0].clone(), original.pages[1].clone()]);
            assert!(gpu.prepare(&middle, &device, &queue));
            assert_eq!(color(&native, gpu.page(1).unwrap(), 2), prefix[2]);
        }
        let failures = Rc::new(Cell::new(0));
        let observed = failures.clone();
        let fault = fail_append.then(|| {
            gpu_allocation::inject(move |label, bytes| {
                if label == "immutable actor page" {
                    return bytes > 4 * 4;
                }
                if label == "actor page upload" && bytes == 8 * 8 * 4 && observed.get() == 0 {
                    observed.set(observed.get() + 1);
                    return true;
                }
                false
            })
        });
        assert!(gpu.prepare(&next, &device, &queue));
        drop(fault);
        assert_eq!(failures.get(), usize::from(fail_append));
        for (index, suffix) in extensions.into_iter().enumerate() {
            assert_eq!(color(&native, gpu.page(index).unwrap(), 3), suffix);
            for (layer, expected) in prefix.into_iter().enumerate() {
                assert_eq!(
                    color(&native, gpu.page(index).unwrap(), layer as u32),
                    expected
                );
            }
        }
    }
}

#[test]
fn replacing_multiple_artwork_pages_keeps_only_one_temporary_texture() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::NOOP,
        backend_options: wgpu::BackendOptions {
            noop: wgpu::NoopBackendOptions::enabled(),
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = bevy::tasks::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) =
        bevy::tasks::block_on(adapter.request_device(&Default::default())).unwrap();
    let device = RenderDevice::from(device);
    let queue = RenderQueue::new(queue);
    let pages = |marker| {
        publication(
            marker,
            (0..4).map(|i| page(&[[marker, i, 0, 255]])).collect(),
        )
    };
    let mut gpu = GpuArtwork::default();
    assert!(gpu.prepare(&pages(1), &device, &queue));
    queue.submit([]);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let resident = instance
        .generate_report()
        .unwrap()
        .hub
        .textures
        .num_allocated;
    assert_eq!(resident, 6, "four artwork pages and two glints");
    let peak = Rc::new(Cell::new(resident));
    let observed = peak.clone();
    let attempts = Rc::new(Cell::new(0));
    let counted = attempts.clone();
    let fault = gpu_allocation::inject(move |label, _| {
        if label != "immutable actor page" {
            return false;
        }
        counted.set(counted.get() + 1);
        let live = instance
            .generate_report()
            .unwrap()
            .hub
            .textures
            .num_allocated;
        observed.set(observed.get().max(live));
        live > resident + 1
    });
    let next = pages(2);
    assert!(gpu.prepare(&next, &device, &queue));
    drop(fault);
    assert_eq!(
        peak.get(),
        resident + 1,
        "each successful replacement retires its old allocation"
    );
    assert_eq!(attempts.get(), next.pages.len());
    for (index, source) in next.pages.iter().enumerate() {
        assert!(gpu.page(index).unwrap().source.shares_pixels_with(source));
    }
}

#[test]
fn replacing_earlier_routes_preserves_later_exact_texture_reuse() {
    let (device, queue) = wgpu::Device::noop(&Default::default());
    let device = RenderDevice::from(device);
    let queue = RenderQueue::new(queue);
    let original = publication(1, (1..=3).map(|i| page(&[[i, 0, 0, 255]])).collect());
    let mut gpu = GpuArtwork::default();
    assert!(gpu.prepare(&original, &device, &queue));
    let ids: Vec<_> = gpu
        .pages
        .iter()
        .flatten()
        .map(|gpu| gpu._texture.id())
        .collect();
    let next = publication(2, (0..3).map(|i| page(&[[i, 0, 0, 255]])).collect());
    let uploads = work_tests::upload_count();
    assert!(gpu.prepare(&next, &device, &queue));
    assert_eq!(work_tests::upload_count() - uploads, 1);
    for (index, id) in ids[..2].iter().enumerate() {
        assert_eq!(gpu.page(index + 1).unwrap()._texture.id(), *id);
    }
}

#[test]
fn failed_artwork_replacement_recovers_without_changing_publication() {
    let (device, queue) = wgpu::Device::noop(&Default::default());
    let device = RenderDevice::from(device);
    let queue = RenderQueue::new(queue);
    let original = publication(1, vec![page(&[[255, 0, 0, 255]])]);
    let next = publication(2, vec![page(&[[0, 255, 0, 255]])]);
    let mut gpu = GpuArtwork::default();
    assert!(gpu.prepare(&original, &device, &queue));
    let previous = gpu.page(0).unwrap()._texture.id();
    {
        let _fault = gpu_allocation::inject(|label, _| label == "immutable actor page");
        assert!(gpu.prepare(&next, &device, &queue));
    }
    assert_eq!(gpu.page(0).unwrap()._texture.id(), previous);
    assert!(gpu.prepare(&next, &device, &queue));
    assert!(
        gpu.page(0)
            .unwrap()
            .source
            .shares_pixels_with(&next.pages[0])
    );
    assert_ne!(gpu.page(0).unwrap()._texture.id(), previous);
}

#[test]
fn shared_artwork_appends_write_only_the_uninitialized_suffix() {
    let (device, queue) = wgpu::Device::noop(&Default::default());
    let device = RenderDevice::from(device);
    let queue = RenderQueue::new(queue);
    let prefix: Vec<_> = (0..5).map(|i| [i, 0, 0, 255]).collect();
    let original = publication(1, vec![page(&prefix), page(&prefix)]);
    let mut gpu = GpuArtwork::default();
    assert!(gpu.prepare(&original, &device, &queue));
    let texture = gpu.page(0).unwrap()._texture.id();
    let mut extended = prefix.clone();
    extended.push([255, 255, 0, 255]);
    let next = publication(2, vec![page(&extended), original.pages[1].clone()]);
    assert!(gpu.prepare(&next, &device, &queue));
    extended.push([0, 255, 255, 255]);
    let last = publication(3, vec![next.pages[0].clone(), page(&extended)]);
    let bytes = Rc::new(Cell::new(0));
    let observed = bytes.clone();
    let _observe = gpu_allocation::inject(move |label, bytes| {
        if label == "actor page upload" {
            observed.set(observed.get() + bytes);
        }
        false
    });
    assert!(gpu.prepare(&last, &device, &queue));
    assert_eq!(
        bytes.get(),
        u64::from(last.pages[1].width) * u64::from(last.pages[1].height) * 4
    );
    assert!(
        gpu.pages
            .iter()
            .flatten()
            .all(|page| page._texture.id() == texture)
    );
}
