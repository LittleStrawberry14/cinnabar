//! Actual public Messaging connect/Login/pack/spawn consumer for the fixed visitor fixture.

use bedrock_net::messaging::PreparedMessaging;
use bedrock_packs::ResourcePackArchive;
use bedrock_services::test_support::HostedVisitor;
use protocol::{
    ClientRandomId, GameData, LoginSequence, LoginSettings, NativeAuthentication,
    NativeConnectionTarget, NativeMessagingTarget, PlaySession, SessionLogin, WorldEvent,
};
use std::io;
use uuid::Uuid;

pub(super) struct Client {
    target: NativeConnectionTarget,
    authentication: NativeAuthentication,
    name: &'static str,
    random_id: ClientRandomId,
}

impl Client {
    /// Attaches the original acknowledged nonce and actual service socket to the verified game proof.
    pub(super) fn new(
        service: HostedVisitor,
        host: Uuid,
        nonce: String,
        name: &'static str,
        random_id: ClientRandomId,
        authentication: NativeAuthentication,
    ) -> io::Result<Self> {
        let prepared = PreparedMessaging::new(service.socket, service.network_id, host)
            .map_err(io::Error::other)?;
        let target = NativeMessagingTarget::new(host, nonce)?;
        let connection = target.connection_target();
        let authentication = authentication.with_messaging(prepared, target)?;
        Ok(Self {
            target: connection,
            authentication,
            name,
            random_id,
        })
    }

    /// Uses the sole public transport/Login/pack/spawn pipeline; no accepted socket or Ready is injected.
    pub(super) async fn join(
        self,
        settings: &LoginSettings,
    ) -> io::Result<(PlaySession, Vec<ResourcePackArchive>, GameData)> {
        let (session, (packs, game)) = LoginSequence::connect_native_authenticated(
            &self.target,
            SessionLogin {
                display_name: self.name,
                client_random_id: self.random_id,
                cache: None,
                skin: None,
                settings,
                tap: None,
                target: None,
            },
            self.authentication,
            |packs, game| (packs.into_archives(), game),
        )
        .await
        .map_err(io::Error::other)?;
        Ok((session, packs, game))
    }
}

/// Requires a real generated radius reply and terrain from the same original Dragonfly connection.
pub(super) async fn radius(
    session: &mut PlaySession,
    radius: u8,
    terrain: &mut bool,
) -> io::Result<()> {
    session
        .send(protocol::request_chunk_radius_packet(radius, radius))
        .await
        .map_err(io::Error::other)?;
    let mut response = false;
    while !(*terrain && response) {
        match session
            .recv_world_event(0)
            .await
            .map_err(io::Error::other)?
        {
            WorldEvent::ChunkRadiusUpdated(actual) if actual == i32::from(radius) => {
                response = true
            }
            WorldEvent::LevelChunk(_) | WorldEvent::SubChunks(_) => *terrain = true,
            _ => {}
        }
    }
    Ok(())
}

/// Keeps cancellation/oneshot diagnostics free of account, world and service identifiers.
pub(super) fn ended() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "original visitor control ended")
}
