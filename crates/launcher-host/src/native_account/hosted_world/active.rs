//! One advertised host incarnation, driven by the application's existing reactor.

mod acquire;

use super::{NativeHostedProcess, NativeHostedWorld};
use bedrock_net::messaging::server::{ClosingMessagingServer, MessagingServer};
use std::{future::Future, io, time::Instant};
use tokio::sync::{mpsc, oneshot};

/// The existing peer driver borrows the host for its lifetime, so it admits one visitor.
pub(super) const VISITOR_LIMIT: usize = 1;

/// An invitation command cannot be retargeted to a replacement host incarnation.
#[derive(Clone)]
pub(crate) struct HostedInvitation(mpsc::Sender<Invite>);

struct Invite {
    xuid: String,
    deadline: Instant,
    result: oneshot::Sender<io::Result<()>>,
}

/// Actual acknowledged route, activity, transport and process belonging to one original host.
/// No runtime is created; keep `run` pinned on the existing application reactor.
#[must_use = "drive the active host and explicitly close its original membership"]
pub struct ActiveHostedWorld {
    world: NativeHostedWorld,
    server: MessagingServer,
    process: NativeHostedProcess,
    identity: protocol::NativeHostingIdentity,
    expires: Instant,
    peer_started: bool,
    invitations: mpsc::Receiver<Invite>,
    invitation: HostedInvitation,
}

/// Accepting and invitation capability ended; the same local and remote owners remain reachable.
#[must_use = "finish retiring the original listener, process and membership"]
pub struct ClosingHostedWorld {
    world: NativeHostedWorld,
    server: ClosingMessagingServer,
    process: NativeHostedProcess,
    deadline: Instant,
}

impl NativeHostedWorld {
    /// Configures the actual accepting route before this world's first original publication.
    /// Activity is acknowledged only after the same membership and process bridge are checked.
    /// Cancellation drops local IO and returns any dispatched remote obligation to its manager slot.
    pub async fn activate(mut self, deadline: Instant) -> io::Result<ActiveHostedWorld> {
        let deadline = self.directory_mut()?.activation_deadline(deadline)?;
        let acquire::Acquired {
            mut server,
            identity,
            messaging_id,
            expires,
        } = acquire::acceptor(&mut self, deadline).await?;
        self.directory_mut()?
            .configure_acceptor(&server, messaging_id, VISITOR_LIMIT)?;
        driving(&mut server, self.advance()).await?;
        driving(&mut server, self.publish_approvals(deadline)).await?;
        let process = driving(&mut server, self.prepare_dragonfly_bridge(deadline)).await?;
        driving(
            &mut server,
            self.directory_mut()?.publish_activity(deadline),
        )
        .await?;
        if Instant::now() >= expires {
            return Err(expired());
        }
        let (send, invitations) = mpsc::channel(1);
        Ok(ActiveHostedWorld {
            world: self,
            server,
            process,
            identity,
            expires,
            peer_started: false,
            invitations,
            invitation: HostedInvitation(send),
        })
    }
}

impl ActiveHostedWorld {
    /// Creates a command capability only from this actually acknowledged active owner.
    pub(crate) fn invitation(&self) -> HostedInvitation {
        self.invitation.clone()
    }

    /// Services original RTA/nonce changes, protected signaling and one serialized visitor.
    /// Token/TURN expiry gates new admission; a started handshake or Ready peer retains its lifetime.
    /// Cancellation during a peer phase ends further admission; consume this owner with `begin_close`.
    /// Dropping the whole owner returns unresolved membership through the sole manager slot.
    pub async fn run(&mut self) -> io::Result<()> {
        if self.peer_started {
            return Err(unavailable());
        }
        loop {
            if Instant::now() >= self.expires {
                return Err(expired());
            }
            enum Event {
                Service,
                Peer(nethernet_tokio::AcceptedSession),
                Invite(Option<Invite>),
            }
            let event = tokio::select! { biased;
                _ = tokio::time::sleep_until(self.expires.into()) => return Err(expired()),
                result = self.world.drive() => { result?; Event::Service },
                invite = self.invitations.recv() => Event::Invite(invite),
                accepted = self.server.accept() => Event::Peer(accepted.map_err(|_| unavailable())?),
            };
            match event {
                Event::Service => {
                    driving(
                        &mut self.server,
                        self.world.publish_approvals(operation_deadline()),
                    )
                    .await?;
                }
                Event::Invite(Some(invite)) => {
                    if invite.result.is_closed() {
                        continue;
                    }
                    let result = if Instant::now() >= self.expires {
                        Err(expired())
                    } else {
                        driving(
                            &mut self.server,
                            self.world
                                .directory_mut()?
                                .invite(&invite.xuid, invite.deadline),
                        )
                        .await
                    };
                    let _ = invite.result.send(result);
                }
                Event::Invite(None) => return Ok(()),
                Event::Peer(accepted) => {
                    if Instant::now() >= self.expires {
                        return Err(expired());
                    }
                    // A canceled peer phase requires closing this host, not admitting another peer.
                    self.peer_started = true;
                    {
                        let peer = serve(
                            &mut self.world,
                            &mut self.server,
                            &mut self.process,
                            &self.identity,
                            accepted,
                        );
                        tokio::pin!(peer);
                        loop {
                            tokio::select! { biased;
                                invite = self.invitations.recv() => {
                                    if let Some(invite) = invite { let _ = invite.result.send(Err(busy())); }
                                }
                                result = &mut peer => { result?; break; }
                            }
                        }
                    }
                    self.peer_started = false;
                }
            }
        }
    }

