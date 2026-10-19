//! New-feature controls on the original native pump and read-ahead FIFO, without a cipher model.
use super::*;
use crate::session_transport::{Inbound, MAX_READ_AHEAD_BYTES};
use bedrock_protocol_1_26_51::{packets, wire};
use std::task::Poll;

/// The existing packet-connection boundary with independently observed original input and writes.
struct DuplexConnection {
    incoming: mpsc::Receiver<Bytes>,
    outgoing: mpsc::Sender<Bytes>,
}

impl PacketConnection for DuplexConnection {
    async fn send_body(&mut self, body: Bytes) -> io::Result<()> {
        self.outgoing.send(body).await.map_err(io::Error::other)
    }

    async fn recv_body(&mut self) -> io::Result<Bytes> {
        self.incoming
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "fixture input ended"))
    }

    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Supplies the original bounded send/receive owners, without another payload queue or task.
fn pair() -> (SessionTransport, mpsc::Sender<Bytes>, mpsc::Receiver<Bytes>) {
    let (incoming, receive) = mpsc::channel(QUEUED_BATCHES);
    let (send, outgoing) = mpsc::channel(QUEUED_BATCHES);
    let transport = SessionTransport::from_native(DuplexConnection {
        incoming: receive,
        outgoing: send,
    });
    (transport, incoming, outgoing)
}

/// Uses the original codec's body, shared by this owning boundary's incoming and outgoing fixture.
fn body(index: usize) -> Bytes {
    crate::session_transport::batch_body(&[crate::Packet::new(packets::SetTime {
        time: wire::ZigZag32(index.try_into().unwrap()),
    })])
    .unwrap()
}

/// Reads the original accepted raw entry through its existing generated decoder.
fn time(inbound: Inbound) -> i32 {
    let Inbound::Packet(raw) = inbound else {
        panic!("unexpected fixture terminal")
    };
    let packets::Packet::SetTime(packet) = *raw.decode().unwrap().data else {
        panic!("unexpected fixture packet")
    };
    packet.time.0
}

#[tokio::test(start_paused = true)]
async fn native_delay_preserves_original_bidirectional_batch_pipeline() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let (mut transport, incoming, mut outgoing) = pair();
        let delay = transport.packet_delay().unwrap();
        // This is a pump-boundary control, not a fabricated authenticated SpawnReady.
        delay.bind(42);
        delay.configure(200, false).unwrap();
        let start = tokio::time::Instant::now();
        let sender = transport.sender();
        let mut receipts = Vec::new();
        let expected: Vec<_> = (0..QUEUED_BATCHES).map(body).collect();
        for body in &expected {
            incoming.send(body.clone()).await.unwrap();
            receipts.push(sender.accept(body.clone()).await.unwrap());
        }
        for _ in 0..QUEUED_BATCHES {
            std::future::poll_fn(|cx| transport.poll_read_ahead_one(MAX_READ_AHEAD_BYTES, cx))
                .await;
        }
        tokio::task::yield_now().await;
        let mut received = Vec::new();
        let early_incoming = {
            let mut receive = std::pin::pin!(transport.recv());
            match std::future::poll_fn(|cx| Poll::Ready(receive.as_mut().poll(cx))).await {
                Poll::Ready(result) => {
                    received.push(time(result.unwrap()));
                    true
                }
                Poll::Pending => false,
            }
        };
        let first_outgoing = outgoing.try_recv().ok();
        let early_outgoing = first_outgoing.is_some();
        let mut sent: Vec<_> = first_outgoing.into_iter().collect();
        tokio::time::advance(Duration::from_millis(200)).await;
        while received.len() < QUEUED_BATCHES {
            received.push(time(transport.recv().await.unwrap()));
        }
        while sent.len() < QUEUED_BATCHES {
            sent.push(outgoing.recv().await.unwrap());
        }
        for receipt in receipts {
            receipt.await.unwrap();
        }
        let elapsed = start.elapsed();
        transport.close().await.unwrap();
        assert!(
            !early_outgoing,
            "native post-login batches bypassed the configured outgoing delay"
        );
        assert!(
            !early_incoming,
            "native post-login batches bypassed the configured incoming delay"
        );
        assert_eq!(
            sent, expected,
            "original admitted bodies changed or reordered"
        );
        assert_eq!(
            received,
            (0..QUEUED_BATCHES)
                .map(|index| i32::try_from(index).unwrap())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            elapsed,
            Duration::from_millis(200),
            "each original admission keeps its own deadline, without serial delays"
        );
    })
    .await
    .expect("original delayed pump failed to finish under its finite fixture budget");
}
