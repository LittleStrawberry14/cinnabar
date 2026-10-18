//! Native direct joins using the shared Rust spawn and play sequence.

mod authentication;
pub use authentication::{NativeAuthentication, NativeCredential, NativeTokenVerifier};
mod connection;
mod packs;
mod transport;
pub(crate) use connection::{Connection, PacketConnection};

use super::{JoinPreparation, LoginSequence, NativeJoinPreparation, PlaySession, SessionLogin};
use crate::codec::SessionFailure;
use crate::session_transport::{MAX_READ_AHEAD_BYTES, SessionTransport};
use crate::{GameData, ProtocolError};
use crate::{NativeConnectionTarget, NativeTransport};
use bedrock_client::{
    handshake::{ClientHandshake, HandshakeError, HandshakeEvent, HandshakeOptions},
    packs::PackNegotiation,
};
use bedrock_net::login::OfflineIdentity;
use bedrock_packs::ResourcePackHandoff;
use bedrock_protocol_1_26_51::packets;
use p384::{ecdsa::SigningKey, elliptic_curve::Generate};
use rand::rngs::SysRng;
use serde_json::{json, value::RawValue};
use std::{
    io,
    sync::Arc,
    time::{Duration, SystemTime},
};

const JOIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Retains a decoded negotiation boundary when a response write fails.
async fn preserve_negotiation_boundary(
    connection: &mut impl PacketConnection,
    error: io::Error,
) -> ProtocolError {
    let drain = async {
        let limit = bedrock_net::BatchLimits::default().max_decoded_bytes;
        let mut retained = 0_usize;
        loop {
            let body = connection.recv_body().await.ok()?;
            retained = retained.checked_add(body.len())?;
            if retained > limit {
                return None;
            }
            for raw in crate::raw_packet::decode_batch_body(body, limit).ok()? {
                if matches!(
                    raw.id,
                    packets::PacketId::Transfer | packets::PacketId::Disconnect
                ) {
                    let packet = raw.decode().ok()?;
                    match negotiation_boundary(*packet.data) {
                        Ok(Some(error)) => return Some(error),
                        Err(reason) => tracing::warn!(
                            ?reason,
                            "skipped unusable transfer while draining negotiation"
                        ),
                        Ok(None) => {}
                    }
                }
            }
        }
    };
    match tokio::time::timeout(super::WRITE_FAILURE_DRAIN, drain).await {
        Ok(Some(boundary)) => boundary,
        _ => failure(error),
    }
}

/// Returns only a validated redirect or the server's decoded disconnect reason.
fn negotiation_boundary(
    packet: packets::Packet,
) -> Result<Option<ProtocolError>, crate::ServerTransferRejection> {
    if let packets::Packet::Disconnect(packet) = packet {
        return Ok(Some(disconnected(packet)));
    }
    Ok(crate::ServerTransferEvent::from_packet_data(&packet)?
        .map(|target| SessionFailure::ServerTransfer(target).into()))
}

#[cfg(test)]
mod tests;

/// Classifies a decoded negotiation packet that did not advance the current phase.
fn negotiation_failure(
    packet: packets::Packet,
) -> Result<ProtocolError, crate::ServerTransferRejection> {
    let id = packet.id();
    if let Some(boundary) = negotiation_boundary(packet)? {
        return Ok(boundary);
    }
    Ok(failure(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected negotiation packet: {id:?}"),
    )))
}

impl LoginSequence {
    /// Joins directly without a Go process. The timeout includes transport, Login,
    /// resource-pack transfer and spawn; dropping the future cancels the connection.
    pub async fn connect_native<P: Send + 'static>(
        target: &NativeConnectionTarget,
        login: SessionLogin<'_>,
        prepare: impl FnOnce(ResourcePackHandoff, GameData) -> P + Send + 'static,
    ) -> Result<(PlaySession, P), ProtocolError> {
        native_join(target, login, None, prepare).await
    }

    /// Joins RakNet or explicitly prepared Player Messaging with an acquired account credential.
    ///
    /// Account HTTP and persistence stay with the application. The connection
    /// retains this key through signed client data and the server key exchange.
    /// Player Messaging additionally requires a protected socket and joined-session target.
    ///
    /// # Errors
    /// Rejects unsupported transport or invalid framing, and returns connection,
    /// Login, pack, spawn or overall join-deadline failures.
    pub async fn connect_native_authenticated<P: Send + 'static>(
        target: &NativeConnectionTarget,
        login: SessionLogin<'_>,
        authentication: NativeAuthentication,
        prepare: impl FnOnce(ResourcePackHandoff, GameData) -> P + Send + 'static,
    ) -> Result<(PlaySession, P), ProtocolError> {
        if target.transport == NativeTransport::NetherNetLan {
            return Err(failure(io::Error::new(
                io::ErrorKind::Unsupported,
                "native account joins require RakNet or prepared signed Player Messaging",
            )));
        }
        native_join(target, login, Some(authentication), prepare).await
    }
}

