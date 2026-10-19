//! One concrete paired play lifetime, owned and polled by the existing application worker.

mod output;
use output::Output;

use super::{HostedApproval, HostedLoginApproval, NativeHostedWorld, SendAuthority};
use bedrock_client::server::session::AcceptedSession;
use bridge::host_world::{ActiveBackendPeer, ClosingBackendPeer, contract::CLOSE_MILLIS};
use protocol::{NativeAcceptingIo, NativeAcceptingRetired, NativeAcceptingSendOutcome};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

/// Actual initialized Login, original hosted approval, external codec and Go world connection.
/// No worker is spawned. The caller retains this owner and polls it on its existing runtime.
#[must_use]
pub struct NativeHostedPeer {
    wire: NativeAcceptingIo,
    backend: ActiveBackendPeer,
    accepted: AcceptedSession<HostedLoginApproval>,
    output: Output,
}

/// New play admission ended; original backend output can finish before actual transport retirement.
#[must_use]
pub struct ClosingHostedPeer {
    wire: Option<NativeAcceptingIo>,
    retired: Option<NativeAcceptingRetired>,
    backend: Option<ClosingBackendPeer>,
    accepted: AcceptedSession<HostedLoginApproval>,
    output: Output,
    deadline: Instant,
    failure: Option<Arc<io::Error>>,
    settled: bool,
    finished: bool,
}

impl NativeHostedPeer {
    /// Takes only the actual typed initialization and both corresponding live IO owners.
    pub(super) fn new(
        wire: NativeAcceptingIo,
        backend: ActiveBackendPeer,
        accepted: AcceptedSession<HostedLoginApproval>,
    ) -> Self {
        Self {
            wire,
            backend,
            accepted,
            output: Output::Idle,
        }
    }

    /// Drives the original host/RTA and both play directions without an idle timeout.
    /// Keep this actual future pinned. Cancellation physically retires the two owned sockets;
    /// the separate host directory remains with its sole application cleanup owner.
    /// A terminal result returns the same peer as a bounded explicit closing owner.
    pub async fn run(
        mut self,
        host: &mut NativeHostedWorld,
    ) -> (ClosingHostedPeer, io::Result<()>) {
        let result = self.play(host).await;
        let mut closing = self.begin_close(cleanup_deadline());
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let error = Arc::new(error);
                closing.failure = Some(Arc::clone(&error));
                Err(repeat(&error))
            }
        };
        (closing, result)
    }

    /// Relays one original raw packet at a time while retaining controls and opposite-direction output.
    async fn play(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        loop {
            host.recheck_approval(&self.accepted.grant().approval)?;
            let wire = &mut self.wire;
            let backend = &mut self.backend;
            let output = &mut self.output;
            let event = host
                .peer_io(None, async {
                    // The losing receive future is dropped before staging touches the same wire.
                    let packet = tokio::select! {
                        packet = wire.receive_play() => return packet.map(Event::External),
                        packet = backend.recv() => packet?,
                    };
                    match packet {
                        Some(packet) => {
                            output.stage(wire, packet, cleanup_deadline())?;
                            Ok(Event::World)
                        }
                        None => Ok(Event::Ended),
                    }
                })
                .await?;
            host.recheck_approval(&self.accepted.grant().approval)?;
            match event {
                Event::External(packet) => {
                    self.backend.queue_send(&packet, cleanup_deadline())?;
                    loop {
                        match host.peer_io(None, self.backend.flush_send()).await {
                            Ok(()) => break,
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                self.world_output(host).await?;
                            }
                            Err(error) => return Err(error),
                        }
                        host.recheck_approval(&self.accepted.grant().approval)?;
                    }
                }
                Event::World => self.complete_output(host).await?,
                Event::Ended => return Ok(()),
            }
        }
    }

    /// Dispatches the sole retained opposite-direction payload before resuming its pending receipt.
    async fn world_output(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        host.recheck_approval(&self.accepted.grant().approval)?;
        let wire = &mut self.wire;
        let backend = &mut self.backend;
        let output = &mut self.output;
        host.peer_io(None, async {
            let packet = backend.recv().await?.ok_or_else(ended)?;
            output.stage(wire, packet, cleanup_deadline())
        })
        .await?;
        host.recheck_approval(&self.accepted.grant().approval)?;
        self.complete_output(host).await
    }

    /// Keeps external completion and the original backend acknowledgment as one retained obligation.
    async fn complete_output(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        self.output
            .forward(host, &self.accepted.grant().approval, &mut self.wire)
            .await?;
        let deadline = self.output.deadline().expect("delivered output retained");
        let backend = &mut self.backend;
        let output = &mut self.output;
        let wire = &mut self.wire;
        host.peer_io(Some(deadline), async {
            backend.received(deadline).await?;
            output.acknowledged(wire)
        })
        .await
    }

    /// Captures one finite closing budget and prevents all new application sends.
    pub fn begin_close(self, deadline: Instant) -> ClosingHostedPeer {
        let deadline = self
            .output
            .deadline()
            .map_or(deadline, |first| first.min(deadline));
        ClosingHostedPeer {
            wire: Some(self.wire),
            retired: None,
            backend: Some(self.backend.begin_close(deadline)),
            accepted: self.accepted,
            output: self.output,
            deadline,
            failure: None,
            settled: false,
            finished: false,
        }
    }
}

