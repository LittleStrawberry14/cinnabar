//! Clock-free queue policy for fixed-refresh synchronized presentation.

use std::{num::NonZeroU32, time::Duration};

use crate::FrameRate;

const HISTORY: usize = 32;
const HEADROOM_SAMPLES: u32 = 120;
const HEADROOM_TIME: Duration = Duration::from_secs(2);
/// Readbacks older than this cannot establish current workload headroom.
const FRAME_WORK_MAX_AGE: Duration = Duration::from_millis(250);

/// CPU feeding work and GPU execution from the same rendered frame, excluding acquisition.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameWorkSample {
    pub frame: u64,
    pub at: Duration,
    pub render: Duration,
    pub gpu: Duration,
}

/// Evidence accumulated for the pure queue-depth decision.
#[derive(Clone, Copy, Debug, Default)]
struct Evidence {
    pressure: u8,
    headroom_samples: u32,
    headroom_elapsed: Duration,
    since_change: Duration,
}

/// Chooses two frames under sustained pressure, and one only after sustained headroom.
fn select_latency(current: NonZeroU32, evidence: Evidence) -> NonZeroU32 {
    if evidence.pressure >= 3 {
        NonZeroU32::new(2).expect("two is nonzero")
    } else if evidence.headroom_samples >= HEADROOM_SAMPLES
        && evidence.headroom_elapsed >= HEADROOM_TIME
        && evidence.since_change >= HEADROOM_TIME
    {
        NonZeroU32::MIN
    } else {
        current
    }
}

/// Deterministic evidence window; callers supply monotonic time and matched, fresh samples.
#[derive(Debug)]
pub struct FrameLatencyPolicy {
    latency: NonZeroU32,
    samples: [FrameWorkSample; HISTORY],
    len: usize,
    next: usize,
    last: Option<FrameWorkSample>,
    headroom_since: Option<Duration>,
    changed_at: Duration,
    evidence: Evidence,
}

impl Default for FrameLatencyPolicy {
    fn default() -> Self {
        Self {
            latency: NonZeroU32::new(2).expect("two is nonzero"),
            samples: [FrameWorkSample::default(); HISTORY],
            len: 0,
            next: 0,
            last: None,
            headroom_since: None,
            changed_at: Duration::ZERO,
            evidence: Evidence::default(),
        }
    }
}

impl FrameLatencyPolicy {
    /// Advances the policy without reading a clock. Missing or stale evidence keeps two frames;
    /// duplicate readbacks never count toward either sustained threshold.
    pub fn update(
        &mut self,
        now: Duration,
        refresh: Option<FrameRate>,
        sample: Option<FrameWorkSample>,
    ) -> NonZeroU32 {
        let Some(refresh) = refresh else {
            *self = Self::default();
            return self.latency;
        };
        if self
            .last
            .is_some_and(|last| now < last.at || now.saturating_sub(last.at) > FRAME_WORK_MAX_AGE)
        {
            *self = Self::default();
            self.changed_at = now;
        }
        let Some(sample) = sample.filter(|sample| {
            sample.at <= now
                && now - sample.at <= FRAME_WORK_MAX_AGE
                && self.last.is_none_or(|last| sample.frame > last.frame)
        }) else {
            return self.latency;
        };
        self.last = Some(sample);
        self.samples[self.next] = sample;
        self.next = (self.next + 1) % HISTORY;
        self.len = (self.len + 1).min(HISTORY);
        let budget = self.budget();
        let interval = Duration::from_nanos(refresh.period_nanos());
        self.evidence.pressure = if budget >= interval.mul_f64(0.95) {
            self.evidence.pressure.saturating_add(1)
        } else {
            0
        };
        if budget <= interval.mul_f64(0.80) {
            let since = *self.headroom_since.get_or_insert(sample.at);
            self.evidence.headroom_samples = self.evidence.headroom_samples.saturating_add(1);
            self.evidence.headroom_elapsed = sample.at.saturating_sub(since);
        } else {
            self.headroom_since = None;
            self.evidence.headroom_samples = 0;
            self.evidence.headroom_elapsed = Duration::ZERO;
        }
        self.evidence.since_change = now.saturating_sub(self.changed_at);
        let next = select_latency(self.latency, self.evidence);
        if next != self.latency {
            self.latency = next;
            self.changed_at = now;
            self.headroom_since = None;
            self.evidence = Evidence::default();
        }
        self.latency
    }

    /// Conservatively adds the independent 95th-percentile CPU and GPU work estimates.
    fn budget(&self) -> Duration {
        let mut cpu = self.samples.map(|sample| sample.render);
        let mut gpu = self.samples.map(|sample| sample.gpu);
        cpu[..self.len].sort_unstable();
        gpu[..self.len].sort_unstable();
        let rank = (self.len * 95).div_ceil(100) - 1;
        cpu[rank].saturating_add(gpu[rank])
    }
}

#[cfg(test)]
mod tests;
