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
        let (send, outgoing) = mpsc::channel::<SendBatch>(QUEUED_BATCHES);
        let (incoming, receive) = mpsc::channel(QUEUED_BATCHES);
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut pending: Option<io::Result<Bytes>> = None;
            let mut writes = Writes::Active(outgoing);
            loop {
                tokio::select! {
                    _ = &mut stopping => break,
                    _ = incoming.closed() => break,
                    permit = incoming.reserve(), if pending.is_some() => {
                        let Ok(permit) = permit else { break };
                        let item = pending.take().expect("pending input");
                        let ended = item.is_err();
                        permit.send(item);
                        if ended { break; }
                    }
                    result = connection.recv_body(), if pending.is_none() => {
                        if result.is_err() { writes.reject(); }
                        pending = Some(result);
                    }
                    batch = writes.recv() => {
                        let Some(batch) = batch else { writes = Writes::Drained; continue };
                        let result = if writes.is_active() {
                            connection.send_body(batch.body).await
                        } else {
                            Err(io::Error::new(io::ErrorKind::BrokenPipe, "native writes stopped"))
                        };
                        if result.is_err() { writes.reject(); }
                        let _ = batch.written.send(result);
                    }
                }
            }
            if let Writes::Active(mut outgoing) = writes {
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

/// Outgoing admission stays owned until even pre-close permits have settled.
enum Writes {
    Active(mpsc::Receiver<SendBatch>),
    Rejecting(mpsc::Receiver<SendBatch>),
    Drained,
}

impl Writes {
    /// Receives every accepted batch, including permits committed after closure.
    async fn recv(&mut self) -> Option<SendBatch> {
        match self {
            Self::Active(receiver) | Self::Rejecting(receiver) => receiver.recv().await,
            Self::Drained => std::future::pending().await,
        }
    }

    /// Rejects new reservations while keeping outstanding admissions observable.
    fn reject(&mut self) {
        let previous = std::mem::replace(self, Self::Drained);
        *self = match previous {
            Self::Active(mut receiver) | Self::Rejecting(mut receiver) => {
                receiver.close();
                Self::Rejecting(receiver)
            }
            Self::Drained => Self::Drained,
        };
    }

    /// Whether this owner may still submit new wire writes.
    fn is_active(&self) -> bool {
        matches!(self, Self::Active(_))
    }
}
