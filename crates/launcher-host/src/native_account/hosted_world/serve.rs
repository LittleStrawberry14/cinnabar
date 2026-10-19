//! Actual accepting Login/pack/world continuations paired with the running Go world.

mod play;
pub use play::{ClosingHostedPeer, NativeHostedPeer};
mod process;
pub use process::NativeHostedProcess;

use super::{HostedApproval, HostedLoginApproval, NativeHostedLoginPolicy, NativeHostedWorld};
use bedrock_client::server::{
    ServerAcknowledgmentEvent, ServerApprovalEvent, ServerHandshake, ServerHandshakeOptions,
    ServerLoginEvent, ServerPacksReady, ServerSettingsEvent,
    session::{ServerPackDelivery, ServerPackEvent, ServerSpawnEvent},
};
use bridge::host_world::{
    ActiveBackendPeer, BackendWorldReady, HostProcess, PendingBackendPeer, contract::Identity,
};
use p384::ecdsa::SigningKey;
use protocol::{NativeAcceptingIo, NativeHostWorld, native_host_packs};
use std::{io, sync::Arc, time::Instant};

impl NativeHostedWorld {
    /// Applies current service state and original membership before synchronous Attach admission.
    /// Cryptographic Login and typed ServerWorldReady remain with the calling pack owner.
    pub(super) async fn attach_backend(
        &mut self,
        approval: &HostedApproval,
        backend: PendingBackendPeer,
        deadline: Instant,
    ) -> io::Result<BackendWorldReady> {
        self.directory_mut()?
            .service_barrier(Some(deadline))
            .await?;
        self.recheck_approval(approval)?;
        let backend = backend.begin_attach(deadline)?;
        let backend = self.peer_io(Some(deadline), backend.finish()).await?;
        self.recheck_approval(approval)?;
        Ok(backend)
    }

    /// Applies current service state and original membership before synchronous Initialized admission.
    /// Cryptographic Login and typed ServerInitialized::finish remain with the caller.
    pub(super) async fn initialize_backend(
        &mut self,
        approval: &HostedApproval,
        backend: BackendWorldReady,
        cache_enabled: bool,
        radius: u32,
        deadline: Instant,
    ) -> io::Result<ActiveBackendPeer> {
        self.directory_mut()?
            .service_barrier(Some(deadline))
            .await?;
        self.recheck_approval(approval)?;
        let backend = backend.begin_initialized(cache_enabled, radius, deadline)?;
        let backend = self.peer_io(Some(deadline), backend.finish()).await?;
        self.recheck_approval(approval)?;
        Ok(backend)
    }

    /// Pairs one actual established transport with this original acknowledged hosted opening.
    ///
    /// The caller owns the server's authenticated signaling/SDP policy and passes that exact
    /// server key allocation. This does not start a listener, runtime, account worker or advertise
    /// a route. Keep this whole future pinned; canceling it closes its actual transport/backend.
    /// The existing host owner drives RTA and retains remote membership independently of the peer.
    pub async fn accept_dragonfly_peer(
        &mut self,
        process: &mut NativeHostedProcess,
        accepted: nethernet_tokio::AcceptedSession,
        key: Arc<SigningKey>,
        options: ServerHandshakeOptions,
        deadline: Instant,
    ) -> io::Result<NativeHostedPeer> {
        self.check_bridge(process)?;
        let policy = self.login_policy(deadline).await?;
        let (mut wire, handshake) = self
            .peer_io(
                Some(deadline),
                NativeAcceptingIo::from_nether(accepted, key, options, deadline),
            )
            .await?;
        let ready = self.login(&mut wire, handshake, &policy, deadline).await?;
        self.recheck_login(ready.login(), &policy, ready.grant())?;
        let player = ready.login().player_identity().ok_or_else(rejected)?;
        let identity = Identity {
            xuid: ready.login().xuid().into(),
            uuid: player.uuid().to_string(),
            display_name: player.display_name().into(),
            playfab_id: player.playfab_id().into(),
            playfab_title_id: player.playfab_title_id().into(),
        };
        // RawValue owns the exact provider-verified object, including unused lexemes and fields.
        let data = ready.login().client_data().to_owned();
        let backend = self
            .peer_io(
                Some(deadline),
                process.process.open_peer(identity, data, deadline),
            )
            .await?;
        self.recheck_login(ready.login(), &policy, ready.grant())?;
        self.check_bridge(process)?;
        self.packs_and_world(wire, ready, backend, &process.process, policy, deadline)
            .await
    }

    /// Polls the one existing directory owner beside this exact retained IO operation.
    pub(super) async fn peer_io<T>(
        &mut self,
        deadline: Option<Instant>,
        future: impl std::future::Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        self.directory_mut()?.peer_io(deadline, future).await
    }

