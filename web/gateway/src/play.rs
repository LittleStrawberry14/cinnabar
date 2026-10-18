//! One actual account lease and exact upstream socket through Login, handoff and play retirement.

use super::{
    control,
    registry::Registry,
    session::{PlayLease, closed, progress},
    upstream,
};
use bridge::{FrameQueue, FrameRead, contract::*};
use bytes::Bytes;
use futures::StreamExt;
use protocol::{
    NativeRelayConnecting, NativeRelayConnection, NativeRelayRetired, RawClientData,
    session_wire::{self, CoreMessage},
};
use std::{collections::VecDeque, io, sync::Arc, time::Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::Framed;

#[cfg(test)]
tokio::task_local! {
    static PREPARED_CLAIMS: tokio::sync::mpsc::Sender<RawClientData>;
}

/// The socket field drops before its account lease even if the caller cancels or unwinds.
struct Playing {
    socket: Option<Socket>,
    lease: PlayLease,
}

enum Socket {
    Connecting(NativeRelayConnecting),
    Established(NativeRelayConnection),
    Retired(NativeRelayRetired),
}

/// Retains bounded, already accepted upstream output while the actual downstream writer is blocked.
#[derive(Default)]
struct Output {
    frames: VecDeque<(Bytes, bool)>,
    bytes: usize,
    ended: bool,
    failure: Option<io::Error>,
}

/// Consumes the selected reliable play stream; account and transport drivers stay caller-owned.
pub(super) async fn run<S>(registry: &Arc<Registry>, stream: S) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // Hello is bounded independently before the play codec admits a larger Connect/game frame.
    let mut hello_io = Framed::new(stream, bridge::BridgeCodec::with_max(RELAY_CONTROL_BYTES));
    let hello = control::read_hello(&mut hello_io, RELAY_CHANNEL_PLAY).await?;
    let attachment = registry.open_play(&hello.resume, hello.generation, Instant::now())?;
    registry.allow_play(&attachment, Instant::now())?;
    let deadline = Instant::now()
        .checked_add(protocol::NATIVE_JOIN_TIMEOUT)
        .ok_or_else(closed)?;
    let acquiring = attachment
        .session
        .begin_play(attachment.generation, hello.join);
    let lease = tokio::select! { biased;
        () = attachment.cancel.cancelled() => return Err(closed()),
        result = tokio::time::timeout_at(deadline.into(), acquiring) => result.map_err(|_| elapsed())??,
    };
    let mut playing = Playing {
        socket: None,
        lease,
    };
    let parts = hello_io.into_parts();
    let stream = super::prefix::Prefix::new(parts.io, parts.read_buf.freeze());
    let (read, write) = tokio::io::split(stream);
    let (reader, frames, writer) = bridge::tasks::prepare_bounded_connection(
        read,
        write,
        bridge::MAX_FRAME_LEN,
        RELAY_QUEUED_FRAME_BYTES,
    );
    let result = {
        let serving = playing.serve(reader, frames, deadline);
        tokio::pin!(serving, writer);
        tokio::select! { biased;
            () = registry.cancelled.cancelled() => Err(closed()),
            () = attachment.cancel.cancelled() => Err(closed()),
            result = &mut serving => result,
            () = &mut writer => Err(closed()),
        }
    };
    let cleanup = playing.close().await;
    result.and(cleanup)
}

