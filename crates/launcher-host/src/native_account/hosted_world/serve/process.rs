//! Exact running-world opening retained beside its actual private process connection.

use super::{NativeHostedWorld, rejected};
use crate::local_worlds::HostOpening;
use bridge::host_world::{
    HostProcess, PendingHostProcess,
    contract::{CAPABILITY_BYTES, OPENING_BYTES},
};
use std::{io, time::Instant};
use zeroize::Zeroizing;

/// One original running-world process, never reconstructed from a local address or display name.
#[must_use]
pub struct NativeHostedProcess {
    pub(super) process: HostProcess,
    opening: HostOpening,
}

impl NativeHostedWorld {
    /// Establishes only a private IPC connection to this original live Dragonfly child.
    /// Call from the existing worlds/account worker, never a Bevy frame. No route is advertised.
    pub async fn prepare_dragonfly_bridge(
        &mut self,
        deadline: Instant,
    ) -> io::Result<NativeHostedProcess> {
        let opening = self.directory_ref()?.opening()?;
        let mut capability = Zeroizing::new([0; CAPABILITY_BYTES as usize]);
        let mut identifier = Zeroizing::new([0; OPENING_BYTES as usize]);
        getrandom::fill(&mut *capability).map_err(|_| rejected())?;
        getrandom::fill(&mut *identifier).map_err(|_| rejected())?;
        let pending = self
            .peer_io(
                Some(deadline),
                PendingHostProcess::bind(&capability, &identifier),
            )
            .await?;
        self.directory_ref()?.request_bridge(pending.bootstrap())?;
        let process = self
            .peer_io(Some(deadline), pending.establish(deadline))
            .await?;
        self.check_process(&opening)?;
        Ok(NativeHostedProcess { process, opening })
    }

    /// Checks the original opaque opening, including its actual account/RTA proof and process owner.
    pub(super) fn check_process(&self, opening: &HostOpening) -> io::Result<()> {
        if !self.directory_ref()?.opening()?.same(opening) {
            return Err(rejected());
        }
        opening.validate()
    }

    /// Rechecks the concrete prepared bridge before any peer is routed into its selected world.
    pub(super) fn check_bridge(&self, process: &NativeHostedProcess) -> io::Result<()> {
        self.check_process(&process.opening)
    }
}

impl NativeHostedProcess {
    /// Explicitly closes this original IPC bridge even after its account/world authority retires.
    /// Failure/cancellation retains the real owner and first finite cleanup deadline for retry.
    pub async fn close(&mut self, deadline: Instant) -> io::Result<()> {
        self.process.close(deadline).await
    }
}
