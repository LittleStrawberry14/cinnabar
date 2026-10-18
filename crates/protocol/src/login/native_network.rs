//! Native direct joins using the shared Rust spawn and play sequence.

mod authentication;
pub use authentication::NativeAuthentication;
mod connection;
mod packs;
mod transport;
pub(crate) use connection::{Connection, PacketConnection};

use super::{JoinPreparation, LoginSequence, NativeJoinPreparation, PlaySession, SessionLogin};
use crate::codec::SessionFailure;
use crate::session_transport::{MAX_READ_AHEAD_BYTES, SessionTransport};
use crate::{GameData, ProtocolError};
use crate::{NativeConnectionTarget, NativeTransport};
use bedrock_net::login::{
    OfflineIdentity, encode_authenticated_request, encode_offline_request, finish_key_exchange,
};
use bedrock_net::{CompressionAlgorithm, CompressionSettings};
use bedrock_protocol_1_26_51::{enums::PlayStatusType, packets, wire};
use p384::{ecdsa::SigningKey, elliptic_curve::Generate};
use rand::rngs::SysRng;
use resource_pack::ResourcePackHandoff;
use serde_json::{json, value::RawValue};
use std::{
    io,
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

    /// Joins RakNet with an already acquired account credential and its owned proof key.
    ///
    /// Account HTTP and persistence stay with the application. The connection
    /// retains this key through signed client data and the server key exchange.
    /// Signed identity-aware NetherNet signaling is not yet supported here.
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
        if target.transport != NativeTransport::RakNet {
            return Err(failure(io::Error::new(
                io::ErrorKind::Unsupported,
                "native account joins require RakNet until signed NetherNet signaling is available",
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
    let (key, authentication) = match authentication {
        Some(NativeAuthentication { key, token, claims }) => (key, Some((token, claims))),
        None => (
            SigningKey::try_generate_from_rng(&mut SysRng)
                .map_err(io::Error::other)
                .map_err(failure)?,
            None,
        ),
    };
    let transport = transport::Transport::connect(target, key)
        .await
        .map_err(failure)?;
    let mut connection = Connection::new(transport);
    connection
        .send(packets::RequestNetworkSettings {
            client_network_version: wire::I32BE(crate::PROTOCOL_VERSION),
        })
        .await?;
    let settings = loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        match *packet.data {
            packets::Packet::NetworkSettings(settings) => break settings,
            packets::Packet::Disconnect(disconnect) => return Err(disconnected(disconnect)),
            other => {
                if let Some(error) = connection.negotiation_failure(other) {
                    return Err(error);
                }
            }
        }
    };
    let algorithm = CompressionAlgorithm::try_from(settings.compression_algorithm.to_raw())
        .unwrap_or(CompressionAlgorithm::Deflate);
    connection.enable_compression(CompressionSettings {
        algorithm,
        threshold: settings.compression_threshold.0,
    });
    let (mut claims, token) = if let Some((token, claims)) = authentication {
        (serde_json::Value::Object(claims), Some(token))
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
    let request = if let Some(token) = token {
        encode_authenticated_request(
            connection.transport.key(),
            &token,
            &claims,
            connection.max_decoded_bytes(),
        )
    } else {
        let identity = uuid::Uuid::new_v4().to_string();
        encode_offline_request(
            connection.transport.key(),
            &OfflineIdentity {
                display_name: login.display_name,
                identity: &identity,
                xuid: "",
                playfab_id: "",
                playfab_title_id: "",
            },
            &claims,
            SystemTime::now(),
            connection.max_decoded_bytes(),
        )
    }
    .map_err(io::Error::other)
    .map_err(failure)?;
    connection
        .send(packets::Login {
            client_network_version: wire::I32BE(crate::PROTOCOL_VERSION),
            connection_request: request,
        })
        .await?;
    let mut handshake_seen = false;
    let mut cache_sent = false;
    let offer = loop {
        let packet = connection.next_packet().await.map_err(failure)?;
        match *packet.data {
            packets::Packet::ServerToClientHandshake(handshake) if !handshake_seen => {
                let token = handshake
                    .handshake_web_token
                    .to_str()
                    .map_err(io::Error::other)
                    .map_err(failure)?;
                let ciphers = finish_key_exchange(
                    connection.transport.key(),
                    token,
                    connection.max_decoded_bytes(),
                )
                .map_err(io::Error::other)
                .map_err(failure)?;
                if connection.transport.batch_encryption() {
                    connection.enable_encryption(ciphers).map_err(failure)?;
                }
                connection.send(packets::ClientToServerHandshake {}).await?;
                handshake_seen = true;
            }
            packets::Packet::PlayStatus(status)
                if status.status == PlayStatusType::Loginsuccess =>
            {
                send_cache_status(&mut connection, login.cache.is_some(), &mut cache_sent).await?;
            }
            packets::Packet::ResourcePacksInfo(info) => {
                send_cache_status(&mut connection, login.cache.is_some(), &mut cache_sent).await?;
                break info;
            }
            packets::Packet::Disconnect(disconnect) => return Err(disconnected(disconnect)),
            packets::Packet::PlayStatus(status) => {
                return Err(failure(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("Login failed: {:?}", status.status),
                )));
            }
            other => {
                if let Some(error) = connection.negotiation_failure(other) {
                    return Err(error);
                }
            }
        }
    };
    let packs = packs::acquire(
        &mut connection,
        offer,
        login.settings.resource_pack_cache_dir.as_deref(),
    )
    .await?;
    let mut transport = SessionTransport::from_native(connection).with_tap(login.tap);
    let preparation = NativeJoinPreparation;
    let mut prepared_job = None;
    let spawned = super::startup::await_spawn(
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
            spawned.runtime_id,
            spawned.terrain_before_spawn,
        ),
        prepared,
    ))
}

/// Advertises the capability exactly once even when the offer precedes LoginSuccess.
async fn send_cache_status(
    connection: &mut Connection,
    enabled: bool,
    sent: &mut bool,
) -> Result<(), ProtocolError> {
    if !*sent {
        connection
            .send(packets::ClientCacheStatus {
                is_cache_supported: enabled,
            })
            .await?;
        *sent = true;
    }
    Ok(())
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
