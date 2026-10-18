//! Actual channel and protocol-boundary witnesses for competing application service termination.

use super::super::*;
use super::{SessionCompanion, run_with_companion};
use std::{future::poll_fn, io, sync::Mutex, task::Poll};
use tokio::{io::AsyncWriteExt, sync::oneshot};

#[derive(Clone)]
struct NoOutbound;

impl NetworkExecutor for NoOutbound {
    type OutboundTask = ();

    fn spawn_outbound(&self, _: impl Future<Output = ()> + Send + 'static) {
        panic!("startup terminal cannot admit outbound play");
    }

    fn timers(&self) -> bridge::tasks::Timers {
        bridge::tasks::Timers(|_| Box::pin(std::future::pending()))
    }
}

struct InlinePreparation;

impl protocol::JoinPreparation for InlinePreparation {
    type Task<T: Send + 'static> = std::future::Ready<T>;

    fn submit<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Self::Task<T> {
        std::future::ready(work())
    }
}

/// Decodes a genuine core terminal through the same framed protocol login as NetworkChannels.
async fn decoded_terminal(kind: u8, body: &[u8]) -> protocol::ProtocolError {
    let mut frame = vec![kind];
    frame.extend_from_slice(body);
    let (reader, mut peer) = tokio::io::duplex(4096);
    peer.write_all(&u32::try_from(frame.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    peer.write_all(&frame).await.unwrap();
    let (reader, frames, writer) = bridge::tasks::prepare_bounded_connection(
        reader,
        tokio::io::sink(),
        bridge::MAX_FRAME_LEN,
        4096,
    );
    let writer = tokio::spawn(writer);
    let settings = protocol::LoginSettings::default();
    let result = protocol::LoginSequence::connect_prepared(
        protocol::SessionIo {
            reader,
            frames,
            timers: bridge::tasks::Timers::tokio(),
        },
        protocol::SessionLogin {
            display_name: "Terminal fixture",
            client_random_id: protocol::ClientRandomId::new(1).unwrap(),
            cache: None,
            skin: None,
            settings: &settings,
            tap: None,
            target: None,
        },
        &InlinePreparation,
        |_, _| (),
    )
    .await;
    writer.await.unwrap();
    match result {
        Err(error) => error,
        Ok(_) => panic!("terminal frame admitted a play session"),
    }
}

struct FailedCompanion {
    failed: oneshot::Receiver<()>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl SessionCompanion for FailedCompanion {
    async fn drive(&mut self) -> io::Result<()> {
        let _ = (&mut self.failed).await;
        Err(io::Error::other("companion fixture failed"))
    }

    async fn close(&mut self) {
        self.order.lock().unwrap().push("companion close");
    }
}

/// Occupies actual channel capacity with nonterminal FIFO records.
fn fill_controls(sender: &mpsc::Sender<NetworkControlEvent<()>>) {
    for _ in 0..CONTROL_EVENT_CAPACITY {
        sender
            .try_send(NetworkControlEvent::BlobCacheTelemetry {
                enabled: false,
                stats: Default::default(),
            })
            .unwrap();
    }
}

/// Releases the exact accepted prefix without consuming any subsequent terminal.
fn release_controls(handle: &mut NetworkHandle<()>) {
    for _ in 0..CONTROL_EVENT_CAPACITY {
        assert!(matches!(
            handle.control_events.try_recv(),
            Ok(NetworkControlEvent::BlobCacheTelemetry { enabled: false, .. })
        ));
    }
}

/// Checks original decoded identity, with no generic companion replacement or trailing terminal.
fn require_original_terminal(
    handle: &mut NetworkHandle<()>,
    expected_message: &str,
    disconnect: Option<protocol::ServerDisconnectEvent>,
    transfer: Option<protocol::ServerTransferEvent>,
) {
    // Play may publish its finite final telemetry before the retained terminal.
    let event = loop {
        match handle.control_events.try_recv() {
            Ok(NetworkControlEvent::BlobCacheTelemetry { .. }) => continue,
            Ok(event) => break event,
            Err(_) => panic!("the accepted network terminal was lost to companion cancellation"),
        }
    };
    match (event, transfer) {
        (NetworkControlEvent::Transferred { target, .. }, Some(expected)) => {
            assert_eq!(target.host, expected.host);
            assert_eq!(target.port, expected.port);
        }
        (
            NetworkControlEvent::Failed {
                message,
                server_disconnect,
                ..
            },
            None,
        ) => {
            assert_eq!(message, expected_message);
            assert_eq!(server_disconnect, disconnect);
        }
        _ => panic!("original network classification was replaced"),
    }
    assert!(
        handle.control_events.try_recv().is_err(),
        "a second terminal followed the original"
    );
}

/// Runs the real startup owner with a decoded terminal blocked on its full control FIFO.
async fn startup_terminal(kind: u8, body: &[u8]) {
    let error = decoded_terminal(kind, body).await;
    let message = error.to_string();
    let disconnect = error.server_disconnect();
    let transfer = error.server_transfer();
    let (mut handle, channels) = network_channels::<()>(SessionGeneration::new(1));
    fill_controls(&channels.control_event_tx);
    let shutdown = handle.shutdown.clone();
    let (fail, failed) = oneshot::channel();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut companion = FailedCompanion {
        failed,
        order: Arc::clone(&order),
    };
    let work = channels.run(
        |_| async move { Err::<PreparedLoginReader, _>(error) },
        NoOutbound,
        SessionTrace::default(),
    );
    let worker = async {
        let result = run_with_companion(work, &mut companion, &shutdown).await;
        order.lock().unwrap().push("network finished");
        companion.close().await;
        result
    };
    tokio::pin!(worker);
    assert!(
        poll_fn(|cx| Poll::Ready(worker.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    fail.send(()).unwrap();
    let competing = poll_fn(|cx| Poll::Ready(worker.as_mut().poll(cx))).await;
    release_controls(&mut handle);
    let result = match competing {
        Poll::Ready(result) => result,
        Poll::Pending => worker.await,
    };
    require_original_terminal(&mut handle, &message, disconnect, transfer);
    assert!(
        result.is_ok(),
        "accepted network termination must own the outcome"
    );
    assert_eq!(
        *order.lock().unwrap(),
        ["network finished", "companion close"]
    );
}

type PreparedLoginReader = (
    protocol::PlaySession<bridge::FramedReader<tokio::io::DuplexStream>>,
    (
        protocol::GameData,
        Option<Result<PreparedSession<()>, String>>,
    ),
);

#[tokio::test]
async fn retained_startup_transfer_survives_companion_failure_with_full_controls() {
    startup_terminal(
        bridge::contract::KIND_TRANSFER,
        br#"{"address":"next.example.test","port":19133,"reload_world":false}"#,
    )
    .await;
}

#[tokio::test]
async fn retained_startup_disconnect_survives_companion_failure_with_full_controls() {
    startup_terminal(bridge::contract::KIND_DISCONNECT, br#"{"reason":25,"message":"original server disconnect","filtered_message":"","hide_screen":false}"#).await;
}

#[tokio::test]
async fn retained_startup_error_survives_companion_failure_with_full_controls() {
    startup_terminal(bridge::contract::KIND_BATCH, &[0xfe]).await;
}

#[tokio::test]
async fn companion_only_failure_cancels_pending_network_before_cleanup() {
    let (mut handle, channels) = network_channels::<()>(SessionGeneration::new(1));
    let shutdown = handle.shutdown.clone();
    let (fail, failed) = oneshot::channel();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut companion = FailedCompanion {
        failed,
        order: Arc::clone(&order),
    };
    let work = channels.run(
        |_| std::future::pending::<Result<PreparedLoginReader, protocol::ProtocolError>>(),
        NoOutbound,
        SessionTrace::default(),
    );
    let worker = async {
        let result = run_with_companion(work, &mut companion, &shutdown).await;
        order.lock().unwrap().push("network finished");
        companion.close().await;
        result
    };
    tokio::pin!(worker);
    assert!(
        poll_fn(|cx| Poll::Ready(worker.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    fail.send(()).unwrap();
    assert_eq!(
        worker.await.unwrap_err().to_string(),
        "companion fixture failed"
    );
    assert!(*shutdown.borrow());
    assert!(handle.control_events.try_recv().is_err());
    assert_eq!(
        *order.lock().unwrap(),
        ["network finished", "companion close"]
    );
}
