//! Bounded duplex packet queues around one native connection owner.

use bytes::Bytes;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::sync::{mpsc, oneshot};

use super::{PacketSender, Reader, SessionTransport};
use crate::login::native_network::{Connection, PacketConnection};

const QUEUED_BATCHES: usize = 4;

type PendingInput = Option<io::Result<Bytes>>;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) struct NativeReader {
    incoming: mpsc::Receiver<io::Result<Bytes>>,
    task: Option<tokio::task::JoinHandle<PendingInput>>,
    tail: PendingInput,
    failure: Option<tokio::task::JoinError>,
    stop: Option<oneshot::Sender<()>>,
    closed: bool,
}

impl Drop for NativeReader {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            std::future::poll_fn(|cx| self.poll_driver(cx)),
        )
        .await;
        self.closed = true;
        match result {
            Ok(Ok(pending)) => {
                if let Some(item) = pending {
                    self.tail = Some(item);
                }
                self.failure
                    .take()
                    .map_or(Ok(()), |error| Err(io::Error::other(error)))
            }
            Ok(Err(error)) => Err(io::Error::other(error)),
            Err(_) => {
                if let Some(task) = &self.task {
                    task.abort();
                }
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native close timed out",
                ))
            }
        }
    }

    /// Takes completed driver ownership once, retaining the handle across pending polls.
    fn poll_driver(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<PendingInput, tokio::task::JoinError>> {
        let Some(task) = &mut self.task else {
            return Poll::Ready(Ok(None));
        };
        let result = std::task::ready!(Pin::new(task).poll(cx));
        self.task = None;
        Poll::Ready(result)
    }

    /// Drains queued bodies before the driver's retained input, without losing either on cancellation.
    pub(super) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<Bytes>>> {
        match self.incoming.poll_recv(cx) {
            Poll::Ready(None) => {}
            other => return other,
        }
        match std::task::ready!(self.poll_driver(cx)) {
            Ok(Some(item)) => self.tail = Some(item),
            Ok(None) => {}
            Err(error) if !self.closed => self.failure = Some(error),
            Err(_) => {}
        }
        Poll::Ready(self.tail.take())
    }
}

#[derive(Clone)]
pub(crate) struct NativeSender {
    send: mpsc::Sender<SendBatch>,
    closed: Arc<Mutex<bool>>,
}

struct SendBatch {
    body: Bytes,
    written: oneshot::Sender<io::Result<()>>,
}

/// Serializes queue commitment with receiver closure and every receiver-drop path.
struct Writes {
    receive: mpsc::Receiver<SendBatch>,
    closed: Arc<Mutex<bool>>,
}

impl Drop for Writes {
    fn drop(&mut self) {
        self.fail();
    }
}

impl Writes {
    /// Prevents every later commitment, including a permit reserved before closure.
    fn close_admission(&mut self) {
        let mut closed = self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *closed = true;
        self.receive.close();
    }

    /// Resolves queued receipts once no producer can append a late batch.
    fn fail(&mut self) {
        self.close_admission();
        while self.receive.try_recv().is_ok() {}
    }

    /// Whether this queue still accepts new transport work.
    fn is_active(&self) -> bool {
        !*self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Creates one bounded queue whose admission gate outlives its receiver.
fn write_channel(capacity: usize) -> (NativeSender, Writes) {
    let (send, receive) = mpsc::channel(capacity);
    let closed = Arc::new(Mutex::new(false));
    (
        NativeSender {
            send,
            closed: Arc::clone(&closed),
        },
        Writes { receive, closed },
    )
}

impl NativeSender {
    /// Commits one bounded batch after capacity is reserved, returning its write receipt.
    pub(super) async fn accept(
        &self,
        body: Bytes,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        Connection::check_body_size(&body)?;
        let permit =
            self.send.reserve().await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "native connection closed")
            })?;
        self.commit(permit, body)
    }

    /// Publishes an already size-checked batch atomically with respect to receiver teardown.
    fn commit(
        &self,
        permit: mpsc::Permit<'_, SendBatch>,
        body: Bytes,
    ) -> io::Result<oneshot::Receiver<io::Result<()>>> {
        let (written, receipt) = oneshot::channel();
        let closed = self
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "native connection closed",
            ));
        }
        permit.send(SendBatch { body, written });
        drop(closed);
        Ok(receipt)
    }
}

impl SessionTransport {
    /// Moves login's connection and unread packets into the same play FIFO.
    pub(crate) fn from_native(mut connection: impl PacketConnection) -> Self {
        let (send, mut writes) = write_channel(QUEUED_BATCHES);
        let (incoming, receive) = mpsc::channel(QUEUED_BATCHES);
        let (stop, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut pending: Option<io::Result<Bytes>> = None;
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
                    result = async {
                        if pending.is_none() {
                            Some(connection.recv_body().await)
                        } else {
                            connection.maintain().await;
                            None
                        }
                    } => {
                        if let Some(result) = result {
                            if result.is_err() { writes.fail(); }
                            pending = Some(result);
                        }
                    }
                    batch = writes.receive.recv(), if writes.is_active() => {
                        let Some(batch) = batch else { break };
                        let result = connection.send_body(batch.body).await;
                        if result.is_err() { writes.fail(); }
                        let _ = batch.written.send(result);
                    }
                }
            }
            let drain = writes.is_active();
            writes.close_admission();
            if drain {
                while let Some(batch) = writes.receive.recv().await {
                    let result = connection.send_body(batch.body).await;
                    let failed = result.is_err();
                    let _ = batch.written.send(result);
                    if failed {
                        break;
                    }
                }
            }
            // Move the same pending input through task completion so close never
            // waits for capacity held by its own caller's receive borrow.
            let _ = connection.close().await;
            pending
        });
        Self {
            reader: Reader::Native(NativeReader {
                incoming: receive,
                task: Some(task),
                tail: None,
                failure: None,
                stop: Some(stop),
                closed: false,
            }),
            frames: PacketSender::Native(send),
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
