//! Exclusive native transport ownership for one direct connection.

mod inbox;
mod messaging;
use inbox::Inbox;
use messaging::MessagingTransport;

use super::{NativeConnectionTarget, NativeTransport};
use bedrock_net::{
    BatchHeader,
    raknet::{RakNetConnection, RakNetOptions},
};
use bytes::Bytes;
use nethernet_tokio::{ConnectionConfig, LanConfig, LanSignaling, NetherClient};
use std::{io, net::SocketAddr, time::Instant};

const LAN_CONNECTION_TYPE: u8 = 4;

pub(super) enum Transport {
    RakNet {
        connection: RakNetConnection,
    },
    NetherNetLan {
        connection: NetherClient,
        nonce: String,
        inbox: Inbox,
    },
    Messaging(Box<MessagingTransport>),
}

impl Transport {
    /// Resolves the requested endpoint and establishes its explicitly selected transport.
    pub(super) async fn connect(
        target: &NativeConnectionTarget,
        messaging: Option<super::authentication::MessagingAdmission>,
    ) -> io::Result<Self> {
        if target.transport == NativeTransport::NetherNetMessaging {
            return MessagingTransport::connect(target, messaging)
                .await
                .map(Box::new)
                .map(Self::Messaging);
        }
        if messaging.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "messaging handoff requires its exact target",
            ));
        }
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
        match target.transport {
            NativeTransport::RakNet => Ok(Self::RakNet {
                connection: RakNetConnection::connect(address, RakNetOptions::default())
                    .await
                    .map_err(io::Error::other)?,
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
                    nonce: data.session_id,
                    inbox: Inbox::default(),
                })
            }
            NativeTransport::NetherNetMessaging => {
                unreachable!("handled before endpoint resolution")
            }
        }
    }

    /// Borrows the LAN discovery admission challenge, absent on RakNet.
    pub(super) fn nonce(&self) -> &str {
        match self {
            Self::NetherNetLan { nonce, .. } => nonce,
            Self::Messaging(transport) => transport.nonce(),
            _ => "",
        }
    }

    /// Selects the transport's Bedrock batch envelope.
    pub(super) fn header(&self) -> BatchHeader {
        match self {
            Self::RakNet { .. } => BatchHeader::RakNet,
            Self::NetherNetLan { .. } => BatchHeader::NetherNet,
            Self::Messaging(_) => BatchHeader::NetherNet,
        }
    }

    /// Application ciphers are required on RakNet; NetherNet already owns DTLS.
    pub(super) fn batch_encryption(&self) -> bool {
        matches!(self, Self::RakNet { .. })
    }

    /// Applies a credential deadline to output admission, preserving any already accepted write's progress.
    pub(super) async fn send_until(
        &mut self,
        message: Bytes,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        match self {
            Self::Messaging(transport) => transport.send_until(message, deadline).await,
            Self::RakNet { connection, .. } => {
                let capacity = connection.wait_send_capacity(message.len());
                match deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline.into(), capacity)
                        .await
                        .map_err(|_| super::authentication::expired())?
                        .map_err(io::Error::other)?,
                    None => capacity.await.map_err(io::Error::other)?,
                }
                if let Some(deadline) = deadline {
                    super::authentication::ensure_current(deadline, Instant::now())?;
                }
                connection.queue_send(message).map_err(io::Error::other)?;
                connection.flush().await.map_err(io::Error::other)
            }
            Self::NetherNetLan {
                connection, inbox, ..
            } => {
                if deadline.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "signed credential admission requires an authenticated transport",
                    ));
                }
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
            Self::Messaging(transport) => transport.recv().await,
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

    /// Polls service/control progress while application payload admission is paused.
    pub(super) async fn maintain(&mut self) {
        match self {
            Self::Messaging(transport) => transport.maintain().await,
            _ => std::future::pending().await,
        }
    }

    /// Sends a graceful close while the owner remains alive.
    pub(super) async fn close(&mut self) -> io::Result<()> {
        match self {
            Self::Messaging(transport) => transport.close().await,
            Self::RakNet { connection, .. } => connection.close().await.map_err(io::Error::other),
            Self::NetherNetLan { connection, .. } => {
                connection.close().await.map_err(io::Error::other)
            }
        }
    }
}
