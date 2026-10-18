//! Established RakNet admission and retained finite local retirement.

use bedrock_net::raknet::{ClosingRakNetConnection, RakNetConnection};
use bytes::Bytes;
use std::{io, time::Instant};

pub(in crate::login::native_network) struct RakNetTransport {
    connection: Option<Connection>,
}

enum Connection {
    Established(RakNetConnection),
    Closing(ClosingRakNetConnection),
}

impl RakNetTransport {
    /// Retains only the actual successfully negotiated socket and its original queues.
    pub(super) fn new(connection: RakNetConnection) -> Self {
        Self {
            connection: Some(Connection::Established(connection)),
        }
    }

    /// Applies the original credential bound to capacity wait and immediate admission.
    pub(super) async fn send_until(
        &mut self,
        message: Bytes,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        let Connection::Established(connection) = self
            .connection
            .as_mut()
            .expect("synchronous transport transition")
        else {
            return Err(super::closing());
        };
        let capacity = connection.wait_send_capacity(message.len());
        match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline.into(), capacity)
                .await
                .map_err(|_| super::super::authentication::expired())?
                .map_err(io::Error::other)?,
            None => capacity.await.map_err(io::Error::other)?,
        }
        if let Some(deadline) = deadline {
            super::super::authentication::ensure_current(deadline, Instant::now())?;
        }
        connection.queue_send(message).map_err(io::Error::other)?;
        connection.flush().await.map_err(io::Error::other)
    }

    /// Receives the same accepted FIFO and terminal before or after consuming local send permission.
    pub(super) async fn recv(&mut self) -> io::Result<Bytes> {
        match self
            .connection
            .as_mut()
            .expect("synchronous transport transition")
        {
            Connection::Established(connection) => connection.recv().await,
            Connection::Closing(connection) => connection.recv().await,
        }
        .map_err(io::Error::other)
    }

    /// Installs the closing owner before awaiting teardown, retaining it across canceled waits.
    pub(super) async fn close(&mut self) -> io::Result<()> {
        let current = self
            .connection
            .take()
            .expect("synchronous transport transition");
        let (connection, failure) = match current {
            Connection::Established(connection) => match connection.begin_close() {
                Ok(connection) => (connection, None),
                Err(error) => {
                    let (connection, cause) = error.into_parts();
                    (connection, Some(cause))
                }
            },
            Connection::Closing(connection) => (connection, None),
        };
        self.connection = Some(Connection::Closing(connection));
        if let Some(failure) = failure {
            return Err(io::Error::other(failure));
        }
        let Some(Connection::Closing(connection)) = self.connection.as_mut() else {
            unreachable!("local close retained its actual closing owner")
        };
        connection.finish_close().await.map_err(io::Error::other)
    }
}
