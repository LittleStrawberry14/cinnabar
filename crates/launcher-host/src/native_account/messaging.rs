//! Selected-account protected Player Messaging acquisition for an explicit joined session.

use super::*;
use bedrock_net::messaging::PreparedMessaging;
use bedrock_services::signaling::websocket::{MessagingSocket, Options, SocketError};

impl NativeAccount {
    /// Prepares signed NetherNet on the same selected account that mints its multiplayer token.
    ///
    /// The caller supplies a validated peer UUID and join nonce obtained from an
    /// explicit admission flow. This does not join MPSD, resolve cards, host a world
    /// or admit a friend listing. Poll on the existing session runtime; dropping
    /// the future retires its socket and persists already visible OAuth rotation.
    pub async fn prepare_messaging(
        self: Arc<Self>,
        target: protocol::NativeMessagingTarget,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<(
        protocol::NativeConnectionTarget,
        protocol::NativeAuthentication,
    )> {
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        tokio::time::timeout(ACQUISITION_TIMEOUT, async {
            let mut operation = Arc::clone(&self).acquire_verified().await?;
            operation.promote()?;
            self.prepare_messaging_borrowed(&mut operation, target, claims, random_id)
                .await
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native messaging account preparation timed out",
            )
        })?
    }

    /// Prepares the protected socket and mint under an already verified selected-account operation.
    /// The caller retains its original overall deadline and any joined-session cleanup owner.
    pub(super) async fn prepare_messaging_borrowed(
        &self,
        operation: &mut Operation,
        target: protocol::NativeMessagingTarget,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<(
        protocol::NativeConnectionTarget,
        protocol::NativeAuthentication,
    )> {
        if !std::ptr::eq(self, operation.account.as_ref()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native messaging operation belongs to another account",
            ));
        }
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        // Finish minting and public-key acquisition before opening a socket
        // that needs continuous heartbeat driving.
        let authentication = self.prepare_borrowed(operation, claims, random_id).await?;
        // This acquisition can rotate OAuth after the mint's checkpoint was published.
        operation.published = false;
        let request = operation
            .minecraft()?
            .messaging_connection()
            .await
            .map_err(|_| messaging_error())?;
        // Publish any credential rotation before awaiting an unrelated network upgrade.
        operation.persist()?;
        let network_id = rand::random();
        let socket = match tokio::time::timeout_at(
            authentication.expires_at().into(),
            MessagingSocket::connect(&request, network_id, Options::default()),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native messaging credential expired during upgrade",
            )
        })? {
            Ok(socket) => socket,
            Err(error) => {
                if error == SocketError::UpgradeStatus(401) {
                    // A delayed refusal cannot invalidate a replacement or another owner's issuance.
                    operation.minecraft()?.reject_messaging(&request);
                    operation.persist()?;
                }
                return Err(messaging_error());
            }
        };
        let prepared = PreparedMessaging::new(socket, network_id, target.peer())
            .map_err(|_| messaging_error())?;
        let endpoint = target.connection_target();
        operation.persist()?;
        let authentication = authentication.with_messaging(prepared, target)?;
        Ok((endpoint, authentication))
    }
}

/// Omits protected destination, authorization, frame and provider diagnostics.
fn messaging_error() -> io::Error {
    io::Error::other("native messaging acquisition failed; retry or sign in again")
}
