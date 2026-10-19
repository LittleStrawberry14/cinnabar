//! Exact routes returned by authenticated Realm and Gathering joins.

use super::*;
use bedrock_services::realms::NetworkProtocol;
use prepare::check_preparation_deadline;

pub(super) struct ResolvedServiceRoute {
    address: String,
    pub(super) messaging_peer: Option<uuid::Uuid>,
}

impl ResolvedServiceRoute {
    /// Consumes only the actual service response, never catalog display metadata.
    pub(super) fn new(protocol: NetworkProtocol, address: String) -> io::Result<Self> {
        let messaging_peer = match protocol {
            NetworkProtocol::Default if !address.trim().is_empty() => None,
            NetworkProtocol::NetherNetJsonRpc => Some(
                uuid::Uuid::parse_str(&address)
                    .ok()
                    .filter(|peer| !peer.is_nil())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid service messaging recipient",
                        )
                    })?,
            ),
            NetworkProtocol::NetherNet => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native service WebSocket signaling is not supported",
                ));
            }
            NetworkProtocol::Default => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "service returned no address",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "service advertised an unsupported transport",
                ));
            }
        };
        Ok(Self {
            address,
            messaging_peer,
        })
    }

    /// Finishes mint/socket acquisition on the same Operation that performed the service join.
    pub(super) async fn prepare(
        self,
        account: &NativeAccount,
        operation: &mut Operation,
        claims: Value,
        random_id: protocol::ClientRandomId,
        deadline: tokio::time::Instant,
    ) -> io::Result<(
        protocol::NativeConnectionTarget,
        protocol::NativeAuthentication,
    )> {
        check_preparation_deadline(deadline)?;
        let authentication = if let Some(peer) = self.messaging_peer {
            let (prepared, authentication) = account
                .prepare_messaging_socket_borrowed(operation, peer, claims, random_id, deadline)
                .await?;
            authentication.with_service_messaging(prepared)?
        } else {
            account
                .prepare_borrowed(operation, claims, random_id)
                .await?
        };
        check_preparation_deadline(deadline)?;
        Ok((self.into_target(), authentication))
    }

    /// Consumes the already-validated transport projection after its original response is retained.
    pub(super) fn into_target(self) -> protocol::NativeConnectionTarget {
        protocol::NativeConnectionTarget {
            address: self
                .messaging_peer
                .map_or(self.address, |peer| peer.to_string()),
            transport: if self.messaging_peer.is_some() {
                protocol::NativeTransport::NetherNetMessaging
            } else {
                protocol::NativeTransport::RakNet
            },
        }
    }
}

/// Joins separate service host/port fields without performing DNS or applying display metadata policy.
pub(super) fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
