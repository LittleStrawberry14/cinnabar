//! Exact addressed-server selection and the application's verified-key trust decision.

use super::{NativeConnectionTarget, NativeTransport};
use futures::future::BoxFuture;
use p384::ecdsa::VerifyingKey;
use std::{fmt, io, sync::Arc, time::Instant};

/// Application-owned first-use authorization after the actual transport verifies its server key.
/// This decision neither supplies account credentials nor replaces SDP/DTLS authentication.
pub trait NativeServerTrust: Send + Sync {
    /// Decides once under the original join cutoff; cancellation must withdraw this join's prompt.
    fn trust<'a>(
        &'a self,
        endpoint: &'a str,
        public_key: &'a VerifyingKey,
        deadline: Instant,
    ) -> BoxFuture<'a, io::Result<bool>>;
}

#[derive(Clone)]
pub(crate) struct AddressedRoute {
    address: String,
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) trust: Arc<dyn NativeServerTrust>,
}

impl fmt::Debug for AddressedRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AddressedRoute(..)")
    }
}

impl NativeConnectionTarget {
    /// Binds one addressed endpoint to its original application's trust capability before any I/O.
    ///
    /// # Errors
    /// Rejects empty, malformed, URL or service input; a bare server host may omit its port.
    pub fn addressed(address: String, trust: Arc<dyn NativeServerTrust>) -> io::Result<Self> {
        if address.is_empty()
            || address.contains(['/', '\\', '?', '#', '@'])
            || address
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            return Err(invalid());
        }
        let raw_ipv6 = address.parse::<std::net::Ipv6Addr>().is_ok();
        let authority = if raw_ipv6 {
            format!("[{address}]")
        } else {
            address.clone()
        };
        let url = url::Url::parse(&format!("http://{authority}")).map_err(|_| invalid())?;
        let host = match url.host().ok_or_else(invalid)? {
            url::Host::Ipv6(host) => host.to_string(),
            host => host.to_string(),
        };
        let explicit = if raw_ipv6 {
            None
        } else if address.starts_with('[') {
            address
                .split_once(']')
                .and_then(|(_, suffix)| suffix.strip_prefix(':'))
        } else {
            address.rsplit_once(':').map(|(_, port)| port)
        };
        let port = explicit
            .map(str::parse::<u16>)
            .transpose()
            .map_err(|_| invalid())?
            .filter(|port| *port != 0);
        Ok(Self {
            address: address.clone(),
            transport: NativeTransport::Addressed,
            local_lan: None,
            addressed: Some(AddressedRoute {
                address,
                host,
                port,
                trust,
            }),
        })
    }

    /// Rejects changed public projections before credentials, discovery or trust can be used.
    pub(crate) fn addressed_route(&self) -> io::Result<&AddressedRoute> {
        match &self.addressed {
            Some(route)
                if self.transport == NativeTransport::Addressed
                    && self.address == route.address =>
            {
                Ok(route)
            }
            _ => Err(invalid()),
        }
    }
}

impl AddressedRoute {
    /// Builds the original explicit RakNet fallback without resolving or changing the route.
    pub(crate) fn raknet_target(&self) -> NativeConnectionTarget {
        let port = self.port.unwrap_or(bridge::contract::DEFAULT_BEDROCK_PORT);
        let address = match self.host.parse::<std::net::Ipv6Addr>() {
            Ok(host) => format!("[{host}]:{port}"),
            Err(_) => format!("{}:{port}", self.host),
        };
        NativeConnectionTarget::new(address, NativeTransport::RakNet)
    }
}

/// Keeps endpoint and policy values out of diagnostics.
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid native addressed route",
    )
}
