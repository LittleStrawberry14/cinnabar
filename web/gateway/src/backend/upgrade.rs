//! Bounded HTTP policy before the maintained codec writes its WebSocket upgrade response.

use super::{current, failed, origin};
use crate::prefix::Prefix;
use bridge::contract::{RELAY_CONTROL_BYTES, RELAY_CONTROL_PATH, RELAY_SOCKET_PROTOCOL};
use bytes::BytesMut;
use http::{HeaderValue, header::SEC_WEBSOCKET_PROTOCOL};
use std::{io, time::Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_websockets::{Config, Limits, ServerBuilder, WebSocketStream};

/// Replays the same accepted request/read-ahead after all policy checks and before any 101 response.
pub(super) async fn control<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    origins: &[String],
    deadline: Instant,
) -> io::Result<WebSocketStream<Prefix<S>>> {
    let mut received = BytesMut::with_capacity(RELAY_CONTROL_BYTES);
    let mut read = [0; RELAY_CONTROL_BYTES];
    loop {
        current(deadline)?;
        match inspect(&received, origins) {
            Ok(true) => break,
            Ok(false) if received.len() < RELAY_CONTROL_BYTES => {}
            Ok(false) => {
                refuse(&mut stream, Status::TooLarge).await;
                return Err(failed(()));
            }
            Err(status) => {
                refuse(&mut stream, status).await;
                return Err(failed(()));
            }
        }
        let available = RELAY_CONTROL_BYTES - received.len();
        let count = stream.read(&mut read[..available]).await?;
        current(deadline)?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        received.extend_from_slice(&read[..count]);
    }
    let limit = RELAY_CONTROL_BYTES + size_of::<u32>();
    let builder = ServerBuilder::new()
        .config(Config::default().frame_size(limit).flush_threshold(limit))
        .limits(Limits::default().max_payload_len(Some(limit)))
        .add_header(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(RELAY_SOCKET_PROTOCOL),
        )
        .map_err(failed)?;
    let (_, socket) = builder
        .accept(Prefix::new(stream, received.freeze()))
        .await
        .map_err(failed)?;
    current(deadline)?;
    Ok(socket)
}

#[derive(Clone, Copy)]
enum Status {
    BadRequest,
    Forbidden,
    NotFound,
    TooLarge,
}

/// Reads only route/origin/subprotocol policy; the existing codec owns WebSocket handshake parsing.
fn inspect(received: &[u8], origins: &[String]) -> Result<bool, Status> {
    // Match the maintained upgrade codec's header capacity, within the stricter byte bound above.
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    if request
        .parse(received)
        .map_err(|_| Status::BadRequest)?
        .is_partial()
    {
        return Ok(false);
    }
    if request.method != Some("GET") || request.version != Some(1) {
        return Err(Status::BadRequest);
    }
    // Play is deliberately absent until shared input/duplex qualification; no path/flag bypass.
    if request.path != Some(RELAY_CONTROL_PATH) {
        return Err(Status::NotFound);
    }
    let host = one_header(request.headers, "host")?
        .filter(|value| !value.is_empty())
        .ok_or(Status::BadRequest)?;
    let origin = one_header(request.headers, "origin")?.unwrap_or_default();
    if !origin::allowed(origin, host, origins) {
        return Err(Status::Forbidden);
    }
    let protocol =
        one_header(request.headers, "sec-websocket-protocol")?.ok_or(Status::BadRequest)?;
    if !protocol
        .split(',')
        .any(|token| token.trim() == RELAY_SOCKET_PROTOCOL)
    {
        return Err(Status::BadRequest);
    }
    Ok(true)
}

/// Rejects ambiguous duplicate policy headers rather than checking one and upgrading another.
fn one_header<'a>(
    headers: &'a [httparse::Header<'a>],
    name: &str,
) -> Result<Option<&'a str>, Status> {
    let mut found = None;
    for header in headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(name))
    {
        if found.is_some() {
            return Err(Status::BadRequest);
        }
        found = Some(std::str::from_utf8(header.value).map_err(|_| Status::BadRequest)?);
    }
    Ok(found)
}

/// Sends a fixed credential-free refusal within the caller's original upgrade deadline.
async fn refuse(stream: &mut (impl AsyncWrite + Unpin), status: Status) {
    let response: &[u8] = match status {
        Status::BadRequest => b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        Status::Forbidden => b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        Status::NotFound => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        Status::TooLarge => b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    };
    let _ = stream.write_all(response).await;
    let _ = stream.flush().await;
}
