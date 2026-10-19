//! Local full-ICE HTTP signaling on an explicitly selected numeric loopback route.

use super::Transport;
use nethernet::identity::{SdpIdentity, TokenTrust};
use nethernet_tokio::{
    ClientIceConfig, ConnectionConfig, NetherClient, signaling::http::HttpSignaling,
};
use std::{io, net::SocketAddr, sync::Arc, time::Instant};

/// Original proof and policy; neither metadata nor a status response grants account authority.
pub(in crate::login::native_network) struct LocalIdentity {
    pub(in crate::login::native_network) identity: Arc<dyn SdpIdentity>,
    pub(in crate::login::native_network) trust: TokenTrust,
    pub(in crate::login::native_network) expires: Option<Instant>,
}

/// Uses the same actual ready association/Inbox/retirement owner as accepting connections.
pub(in crate::login::native_network) async fn connect(
    address: SocketAddr,
    identity: LocalIdentity,
    deadline: tokio::time::Instant,
) -> io::Result<Transport> {
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local HTTP transport requires a numeric loopback endpoint",
        ));
    }
    let cutoff = identity
        .expires
        .map_or(deadline, |expires| deadline.min(expires.into()));
    let result = tokio::time::timeout_at(cutoff, async {
        check_deadline(cutoff, identity.expires)?;
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(cutoff.saturating_duration_since(tokio::time::Instant::now()))
            .build()
            .map_err(io::Error::other)?;
        let signaling = HttpSignaling::with_client(rand::random::<u64>().to_string(), client);
        let mut bind = address;
        bind.set_port(0);
        let mut config = ConnectionConfig {
            identity: Some(identity.identity),
            server_trust: Some(identity.trust),
            client_ice: Some(ClientIceConfig {
                ice: Default::default(),
                bind_addr: Some(bind),
            }),
            attempts: 1,
            ..Default::default()
        };
        config.timeouts.negotiation = config
            .timeouts
            .negotiation
            .min(cutoff.saturating_duration_since(tokio::time::Instant::now()));
        let client = NetherClient::connect_with(signaling, format!("http://{address}"), config)
            .await
            .map_err(io::Error::other)?;
        check_deadline(cutoff, identity.expires)?;
        Ok(Transport::LocalHttp(Box::new(
            super::AcceptedTransport::new(client.into_accepted()),
        )))
    })
    .await;
    // Credential expiry keeps its authentication classification rather than becoming a transport timeout.
    check_deadline(cutoff, identity.expires)?;
    result.map_err(|_| timed_out())?
}

/// Rechecks original cutoffs even when an awaited result is already ready.
fn check_deadline(cutoff: tokio::time::Instant, expires: Option<Instant>) -> io::Result<()> {
    if let Some(expires) = expires {
        super::super::authentication::ensure_current(expires, Instant::now())?;
    }
    if tokio::time::Instant::now() >= cutoff {
        return Err(timed_out());
    }
    Ok(())
}

/// Omits local endpoint/proof details from the finite transport failure.
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "native local HTTP join timed out")
}
