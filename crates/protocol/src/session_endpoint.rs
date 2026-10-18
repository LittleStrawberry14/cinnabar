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
}

/// One explicit direct connection; account policy is supplied separately at Login.
#[derive(Debug, Clone)]
pub struct NativeConnectionTarget {
    /// Numeric endpoint or DNS name with its port.
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
