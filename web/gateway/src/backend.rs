//! Caller-owned accepted TCP sockets, admission, TLS and the native control service.

mod origin;
mod upgrade;

use super::{
    control, play,
    registry::{ConnectionPermit, Registry, RegistryDriver},
    session::closed,
    socket::Socket,
};
use bridge::contract::RELAY_HELLO_SECONDS;
use futures::{Stream, StreamExt, stream::FuturesUnordered};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, ConfigBuilder, ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer},
        server::WantsServerCert,
    },
};

type Work = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

/// A native TLS/WebSocket gateway driven by its application's accepted TCP stream.
///
/// Owns ephemeral account sessions and their control/play connections. The application supplies
/// its TLS material, accepted sockets and Tokio runtime; construction starts no background work.
pub struct Gateway {
    registry: Arc<Registry>,
    tls: TlsAcceptor,
    origins: Arc<[String]>,
}

/// Field order retires actual sockets before the service driver erases ephemeral credentials.
struct Running {
    connections: FuturesUnordered<Work>,
    services: RegistryDriver,
    registry: Arc<Registry>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.registry.cancelled.cancel();
    }
}

impl Gateway {
    /// Configures TLS 1.3 using supplied certificates and key without changing global TLS state.
    ///
    /// With an empty `origins` list, a nonempty HTTP Origin must have the request Host's authority.
    /// Otherwise it must match a configured origin exactly. An absent Origin remains permitted.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS configuration or certificate/key pair is rejected.
    pub fn new(
        certificates: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        origins: Vec<String>,
    ) -> io::Result<Self> {
        let config = tls_builder()?
            .with_single_cert(certificates, key)
            .map_err(failed)?;
        Ok(Self::with_config(config, origins))
    }

    /// Shares validated operator certificate replacements with new handshakes on this gateway.
    #[cfg(feature = "operator")]
    pub(super) fn with_resolver(
        resolver: Arc<dyn rustls::server::ResolvesServerCert>,
        origins: Vec<String>,
    ) -> io::Result<Self> {
        Ok(Self::with_config(
            tls_builder()?.with_cert_resolver(resolver),
            origins,
        ))
    }

    /// Keeps the same TLS policy and account owner for supplied and file-backed certificates.
    fn with_config(mut config: ServerConfig, origins: Vec<String>) -> Self {
        // A counted TCP socket carries one HTTP/1 upgrade, never multiplexed HTTP/2 requests.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self {
            registry: Registry::new(),
            tls: TlsAcceptor::from(Arc::new(config)),
            origins: origins.into(),
        }
    }

    /// Drives accepted sockets and account work on the caller's Tokio runtime with I/O and timers.
    ///
    /// Ending `incoming` stops admission, cancels active work and retires connection handlers
    /// before erasing accounts. Dropping this future drops its handlers instead of awaiting
    /// their retirement. Individual connection refusals do not stop the gateway.
    ///
    /// # Errors
    ///
    /// Returns an accepted-stream or account-service driver error after retiring active handlers.
    pub async fn run(
        self,
        mut incoming: impl Stream<Item = io::Result<TcpStream>> + Unpin,
    ) -> io::Result<()> {
        let registry = Arc::clone(&self.registry);
        let mut running = Running {
            connections: FuturesUnordered::new(),
            services: registry.drive(),
            registry,
        };
        let mut stopping = false;
        let mut result = Ok(());
        loop {
            if stopping && running.connections.is_empty() {
                return result;
            }
            tokio::select! { biased;
                () = self.registry.cancelled.cancelled(), if !stopping => stopping = true,
                Some(_result) = running.connections.next(), if !running.connections.is_empty() => {},
                completed = &mut running.services, if !stopping => {
                    result = completed;
                    self.registry.cancelled.cancel();
                    stopping = true;
                }
                accepted = incoming.next(), if !stopping => {
                    match accepted {
                        Some(Ok(stream)) => {
                            let now = Instant::now();
                            let Ok(peer) = stream.peer_addr() else { continue };
                            let Ok(permit) = self.registry.admit(peer.ip(), now) else { continue };
                            let stream = Admitted { stream, _permit: permit };
                            let registry = Arc::clone(&self.registry);
                            let tls = self.tls.clone();
                            let origins = Arc::clone(&self.origins);
                            running.connections.push(Box::pin(serve(registry, tls, origins, peer.ip(), stream, now)));
                        }
                        Some(Err(error)) => {
                            result = Err(error);
                            self.registry.cancelled.cancel();
                            stopping = true;
                        }
                        None => {
                            self.registry.cancelled.cancel();
                            stopping = true;
                        }
                    }
                }
            }
        }
    }
}

/// Both native entrypoints use one provider, protocol version and client-auth policy.
fn tls_builder() -> io::Result<ConfigBuilder<ServerConfig, WantsServerCert>> {
    Ok(
        ServerConfig::builder_with_provider(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        )
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(failed)?
        .with_no_client_auth(),
    )
}

/// Attaches capacity to the physical socket, including a canceled TLS/upgrade/control future.
struct Admitted {
    stream: TcpStream,
    _permit: ConnectionPermit,
}

impl AsyncRead for Admitted {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Admitted {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// One original deadline covers TLS and HTTP before the independently bounded application Hello.
async fn serve(
    registry: Arc<Registry>,
    tls: TlsAcceptor,
    origins: Arc<[String]>,
    peer: std::net::IpAddr,
    stream: Admitted,
    accepted: Instant,
) -> io::Result<()> {
    let deadline = accepted
        .checked_add(Duration::from_secs(u64::from(RELAY_HELLO_SECONDS)))
        .ok_or_else(closed)?;
    let upgrading = async {
        let stream = tls.accept(stream).await.map_err(failed)?;
        current(deadline)?;
        upgrade::accept(stream, &origins, deadline).await
    };
    current(deadline)?;
    let (channel, socket) = tokio::select! { biased;
        () = registry.cancelled.cancelled() => return Err(closed()),
        result = tokio::time::timeout_at(deadline.into(), upgrading) => result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "gateway upgrade timed out"))??,
    };
    current(deadline)?;
    let socket = Socket::new(socket, channel.message_limit());
    match channel {
        upgrade::Channel::Control => control::run(&registry, peer, socket).await,
        upgrade::Channel::Play => play::run(&registry, socket).await,
    }
}

/// Never returns certificate, HTTP input or peer details to an application stream.
fn failed<T>(_cause: T) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "gateway transport refused")
}

/// A completed borrowed wait cannot restart or outlive the original accepted-socket budget.
fn current(deadline: Instant) -> io::Result<()> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "gateway upgrade timed out",
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests;