impl Playing {
    /// Retains the actual SDK owner across mint and the same socket across every later phase.
    async fn serve(
        &mut self,
        mut reader: impl FrameRead,
        frames: FrameQueue,
        deadline: Instant,
    ) -> io::Result<()> {
        upstream::current(&self.lease, deadline)?;
        let frame = tokio::time::timeout_at(deadline.into(), reader.next())
            .await
            .map_err(|_| elapsed())?
            .ok_or_else(closed)?
            .map_err(invalid)?;
        upstream::current(&self.lease, deadline)?;
        let request = session_wire::decode_native_connect(&frame).map_err(invalid)?;
        if request.protocol != protocol::PROTOCOL_VERSION
            || request.client_data.game_version()?.as_deref() != Some(protocol::GAME_VERSION)
        {
            return Err(invalid(()));
        }
        let random = request
            .client_data
            .random_id()?
            .ok_or_else(|| invalid(()))?;
        let mut claims = request.client_data;
        let name = claims
            .display_name()?
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid(()))?;
        self.lease
            .progress(progress(bridge::ConnectStage::Connecting))?;
        let (claims, authentication) = if let Some(account) = &mut self.lease.account {
            let preparing = account.prepare(claims, random, deadline);
            tokio::pin!(preparing);
            let authentication = tokio::select! { biased;
                () = self.lease.cancel.cancelled() => return Err(closed()),
                result = &mut preparing => result?,
            };
            (RawClientData::default(), Some(authentication))
        } else {
            self.lease
                .session
                .guest_claims(&mut claims, random, &name)?;
            (claims, None)
        };
        upstream::current(&self.lease, deadline)?;
        #[cfg(test)]
        let _ = PREPARED_CLAIMS.try_with(|prepared| {
            prepared
                .try_send(claims.clone())
                .expect("fixture prepared-claims capacity");
        });
        let connected = upstream::connect(&self.lease, request.target, deadline).await?;
        self.socket = Some(Socket::Connecting(connected));
        self.negotiate(
            reader,
            frames,
            claims,
            request.client_cache,
            authentication,
            deadline,
        )
        .await
    }

    /// Negotiates the actual retained preconnected owner; it performs no replacement dial or admission.
    async fn negotiate(
        &mut self,
        mut reader: impl FrameRead,
        frames: FrameQueue,
        claims: RawClientData,
        client_cache: bool,
        authentication: Option<protocol::NativeAuthentication>,
        deadline: Instant,
    ) -> io::Result<()> {
        upstream::current(&self.lease, deadline)?;
        let Some(Socket::Connecting(socket)) = self.socket.as_mut() else {
            unreachable!("actual preconnected owner")
        };
        let negotiated = tokio::time::timeout_at(
            deadline.into(),
            socket.negotiate(claims, client_cache, authentication),
        )
        .await;
        if let Err(error) = negotiated
            .map_err(|_| elapsed())
            .and_then(|result| result.map_err(invalid))
        {
            if let Some(terminal) = socket.take_terminal() {
                frames
                    .send(session_wire::encode_core_message(&terminal).map_err(invalid)?)
                    .await
                    .map_err(invalid)?;
            }
            return Err(error);
        }
        upstream::current(&self.lease, deadline)?;
        let Some(Socket::Connecting(socket)) = self.socket.take() else {
            unreachable!("completed same socket")
        };
        let (socket, handoff) = match socket.try_finish() {
            Ok(complete) => complete,
            Err(socket) => {
                self.socket = Some(Socket::Connecting(socket));
                return Err(invalid(()));
            }
        };
        self.socket = Some(Socket::Established(socket));
        self.lease.current()?;
        let mut output = Output::default();
        self.write(
            &frames,
            session_wire::encode_core_message(&CoreMessage::Handoff(handoff.handoff))
                .map_err(invalid)?,
            &mut output,
            Some(deadline),
        )
        .await?;
        for (index, archive) in handoff.archives.into_iter().enumerate() {
            let index = u32::try_from(index).map_err(invalid)?;
            for chunk in archive.chunks(PACK_CHUNK_BYTES) {
                self.lease.current()?;
                let message = CoreMessage::PackData {
                    index,
                    data: Bytes::copy_from_slice(chunk),
                };
                self.write(
                    &frames,
                    session_wire::encode_core_message(&message).map_err(invalid)?,
                    &mut output,
                    Some(deadline),
                )
                .await?;
            }
        }
        upstream::current(&self.lease, deadline)?;
        self.lease.progress(progress(bridge::ConnectStage::Idle))?;
        let mut send_failure = None;
        let mut drain_deadline = None;
        loop {
            if let Some((frame, terminal)) = output.pop() {
                self.write(&frames, frame, &mut output, None).await?;
                if terminal {
                    return Ok(());
                }
                continue;
            }
            if let Some(error) = output.failure.take() {
                return Err(error);
            }
            let Some(Socket::Established(socket)) = self.socket.as_mut() else {
                return Err(closed());
            };
            tokio::select! {
                () = self.lease.cancel.cancelled() => return Err(closed()),
                () = expires(drain_deadline), if drain_deadline.is_some() => return Err(send_failure.take().expect("failed send armed its original drain")),
                message = socket.recv(), if !output.ended => output.push_result(message)?,
                frame = reader.next(), if send_failure.is_none() => {
                    let frame = frame.ok_or_else(closed)?.map_err(invalid)?;
                    self.lease.current()?;
                    let body = session_wire::batch_frame_body(&frame).ok_or_else(|| invalid(()))?;
                    let Some(Socket::Established(socket)) = self.socket.as_mut() else { unreachable!("same play owner") };
                    if let Err(error) = socket.send_body(body).await {
                        send_failure = Some(error);
                        drain_deadline = Some(Instant::now().checked_add(protocol::WRITE_FAILURE_DRAIN).ok_or_else(closed)?);
                    }
                    self.lease.current()?;
                }
            }
        }
    }

    /// Keeps the exact output future while bounded upstream prefix and terminal input arrive.
    async fn write(
        &mut self,
        frames: &FrameQueue,
        frame: Bytes,
        output: &mut Output,
        deadline: Option<Instant>,
    ) -> io::Result<()> {
        current(&self.lease, deadline)?;
        let writing = frames.send(frame);
        tokio::pin!(writing);
        loop {
            let Some(Socket::Established(socket)) = self.socket.as_mut() else {
                return Err(closed());
            };
            tokio::select! { biased;
                () = self.lease.cancel.cancelled() => return Err(closed()),
                () = expires(deadline), if deadline.is_some() => return Err(elapsed()),
                result = &mut writing => { result.map_err(invalid)?; return current(&self.lease, deadline); }
                message = socket.recv(), if !output.ended && output.bytes < RELAY_QUEUED_FRAME_BYTES => output.push_result(message)?,
            }
        }
    }

    /// Installs retirement synchronously before awaiting the original finite transport close.
    async fn close(&mut self) -> io::Result<()> {
        let Some(socket) = self.socket.take() else {
            return Ok(());
        };
        self.socket = Some(Socket::Retired(match socket {
            Socket::Connecting(socket) => socket.begin_close(),
            Socket::Established(socket) => socket.begin_close(),
            Socket::Retired(socket) => socket,
        }));
        let Some(Socket::Retired(socket)) = self.socket.as_mut() else {
            unreachable!("retained retirement")
        };
        socket.finish_close().await
    }
}

