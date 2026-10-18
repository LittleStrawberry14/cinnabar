use super::*;
use std::future::Future;

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
