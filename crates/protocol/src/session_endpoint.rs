//! Application-owned choice of connection backend.

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
mod addressed;
#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
pub use addressed::NativeServerTrust;

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
/// Largest host-issued admission nonce retained for a native game login.
pub const MAX_NATIVE_JOIN_NONCE_BYTES: usize = 4096;

#[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
/// Bounds one native connection's admission and protocol startup.
pub const NATIVE_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Native direct transport selected by application composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransport {
    /// Addressed-server HTTP discovery followed by its selected NetherNet or RakNet transport.
    Addressed,
    /// Reliable ordered UDP with application batch encryption.
    RakNet,
    /// Directed LAN discovery and ICE/DTLS/SCTP with self-signed Login.
    NetherNetLan,
    /// Protected Player Messaging with signed SDP and explicit friend or service admission.
    NetherNetMessaging,
    /// Original string-routed WebSocket signaling from an authenticated service join.
    NetherNetLegacy,
    /// HTTP/full-ICE signaling to an explicitly selected numeric loopback world process.
    LocalNetherNetHttp,
    /// Offline LAN admission to one selected level on an original loopback world process.
    LocalNetherNetLan,
}

/// An explicit Player Messaging route, distinct from a discovery listing.
/// Friend nonces come from joined-session admission; service routes have their own private policy.
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

    /// Returns the exact Player Messaging recipient supplied by the admission owner.
    pub fn peer(&self) -> uuid::Uuid {
        self.peer
    }

    /// Produces the sole endpoint eligible for this prepared account handoff.
    pub fn connection_target(&self) -> NativeConnectionTarget {
        NativeConnectionTarget::new(self.peer.to_string(), NativeTransport::NetherNetMessaging)
    }

    /// Borrows the joined-session admission challenge only for signed client data.
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
}

#[cfg(all(test, feature = "native-networking", not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    struct RefuseTrust;
    impl NativeServerTrust for RefuseTrust {
        fn trust<'a>(
            &'a self,
            _: &'a str,
            _: &'a p384::ecdsa::VerifyingKey,
            _: std::time::Instant,
        ) -> futures::future::BoxFuture<'a, std::io::Result<bool>> {
            Box::pin(std::future::ready(Ok(false)))
        }
    }

    /// A valid scoped native socket must survive addressed selection into the actual RakNet fallback.
    #[test]
    fn addressed_scoped_ipv6_retains_its_original_family_and_scope_for_raknet() {
        use std::{net::SocketAddr, sync::Arc};
        for address in ["[::1%3]:19132", "[fe80::1%7]:19133"] {
            let original: SocketAddr = address.parse().unwrap();
            let target = NativeConnectionTarget::addressed(address.into(), Arc::new(RefuseTrust))
                .expect("a valid scoped native endpoint was rejected as an HTTP URL");
            target.validate().unwrap();
            assert_eq!(target.address, address);
            let fallback = target.addressed_route().unwrap().raknet_target();
            assert_eq!(fallback.transport, NativeTransport::RakNet);
            assert_eq!(
                fallback.address.parse::<SocketAddr>().unwrap(),
                original,
                "addressed RakNet fallback lost the original IPv6 scope"
            );
        }
    }

    /// Only a validated exact endpoint can carry the original join's trust capability.
    #[test]
    fn addressed_selection_validates_its_host_and_rejects_changed_public_projections() {
        use std::sync::Arc;
        for address in [
            "",
            "http://host:19132",
            "user@host:19132",
            "host/path:19132",
            "host\\path:19132",
            "host:65536",
            "host\n:19132",
            "[bad]:19132",
        ] {
            assert!(
                NativeConnectionTarget::addressed(address.into(), Arc::new(RefuseTrust)).is_err()
            );
        }
        for address in ["host.invalid", "host.invalid:19133", "[::1]:19132", "::1"] {
            let target =
                NativeConnectionTarget::addressed(address.into(), Arc::new(RefuseTrust)).unwrap();
            target.validate().unwrap();
            let mut changed = target.clone();
            changed.address.push('x');
            assert!(changed.validate().is_err());
            let mut changed = target;
            changed.transport = NativeTransport::RakNet;
            assert!(changed.validate().is_err());
        }
        assert!(
            NativeConnectionTarget::new("host.invalid:19132".into(), NativeTransport::Addressed)
                .validate()
                .is_err()
        );
    }

    /// Local policy cannot be fabricated or redirected by changing the public endpoint projection.
    #[test]
    fn selected_local_lan_rejects_invalid_or_changed_route_capabilities() {
        use std::{net::SocketAddr, time::Duration};
        let budget = NATIVE_JOIN_TIMEOUT;
        for value in ["192.0.2.1:1234", "[::1]:1234", "127.0.0.1:0"] {
            assert!(
                NativeConnectionTarget::local_lan(value.parse().unwrap(), "World".into(), budget)
                    .is_err()
            );
        }
        let address: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert!(NativeConnectionTarget::local_lan(address, String::new(), budget).is_err());
        assert!(
            NativeConnectionTarget::local_lan(address, "World".into(), Duration::ZERO).is_err()
        );
        assert!(
            NativeConnectionTarget::new(address.to_string(), NativeTransport::LocalNetherNetLan)
                .validate()
                .is_err()
        );
        let original = NativeConnectionTarget::local_lan(address, "World".into(), budget).unwrap();
        assert_eq!(
            original.local_lan_route().unwrap(),
            (address, "World", budget)
        );
        let mut changed = original.clone();
        changed.address = "127.0.0.1:4321".into();
        assert!(changed.validate().is_err());
        let mut changed = original;
        changed.transport = NativeTransport::RakNet;
        assert!(changed.validate().is_err());
    }

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

        assert!(NativeMessagingTarget::new(peer, String::new()).is_err());
    }
}

