//! One concrete paired play lifetime, owned and polled by the existing application worker.

use super::{HostedLoginApproval, NativeHostedWorld};
use bedrock_client::server::session::AcceptedSession;
use bridge::host_world::{ActiveBackendPeer, ClosingBackendPeer, contract::CLOSE_MILLIS};
use protocol::{NativeAcceptingIo, NativeAcceptingRetired};
use std::{
    io,
    time::{Duration, Instant},
};

/// Actual initialized Login, original hosted approval, external codec and Go world connection.
/// No worker is spawned. The caller retains this owner and polls it on its existing runtime.
#[must_use]
pub struct NativeHostedPeer {
    wire: NativeAcceptingIo,
    backend: ActiveBackendPeer,
    accepted: AcceptedSession<HostedLoginApproval>,
}

/// New play admission ended; original backend output can finish before actual transport retirement.
#[must_use]
pub struct ClosingHostedPeer {
    wire: Option<NativeAcceptingIo>,
    retired: Option<NativeAcceptingRetired>,
    backend: ClosingBackendPeer,
    _accepted: AcceptedSession<HostedLoginApproval>,
    deadline: Instant,
    backend_done: bool,
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
        (self.begin_close(cleanup_deadline()), result)
    }

    /// Relays one original raw packet at a time while retaining controls and opposite-direction output.
    async fn play(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        loop {
            host.recheck_approval(&self.accepted.grant().approval)?;
            let wire = &mut self.wire;
            let backend = &mut self.backend;
            let event = host
                .peer_io(None, async {
                    tokio::select! {
                        packet = wire.receive_play() => packet.map(Event::External),
                        packet = backend.recv() => packet.map(Event::World),
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
                Event::World(Some(packet)) => {
                    let deadline = cleanup_deadline();
                    host.peer_io(Some(deadline), self.wire.send_raw(packet, deadline))
                        .await?;
                    host.recheck_approval(&self.accepted.grant().approval)?;
                    host.peer_io(Some(deadline), self.backend.received(deadline))
                        .await?;
                }
                Event::World(None) => return Ok(()),
            }
        }
    }

    /// Dispatches the sole retained opposite-direction payload before resuming its pending receipt.
    async fn world_output(&mut self, host: &mut NativeHostedWorld) -> io::Result<()> {
        host.recheck_approval(&self.accepted.grant().approval)?;
        let packet = host
            .peer_io(None, self.backend.recv())
            .await?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "native world ended during output",
                )
            })?;
        host.recheck_approval(&self.accepted.grant().approval)?;
        let deadline = cleanup_deadline();
        host.peer_io(Some(deadline), self.wire.send_raw(packet, deadline))
            .await?;
        host.recheck_approval(&self.accepted.grant().approval)?;
        host.peer_io(Some(deadline), self.backend.received(deadline))
            .await
    }

    /// Captures one finite closing budget and prevents all new application sends.
    pub fn begin_close(self, deadline: Instant) -> ClosingHostedPeer {
        ClosingHostedPeer {
            wire: Some(self.wire),
            retired: None,
            backend: self.backend.begin_close(deadline),
            _accepted: self.accepted,
            deadline,
            backend_done: false,
            finished: false,
        }
    }
}

enum Event {
    External(bytes::Bytes),
    World(Option<bytes::Bytes>),
}

impl ClosingHostedPeer {
    /// Drains already accepted Go output, exchanges final receipts/Close, then retires the transport.
    /// Cancellation retains partial framing and the same deadline. Repeating completed close is safe.
    /// Expiry or physical abort may discard unread input; no peer-Minecraft processing is promised.
    pub async fn finish_close(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        while !self.backend_done {
            match self.backend.finish_close().await {
                Ok(()) => self.backend_done = true,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let packet = self.backend.recv().await?.ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "native world closed before final receipt",
                        )
                    })?;
                    self.wire
                        .as_mut()
                        .expect("established until backend drain")
                        .send_raw(packet, self.deadline)
                        .await?;
                    self.backend.received().await?;
                }
                Err(error) => return Err(error),
            }
        }
        if self.retired.is_none() {
            self.retired = Some(self.wire.take().expect("one consuming close").begin_close());
        }
        self.retired
            .as_mut()
            .expect("retired transport")
            .finish_close(self.deadline)
            .await?;
        self.finished = true;
        Ok(())
    }
}

/// One finite edge operation budget; idle play never uses this deadline.
fn cleanup_deadline() -> Instant {
    Instant::now() + Duration::from_millis(u64::from(CLOSE_MILLIS))
}