/// Applies the same cancellation and join deadline to either explicit identity.
async fn native_join<P: Send + 'static>(
    target: &NativeConnectionTarget,
    login: SessionLogin<'_>,
    authentication: Option<NativeAuthentication>,
    prepare: impl FnOnce(ResourcePackHandoff, GameData) -> P + Send + 'static,
) -> Result<(PlaySession, P), ProtocolError> {
    tokio::time::timeout(
        JOIN_TIMEOUT,
        connect(target, login, authentication, prepare),
    )
    .await
    .map_err(|_| {
        failure(io::Error::new(
            io::ErrorKind::TimedOut,
            "native join timed out",
        ))
    })?
}

/// Negotiates one native connection before handing the same owner to play.
async fn connect<P: Send + 'static>(
    target: &NativeConnectionTarget,
    login: SessionLogin<'_>,
    authentication: Option<NativeAuthentication>,
    prepare: impl FnOnce(ResourcePackHandoff, GameData) -> P + Send + 'static,
) -> Result<(PlaySession, P), ProtocolError> {
    if login.target.is_some() {
        return Err(failure(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native join does not accept a core target override",
        )));
    }
    let (key, authentication, messaging) = match authentication {
        Some(authentication) => {
            authentication::ensure_current(authentication.expires, std::time::Instant::now())
                .map_err(ProtocolError::Authentication)?;
            let handoff = authentication
                .into_handoff()
                .map_err(ProtocolError::Authentication)?;
            (
                handoff.key,
                Some((handoff.token, handoff.claims, handoff.expires)),
                handoff.messaging,
            )
        }
        None => (
            std::sync::Arc::new(
                SigningKey::try_generate_from_rng(&mut SysRng)
                    .map_err(io::Error::other)
                    .map_err(failure)?,
            ),
            None,
            None,
        ),
    };
    let transport = transport::Transport::connect(target, messaging)
        .await
        .map_err(|error| {
            if authentication::is_expired(&error) {
                ProtocolError::Authentication(error)
            } else {
                failure(error)
            }
        })?;
    let mut connection = Connection::new(transport);
    let mut handshake = ClientHandshake::new(
        key,
        HandshakeOptions {
            batch_encryption: connection.transport.batch_encryption(),
            blob_cache: login.cache.is_some(),
            max_login_bytes: connection.max_decoded_bytes(),
            ..Default::default()
        },
    )
    .map_err(handshake_failure)?;
    connection
        .send(handshake.start().map_err(handshake_failure)?)
        .await?;
    loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        let event = handshake.receive(*packet.data).map_err(handshake_failure)?;
        match event {
            HandshakeEvent::Compression(settings) => {
                connection.enable_compression(settings);
                break;
            }
            event => {
                if apply_handshake(&mut connection, event).await?.is_some() {
                    return Err(failure(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "resource offer preceded Login",
                    )));
                }
            }
        }
    }
    let (mut claims, token) = if let Some((token, claims, expires)) = authentication {
        (serde_json::Value::Object(claims), Some((token, expires)))
    } else {
        let mut claims = super::client_data::login_client_data(
            login.display_name,
            login.client_random_id,
            login.skin.as_ref(),
            login.settings,
        );
        // Signed-out desktop identity is independent of Xbox title/device authentication.
        claims["DeviceOS"] = json!(native_device_os());
        claims["DeviceModel"] = json!("");
        claims["DeviceId"] = json!(uuid::Uuid::new_v4().to_string());
        claims["DefaultInputMode"] = claims["CurrentInputMode"].clone();
        (claims, None)
    };
    claims["ServerAddress"] = json!(target.address);
    claims["Nonce"] = json!(connection.transport.nonce());
    let claims = RawValue::from_string(
        serde_json::to_string(&claims)
            .map_err(io::Error::other)
            .map_err(failure)?,
    )
    .map_err(io::Error::other)
    .map_err(failure)?;
    let deadline = token.as_ref().map(|(_, expires)| *expires);
    let packet = if let Some((token, expires)) = token {
        authentication::ensure_current(expires, std::time::Instant::now())
            .map_err(ProtocolError::Authentication)?;
        handshake.login_authenticated(&token, &claims)
    } else {
        let identity = uuid::Uuid::new_v4().to_string();
        handshake.login_offline(
            &OfflineIdentity {
                display_name: login.display_name,
                identity: &identity,
                xuid: "",
                playfab_id: "",
                playfab_title_id: "",
            },
            &claims,
            SystemTime::now(),
        )
    }
    .map_err(handshake_failure)?;
    match deadline {
        Some(deadline) => connection.send_before(packet, deadline).await?,
        None => connection.send(packet).await?,
    }
    let negotiation = loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        let event = handshake.receive(*packet.data).map_err(handshake_failure)?;
        if let Some(negotiation) = apply_handshake(&mut connection, event).await? {
            break negotiation;
        }
    };
    let packs = packs::acquire(
        &mut connection,
        &mut handshake,
        negotiation,
        login.settings.resource_pack_cache_dir.as_deref(),
    )
    .await?;
    let mut transport = SessionTransport::from_native(connection).with_tap(login.tap);
    let preparation = NativeJoinPreparation;
    let mut prepared_job = None;
    let (initialization, spawned) = super::startup::await_spawn(
        &mut transport,
        login.settings.view_distance_chunks,
        |start, items| {
            prepared_job = Some(
                preparation.submit(move || prepare(packs, GameData::from_wire(&start, &items))),
            );
        },
    )
    .await?;
    let job = prepared_job.expect("spawn requires StartGame and item registry");
    tokio::pin!(job);
    let prepared = tokio::select! {
        value = &mut job => value,
        () = transport.read_ahead(MAX_READ_AHEAD_BYTES) => job.await,
    };
    Ok((
        PlaySession::new(
            transport,
            login.cache,
            initialization,
            spawned.terrain_received,
        ),
        prepared,
    ))
}

