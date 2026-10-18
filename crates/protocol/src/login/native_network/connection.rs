//! Compression, encryption and packet FIFO retained for the entire connection.

use super::super::startup::{DeferredPackets, MAX_DEFERRED_PACKET_BYTES};
use super::transport::Transport;
use crate::{Packet, packet::GameHeader};
use bedrock_net::encryption::{BatchDecryptor, BatchEncryptor};
use bedrock_net::{BatchCodec, BatchError, BatchLimits, CompressionSettings, DecodeOutcome};
use bedrock_protocol_1_26_51::{packets, wire};
use bytes::{Bytes, BytesMut};
use std::{collections::VecDeque, io};

mod codec;
use codec::Codec;

/// Exclusive packet I/O consumed by the native session pump.
pub(crate) trait PacketConnection: Send + 'static {
    /// Sends one complete batch without cancelling cipher or transport progress.
    fn send_body(
        &mut self,
        body: Bytes,
    ) -> impl std::future::Future<Output = io::Result<()>> + Send;
    /// Retains partial input when the pump cancels a receive wait.
    fn recv_body(&mut self) -> impl std::future::Future<Output = io::Result<Bytes>> + Send;
    /// Progresses caller-owned service/control work without consuming application payloads.
    /// A terminal cause remains staged for ordinary reads to drain accepted data first.
    fn maintain(&mut self) -> impl std::future::Future<Output = ()> + Send {
        std::future::pending()
    }
    /// Flushes accepted transport output and ends this connection.
    fn close(&mut self) -> impl std::future::Future<Output = io::Result<()>> + Send;
}

pub(crate) struct Connection {
    pub(super) transport: Transport,
    codec: Option<Codec>,
    limits: BatchLimits,
    pending: VecDeque<Bytes>,
    current: Option<Bytes>,
    deferred: DeferredPackets,
    ciphers: Option<(BatchEncryptor, BatchDecryptor)>,
    negotiation_skips: u64,
}

impl Connection {
    /// Retains usable redirects and counts unusable decoded targets without ending the join.
    pub(super) fn negotiation_failure(
        &mut self,
        packet: packets::Packet,
    ) -> Option<crate::ProtocolError> {
        match super::negotiation_failure(packet) {
            Ok(error) => Some(error),
            Err(reason) => {
                self.negotiation_skips = self.negotiation_skips.saturating_add(1);
                tracing::warn!(
                    ?reason,
                    skipped = self.negotiation_skips,
                    "skipped unusable negotiation transfer"
                );
                None
            }
        }
    }
    /// Starts an uncompressed session and retains all later state on this same owner.
    pub(super) fn new(transport: Transport) -> Self {
        let limits = BatchLimits::default();
        Self {
            codec: Some(Codec::Plain(BatchCodec::new(transport.header(), limits))),
            transport,
            limits,
            pending: VecDeque::new(),
            current: None,
            deferred: DeferredPackets::default(),
            ciphers: None,
            negotiation_skips: 0,
        }
    }

    /// The shared decoded-byte bound for Login and subsequent packets.
    pub(super) fn max_decoded_bytes(&self) -> usize {
        self.limits.max_decoded_bytes
    }