enum Event {
    External(bytes::Bytes),
    World,
    Ended,
}

impl ClosingHostedPeer {
    /// Drains accepted Go output with fresh original authority before each new external commit.
    /// Cancellation retains partial framing, the delivered obligation and its first deadline.
    /// Revocation permits only an already committed whole effect's receipt; partial output aborts
    /// the backend without reporting Flushed. No peer-Minecraft processing is promised.
    pub async fn finish_close(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        if self.finished {
            return self.result();
        }
        if self.failure.is_none()
            && let Err(error) = self.drain_backend(host).await
        {
            self.failure = Some(Arc::new(error));
        }
        if self.failure.is_some() && !self.settled {
            self.settle_output().await;
            self.backend = None;
            self.settled = true;
        }
        if self.retired.is_none() {
            self.retired = Some(self.wire.take().expect("one consuming close").begin_close());
        }
        let result = self
            .retired
            .as_mut()
            .expect("retired transport")
            .finish_close(self.deadline)
            .await;
        if result.is_ok() {
            self.finished = true;
        }
        self.result().and(result)
    }

    /// Resumes an existing delivery before reading another packet from the original backend slot.
    async fn drain_backend(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        loop {
            if let Some(deadline) = self.output.deadline() {
                let wire = self.wire.as_mut().expect("established until backend drain");
                self.output
                    .forward(host, &self.accepted.grant().approval, wire)
                    .await?;
                let backend = self.backend.as_mut().expect("live backend drain");
                let output = &mut self.output;
                host.peer_io(Some(deadline), async {
                    backend.received().await?;
                    output.acknowledged(wire)
                })
                .await?;
            }
            let Some(backend) = &mut self.backend else {
                return Ok(());
            };
            match host
                .peer_io(Some(self.deadline), backend.finish_close())
                .await
            {
                Ok(()) => {
                    self.backend = None;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let wire = self.wire.as_mut().expect("established until backend drain");
                    let output = &mut self.output;
                    let deadline = self.deadline;
                    host.peer_io(Some(deadline), async {
                        let packet = backend.recv().await?.ok_or_else(ended)?;
                        output.stage(wire, packet, deadline)
                    })
                    .await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Seals new admission and settles only the exact committed receipt without requiring new authority.
    async fn settle_output(&mut self) {
        let wire = self.wire.as_mut().expect("original wire until retirement");
        let result = wire.begin_send_retirement(self.deadline).finish().await;
        if matches!(result, Ok(NativeAcceptingSendOutcome::Complete))
            && self.output.deadline().is_some()
        {
            self.output.externally_complete();
            if let Some(backend) = &mut self.backend
                && backend.received().await.is_ok()
            {
                let _ = self.output.acknowledged(wire);
            }
        }
    }

    /// Keeps the first failure primary through later transport retirement and repeated completion.
    fn result(&self) -> io::Result<()> {
        self.failure
            .as_ref()
            .map_or(Ok(()), |error| Err(repeat(error)))
    }
}

/// One finite edge operation budget; idle play never uses this deadline.
fn cleanup_deadline() -> Instant {
    Instant::now() + Duration::from_millis(u64::from(CLOSE_MILLIS))
}

/// Ends a delivered-output path without exposing world identifiers.
fn ended() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "native world ended during output",
    )
}

/// Preserves the first failure's class and cause across explicit closing calls.
fn repeat(error: &Arc<io::Error>) -> io::Error {
    io::Error::new(error.kind(), Arc::clone(error))
}
