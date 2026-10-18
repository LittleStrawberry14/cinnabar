//! Application-owned choice of connection backend.

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
/// Largest host-issued admission nonce retained for a native game login.
pub const MAX_NATIVE_JOIN_NONCE_BYTES: usize = 4096;

/// Native direct transport selected by application composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransport {
    /// Reliable ordered UDP with application batch encryption.
    RakNet,
    /// Directed LAN discovery and ICE/DTLS/SCTP with self-signed Login.
    NetherNetLan,
    /// Protected Player Messaging with signed SDP and an explicit joined-session nonce.
    NetherNetMessaging,
}

/// An explicit joined-session route, distinct from a friend-world listing.
/// The nonce must come from the caller's join admission, never a guessed card field.
/// No Debug implementation exposes the admission challenge.
#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
pub struct NativeMessagingTarget {
    peer: uuid::Uuid,
    nonce: zeroize::Zeroizing<String>,
}

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
impl NativeMessagingTarget {
    /// Validates the peer messaging UUID and a finite, nonempty opaque join nonce before any I/O.
    pub fn new(peer: uuid::Uuid, nonce: String) -> std::io::Result<Self> {
        let nonce = zeroize::Zeroizing::new(nonce);
        if peer.is_nil()
            || nonce.is_empty()
            || nonce.len() > MAX_NATIVE_JOIN_NONCE_BYTES
            || nonce.chars().any(char::is_control)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid native messaging join target",
            ));
        }
        Ok(Self { peer, nonce })
    }

    /// Returns the exact authenticated Player Messaging recipient.
    pub fn peer(&self) -> uuid::Uuid {
        self.peer
    }

    /// Produces the sole endpoint eligible for this prepared account handoff.
    pub fn connection_target(&self) -> NativeConnectionTarget {
        NativeConnectionTarget {
            address: self.peer.to_string(),
            transport: NativeTransport::NetherNetMessaging,
        }
    }

    /// Borrows the joined-session admission challenge only for signed client data.
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
}

#[cfg(all(test, feature = "native-networking", not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A listing cannot supply an absent peer/challenge or an unbounded join handoff.
    #[test]
    fn messaging_target_requires_an_explicit_finite_join_challenge() {
        let peer = uuid::Uuid::from_u128(1);
        for nonce in [
            String::new(),
            "invalid\nchallenge".into(),
            "x".repeat(MAX_NATIVE_JOIN_NONCE_BYTES + 1),
        ] {
            assert!(NativeMessagingTarget::new(peer, nonce).is_err());
        }
        assert!(NativeMessagingTarget::new(uuid::Uuid::nil(), "challenge".into()).is_err());
        let target = NativeMessagingTarget::new(peer, "synthetic-challenge".into()).unwrap();
        let endpoint = target.connection_target();
        assert_eq!(endpoint.transport, NativeTransport::NetherNetMessaging);
        assert_eq!(endpoint.address, peer.to_string());
        assert_eq!(target.nonce(), "synthetic-challenge");
    }
}

/// One explicit direct connection; account policy is supplied separately at Login.
#[derive(Debug, Clone)]
pub struct NativeConnectionTarget {
    /// Endpoint with a port, or the exact peer UUID for a prepared Messaging handoff.
    pub address: String,
    /// Transport chosen for this endpoint.
    pub transport: NativeTransport,
}

/// A core endpoint or a native direct connection, chosen once for the join.
#[derive(Debug, Clone)]
pub enum SessionEndpoint {
    /// Continue a connection acquired by the existing Go account/session owner.
    Core(std::path::PathBuf),
    #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
    /// Establish and retain the server connection in Rust.
    Native(crate::NativeConnectionTarget),
}

impl SessionEndpoint {
    /// Returns a control endpoint only for a session that actually uses the Go core.
    pub fn core_directory(&self) -> Option<&std::path::Path> {
        match self {
            Self::Core(directory) => Some(directory),
            #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
            Self::Native(_) => None,
        }
    }
}