    /// Applies negotiated compression without replacing unread packets.
    pub(super) fn enable_compression(&mut self, settings: CompressionSettings) -> io::Result<()> {
        let current = self.codec.take().expect("codec transition is synchronous");
        match current {
            Codec::Plain(codec) => {
                self.codec = Some(Codec::Negotiated(codec.negotiate(settings)));
                Ok(())
            }
            configured @ Codec::Negotiated(_) => {
                self.codec = Some(configured);
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "repeated network settings",
                ))
            }
        }
    }

    /// Borrows the actual codec; its only move occurs in a synchronous settings transition.
    fn codec(&self) -> &Codec {
        self.codec
            .as_ref()
            .expect("codec transition is synchronous")
    }

    /// Installs both directional streams once, before the client handshake acknowledgment.
    pub(super) fn enable_encryption(
        &mut self,
        ciphers: (BatchEncryptor, BatchDecryptor),
    ) -> io::Result<()> {
        if self.ciphers.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "repeated server key exchange",
            ));
        }
        self.ciphers = Some(ciphers);
        Ok(())
    }

    /// Sends one negotiation response, preserving received terminal input if writing fails.
    pub(super) async fn send<P>(&mut self, packet: P) -> Result<(), crate::ProtocolError>
    where
        packets::Packet: From<P>,
    {
        self.send_packets(&[Packet::new(packet)]).await
    }

    /// Sends a bounded negotiation batch immediately, retaining terminal input on a failed write.
    pub(super) async fn send_packets(
        &mut self,
        packets: &[Packet],
    ) -> Result<(), crate::ProtocolError> {
        self.send_packets_until(packets, None).await
    }

    /// Admits Login only while its original credential remains valid, including capacity waits.
    pub(super) async fn send_before<P>(
        &mut self,
        packet: P,
        deadline: std::time::Instant,
    ) -> Result<(), crate::ProtocolError>
    where
        packets::Packet: From<P>,
    {
        self.send_packets_until(&[Packet::new(packet)], Some(deadline))
            .await
    }

    /// Preserves received terminal input on transport failure, but reports expiry as authentication failure.
    async fn send_packets_until(
        &mut self,
        packets: &[Packet],
        deadline: Option<std::time::Instant>,
    ) -> Result<(), crate::ProtocolError> {
        let body = crate::session_transport::batch_body(packets)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "native packet could not encode")
            })
            .map_err(super::failure)?;
        match self.send_body_until(body, deadline).await {
            Ok(()) => Ok(()),
            Err(error) if super::authentication::is_expired(&error) => {
                Err(crate::ProtocolError::Authentication(error))
            }
            Err(error) => Err(super::preserve_negotiation_boundary(self, error).await),
        }
    }

    /// Encodes without restarting a finite Login credential deadline.
    async fn send_body_until(
        &mut self,
        body: Bytes,
        deadline: Option<std::time::Instant>,
    ) -> io::Result<()> {
        Self::check_body_size(&body)?;
        let entries = wire::Frames::new(body, self.limits.max_packets)
            .map(|frame| {
                let frame = frame.map_err(io::Error::other)?;
                let mut reader = wire::Reader::from_shared(frame.frame());
                reader.read_var_u32().map_err(io::Error::other)?;
                Ok(frame.frame().slice(reader.position()..))
            })
            .collect::<io::Result<Vec<_>>>()?;
        for mut message in self.codec().encode(&entries).map_err(io::Error::other)? {
            if let Some((encryptor, _)) = &mut self.ciphers {
                let marker = self.transport.header().marker();
                let body = if marker.is_some() {
                    message.slice(1..)
                } else {
                    message
                };
                message = envelope(marker, encryptor.encrypt(body).map_err(io::Error::other)?);
                self.check_wire_size(&message)?;
            }
            self.transport.send_until(message, deadline).await?;
        }
        Ok(())
    }

    /// Refuses an oversized queue item before it can retain connection capacity.
    pub(crate) fn check_body_size(body: &[u8]) -> io::Result<()> {
        if body.len() > BatchLimits::default().max_decoded_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native outgoing batch exceeds decoded byte limit",
            ));
        }
        Ok(())
    }

    /// Reads a complete batch, retaining decoded siblings across every protocol transition.
    async fn fill(&mut self) -> io::Result<()> {
        while self.pending.is_empty() {
            let mut message = self.transport.recv().await?;
            self.check_wire_size(&message)?;
            if let Some((_, decryptor)) = &mut self.ciphers {
                let marker = self.transport.header().marker();
                let body = if let Some(marker) = marker {
                    let actual = *message
                        .first()
                        .ok_or_else(|| io::Error::other(BatchError::MissingBatchHeader))?;
                    if actual != marker {
                        return Err(io::Error::other(BatchError::InvalidBatchHeader(actual)));
                    }
                    message.slice(1..)
                } else {
                    message
                };
                message = envelope(marker, decryptor.decrypt(body).map_err(io::Error::other)?);
            }
            match self.codec().decode(message).map_err(io::Error::other)? {
                DecodeOutcome::Packets {
                    packets,
                    skipped_empty_packets,
                    ..
                } => {
                    if skipped_empty_packets != 0 {
                        tracing::debug!(
                            skipped_empty_packets,
                            "skipped empty native batch entries"
                        );
                    }
                    self.pending.extend(packets);
                }
                DecodeOutcome::Dropped(reason) => tracing::debug!(?reason, "dropped native batch"),
            }
        }
        Ok(())
    }

    /// Restores early raw frames ahead of unread siblings on this same FIFO.
    pub(super) fn replay_deferred(&mut self) {
        for packet in self.take_deferred().into_iter().rev() {
            let bytes = packet.inner_frame();
            let mut reader = wire::Reader::from_shared(bytes);
            reader
                .read_var_u32()
                .expect("deferred packet has a validated length prefix");
            self.pending.push_front(bytes.slice(reader.position()..));
        }
    }

    /// Removes one original complete header/body while retaining siblings for the next call.
    pub(super) async fn next_raw(&mut self) -> io::Result<Bytes> {
        self.current = None;
        self.fill().await?;
        let bytes = self.pending.pop_front().expect("fill supplies packets");
        self.current = Some(bytes.clone());
        Ok(bytes)
    }

    /// Restores actual already-consumed handoff frames ahead of the original unread siblings.
    pub(super) fn prepend_raw(&mut self, packets: Vec<Bytes>) {
        for packet in packets.into_iter().rev() {
            self.pending.push_front(packet);
        }
    }

    /// Restores an unconsumed actual terminal before retiring a partial relay negotiation.
    pub(super) fn replay_current(&mut self) {
        if let Some(bytes) = self.current.take() {
            self.pending.push_front(bytes);
        }
    }

    /// Borrows a decoded terminal only when the last actual packet carried it.
    pub(super) fn current_terminal(&self) -> io::Result<Option<crate::session_wire::CoreMessage>> {
        self.current
            .as_ref()
            .map(|bytes| super::relay::terminal(bytes))
            .transpose()
            .map(Option::flatten)
    }

    /// Decodes one handshake packet while preserving every sibling from its batch.
    pub(super) async fn next_packet(&mut self) -> io::Result<Packet> {
        self.current = None;
        self.fill().await?;
        let bytes = self.pending.pop_front().expect("fill supplies packets");
        let mut reader = wire::Reader::from_shared(&bytes);
        let header = wire::PacketHeader::from_raw(reader.read_var_u32().map_err(io::Error::other)?);
        let id = packets::PacketId::from_raw(header.id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown native handshake packet",
            )
        })?;
        let body = bytes.slice(reader.position()..);
        let data = crate::codec::decode_body(id, &body).map_err(io::Error::other)?;
        self.current = Some(bytes);
        Ok(Packet::from_parts(
            GameHeader {
                id,
                from_subclient: header.sender_subclient.into(),
                to_subclient: header.target_subclient.into(),
            },
            data,
        ))
    }

    /// Handles terminals immediately and retains other unhandled input in its original wire form.
    pub(super) fn defer_unhandled(
        &mut self,
        packet: packets::Packet,
    ) -> Result<(), crate::ProtocolError> {
        if matches!(
            packet,
            packets::Packet::Disconnect(_) | packets::Packet::Transfer(_)
        ) {
            if let Some(error) = self.negotiation_failure(packet) {
                return Err(error);
            }
            return Ok(());
        }
        let bytes = self.current.take().ok_or_else(|| {
            super::failure(io::Error::new(
                io::ErrorKind::InvalidData,
                "native deferred packet has no received frame",
            ))
        })?;
        retain_early(&mut self.deferred, bytes)
    }

    /// Transfers bounded early input once, ahead of packets not yet consumed by negotiation.
    pub(super) fn take_deferred(&mut self) -> Vec<crate::raw_packet::RawPacket> {
        self.current = None;
        std::mem::take(&mut self.deferred).into_packets()
    }

    /// Checks ciphertext input before decrypting or allocating decoded state.
    fn check_wire_size(&self, message: &[u8]) -> io::Result<()> {
        if message.len() > self.limits.max_encoded_bytes {
            return Err(io::Error::other(BatchError::EncodedTooLarge {
                actual: message.len(),
                maximum: self.limits.max_encoded_bytes,
            }));
        }
        Ok(())
    }
}

