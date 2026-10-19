use super::*;

/// Delivers a matched frame on a deterministic 60 Hz timeline.
fn frame(policy: &mut FrameLatencyPolicy, id: u64, cpu_ms: u64, gpu_ms: u64) -> u32 {
    let now = Duration::from_micros(id * 16_667);
    policy
        .update(
            now,
            FrameRate::from_hz(60),
            Some(FrameWorkSample {
                frame: id,
                at: now,
                render: Duration::from_millis(cpu_ms),
                gpu: Duration::from_millis(gpu_ms),
            }),
        )
        .get()
}

/// Supplies sustained headroom, including the minimum elapsed qualification time.
fn qualify(policy: &mut FrameLatencyPolicy) {
    for id in 1..=121 {
        let selected = frame(policy, id, 4, 5);
        assert_eq!(selected, if id == 121 { 1 } else { 2 });
    }
}

#[test]
fn fitting_work_uses_one_only_after_sustained_headroom() {
    qualify(&mut FrameLatencyPolicy::default());
}

#[test]
fn sustained_cpu_plus_gpu_pressure_restores_overlap() {
    let mut policy = FrameLatencyPolicy::default();
    qualify(&mut policy);
    assert_eq!(frame(&mut policy, 122, 9, 9), 1);
    assert_eq!(frame(&mut policy, 123, 9, 9), 1);
    assert_eq!(frame(&mut policy, 124, 9, 9), 1);
    assert_eq!(frame(&mut policy, 125, 9, 9), 2);
}

#[test]
fn returning_to_one_requires_new_sustained_headroom() {
    let mut policy = FrameLatencyPolicy::default();
    qualify(&mut policy);
    for id in 122..=160 {
        frame(&mut policy, id, 9, 9);
    }
    for id in 161..=310 {
        assert_eq!(frame(&mut policy, id, 4, 5), 2);
    }
    assert_eq!(frame(&mut policy, 311, 4, 5), 1);
}

#[test]
fn threshold_jitter_does_not_flap_the_surface() {
    let mut policy = FrameLatencyPolicy::default();
    qualify(&mut policy);
    let mut changes = 0;
    let mut previous = 1;
    for id in 122..=800 {
        let selected = frame(&mut policy, id, 8, if id % 2 == 0 { 7 } else { 9 });
        changes += u32::from(selected != previous);
        previous = selected;
    }
    assert_eq!(previous, 2);
    assert_eq!(changes, 1);
}

#[test]
fn missing_stale_and_duplicate_samples_cannot_prove_headroom() {
    let mut policy = FrameLatencyPolicy::default();
    let refresh = FrameRate::from_hz(60);
    for _ in 0..200 {
        assert_eq!(frame(&mut policy, 1, 4, 5), 2);
    }
    let mut policy = FrameLatencyPolicy::default();
    qualify(&mut policy);
    assert_eq!(
        policy.update(Duration::from_secs(3), refresh, None).get(),
        2
    );
    assert_eq!(policy.update(Duration::from_secs(3), None, None).get(), 2);
    assert_eq!(
        policy
            .update(
                Duration::from_secs(3),
                refresh,
                Some(FrameWorkSample::default())
            )
            .get(),
        2
    );
}

#[test]
fn policy_uses_display_refresh_instead_of_a_slower_frame_cap() {
    let mut policy = FrameLatencyPolicy::default();
    for id in 1..=300 {
        let now = Duration::from_millis(id * 10);
        assert_eq!(
            policy
                .update(
                    now,
                    FrameRate::from_hz(144),
                    Some(FrameWorkSample {
                        frame: id,
                        at: now,
                        render: Duration::from_millis(4),
                        gpu: Duration::from_millis(5),
                    })
                )
                .get(),
            2
        );
    }
}

#[test]
fn a_single_hitch_does_not_recreate_a_qualified_surface() {
    let mut policy = FrameLatencyPolicy::default();
    qualify(&mut policy);
    assert_eq!(frame(&mut policy, 122, 40, 40), 1);
    for id in 123..=200 {
        assert_eq!(frame(&mut policy, id, 4, 5), 1);
    }
}

#[test]
fn many_fast_samples_still_require_two_seconds_of_headroom() {
    let mut policy = FrameLatencyPolicy::default();
    for id in 1..=350 {
        let now = Duration::from_micros(id * 6_944);
        let selected = policy.update(
            now,
            FrameRate::from_hz(144),
            Some(FrameWorkSample {
                frame: id,
                at: now,
                render: Duration::from_millis(1),
                gpu: Duration::from_millis(1),
            }),
        );
        assert_eq!(selected.get(), if id < 290 { 2 } else { 1 });
    }
}
