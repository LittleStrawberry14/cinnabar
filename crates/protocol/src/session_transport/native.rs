//! Bounded duplex packet queues around one native connection owner.

use bytes::Bytes;
use std::{
    io,
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};

use super::{PacketSender, Reader, SessionTransport};
use crate::login::native_network::{Connection, PacketConnection};

const QUEUED_BATCHES: usize = 4;

#[cfg(test)]
mod tests;

pub(crate) struct NativeReader {
    incoming: mpsc::Receiver<io::Result<Bytes>>,
    task: tokio::task::JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
    closed: bool,
}

impl Drop for NativeReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NativeReader {
    /// Drains accepted writes and bounds teardown; cancellation retains the join handle.
    pub(super) async fn close(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task).await;
        self.closed = true;
        match result {
            Ok(result) => result.map_err(io::Error::other),
            Err(_) => {
                self.task.abort();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native close timed out",
                ))
            }
        }
    }

    /// Polls complete, bounded batch bodies without losing an item on cancellation.
    pub(super) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<Bytes>>> {
        self.incoming.poll_recv(cx)
    }
}

#[derive(Clone)]
pub(crate) struct NativeSender(mpsc::Sender<SendBatch>);

struct SendBatch {
    body: Bytes,
    written: oneshot::Sender<io::Result<()>>,
}

impl NativeSender {
    /// Commits one bounded batch after capacity is reserved, returning its write receipt.
    pub(super) async fn accept(
        &self,
        body: Bytes,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        Connection::check_body_size(&body)?;
        let permit =
            self.0.reserve().await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "native connection closed")
            })?;
        let (written, receipt) = oneshot::channel();
        permit.send(SendBatch { body, written });
        Ok(receipt)
    }
}

impl SessionTransport {
    /// Moves login's connection and unread packets into the same play FIFO.
    pub(crate) fn from_native(mut connection: impl PacketConnection) -> Self {
        let (send, mut outgoing) = mpsc::channel::<SendBatch>(QUEUED_BATCHES);
        let (incoming, receive) = mpsc::channel(QUEUED_BATCHES);
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut pending = None;
            let mut writes_failed = false;
            let mut reads_failed = false;
            loop {
                tokio::select! {
                    _ = &mut stopping => break,
                    _ = incoming.closed() => break,
                    permit = incoming.reserve(), if pending.is_some() => {
                        let Ok(permit) = permit else { break };
                        permit.send(pending.take().expect("pending input"));
                        if reads_failed { break; }
                    }
                    result = connection.recv_body(), if pending.is_none() && !reads_failed => {
                        if result.is_err() {
                            reads_failed = true;
                            fail_queued_writes(&mut outgoing);
                        }
                        pending = Some(result);
                    }
                    batch = outgoing.recv(), if !writes_failed && !reads_failed => {
                        let Some(batch) = batch else { break };
                        let result = connection.send_body(batch.body).await;
                        writes_failed = result.is_err();
                        let _ = batch.written.send(result);
                        if writes_failed { fail_queued_writes(&mut outgoing); }
                    }
                }
            }
            if !writes_failed && !reads_failed {
                outgoing.close();
                while let Some(batch) = outgoing.recv().await {
                    let result = connection.send_body(batch.body).await;
                    let failed = result.is_err();
                    let _ = batch.written.send(result);
                    if failed {
                        break;
                    }
                }
            }
            let _ = connection.close().await;
        });
        Self {
            reader: Reader::Native(NativeReader {
                incoming: receive,
                task,
                stop: Some(stop),
                closed: false,
            }),
            frames: PacketSender::Native(NativeSender(send)),
            timers: bridge::tasks::Timers::tokio(),
            startup: Default::default(),
            received: Default::default(),
            ahead: Default::default(),
            ahead_bytes: 0,
            ended: false,
            sending: Default::default(),
            tap: None,
            current: None,
            split_frames: 0,
            delivered: (0, 0),
        }
    }
}

/// Rejects queued writes immediately so their failure can unblock the receive boundary drain.
fn fail_queued_writes(outgoing: &mut mpsc::Receiver<SendBatch>) {
    outgoing.close();
    while outgoing.try_recv().is_ok() {}
}