    /// Executes only the reusable typed accepting handshake, with exact flush continuations.
    async fn login(
        &mut self,
        wire: &mut NativeAcceptingIo,
        mut handshake: ServerHandshake,
        policy: &NativeHostedLoginPolicy,
        deadline: Instant,
    ) -> io::Result<ServerPacksReady<HostedLoginApproval>> {
        let mut login = loop {
            let packet = self.peer_io(Some(deadline), wire.receive(deadline)).await?;
            self.directory_ref()?.check_login_policy(policy)?;
            match handshake
                .receive(
                    packet,
                    policy,
                    Instant::now(),
                    policy.utc().map_err(|_| rejected())?,
                )
                .map_err(|_| rejected())?
            {
                ServerSettingsEvent::Settings(pending) => {
                    self.peer_io(
                        Some(deadline),
                        wire.send(vec![pending.packet().clone().into()], deadline),
                    )
                    .await?;
                    self.directory_ref()?.check_login_policy(policy)?;
                    wire.compression_flushed(pending.compression())?;
                    break pending.flushed(Instant::now()).map_err(|_| rejected())?;
                }
                ServerSettingsEvent::Refused(status) => {
                    self.peer_io(Some(deadline), wire.send(vec![status.into()], deadline))
                        .await?;
                    return Err(rejected());
                }
                ServerSettingsEvent::Unhandled {
                    handshake: retained,
                    packet,
                } => {
                    wire.handle_unhandled(&packet)?;
                    handshake = retained;
                }
            }
        };
        let verified = loop {
            let packet = self.peer_io(Some(deadline), wire.receive(deadline)).await?;
            self.directory_ref()?.check_login_policy(policy)?;
            match login
                .receive(
                    packet,
                    policy,
                    Instant::now(),
                    policy.utc().map_err(|_| rejected())?,
                )
                .map_err(|_| rejected())?
            {
                ServerLoginEvent::Verified(verified) => break verified,
                ServerLoginEvent::Unhandled {
                    login: retained,
                    packet,
                } => {
                    wire.handle_unhandled(&packet)?;
                    login = retained;
                }
            }
        };
        let approved = verified
            .authorize(|login| self.approve_login(login, policy))
            .map_err(|_| rejected())?;
        self.recheck_login(approved.login(), policy, approved.grant())?;
        let approval = approved
            .accept(
                policy,
                None,
                Instant::now(),
                policy.utc().map_err(|_| rejected())?,
            )
            .map_err(|_| rejected())?;
        let mut acknowledgment = match approval {
            ServerApprovalEvent::Ready(ready) => return Ok(ready),
            ServerApprovalEvent::KeyExchange(pending) => {
                self.recheck_login_grant(policy, pending.grant())?;
                self.peer_io(
                    Some(deadline),
                    wire.send(vec![pending.packet().clone().into()], deadline),
                )
                .await?;
                self.recheck_login_grant(policy, pending.grant())?;
                let (acknowledgment, ciphers) =
                    pending.flushed(Instant::now()).map_err(|_| rejected())?;
                wire.encryption_flushed(*ciphers)?;
                acknowledgment
            }
        };
        loop {
            let packet = self.peer_io(Some(deadline), wire.receive(deadline)).await?;
            self.recheck_login_grant(policy, acknowledgment.grant())?;
            match acknowledgment
                .receive(
                    packet,
                    policy,
                    Instant::now(),
                    policy.utc().map_err(|_| rejected())?,
                )
                .map_err(|_| rejected())?
            {
                ServerAcknowledgmentEvent::Ready(ready) => return Ok(ready),
                ServerAcknowledgmentEvent::Unhandled {
                    acknowledgment: retained,
                    packet,
                } => {
                    wire.handle_unhandled(&packet)?;
                    acknowledgment = retained;
                }
            }
        }
    }

