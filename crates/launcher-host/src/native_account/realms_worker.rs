//! One cancellable Realms listing on the application's background runtime.

use super::{NativeAccount, RealmListing};
use crossbeam_channel::{Receiver, bounded};
use std::{io, sync::Arc};
use tokio::sync::oneshot;

/// Owns one request's cancellation. Dropping it never waits on HTTP or credential persistence.
pub struct RealmsWorker {
    result: Receiver<io::Result<RealmListing>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl std::fmt::Debug for RealmsWorker {
    /// Omits account identities, paths and SDK state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RealmsWorker")
    }
}

impl RealmsWorker {
    /// Starts one request; the shared account serializes cleanup against a subsequent connection prepare.
    pub fn start(account: Arc<NativeAccount>) -> io::Result<Self> {
        let (sender, result) = bounded(1);
        let (cancel, mut cancelled) = oneshot::channel();
        std::thread::Builder::new()
            .name("native-realms".into())
            .spawn(move || {
                let result = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async {
                        tokio::select! { biased;
                            _ = &mut cancelled => None,
                            result = account.list_realms() => Some(result),
                        }
                    }),
                    Err(_) => Some(Err(io::Error::other("Realms worker could not start"))),
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
    pub fn poll(&mut self) -> Option<io::Result<RealmListing>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Some(Err(io::Error::other("Realms worker stopped")))
            }
        }
    }
}

impl Drop for RealmsWorker {
    /// Cancellation drops only the HTTP future; Operation retains acquired refresh until protected persistence.
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}
