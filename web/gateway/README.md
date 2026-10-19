# Native browser gateway

`web-gateway` serves browser control and play streams through the native Rust networking stack.
[`Gateway`] owns ephemeral account sessions and their connections. Your application supplies
certificates, an accepted TCP stream and a Tokio runtime with I/O and timers enabled.

```rust,no_run
use futures::stream;
use std::io;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use web_gateway::Gateway;

async fn serve(
    listener: TcpListener,
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> io::Result<()> {
    let gateway = Gateway::new(certificates, key, Vec::new())?;
    let incoming = Box::pin(stream::unfold(listener, |listener| async move {
        let accepted = listener.accept().await.map(|(stream, _)| stream);
        Some((accepted, listener))
    }));
    gateway.run(incoming).await
}
```

The gateway accepts TLS 1.3 HTTP/1.1 upgrades for the generated
[`control`](bridge::contract::RELAY_CONTROL_PATH) and
[`play`](bridge::contract::RELAY_PLAY_PATH) routes with the
[shared WebSocket subprotocol](bridge::contract::RELAY_SOCKET_PROTOCOL).
A control connection issues the capability and generation used by its separate play connection;
play runs native RakNet login, resource-pack transfer and the shared web-session handoff.
Upstream connections use the existing public-address admission policy.

`Gateway::new` takes exact allowed origin strings. With an empty list, a nonempty Origin must
have the request Host's authority; missing Origin is permitted. Origin checks do not replace
the control capability or account checks.

End the accepted stream and await `Gateway::run` to stop admission and retire active connections
before their account state is erased. Dropping the run future drops the handlers immediately;
it does not await protocol retirement. Individual connection refusals are contained, while
accepted-stream and service-driver errors are returned to the application.

This crate is a native Tokio edge. A Bevy application can drive it on its existing I/O runtime;
it does not need a runtime per connection. Browser clients use ordinary WebSocket connections.
The crate adds no listener binding, process configuration or executor of its own.
