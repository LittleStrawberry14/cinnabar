use super::*;
use bevy::render::MainWorld;

/// Creates fixed-refresh context and a shared recommendation without a native surface.
fn fixture() -> (SubApp, Entity) {
    let mut main = MainWorld::default();
    main.init_resource::<Messages<WindowOccluded>>();
    let window = main.spawn((Window::default(), PrimaryWindow)).id();
    let mut app = SubApp::new();
    let policy = PresentModePolicy::default();
    policy.publish_display_timing(DisplayTiming {
        refresh: render_model::FrameRate::from_hz(60),
        vrr: VrrStatus::Inactive,
    });
    app.insert_resource(policy).insert_resource(main);
    install(&mut app);
    app.world_mut().run_schedule(ExtractSchedule);
    (app, window)
}

#[test]
fn acquisition_and_surface_recreation_do_not_become_render_work() {
    let mut adaptive = AdaptiveFrameLatency::default();
    let epoch = adaptive.epoch;
    adaptive.frame = 1;
    adaptive.started = Some(epoch);
    adaptive.surface_elapsed = Duration::from_millis(20);
    adaptive.finish(epoch + Duration::from_millis(25));
    assert_eq!(adaptive.cpu[1].unwrap().render, Duration::from_millis(5));
    assert_eq!(adaptive.frame(), 1);

    adaptive.frame = 2;
    adaptive.started = Some(epoch);
    adaptive.discard_current_frame();
    adaptive.finish(epoch + Duration::from_secs(1));
    assert!(adaptive.cpu[2].is_none());
}

#[test]
fn resize_and_focus_changes_discard_old_context_and_pending_readbacks() {
    let (mut app, window) = fixture();
    let policy = app.world().resource::<PresentModePolicy>().clone();
    for resize in [true, false] {
        policy.publish_frame_latency(std::num::NonZeroU32::MIN);
        app.world_mut().resource_mut::<AdaptiveFrameLatency>().cpu[1] = Some(FrameWorkSample {
            frame: 1,
            ..Default::default()
        });
        app.world_mut().run_schedule(ExtractSchedule);
        assert!(app.world().resource::<AdaptiveFrameLatency>().cpu[1].is_some());
        {
            let mut main = app.world_mut().resource_mut::<MainWorld>();
            let mut window = main.get_mut::<Window>(window).unwrap();
            if resize {
                window.resolution.set_physical_resolution(640, 480);
            } else {
                window.focused = false;
            }
        }
        app.world_mut().run_schedule(ExtractSchedule);
        let mut adaptive = app.world_mut().resource_mut::<AdaptiveFrameLatency>();
        assert!(adaptive.cpu.iter().all(Option::is_none));
        let now = adaptive.epoch;
        adaptive.record_gpu(1, Some(Duration::from_millis(2)), now, &policy);
        assert_eq!(
            policy
                .frame_latency
                .latency
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );
    }
}

#[test]
fn matched_gpu_readbacks_qualify_headroom_but_old_or_partial_ones_do_not() {
    let (mut app, _) = fixture();
    let policy = app.world().resource::<PresentModePolicy>().clone();
    let mut adaptive = app.world_mut().resource_mut::<AdaptiveFrameLatency>();
    for frame in 1..=121 {
        let at = Duration::from_micros(frame * 16_667);
        let index = frame as usize % adaptive.cpu.len();
        adaptive.cpu[index] = Some(FrameWorkSample {
            frame,
            at,
            render: Duration::from_millis(4),
            gpu: Duration::ZERO,
        });
        let now = adaptive.epoch + at;
        adaptive.record_gpu(frame + 1, Some(Duration::from_millis(5)), now, &policy);
        adaptive.record_gpu(frame, None, now, &policy);
        assert!(adaptive.cpu[index].is_some());
        adaptive.record_gpu(frame, Some(Duration::from_millis(5)), now, &policy);
        assert!(adaptive.cpu[index].is_none());
    }
    assert_eq!(
        policy
            .frame_latency
            .latency
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    let context = adaptive.context.as_mut().unwrap();
    context.display.vrr = VrrStatus::Active;
    assert!(adaptive.refresh().is_none());
}
