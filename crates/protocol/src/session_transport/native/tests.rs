use super::*;
use std::future::Future;

struct ReplayConnection {
    incoming: mpsc::Receiver<Bytes>,
    fifth_read: Option<oneshot::Sender<()>>,
    reads: usize,
}

#[tokio::test(start_paused = true)]
async fn a_permit_committed_after_failure_is_rejected_without_waiting_for_ingress() {
    use bedrock_protocol_1_26_51::packets;
    let (incoming, receive) = mpsc::channel(QUEUED_BATCHES + 1);
    let (fifth_read, received_fifth) = oneshot::channel();
    let transport = SessionTransport::from_native(ReplayConnection {
        incoming: receive,
        fifth_read: Some(fifth_read),
        reads: 0,
    });
    let body =
        crate::session_transport::batch_body(&[crate::Packet::new(packets::SetTime::default())])
            .unwrap();
    for _ in 0..=QUEUED_BATCHES {
        incoming.send(body.clone()).await.unwrap();
    }
    received_fifth.await.unwrap();
    let PacketSender::Native(sender) = transport.sender() else {
        panic!("native fixture")
    };
    let sender = sender.0;
    let permit = sender.reserve().await.unwrap();
    assert!(transport.sender().send(body.clone()).await.is_err());
    let (written, receipt) = oneshot::channel();
    permit.send(SendBatch { body, written });
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), receipt).await;
    assert!(
        result.is_ok(),
        "a pre-close admission kept its receipt unresolved"
    );
    assert!(result.unwrap().map_or(true, |written| written.is_err()));
}

impl PacketConnection for ReplayConnection {
    async fn send_body(&mut self, _: Bytes) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "peer closed"))
    }

    async fn recv_body(&mut self) -> io::Result<Bytes> {
        let bytes = self
            .incoming
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed"))?;
        self.reads += 1;
        if self.reads == QUEUED_BATCHES + 1
            && let Some(read) = self.fifth_read.take()
        {
            let _ = read.send(());
        }
        Ok(bytes)
    }

    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn write_failure_keeps_a_received_transfer_behind_full_ingress() {
    use bedrock_protocol_1_26_51::{packets, wire};
    let (incoming, receive) = mpsc::channel(QUEUED_BATCHES + 1);
    let (fifth_read, received_fifth) = oneshot::channel();
    let mut transport = SessionTransport::from_native(ReplayConnection {
        incoming: receive,
        fifth_read: Some(fifth_read),
        reads: 0,
    });
    for _ in 0..QUEUED_BATCHES {
        incoming
            .send(
                crate::session_transport::batch_body(&[crate::Packet::new(
                    packets::SetTime::default(),
                )])
                .unwrap(),
            )
            .await
            .unwrap();
    }
    incoming
        .send(
            crate::session_transport::batch_body(&[crate::Packet::new(packets::Transfer {
                server_address: "target.example".into(),
                server_port: wire::U16LE(19132),
                ..Default::default()
            })])
            .unwrap(),
        )
        .await
        .unwrap();
    received_fifth.await.unwrap();
    let body =
        crate::session_transport::batch_body(&[crate::Packet::new(packets::SetTime::default())])
            .unwrap();
    assert!(transport.sender().send(body).await.is_err());
    for _ in 0..QUEUED_BATCHES {
        assert!(matches!(
            transport.recv().await.unwrap(),
            super::super::Inbound::Packet(_)
        ));
    }
    let super::super::Inbound::Packet(packet) = transport.recv().await.unwrap() else {
        panic!("expected original Transfer")
    };
    assert_eq!(packet.id, packets::PacketId::Transfer);
}

#[tokio::test]
async fn cancelling_admission_neither_accepts_nor_repeats_a_batch() {
    let (send, mut receive) = mpsc::channel(1);
    let sender = NativeSender(send);
    let first = sender.accept(Bytes::from_static(b"first")).await.unwrap();
    {
        let mut blocked = std::pin::pin!(sender.accept(Bytes::from_static(b"cancelled")));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(blocked.as_mut().poll(&mut cx).is_pending());
    }
    let accepted = receive.recv().await.unwrap();
    assert_eq!(accepted.body, b"first"[..]);
    accepted.written.send(Ok(())).unwrap();
    first.await.unwrap().unwrap();
    assert!(matches!(
        receive.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let receipt = sender.accept(Bytes::from_static(b"next")).await.unwrap();
    drop(receipt);
    let accepted = receive.recv().await.unwrap();
    assert_eq!(accepted.body, b"next"[..]);
    assert!(accepted.written.send(Ok(())).is_err());
    assert!(matches!(
        receive.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn oversized_batches_never_consume_queue_capacity() {
    let (send, mut receive) = mpsc::channel(1);
    let sender = NativeSender(send);
    let bytes = Bytes::from(vec![
        0;
        bedrock_net::BatchLimits::default().max_decoded_bytes
            + 1
    ]);
    assert_eq!(
        sender.accept(bytes).await.unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(matches!(
        receive.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let _receipt = sender.accept(Bytes::from_static(b"fits")).await.unwrap();
    assert_eq!(receive.recv().await.unwrap().body, b"fits"[..]);
}