/// Restores only the length prefix around original packet bytes under the existing startup bound.
fn retain_early(deferred: &mut DeferredPackets, bytes: Bytes) -> Result<(), crate::ProtocolError> {
    let mut frame = Vec::new();
    wire::Writer::with_limit(&mut frame, MAX_DEFERRED_PACKET_BYTES)
        .write_len_var_u32(bytes.len())
        .map_err(io::Error::other)
        .map_err(super::failure)?;
    let retained = frame.len().saturating_add(bytes.len());
    if retained > MAX_DEFERRED_PACKET_BYTES {
        return Err(crate::codec::SessionFailure::BatchTooLarge {
            actual: retained,
            max: MAX_DEFERRED_PACKET_BYTES,
        }
        .into());
    }
    frame.extend_from_slice(&bytes);
    let mut packets =
        crate::raw_packet::decode_batch_body(Bytes::from(frame), MAX_DEFERRED_PACKET_BYTES)?;
    let packet = packets.pop().expect("one original packet was framed");
    deferred.push(packet)?;
    Ok(())
}

/// Restores the cleartext marker around a directional cipher body when required.
fn envelope(marker: Option<u8>, body: Bytes) -> Bytes {
    if let Some(marker) = marker {
        let mut message = BytesMut::with_capacity(body.len() + 1);
        message.extend_from_slice(&[marker]);
        message.extend_from_slice(&body);
        message.freeze()
    } else {
        body
    }
}

impl PacketConnection for Connection {
    /// Advances the cipher only inside this owner's uninterrupted write operation.
    async fn send_body(&mut self, body: Bytes) -> io::Result<()> {
        self.send_body_until(body, None).await
    }

    /// Reframes the unread batch as ordinary Bedrock entries for lazy play decoding.
    async fn recv_body(&mut self) -> io::Result<Bytes> {
        self.fill().await?;
        let mut body = Vec::new();
        for packet in self.pending.drain(..) {
            let mut writer = wire::Writer::with_limit(&mut body, self.limits.max_decoded_bytes);
            writer
                .write_len_var_u32(packet.len())
                .map_err(io::Error::other)?;
            body.extend_from_slice(&packet);
        }
        Ok(Bytes::from(body))
    }

    /// Keeps protected service progress alive without decoding or consuming game input.
    async fn maintain(&mut self) {
        self.transport.maintain().await;
    }

    /// Ends the actual transport, without starting another connection.
    async fn close(&mut self) -> io::Result<()> {
        self.transport.close().await
    }
}

#[cfg(test)]
mod tests;