/// One explicit direct connection; account policy is supplied separately at Login.
#[derive(Debug, Clone)]
pub struct NativeConnectionTarget {
    /// Endpoint with a port, or the exact typed service recipient for a prepared signaling handoff.
    pub address: String,
    /// Transport chosen for this endpoint.
    pub transport: NativeTransport,
    local_lan: Option<LocalLanRoute>,
    #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
    addressed: Option<addressed::AddressedRoute>,
}

#[derive(Debug, Clone)]
struct LocalLanRoute {
    address: std::net::SocketAddr,
    level: String,
    discovery_budget: std::time::Duration,
}

impl NativeConnectionTarget {
    /// Retains an explicit direct or service endpoint without resolving or changing its transport.
    pub fn new(address: String, transport: NativeTransport) -> Self {
        Self {
            address,
            transport,
            local_lan: None,
            #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
            addressed: None,
        }
    }

    /// Binds local offline discovery to the original process endpoint and selected level.
    /// Callers still own process lifetime; this capability cannot follow another endpoint.
    pub fn local_lan(
        address: std::net::SocketAddr,
        level: String,
        discovery_budget: std::time::Duration,
    ) -> std::io::Result<Self> {
        if !address.is_ipv4()
            || !address.ip().is_loopback()
            || address.port() == 0
            || level.is_empty()
            || discovery_budget.is_zero()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "local LAN requires a selected level and IPv4 loopback endpoint",
            ));
        }
        Ok(Self {
            address: address.to_string(),
            transport: NativeTransport::LocalNetherNetLan,
            local_lan: Some(LocalLanRoute {
                address,
                level,
                discovery_budget,
            }),
            #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
            addressed: None,
        })
    }

    /// Validates the original selected route before discovery; public projections grant no new route.
    pub fn local_lan_route(
        &self,
    ) -> std::io::Result<(std::net::SocketAddr, &str, std::time::Duration)> {
        if self.transport == NativeTransport::LocalNetherNetLan
            && let Some(route) = &self.local_lan
            && self.address == route.address.to_string()
            && route.address.is_ipv4()
            && route.address.ip().is_loopback()
            && route.address.port() != 0
            && !route.level.is_empty()
        {
            return Ok((route.address, &route.level, route.discovery_budget));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "local LAN selection changed or is unavailable",
        ))
    }

    /// Rejects a changed local projection before any native credential or transport work.
    pub fn validate(&self) -> std::io::Result<()> {
        if self.local_lan.is_some() || self.transport == NativeTransport::LocalNetherNetLan {
            self.local_lan_route()?;
        }
        #[cfg(all(feature = "native-networking", not(target_arch = "wasm32")))]
        if self.addressed.is_some() || self.transport == NativeTransport::Addressed {
            self.addressed_route()?;
        }
        Ok(())
    }

    /// Checks only local HTTP endpoint syntax; callers still own process/account admission.
    ///
    /// # Errors
    /// Rejects another transport, DNS/URL input, non-loopback addresses and a zero port.
    pub fn local_http_address(&self) -> std::io::Result<std::net::SocketAddr> {
        if self.transport == NativeTransport::LocalNetherNetHttp
            && let Ok(address) = self.address.parse::<std::net::SocketAddr>()
            && address.ip().is_loopback()
            && address.port() != 0
        {
            return Ok(address);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "local HTTP transport requires a numeric loopback endpoint",
        ))
    }
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