/// Applies ordered pure-core effects on the retained connection without restarting its FIFO.
async fn apply_handshake(
    connection: &mut Connection,
    event: HandshakeEvent,
) -> Result<Option<PackNegotiation>, ProtocolError> {
    match event {
        HandshakeEvent::Send(packets) => packs::send_packets(connection, packets).await?,
        HandshakeEvent::KeyExchange { ciphers, response } => {
            if let Some(ciphers) = ciphers {
                connection.enable_encryption(*ciphers).map_err(failure)?;
            }
            connection.send(response).await?;
        }
        HandshakeEvent::Packs {
            cache_status,
            negotiation,
        } => {
            if let Some(status) = cache_status {
                connection.send(status).await?;
            }
            return Ok(Some(negotiation));
        }
        HandshakeEvent::Unhandled(packet) => {
            if let Some(error) = connection.negotiation_failure(*packet) {
                return Err(error);
            }
        }
        _ => {
            return Err(failure(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected client handshake effect",
            )));
        }
    }
    Ok(None)
}

/// Keeps the reusable handshake failure as a typed cause of the native connection error.
fn handshake_failure(error: HandshakeError) -> ProtocolError {
    let kind = if matches!(error, HandshakeError::LoginRefused(_)) {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::InvalidData
    };
    failure(io::Error::new(kind, error))
}

/// Preserves the server's disconnect text through the existing startup error model.
fn disconnected(packet: packets::Disconnect) -> ProtocolError {
    let (message, filtered) = crate::disconnect::packet_texts(&packet);
    crate::disconnect::join_disconnect("native Login", &packet.reason, &message, &filtered).into()
}

/// Attaches the native transport error without labelling it as malformed packet input.
fn failure(error: io::Error) -> ProtocolError {
    SessionFailure::NativeTransport(error).into()
}

/// Reports the actual host family for a signed-out connection, independently of OAuth titles.
fn native_device_os() -> i32 {
    if cfg!(target_os = "android") {
        1
    } else if cfg!(target_os = "ios") {
        2
    } else if cfg!(target_os = "macos") {
        3
    } else if cfg!(target_os = "windows") {
        8
    } else {
        15
    }
}
