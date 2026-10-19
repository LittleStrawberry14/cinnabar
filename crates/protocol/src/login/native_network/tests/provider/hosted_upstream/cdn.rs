//! Actual CDN-only upstream acquisition and original signed downstream pack/world exchange.

use super::*;
use crate::login::native_network::packs::PUBLIC_CDN_ENDPOINT;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn hosted_cdn_only_archive_reoriginates_metadata_and_finishes_both_original_legs() {
    tokio::time::timeout(TIMEOUT, async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let bytes = archive_bytes(PACK_ID, &[]);
        let download = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            assert!(header.starts_with(b"GET /only-archive HTTP/1.1\r\n"));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&bytes).await.unwrap();
        };
        let application = Box::pin(PUBLIC_CDN_ENDPOINT.scope(
            endpoint,
            original_upstream_with_cdn(Route::LocalHttp, Some(endpoint)),
        ));
        tokio::join!(download, application);
    })
    .await
    .expect("finite original hosted CDN/pack/world exchange stalled");
}
