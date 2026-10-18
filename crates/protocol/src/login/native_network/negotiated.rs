//! Exclusive native connection state after Login and resource-pack negotiation.

use super::{
    Connection, NativeAuthentication, apply_handshake, authentication, failure, handshake_failure,
    packs, transport,
};
use crate::{NativeConnectionTarget, ProtocolError, SessionLogin};
use bedrock_client::handshake::{ClientHandshake, HandshakeEvent, HandshakeOptions};
use bedrock_net::login::OfflineIdentity;
use bedrock_packs::ResourcePackHandoff;
use p384::{ecdsa::SigningKey, elliptic_curve::Generate};
use rand::rngs::SysRng;
use serde_json::{json, value::RawValue};
use std::{io, sync::Arc, time::SystemTime};

/// Moves the same codec, ciphers, unread FIFO and selected archives into startup.
pub(super) struct NegotiatedConnection {
    pub(super) connection: Connection,
    pub(super) packs: ResourcePackHandoff,
}

/// Completes the sole transport, Login and pack negotiation under the caller's join deadline.
pub(super) async fn negotiate(
    target: &NativeConnectionTarget,
    login: &SessionLogin<'_>,
    authentication: Option<NativeAuthentication>,
) -> Result<NegotiatedConnection, ProtocolError> {
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
            Arc::new(
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
        let mut claims = super::super::client_data::login_client_data(
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
    Ok(NegotiatedConnection { connection, packs })
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