    /// Keeps the actual pack continuation/source lease until the real client runtime-ID handoff.
    async fn packs_and_world(
        &mut self,
        mut wire: NativeAcceptingIo,
        ready: ServerPacksReady<HostedLoginApproval>,
        mut backend: PendingBackendPeer,
        process: &HostProcess,
        policy: NativeHostedLoginPolicy,
        deadline: Instant,
    ) -> io::Result<NativeHostedPeer> {
        self.recheck_login(ready.login(), &policy, ready.grant())?;
        let snapshot = native_host_packs(process.snapshot())?;
        let delivery = ServerPackDelivery {
            chunk_size: bridge::host_world::contract::MAX_PACK_READ_BYTES,
            ..Default::default()
        };
        let (mut session, packets) = ready
            .start_packs(
                snapshot,
                delivery,
                &policy,
                Instant::now(),
                policy.utc().map_err(|_| rejected())?,
            )
            .map_err(|_| rejected())?;
        self.peer_io(Some(deadline), wire.send(packets, deadline))
            .await?;
        let ready = loop {
            self.recheck_login(session.login(), &policy, session.grant())?;
            let packet = self.peer_io(Some(deadline), wire.receive(deadline)).await?;
            self.recheck_login(session.login(), &policy, session.grant())?;
            match session
                .receive(packet, Instant::now())
                .map_err(|_| rejected())?
            {
                ServerPackEvent::Send {
                    session: retained,
                    packets,
                } => {
                    self.recheck_login(retained.login(), &policy, retained.grant())?;
                    self.peer_io(Some(deadline), wire.send(packets, deadline))
                        .await?;
                    session = retained;
                }
                ServerPackEvent::ReadChunk(read) => {
                    self.recheck_login(read.login(), &policy, read.grant())?;
                    let archive = u32::try_from(read.archive_index()).map_err(|_| rejected())?;
                    let length = u32::try_from(read.byte_len()).map_err(|_| rejected())?;
                    let bytes = self
                        .peer_io(
                            Some(deadline),
                            backend.read_pack(archive, read.offset(), length, deadline),
                        )
                        .await?;
                    self.recheck_login(read.login(), &policy, read.grant())?;
                    let pending = read
                        .supply(bytes.to_vec(), Instant::now())
                        .map_err(|_| rejected())?;
                    self.recheck_login(pending.login(), &policy, pending.grant())?;
                    self.peer_io(
                        Some(deadline),
                        wire.send(vec![pending.packet().clone().into()], deadline),
                    )
                    .await?;
                    self.recheck_login(pending.login(), &policy, pending.grant())?;
                    let (retained, packets) =
                        pending.flushed(Instant::now()).map_err(|_| rejected())?;
                    self.peer_io(Some(deadline), wire.send(packets, deadline))
                        .await?;
                    session = retained;
                }
                ServerPackEvent::Chunk(pending) => {
                    self.recheck_login(pending.login(), &policy, pending.grant())?;
                    self.peer_io(
                        Some(deadline),
                        wire.send(vec![pending.packet().clone().into()], deadline),
                    )
                    .await?;
                    self.recheck_login(pending.login(), &policy, pending.grant())?;
                    let (retained, packets) =
                        pending.flushed(Instant::now()).map_err(|_| rejected())?;
                    self.peer_io(Some(deadline), wire.send(packets, deadline))
                        .await?;
                    session = retained;
                }
                ServerPackEvent::WorldReady(ready) => break ready,
                ServerPackEvent::Unhandled {
                    session: retained,
                    packet,
                } => {
                    wire.handle_unhandled(&packet)?;
                    session = retained;
                }
            }
        };
        self.recheck_login(ready.login(), &policy, ready.grant())?;
        let backend = self
            .attach_backend(&ready.grant().approval, backend, deadline)
            .await?;
        self.recheck_login(ready.login(), &policy, ready.grant())?;
        let (mut packets, world) = NativeHostWorld::parse(backend.world())?.into_parts();
        let (mut spawning, startup) = ready
            .start_world(
                world,
                &policy,
                Instant::now(),
                policy.utc().map_err(|_| rejected())?,
            )
            .map_err(|_| rejected())?;
        packets.extend(startup);
        self.peer_io(Some(deadline), wire.send(packets, deadline))
            .await?;
        let initialized = loop {
            self.recheck_login(spawning.login(), &policy, spawning.grant())?;
            let packet = self.peer_io(Some(deadline), wire.receive(deadline)).await?;
            self.recheck_login(spawning.login(), &policy, spawning.grant())?;
            match spawning
                .receive(packet, Instant::now())
                .map_err(|_| rejected())?
            {
                ServerSpawnEvent::Send { session, packets } => {
                    self.recheck_login(session.login(), &policy, session.grant())?;
                    self.peer_io(Some(deadline), wire.send(packets, deadline))
                        .await?;
                    spawning = session;
                }
                ServerSpawnEvent::Initialized(initialized) => break initialized,
                ServerSpawnEvent::Unhandled { session, packet } => {
                    wire.handle_unhandled(&packet)?;
                    spawning = session;
                }
            }
        };
        self.recheck_login(initialized.login(), &policy, initialized.grant())?;
        let accepted = initialized
            .finish(
                &policy,
                Instant::now(),
                policy.utc().map_err(|_| rejected())?,
            )
            .map_err(|_| rejected())?;
        let radius = u32::try_from(accepted.chunk_radius()).map_err(|_| rejected())?;
        let backend = self
            .initialize_backend(
                &accepted.grant().approval,
                backend,
                accepted.client_cache_enabled(),
                radius,
                deadline,
            )
            .await?;
        self.recheck_login(accepted.login(), &policy, accepted.grant())?;
        Ok(NativeHostedPeer::new(wire, backend, accepted))
    }
}

/// Does not format provider tokens, signed client data or hosted-world identifiers.
fn rejected() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native hosted peer rejected",
    )
}
