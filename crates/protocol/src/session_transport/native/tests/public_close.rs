//! The ordinary close borrow must finish before the same receiver starts draining.

use super::*;
use crate::session_transport::Inbound;
use bedrock_protocol_1_26_51::{packets, wire};

/// Public close preserves all four queued bodies and the already accepted fifth for later reads.
#[tokio::test(start_paused = true)]
async fn public_close_preserves_the_pending_fifth_body_before_any_receive() {
    tokio::time::timeout(test_time::DEFAULT_TIMEOUT, async {
        let (incoming, receive) = mpsc::channel(QUEUED_BATCHES + 1);
        let (fifth_read, received_fifth) = oneshot::channel();
        let (closed, observed_close) = oneshot::channel();
        let mut transport = SessionTransport::from_native(ReplayConnection {
            incoming: receive,
            fifth_read: Some(fifth_read),
            reads: 0,
            closed: Some(closed),
        });
        for index in 0..=QUEUED_BATCHES {
            incoming
                .send(
                    crate::session_transport::batch_body(&[crate::Packet::new(packets::SetTime {
                        time: wire::ZigZag32(i32::try_from(index).unwrap()),
                    })])
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        received_fifth.await.unwrap();
        let close = transport.close().await;
        observed_close.await.unwrap();
        let mut received = Vec::new();
        for _ in 0..=QUEUED_BATCHES {
            match transport.recv().await {
                Ok(Inbound::Packet(packet)) => received.push(packet),
                _ => break,
            }
        }
        drop(transport);
        drop(incoming);
        assert!(
            close.is_ok(),
            "ordinary close waited for its exclusively borrowed receiver: {close:?}"
        );
        assert_eq!(received.len(), QUEUED_BATCHES + 1);
        for (index, raw) in received.into_iter().enumerate() {
            let packet = raw.decode().unwrap();
            let packets::Packet::SetTime(actual) = &*packet.data else {
                panic!("original SetTime body")
            };
            assert_eq!(actual.time.0, i32::try_from(index).unwrap());
        }
    })
    .await
    .expect("ordinary close and its accepted input did not retire");
}
