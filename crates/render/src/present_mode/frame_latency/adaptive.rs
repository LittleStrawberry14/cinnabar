//! Matches delayed GPU execution spans to render work without feeding acquisition waits back.

use std::time::{Duration, Instant};

use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderSystems, renderer::RenderAdapter,
        view::window::prepare_windows,
    },
    window::{PresentMode, PrimaryWindow, WindowOccluded},
};
use render_model::{DisplayTiming, FrameLatencyPolicy, FrameWorkSample, VrrStatus};

use crate::present_mode::PresentModePolicy;

/// A context change discards evidence from the old display, window, size or delivery mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Context {
    window: Entity,
    size: [u32; 2],
    mode: PresentMode,
    display: DisplayTiming,
    active: bool,
}

/// Frame-tagged CPU work retained long enough to match the nonblocking GPU readback ring.
#[derive(Resource)]
pub(crate) struct AdaptiveFrameLatency {
    epoch: Instant,
    context: Option<Context>,
    model: FrameLatencyPolicy,
    frame: u64,
    started: Option<Instant>,
    surface_started: Option<Instant>,
    surface_elapsed: Duration,
    cpu: [Option<FrameWorkSample>; crate::gpu_timing::readback::SLOTS + 1],
}

impl Default for AdaptiveFrameLatency {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            context: None,
            model: FrameLatencyPolicy::default(),
            frame: 0,
            started: None,
            surface_started: None,
            surface_elapsed: Duration::ZERO,
            cpu: [None; crate::gpu_timing::readback::SLOTS + 1],
        }
    }
}

impl AdaptiveFrameLatency {
    /// Identifies this render schedule for its query readback slot.
    pub(crate) fn frame(&self) -> u64 {
        self.frame
    }

    /// Excludes the surface-reconfiguration hitch from workload evidence.
    pub(crate) fn discard_current_frame(&mut self) {
        self.started = None;
    }

    /// Retains feeding work on the caller's clock, leaving acquisition outside the sample.
    fn finish(&mut self, now: Instant) {
        let Some(started) = self.started.take() else {
            return;
        };
        let index = self.frame as usize % self.cpu.len();
        self.cpu[index] = Some(FrameWorkSample {
            frame: self.frame,
            at: now.saturating_duration_since(self.epoch),
            render: now
                .saturating_duration_since(started)
                .saturating_sub(self.surface_elapsed),
            gpu: Duration::ZERO,
        });
    }

    /// Accepts only a matching CPU sample and a complete coarse GPU span; sparse pass timing
    /// and submission-to-callback latency never establish execution headroom.
    pub(crate) fn record_gpu(
        &mut self,
        frame: u64,
        gpu: Option<Duration>,
        now: Instant,
        policy: &PresentModePolicy,
    ) {
        let Some(gpu) = gpu else { return };
        let index = frame as usize % self.cpu.len();
        let Some(mut sample) = self.cpu[index].filter(|sample| sample.frame == frame) else {
            return;
        };
        self.cpu[index] = None;
        sample.gpu = gpu;
        let refresh = self.refresh();
        let selected = self.model.update(
            now.saturating_duration_since(self.epoch),
            refresh,
            Some(sample),
        );
        policy.publish_frame_latency(selected);
    }

    /// The actual fixed-refresh interval, without substituting a software frame cap.
    fn refresh(&self) -> Option<render_model::FrameRate> {
        self.context
            .filter(|context| {
                context.active
                    && context.display.vrr != VrrStatus::Active
                    && matches!(context.mode, PresentMode::Fifo | PresentMode::FifoRelaxed)
            })
            .and_then(|context| context.display.refresh)
    }
}

/// Installs Windows measurements independently of input-admission prediction.
pub(super) fn install(app: &mut SubApp) {
    app.init_resource::<AdaptiveFrameLatency>()
        .add_systems(ExtractSchedule, extract_context)
        .add_systems(
            Render,
            (
                begin_frame.before(RenderSystems::ExtractCommands),
                end_frame.in_set(RenderSystems::PostCleanup),
                (
                    begin_surface.before(prepare_windows),
                    end_surface.after(prepare_windows),
                )
                    .in_set(RenderSystems::PrepareViews),
            ),
        );
}

/// Invalidates measurements when the primary window's delivery context changes.
fn extract_context(
    windows: Extract<Query<(Entity, &Window), With<PrimaryWindow>>>,
    mut events: Extract<MessageReader<WindowOccluded>>,
    mut occluded: Local<(Option<Entity>, bool)>,
    mut adaptive: ResMut<AdaptiveFrameLatency>,
    policy: Res<PresentModePolicy>,
) {
    let primary = windows.single().ok();
    let entity = primary.map(|(entity, _)| entity);
    if occluded.0 != entity {
        *occluded = (entity, false);
    }
    for event in events.read() {
        if Some(event.window) == entity {
            occluded.1 = event.occluded;
        }
    }
    let context = primary.map(|(entity, window)| Context {
        window: entity,
        size: [window.physical_width(), window.physical_height()],
        mode: window.present_mode,
        display: *policy
            .frame_latency
            .display
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        active: window.focused
            && window.visible
            && !occluded.1
            && window.physical_width() != 0
            && window.physical_height() != 0
            && policy.preference().intent() == render_model::PresentationIntent::Synchronized,
    });
    if adaptive.context != context {
        adaptive.context = context;
        adaptive.model = FrameLatencyPolicy::default();
        adaptive.cpu.fill(None);
        policy.publish_frame_latency(std::num::NonZeroU32::new(2).expect("two is nonzero"));
    }
}

/// Starts work timing before extraction commands and checks for expired GPU evidence.
fn begin_frame(
    mut adaptive: ResMut<AdaptiveFrameLatency>,
    adapter: Res<RenderAdapter>,
    policy: Res<PresentModePolicy>,
    mut supported: Local<Option<bool>>,
) {
    let now = Instant::now();
    adaptive.frame += 1;
    adaptive.surface_elapsed = Duration::ZERO;
    adaptive.surface_started = None;
    let supported = *supported.get_or_insert_with(|| {
        matches!(
            adapter.get_info().backend,
            wgpu::Backend::Dx12 | wgpu::Backend::Vulkan
        )
    });
    adaptive.started = supported.then_some(now);
    let refresh = supported.then(|| adaptive.refresh()).flatten();
    let elapsed = now.saturating_duration_since(adaptive.epoch);
    let selected = adaptive.model.update(elapsed, refresh, None);
    policy.publish_frame_latency(selected);
}

/// Retains this frame's render feeding work after subtracting surface acquisition.
fn end_frame(mut adaptive: ResMut<AdaptiveFrameLatency>) {
    adaptive.finish(Instant::now());
}

/// Starts the drawable-acquisition interval to exclude from CPU feeding work.
fn begin_surface(mut adaptive: ResMut<AdaptiveFrameLatency>) {
    adaptive.surface_started = Some(Instant::now());
}

/// Removes surface backpressure from the workload that chooses queue depth.
fn end_surface(mut adaptive: ResMut<AdaptiveFrameLatency>) {
    if let Some(started) = adaptive.surface_started.take() {
        adaptive.surface_elapsed += started.elapsed();
    }
}

#[cfg(test)]
mod tests;
