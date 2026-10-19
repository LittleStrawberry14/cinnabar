//! Application-owned choice of connection backend.

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
/// Largest host-issued admission nonce retained for a native game login.
pub const MAX_NATIVE_JOIN_NONCE_BYTES: usize = 4096;

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
/// Bounds one native connection's admission and protocol startup.
pub const NATIVE_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Native direct transport selected by application composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransport {
    /// Reliable ordered UDP with application batch encryption.
    RakNet,
    /// Directed LAN discovery and ICE/DTLS/SCTP with self-signed Login.
    NetherNetLan,
    /// Protected Player Messaging with signed SDP and explicit friend or service admission.
    NetherNetMessaging,
}

/// An explicit Player Messaging route, distinct from a discovery listing.
/// Friend nonces come from joined-session admission; authenticated services use a private nonce-free policy.
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

    /// Builds transport metadata for the caller-authorized nonce-free service handoff.
    /// Friend routes remain available only through the nonce-bearing constructor.
    pub(crate) fn service(peer: uuid::Uuid) -> std::io::Result<Self> {
        if peer.is_nil() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid native service messaging target",
            ));
        }
        Ok(Self {
            peer,
            nonce: zeroize::Zeroizing::new(String::new()),
        })
    }

    /// Returns the exact Player Messaging recipient supplied by the admission owner.
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

        let realm = NativeMessagingTarget::service(peer).unwrap();
        assert_eq!(realm.peer(), peer);
        assert_eq!(realm.connection_target().address, peer.to_string());
        assert_eq!(realm.nonce(), "");
        assert!(NativeMessagingTarget::service(uuid::Uuid::nil()).is_err());
        assert!(NativeMessagingTarget::new(peer, String::new()).is_err());
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