    /// Withdraws invite/accept capability synchronously and captures one original finite cleanup cutoff.
    pub fn begin_close(mut self, deadline: Instant) -> ClosingHostedWorld {
        self.invitations.close();
        while let Ok(invite) = self.invitations.try_recv() {
            let _ = invite.result.send(Err(unavailable()));
        }
        ClosingHostedWorld {
            world: self.world,
            server: self.server.begin_close(),
            process: self.process,
            deadline,
        }
    }
}

/// Retains the exact handshake/play/close continuation beside the same Messaging driver.
async fn serve(
    world: &mut NativeHostedWorld,
    server: &mut MessagingServer,
    process: &mut NativeHostedProcess,
    identity: &protocol::NativeHostingIdentity,
    accepted: nethernet_tokio::AcceptedSession,
) -> io::Result<()> {
    let deadline = operation_deadline();
    driving(server, world.publish_approvals(deadline)).await?;
    let peer = driving(
        server,
        world.accept_dragonfly_peer(
            process,
            accepted,
            identity.signing_key(),
            Default::default(),
            deadline,
        ),
    )
    .await?;
    driving(server, world.directory_mut()?.visitor(true, deadline)).await?;
    driving(server, world.publish_approvals(deadline)).await?;
    let (mut closing, outcome) = driving(server, async { Ok(peer.run(world).await) }).await?;
    let closed = driving(server, async { Ok(closing.finish_close(world).await) }).await?;
    if !closing.is_finished() {
        return closed.and(Err(unavailable()));
    }
    if let Err(error) = outcome.and(closed) {
        tracing::debug!(kind = ?error.kind(), "native hosted peer ended");
    }
    drop(closing);
    let deadline = operation_deadline();
    driving(server, world.directory_mut()?.visitor(false, deadline)).await?;
    driving(server, world.publish_approvals(deadline)).await
}

impl ClosingHostedWorld {
    /// Attempts every original owner, retaining failed/ambiguous membership for explicit retry or Drop.
    /// Repeated calls use the same first cutoff; no success is inferred from physical socket destruction.
    pub async fn finish_close(&mut self) -> io::Result<()> {
        let listener = bounded(self.deadline, self.server.finish_close())
            .await
            .and_then(|r| r.map_err(|_| unavailable()));
        let process = self.process.close(self.deadline).await;
        let directory = self.world.close(self.deadline).await;
        listener.and(process).and(directory)
    }
}

impl HostedInvitation {
    /// Sends once to the exact active owner; cancellation never retries an ambiguous HTTP dispatch.
    pub(crate) async fn send(&self, xuid: &str, deadline: Instant) -> io::Result<()> {
        let (result, receive) = oneshot::channel();
        bounded(
            deadline,
            self.0.send(Invite {
                xuid: xuid.to_owned(),
                deadline,
                result,
            }),
        )
        .await?
        .map_err(|_| unavailable())?;
        bounded(deadline, receive)
            .await?
            .map_err(|_| unavailable())?
    }
}

/// Keeps the actual application future pinned through signaling progress; no operation is replayed.
async fn driving<T>(
    server: &mut MessagingServer,
    future: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    tokio::pin!(future);
    tokio::select! { biased;
        result = server.maintain() => { result.map_err(|_| unavailable())?; Err(unavailable()) },
        result = &mut future => result,
    }
}

/// Bounds one edge operation without imposing an idle deadline on play.
fn operation_deadline() -> Instant {
    Instant::now() + super::super::ACQUISITION_TIMEOUT
}

/// Applies an original caller cutoff to acquisition/retirement without renewing it.
async fn bounded<T>(deadline: Instant, future: impl Future<Output = T>) -> io::Result<T> {
    if Instant::now() >= deadline {
        return Err(expired());
    }
    let result = tokio::time::timeout_at(deadline.into(), future)
        .await
        .map_err(|_| expired())?;
    if Instant::now() >= deadline {
        return Err(expired());
    }
    Ok(result)
}

/// Redacts account, service, world and transport identifiers.
fn unavailable() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "native hosting is unavailable")
}
/// Stops new admission when the original proof or relay lease expires.
fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "native hosting admission expired")
}
/// Refuses additional invitation dispatch while the sole visitor owns capacity.
fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "native hosted visitor capacity is occupied",
    )
}