impl Output {
    /// Stages a transport cause behind every packet already accepted during output backpressure.
    fn push_result(&mut self, result: io::Result<CoreMessage>) -> io::Result<()> {
        match result {
            Ok(message) => self.push(message),
            Err(error) => {
                self.ended = true;
                self.failure = Some(error);
                Ok(())
            }
        }
    }

    /// Accepts one real already-bounded message; one last item may cross the byte threshold.
    fn push(&mut self, message: CoreMessage) -> io::Result<()> {
        if self.ended {
            return Err(invalid(()));
        }
        let terminal = matches!(
            message,
            CoreMessage::Disconnect(_) | CoreMessage::Transfer(_)
        );
        let frame = session_wire::encode_core_message(&message).map_err(invalid)?;
        self.bytes = self
            .bytes
            .checked_add(frame.len())
            .ok_or_else(|| invalid(()))?;
        self.ended = terminal;
        self.frames.push_back((frame, terminal));
        Ok(())
    }

    /// Releases only the actual front message, keeping the accepted terminal behind its prefix.
    fn pop(&mut self) -> Option<(Bytes, bool)> {
        let front = self.frames.pop_front()?;
        self.bytes -= front.0.len();
        Some(front)
    }
}

/// Redacts peer/account/provider/input diagnostics at the browser stream boundary.
fn invalid<T>(_error: T) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "gateway play failed")
}

/// Reports only the original finite join budget.
fn elapsed() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "gateway join timed out")
}

/// Keeps original finite join/drain waits; a play phase without one has no invented idle timer.
async fn expires(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// Rechecks account/controller authority and the original deadline after actual output completion.
fn current(lease: &PlayLease, deadline: Option<Instant>) -> io::Result<()> {
    match deadline {
        Some(deadline) => upstream::current(lease, deadline),
        None => lease.current(),
    }
}

#[cfg(test)]
mod tests;
