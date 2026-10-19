//! Thin real Quinn peer using maintained HTTP/3/QPACK and stream-header codecs.

use super::*;
use bytes::{Buf, BytesMut};
use futures::future::poll_fn;
use h3::{
    frame::FrameStream,
    proto::{
        frame::{Frame, SettingId, Settings},
        headers::Header,
    },
    quic::{Connection, OpenStreams, SendStream},
    stream::{BidiStreamHeader, BufRecvStream, UniStreamHeader, WriteBuf},
    webtransport::SessionId,
};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::rustls::{
    ClientConfig, RootCertStore,
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
};

type RequestStream = FrameStream<h3_quinn::BidiStream<Bytes>, Bytes>;

pub(super) struct Fixture {
    pub(super) registry: Arc<Registry>,
    pub(super) tcp: SocketAddr,
    pub(super) prefixes: tokio::sync::mpsc::Receiver<()>,
    udp: quinn::Endpoint,
    client: quinn::Endpoint,
    task: JoinHandle<io::Result<()>>,
}

impl Fixture {
    /// Supplies one actual certificate, paired listeners, registry and original serving future.
    pub(super) async fn new(upstream: Option<SocketAddr>) -> Self {
        if let Some(upstream) = upstream {
            assert!(upstream.ip().is_loopback() && upstream.port() != 0);
        }
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
        let gateway =
            super::super::super::Gateway::new(vec![certificate.clone()], key, Vec::new()).unwrap();
        let registry = Arc::clone(&gateway.registry);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let tcp = listener.local_addr().unwrap();
        let udp = quinn::Endpoint::server(
            gateway.quic_config().unwrap(),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let tcp_incoming = Box::pin(futures::stream::unfold(listener, |listener| async {
            Some((listener.accept().await.map(|(socket, _)| socket), listener))
        }));
        let quic_incoming = Box::pin(futures::stream::unfold(udp.clone(), |endpoint| async {
            endpoint
                .accept()
                .await
                .map(|incoming| (Ok(incoming), endpoint))
        }));
        let (observations, prefixes) = tokio::sync::mpsc::channel(QUEUED_FRAMES);
        let task = tokio::spawn(async move {
            let run = super::super::ACCEPTED_PREFIX.scope(
                observations,
                gateway.run_paired(tcp_incoming, quic_incoming),
            );
            if let Some(upstream) = upstream {
                crate::upstream::FIXTURE_UPSTREAM.scope(upstream, run).await
            } else {
                run.await
            }
        });
        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        let mut tls = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        tls.alpn_protocols = vec![b"h3".to_vec()];
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        let mut client = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        client.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
        Self {
            registry,
            tcp,
            prefixes,
            udp,
            client,
            task,
        }
    }

    /// Completes actual TLS with the fixture's explicit localhost trust and no resolver.
    pub(super) async fn connect(&self) -> Result<quinn::Connection, quinn::ConnectionError> {
        self.client
            .connect(self.udp.local_addr().unwrap(), "localhost")
            .unwrap()
            .await
    }

    /// Opens a real retained HTTP/3 control stream and one CONNECT request using original codecs.
    pub(super) async fn peer(&self, path: &str, origin: Option<&str>) -> Peer {
        let connection = self.connect().await.unwrap();
        let quic = h3_quinn::Connection::new(connection.clone());
        let mut opener = <h3_quinn::Connection as Connection<Bytes>>::opener(&quic);
        let mut settings = Settings::default();
        for id in [
            SettingId::ENABLE_CONNECT_PROTOCOL,
            SettingId::H3_DATAGRAM,
            SettingId::ENABLE_WEBTRANSPORT,
            SettingId::WEBTRANSPORT_MAX_SESSIONS,
        ] {
            settings.insert(id, 1).unwrap();
        }
        let mut control = poll_fn(|cx| opener.poll_open_send(cx)).await.unwrap();
        control
            .send_data(WriteBuf::<Bytes>::from(UniStreamHeader::Control(settings)))
            .unwrap();
        poll_fn(|cx| control.poll_ready(cx)).await.unwrap();
        let mut peer = Peer {
            connection,
            opener,
            session: SessionId::try_from(0).unwrap(),
            connect: None,
            _settings: control,
            critical: std::sync::Mutex::new(Vec::new()),
        };
        let request = peer.request(path, origin).await;
        peer.session = request.id().into();
        peer.connect = Some(request);
        peer
    }

    /// Awaits the original handlers, permit release and service erasure before dropping endpoints.
    pub(super) async fn finish(&mut self) {
        self.registry.cancelled.cancel();
        self.client.close(0_u32.into(), b"");
        tokio::time::timeout(Duration::from_secs(15), &mut self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(self.registry.test_counts(), (0, 0));
        self.udp.close(0_u32.into(), b"");
        self.udp.wait_idle().await;
        self.client.wait_idle().await;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.registry.cancelled.cancel();
        self.task.abort();
        self.udp.close(0_u32.into(), b"");
        self.client.close(0_u32.into(), b"");
    }
}

pub(super) struct Peer {
    pub(super) connection: quinn::Connection,
    pub(super) session: SessionId,
    opener: h3_quinn::OpenStreams,
    connect: Option<RequestStream>,
    _settings: h3_quinn::SendStream<Bytes>,
    critical: std::sync::Mutex<Vec<quinn::RecvStream>>,
}

impl Peer {
    /// Uses maintained header and QPACK encoders; the fixture supplies no HTTP parser or factory.
    pub(super) async fn request(&mut self, path: &str, origin: Option<&str>) -> RequestStream {
        let mut stream = poll_fn(|cx| self.opener.poll_open_bidi(cx)).await.unwrap();
        let mut fields = http::HeaderMap::new();
        if let Some(origin) = origin {
            fields.insert(http::header::ORIGIN, origin.parse().unwrap());
        }
        let mut extensions = http::Extensions::new();
        extensions.insert(Protocol::WEB_TRANSPORT);
        let headers = Header::request(
            Method::CONNECT,
            format!("https://localhost{path}").parse().unwrap(),
            fields,
            extensions,
        )
        .unwrap();
        let mut encoded = BytesMut::new();
        h3::qpack::encode_stateless(&mut encoded, headers).unwrap();
        stream.send_data(Frame::Headers(encoded.freeze())).unwrap();
        poll_fn(|cx| stream.poll_ready(cx)).await.unwrap();
        FrameStream::new(BufRecvStream::new(stream))
    }

    /// Observes actual HEADERS on the original CONNECT before using application streams.
    pub(super) async fn status(&mut self) -> StatusCode {
        response(self.connect.as_mut().unwrap()).await
    }

    /// Produces the maintained protocol preface for this actual CONNECT stream's identity.
    pub(super) fn prefix(&self) -> Bytes {
        let mut bytes = WriteBuf::<Bytes>::from(BidiStreamHeader::WebTransportBidi(self.session));
        bytes.copy_to_bytes(bytes.remaining())
    }

    /// Opens the actual QUIC bidi and prefixes it with the original CONNECT session identity.
    pub(super) async fn stream(&self) -> tokio::io::Join<quinn::RecvStream, quinn::SendStream> {
        let (mut send, recv) = self.connection.open_bi().await.unwrap();
        send.write_all(&self.prefix()).await.unwrap();
        tokio::io::join(recv, send)
    }

    /// Observes original CONNECT DATA then FIN while the actual QUIC owner remains retained.
    pub(super) async fn connect_end(&mut self) -> usize {
        let stream = self.connect.as_mut().unwrap();
        let mut bytes = 0;
        loop {
            match poll_fn(|cx| stream.poll_next(cx)).await.unwrap() {
                Some(Frame::Data(_)) => {
                    while let Some(chunk) = poll_fn(|cx| stream.poll_data(cx)).await.unwrap() {
                        bytes += chunk.remaining();
                    }
                }
                None => return bytes,
                _ => panic!("unexpected CONNECT close frame"),
            }
        }
    }

    /// Consumes only maintained WT headers; the existing web receiver checks join/hash/frames/FIN.
    pub(super) async fn pack_stream(&self) -> quinn::RecvStream {
        use h3::proto::stream::StreamType;
        loop {
            let mut stream = self.connection.accept_uni().await.unwrap();
            let kind = read_varint(&mut stream).await;
            if kind == StreamType::WEBTRANSPORT_UNI.value() {
                let session =
                    h3::webtransport::SessionId::try_from(read_varint(&mut stream).await).unwrap();
                assert_eq!(
                    session, self.session,
                    "archive stays on the actual CONNECT session"
                );
                return stream;
            }
            let mut critical = self.critical.lock().unwrap();
            assert!(critical.len() < RELAY_QUIC_UNIDIRECTIONAL_STREAMS);
            critical.push(stream);
        }
    }

    /// Aborts only this actual fixture connection; the original gateway performs application cleanup.
    pub(super) fn close(&self) {
        self.connection.close(0_u32.into(), b"");
    }
}

/// Uses maintained HTTP/3 and QPACK decoding to observe the actual server response.
pub(super) async fn response(stream: &mut RequestStream) -> StatusCode {
    let Some(Frame::Headers(mut encoded)) = poll_fn(|cx| stream.poll_next(cx)).await.unwrap()
    else {
        panic!("actual HEADERS response")
    };
    let decoded =
        h3::qpack::decode_stateless(&mut encoded, u64::try_from(RELAY_CONTROL_BYTES).unwrap())
            .unwrap();
    Header::try_from(decoded.fields)
        .unwrap()
        .into_response_parts()
        .unwrap()
        .0
}

/// Uses the maintained decoder and encoded-size contract, retaining no advertised body buffer.
async fn read_varint(stream: &mut quinn::RecvStream) -> u64 {
    use h3::proto::varint::VarInt;
    let mut encoded = [0; VarInt::MAX_SIZE];
    stream.read_exact(&mut encoded[..1]).await.unwrap();
    let size = VarInt::encoded_size(encoded[0]);
    stream.read_exact(&mut encoded[1..size]).await.unwrap();
    VarInt::decode(&mut &encoded[..size]).unwrap().into_inner()
}
