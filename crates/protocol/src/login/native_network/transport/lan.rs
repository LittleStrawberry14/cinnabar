//! The actual ready LAN client and its same receiver queues after local retirement.

use super::Inbox;
use bytes::Bytes;
use nethernet_tokio::{AcceptedSession, NetherClient, Retired};
use std::{io, time::Instant};

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
