//! One cancellable account catalog read on the application's background runtime.

use super::{CatalogListing, NativeAccount};
use crossbeam_channel::{Receiver, bounded};
use std::{io, sync::Arc};
use tokio::sync::oneshot;

/// Owns one request's cancellation. Dropping it never waits on HTTP or credential persistence.
pub struct CatalogWorker {
    result: Receiver<io::Result<CatalogListing>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl std::fmt::Debug for CatalogWorker {
    /// Omits account identities, paths and SDK state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CatalogWorker")
    }
}

impl CatalogWorker {
    /// Starts one request; the shared account serializes cleanup against a subsequent connection prepare.
    pub fn start(account: Arc<NativeAccount>) -> io::Result<Self> {
        let (sender, result) = bounded(1);
        let (cancel, mut cancelled) = oneshot::channel();
        std::thread::Builder::new()
            .name("native-catalog".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async {
                        tokio::select! { biased;
                            _ = &mut cancelled => None,
                            result = account.list_catalog() => Some(result),
                        }
                    }),
                    Err(_) => Some(Err(io::Error::other(
                        "native account catalog worker could not start",
                    ))),
                };
                if let Some(result) = result {
                    let _ = sender.try_send(result);
                }
            })?;
        Ok(Self {
            result,
            cancel: Some(cancel),
        })
    }

    /// Reads a completed list without waiting from the frame.
    pub fn poll(&mut self) -> Option<io::Result<CatalogListing>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Err(io::Error::other(
                "native account catalog worker stopped",
            ))),
        }
    }
}

impl Drop for CatalogWorker {
    /// Cancellation drops only the HTTP future; Operation retains acquired refresh until protected persistence.
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
