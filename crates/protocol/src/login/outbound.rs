use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use crate::session_transport::{PacketSender, PacketWritten};
use bytes::Bytes;
use simulation_clock::Moment;

use super::PlaySession;
use super::cache_status::SharedCacheStatus;
use crate::packet_tap::PacketTap;
use crate::{Packet, ProtocolError};
use bedrock_client::spawn::SpawnReady;

/// The write half of a play session: frames whole batches without borrowing the session.
pub struct PlayOutbound {
    frames: PacketSender,
    pending_initialization: Option<SpawnReady>,
    tap: Option<Arc<dyn PacketTap>>,
    cache_status: SharedCacheStatus,
    pending: VecDeque<PendingBatch>,
}

#[cfg(test)]
mod tests;

type AcceptFrame = Pin<Box<dyn Future<Output = Result<PacketWritten, ProtocolError>> + Send>>;

/// An entire submitted batch, retained until its exact frame flushes across cancelled waits.
struct PendingBatch {
    packets: Vec<Packet>,
    opportunity: Option<Moment>,
    prepared: bool,
    status_epoch: Option<u64>,
    frame: Option<Bytes>,
    accepting: Option<Mutex<AcceptFrame>>,
    written: Option<PacketWritten>,
}

impl PendingBatch {
    /// Transfers ordinary packet ownership before the first capacity or flush wait.
    fn new(packets: Vec<Packet>, opportunity: Option<Moment>) -> Self {
        Self {
            packets,
            opportunity,
            prepared: false,
            status_epoch: None,
            frame: None,
            accepting: None,
            written: None,
        }
    }

    /// Serializes epoch checks and queue acceptance, then observes the accepted write once.
    fn poll(
        &mut self,
        frames: &PacketSender,
        status: &SharedCacheStatus,
        tap: Option<&dyn PacketTap>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), ProtocolError>> {
        if self.written.is_none() {
            let mut status = status.lock().expect("cache status lock");
            if !self.prepared {
                if let Some(now) = self.opportunity.take()
                    && let Some(packet) = status.select(now)
                {
                    self.status_epoch = Some(status.epoch);
                    self.packets.push(Packet::new(packet));
                }
                self.prepared = true;
            }
            if self.status_epoch.is_some_and(|epoch| epoch != status.epoch) {
                // The trailing status is still ours until admission. Ordinary packets survive.
                self.packets.pop();
                self.status_epoch = None;
                self.accepting = None;
                self.frame = None;
            }
            if self.accepting.is_none() {
                let Some(frame) = crate::session_transport::batch_body(&self.packets) else {
                    return Poll::Ready(Ok(()));
                };
                self.frame = Some(frame.clone());
                let frames = frames.clone();
                self.accepting = Some(Mutex::new(Box::pin(
                    async move { frames.accept(frame).await },
                )));
            }
            // `accept` never suspends after committing its permit. Record its ticket under the
            // same lock as the epoch check, so reset cannot retract an accepted frame.
            let written = ready!(
                self.accepting
                    .as_mut()
                    .expect("prepared admission")
                    .get_mut()
                    .expect("batch admission lock")
                    .as_mut()
                    .poll(cx)
            )?;
            self.written = Some(written);
            self.accepting = None;
            if let Some(tap) = tap {
                crate::packet_tap::record_serverbound_body(
                    tap,
                    self.frame.as_ref().expect("accepted frame"),
                );
            }
            self.frame = None;
            self.packets.clear();
        }
        Pin::new(self.written.as_mut().expect("accepted write")).poll(cx)
    }
}

impl<R: bridge::FrameRead> PlaySession<R> {
    /// Detaches the write half; batches it sends share the session's own send FIFO and tap.
    pub fn outbound(&mut self) -> Result<PlayOutbound, ProtocolError> {
        Ok(PlayOutbound {
            frames: self.transport.sender(),
            pending_initialization: self.pending_initialization.take(),
            tap: self.transport.tap(),
            cache_status: self.cache_status.clone(),
            pending: VecDeque::new(),
        })
    }
}

impl PlayOutbound {
    /// Writes `packets` as one batch behind every frame already accepted; resolves once the
    /// batch is flushed, waiting for queue capacity first.
    /// An explicit simulation opportunity may append one cache status; ordinary frames do not.
    /// Cancellation retains every submitted batch until the next call resumes its write.
    pub async fn send_batch(
        &mut self,
        packets: Vec<Packet>,
        opportunity: Option<Moment>,
    ) -> Result<(), ProtocolError> {
        for packet in &packets {
            crate::codec::validate_packet(packet)?;
        }
        if !packets.is_empty() || opportunity.is_some() {
            self.pending
                .push_back(PendingBatch::new(packets, opportunity));
        }
        poll_fn(|cx| {
            while let Some(batch) = self.pending.front_mut() {
                let result =
                    ready!(batch.poll(&self.frames, &self.cache_status, self.tap.as_deref(), cx));
                self.pending.pop_front();
                if let Err(error) = result {
                    return Poll::Ready(Err(error));
                }
            }
            Poll::Ready(Ok(()))
        })
        .await
    }

    /// Takes the one-shot loading-end and initialization packets the session would send.
    pub fn take_finish_loading(&mut self) -> Vec<Packet> {
        self.pending_initialization
            .take()
            .map(super::startup::loading_finished)
            .unwrap_or_default()
    }
}
