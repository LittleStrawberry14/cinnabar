//! The invite screen's requests, the account's Xbox friends and the invites the player sends,
//! run in order on their own worker so neither waits on the polling workers.

use super::profile_worker::RESPONSE_TIMEOUT;
use super::*;
use tokio::sync::watch;

mod source;
pub(super) use source::Source;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wake {
    Account,
    Stop,
}

/// The original request queue plus cancellation for its retained asynchronous operation.
pub(super) struct Requests {
    requests: Sender<Request>,
    wake: watch::Sender<Wake>,
}

struct Selection {
    generation: u64,
    source: Source,
}

impl Selection {
    /// Captures both the UI generation and the concrete reader when the request is queued.
    fn capture(snapshot: &Snapshot) -> Self {
        Self {
            generation: snapshot.auth_generation,
            source: snapshot.people_source.clone(),
        }
    }

    /// Rejects replaced account requests before dispatch and after artwork.
    fn current(&self, snapshot: &Snapshot) -> bool {
        self.generation == snapshot.auth_generation
            && self.source.same_account(&snapshot.people_source)
    }
}

impl Requests {
    /// Queues the selected owner rather than resolving a later account at worker dispatch.
    pub(super) fn people(&self, shared: &Mutex<Snapshot>) {
        let selected = Selection::capture(&shared.lock().unwrap_or_else(|p| p.into_inner()));
        let _ = self.requests.send(Request::People(selected));
    }

    /// Keeps outgoing invitations on the original active Go host and requesting generation.
    pub(super) fn send(&self, shared: &Mutex<Snapshot>, xuids: Vec<String>) {
        let selected = Selection::capture(&shared.lock().unwrap_or_else(|p| p.into_inner()));
        let _ = self.requests.send(Request::Send(selected, xuids));
    }
}

impl Drop for Requests {
    /// Cancels the actual in-flight read/RPC before its tracked worker leaves the app runtime.
    fn drop(&mut self) {
        self.wake.send_replace(Wake::Stop);
    }
}

/// Coalesces account retirement without reviving a stopped request worker.
pub(super) fn retire(sender: &watch::Sender<Wake>) {
    sender.send_if_modified(|wake| *wake != Wake::Stop);
}

enum Request {
    People(Selection),
    Send(Selection, Vec<String>),
}

/// Starts the worker; dropping the account link stops it and drops what is still queued.
pub(super) fn start(
    socket_dir: PathBuf,
    artwork_dir: PathBuf,
    shared: Arc<Mutex<Snapshot>>,
    stop: Receiver<()>,
    runtime: Handle,
    tasks: TaskTracker,
) -> Requests {
    let (sender, requests) = crossbeam_channel::unbounded();
    let (wake, mut changed) = watch::channel(Wake::Account);
    shared.lock().unwrap_or_else(|p| p.into_inner()).people_wake = Some(wake.clone());
    thread::spawn(crate::worker::tracked(
        move || {
            let images = artwork::people_images(&artwork_dir);
            loop {
                let request = crossbeam_channel::select_biased! {
                    recv(stop) -> _ => return,
                    recv(requests) -> request => match request {
                        Ok(request) => request,
                        Err(_) => return,
                    },
                };
                match request {
                    Request::People(selected) => {
                        let people = runtime.block_on(people(
                            &socket_dir,
                            &images,
                            &shared,
                            &selected,
                            &mut changed,
                        ));
                        let mut snapshot = shared.lock().unwrap_or_else(|p| p.into_inner());
                        if *changed.borrow() != Wake::Stop && selected.current(&snapshot) {
                            snapshot.people = Some(people);
                        }
                    }
                    Request::Send(selected, xuids) => runtime.block_on(send(
                        &socket_dir,
                        &xuids,
                        &shared,
                        &selected,
                        &mut changed,
                    )),
                }
                if *changed.borrow() == Wake::Stop {
                    return;
                }
            }
        },
        &tasks,
    ));
    Requests {
        requests: sender,
        wake,
    }
}

/// Keeps one actual fetch and one actual artwork future through unrelated wakeups.
async fn people(
    socket_dir: &std::path::Path,
    images: &crate::remote_images::ImageDirectory,
    shared: &Mutex<Snapshot>,
    selected: &Selection,
    changed: &mut watch::Receiver<Wake>,
) -> Result<Vec<bridge::Person>, ()> {
    let loaded = wait(
        tokio::time::timeout(RESPONSE_TIMEOUT, selected.source.fetch(socket_dir)),
        shared,
        selected,
        changed,
    )
    .await?;
    let mut loaded = match loaded {
        Ok(Ok(people)) => people,
        Ok(Err(())) => {
            tracing::warn!("friends list unavailable");
            return Err(());
        }
        Err(_) => {
            tracing::warn!("friends list timed out");
            return Err(());
        }
    };
    wait(
        artwork::fill(images, artwork::people_slots(loaded.people_mut())),
        shared,
        selected,
        changed,
    )
    .await?;
    loaded.into_current()
}

/// Retains the pinned phase until completion, account retirement or terminal worker stop.
async fn wait<T>(
    phase: impl std::future::Future<Output = T>,
    shared: &Mutex<Snapshot>,
    selected: &Selection,
    changed: &mut watch::Receiver<Wake>,
) -> Result<T, ()> {
    tokio::pin!(phase);
    loop {
        let stopped = *changed.borrow_and_update() == Wake::Stop;
        if stopped || !selected.current(&shared.lock().unwrap_or_else(|p| p.into_inner())) {
            return Err(());
        }
        tokio::select! { biased;
            result = changed.changed() => if result.is_err() { return Err(()); },
            result = &mut phase => return Ok(result),
        }
    }
}

/// Invites each friend through the active Go host; retirement cancels its current RPC.
async fn send(
    socket_dir: &std::path::Path,
    xuids: &[String],
    shared: &Mutex<Snapshot>,
    selected: &Selection,
    changed: &mut watch::Receiver<Wake>,
) {
    for xuid in xuids {
        let sent = wait(
            tokio::time::timeout(RESPONSE_TIMEOUT, bridge::invite_to_world(socket_dir, xuid)),
            shared,
            selected,
            changed,
        )
        .await;
        match sent {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(_))) => tracing::warn!("game invite not sent"),
            Ok(Err(_)) => tracing::warn!("game invite timed out"),
            Err(()) => return,
        }
    }
}

#[cfg(test)]
mod tests;
