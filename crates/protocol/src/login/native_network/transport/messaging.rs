//! The signed Player Messaging edge of the existing native connection owner.

use super::Inbox;
use crate::login::native_network::authentication::{MessagingAdmission, ensure_current, expired};
use bedrock_net::messaging::MessagingConnection;
use bytes::Bytes;
use nethernet_tokio::{ConnectionConfig, NetherError};
use p384::ecdsa::SigningKey;
use std::{io, sync::Arc, time::Instant};

pub(super) struct MessagingTransport {
    connection: MessagingConnection,
    key: Arc<SigningKey>,
    target: crate::NativeMessagingTarget,
    inbox: Inbox,
}

impl MessagingTransport {
    /// Checks the exact joined-session route before using the protected socket.
    pub(super) async fn connect(
        endpoint: &crate::NativeConnectionTarget,
        key: Arc<SigningKey>,
        admission: Option<MessagingAdmission>,
    ) -> io::Result<Self> {
        let admission = admission.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native messaging requires selected-account admission",
            )
        })?;
        if endpoint.address != admission.target.peer().to_string() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native messaging target mismatch",
            ));
        }
        ensure_current(admission.expires, Instant::now())?;
        let expires = admission.expires;
        let connection = admission
            .prepared
            .connect(
                ConnectionConfig {
                    identity: Some(admission.identity),
                    server_trust: Some(admission.trust),
                    attempts: 1,
                    ..ConnectionConfig::default()
                },
                admission.expires,
            )
            .await
            .map_err(|error| {
                if Instant::now() >= expires {
                    expired()
                } else {
                    io::Error::other(error)
                }
            })?;
        Ok(Self {
            connection,
            key,
            target: admission.target,
            inbox: Inbox::default(),
        })
    }

    /// Borrows the exact proof owner also held by signed SDP identity.
    pub(super) fn key(&self) -> &SigningKey {
        &self.key
    }

    /// Borrows only the explicitly admitted session challenge.
    pub(super) fn nonce(&self) -> &str {
        self.target.nonce()
    }

    /// Drives service and incoming data while waiting for actual SCTP output admission.
    pub(super) async fn send_until(
        &mut self,
        message: Bytes,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        let session = self.connection.session();
        let sending = async {
            let result = match deadline {
                Some(deadline) => session.send_before(message, deadline).await,
                None => session.send(message).await,
            };
            match result {
                Err(NetherError::Timeout) if deadline.is_some() => Err(expired()),
                result => result.map_err(io::Error::other),
            }
        };
        let incoming = futures::stream::unfold(&mut self.connection, |connection| async move {
            match connection.recv().await {
                Ok(Some(message)) => Some((Ok(message), connection)),
                Ok(None) => None,
                Err(error) => Some((Err(io::Error::other(error)), connection)),
            }
        });
        self.inbox.send(sending, incoming).await
    }

    /// Preserves messages accepted while a write was pending, ahead of the next transport read.
    pub(super) async fn recv(&mut self) -> io::Result<Bytes> {
        if let Some(message) = self.inbox.pop() {
            return Ok(message);
        }
        self.connection
            .recv()
            .await
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "NetherNet peer closed"))
    }

    /// Ends the existing transport without creating a cleanup task.
    pub(super) async fn close(&mut self) -> io::Result<()> {
        self.connection.close().await.map_err(io::Error::other)
    }
}
