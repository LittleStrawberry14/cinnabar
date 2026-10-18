//! Exclusive native transport ownership for one direct connection.

mod inbox;
use inbox::Inbox;

use super::{NativeConnectionTarget, NativeTransport};
use bedrock_net::{
    BatchHeader,
    raknet::{RakNetConnection, RakNetOptions},
};
use bytes::Bytes;
use nethernet_tokio::{ConnectionConfig, LanConfig, LanSignaling, NetherClient};
use p384::{ecdsa::SigningKey, elliptic_curve::Generate};
use rand::rngs::SysRng;
use std::{io, net::SocketAddr};

const LAN_CONNECTION_TYPE: u8 = 4;

pub(super) enum Transport {
    RakNet {
        connection: RakNetConnection,
        key: SigningKey,
    },
    NetherNetLan {
        connection: NetherClient,
        key: SigningKey,
        nonce: String,
        inbox: Inbox,
    },
}

impl Transport {
    /// Resolves the requested endpoint and establishes its explicitly selected transport.
    pub(super) async fn connect(target: &NativeConnectionTarget) -> io::Result<Self> {
        let address = tokio::net::lookup_host(&target.address)
            .await?
            .next()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "native endpoint did not resolve",
                )
            })?;
        if address.port() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native endpoint port is zero",
            ));
        }
        let key = SigningKey::try_generate_from_rng(&mut SysRng).map_err(io::Error::other)?;
        match target.transport {
            NativeTransport::RakNet => Ok(Self::RakNet {
                connection: RakNetConnection::connect(address, RakNetOptions::default())
                    .await
                    .map_err(io::Error::other)?,
                key,
            }),
            NativeTransport::NetherNetLan => {
                if !address.is_ipv4() {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "native NetherNet LAN requires IPv4",
                    ));
                }
                let _ = rustls::crypto::ring::default_provider().install_default();
                let route = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
                route.connect(address).await?;
                let local = SocketAddr::new(route.local_addr()?.ip(), 0);
                drop(route);
                let signaling = LanSignaling::with_config(
                    rand::random(),
                    local,
                    LanConfig {
                        broadcast_address: Some(address),
                        ..LanConfig::default()
                    },
                )
                .await
                .map_err(io::Error::other)?;
                let mut polls = tokio::time::interval(std::time::Duration::from_millis(50));
                let (id, data) = loop {
                    polls.tick().await;
                    let mut found = None;
                    for (id, data) in signaling.discover().await {
                        if signaling.get_address(id).await == Some(address) {
                            found = Some((id, data));
                            break;
                        }
                    }
                    if let Some(found) = found {
                        break found;
                    }
                };
                if !data.flag_b || data.session_id.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "LAN peer does not advertise self-signed admission with a nonce",
                    ));
                }
                if data.connection_type != LAN_CONNECTION_TYPE
                    || data.protocol_version != crate::PROTOCOL_VERSION as u32
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "LAN peer advertises an incompatible transport or protocol",
                    ));
                }
                let connection = NetherClient::connect_with(
                    signaling,
                    id.to_string(),
                    ConnectionConfig {
                        allow_unsigned_answers: true,
                        attempts: 1,
                        ..ConnectionConfig::default()
                    },
                )
                .await
                .map_err(io::Error::other)?;
                Ok(Self::NetherNetLan {
                    connection,
                    key,
                    nonce: data.session_id,
                    inbox: Inbox::default(),
                })
            }
        }
    }

    /// Borrows this connection's application proof key.
    pub(super) fn key(&self) -> &SigningKey {
        match self {
            Self::RakNet { key, .. } | Self::NetherNetLan { key, .. } => key,
        }
    }

    /// Borrows the LAN discovery admission challenge, absent on RakNet.
    pub(super) fn nonce(&self) -> &str {
        match self {
            Self::NetherNetLan { nonce, .. } => nonce,
            _ => "",
        }
    }

    /// Selects the transport's Bedrock batch envelope.
    pub(super) fn header(&self) -> BatchHeader {
        match self {
            Self::RakNet { .. } => BatchHeader::RakNet,
            Self::NetherNetLan { .. } => BatchHeader::NetherNet,
        }
    }

    /// Application ciphers are required on RakNet; NetherNet already owns DTLS.
    pub(super) fn batch_encryption(&self) -> bool {
        matches!(self, Self::RakNet { .. })
    }

    /// Admits and flushes one complete message. The connection owner never cancels this write.
    pub(super) async fn send(&mut self, message: Bytes) -> io::Result<()> {
        match self {
            Self::RakNet { connection, .. } => {
                connection
                    .wait_send_capacity(message.len())
                    .await
                    .map_err(io::Error::other)?;
                connection.queue_send(message).map_err(io::Error::other)?;
                connection.flush().await.map_err(io::Error::other)
            }
            Self::NetherNetLan {
                connection, inbox, ..
            } => {
                let session = connection.session();
                let sending = async { session.send(message).await.map_err(io::Error::other) };
                let incoming = futures::stream::unfold(connection, |connection| async move {
                    match connection.recv().await {
                        Ok(Some(message)) => Some((Ok(message), connection)),
                        Ok(None) => None,
                        Err(error) => Some((Err(io::Error::other(error)), connection)),
                    }
                });
                inbox.send(sending, incoming).await
            }
        }
    }

    /// Receives one complete message; both drivers retain partial input on cancellation.
    pub(super) async fn recv(&mut self) -> io::Result<Bytes> {
        match self {
            Self::RakNet { connection, .. } => connection.recv().await.map_err(io::Error::other),
            Self::NetherNetLan {
                connection, inbox, ..
            } => {
                if let Some(message) = inbox.pop() {
                    return Ok(message);
                }
                connection
                    .recv()
                    .await
                    .map_err(io::Error::other)?
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "NetherNet peer closed")
                    })
            }
        }
    }

    /// Sends a graceful close while the owner remains alive.
    pub(super) async fn close(&mut self) -> io::Result<()> {
        match self {
            Self::RakNet { connection, .. } => connection.close().await.map_err(io::Error::other),
            Self::NetherNetLan { connection, .. } => {
                connection.close().await.map_err(io::Error::other)
            }
        }
    }
}
