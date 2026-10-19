//! The actual ready LAN client and its same receiver queues after local retirement.

use super::{Inbox, LAN_CONNECTION_TYPE};
use bytes::Bytes;
use nethernet_tokio::{
    AcceptedSession, ConnectionConfig, LanConfig, LanSignaling, NetherClient, Retired,
};
use std::{io, net::SocketAddr, time::Instant};

pub(in crate::login::native_network) struct LanTransport {
    connection: Option<Connection>,
    nonce: String,
    inbox: Inbox,
}

enum Connection {
    Established(NetherClient),
    Retired(AcceptedSession<Retired>),
}

impl LanTransport {
    /// Discovers and connects one compatible IPv4 endpoint under the selection owner's cutoff.
    pub(super) async fn connect(address: SocketAddr) -> io::Result<Self> {
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
        Ok(Self::new(connection, data.session_id))
    }

    /// Retains only an actual ready client and its discovery challenge on the same owner.
    pub(super) fn new(connection: NetherClient, nonce: String) -> Self {
        Self {
            connection: Some(Connection::Established(connection)),
            nonce,
            inbox: Inbox::default(),
        }
    }

    /// Borrows the original discovery nonce without inferring any authenticated account authority.
    pub(super) fn nonce(&self) -> &str {
        &self.nonce
    }

    /// Polls original ingress beside established sending without duplicating lifecycle ownership.
    pub(super) async fn send_until(
        &mut self,
        message: Bytes,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        if deadline.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "signed credential admission requires an authenticated transport",
            ));
        }
        let Connection::Established(connection) = self
            .connection
            .as_mut()
            .expect("synchronous transport transition")
        else {
            return Err(super::closing());
        };
        let sender = connection.sender();
        let sending = async { sender.send(message).await.map_err(io::Error::other) };
        let incoming = futures::stream::unfold(connection, |connection| async move {
            match connection.recv().await {
                Ok(Some(message)) => Some((Ok(message), connection)),
                Ok(None) => None,
                Err(error) => Some((Err(io::Error::other(error)), connection)),
            }
        });
        self.inbox.send(sending, incoming).await
    }

    /// Drains previously accepted wire messages before the retained live or retired receiver.
    pub(super) async fn recv(&mut self) -> io::Result<Bytes> {
        if let Some(message) = self.inbox.pop() {
            return Ok(message);
        }
        let result = match self
            .connection
            .as_mut()
            .expect("synchronous transport transition")
        {
            Connection::Established(connection) => connection.recv().await,
            Connection::Retired(connection) => connection.reliable.recv().await,
        };
        result
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "NetherNet peer closed"))
    }

    /// Consumes the ready lifecycle synchronously and retains both original accepted receivers.
    pub(super) fn close(&mut self) {
        let current = self
            .connection
            .take()
            .expect("synchronous transport transition");
        self.connection = Some(match current {
            Connection::Established(connection) => Connection::Retired(connection.close()),
            retired @ Connection::Retired(_) => retired,
        });
    }
}
