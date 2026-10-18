//! Retained profile phases and their account retirement boundary.

use super::*;
use crate::remote_images::ImageDirectory;
use source::Loaded;
use std::future::Future;
use tokio::{sync::watch, time::Instant as AsyncInstant};

pub(super) enum Outcome {
    Complete(Result<Loaded, Failure>, bool),
    Retired,
    Stopped,
}

pub(super) enum Failure {
    Unavailable,
    TimedOut,
}

enum Exit {
    Retired,
    Stopped,
    TimedOut,
    Unavailable,
}

/// Pins each actual fetch/artwork phase across same-account refresh wakes.
/// Generation/source changes and terminal stop drop it before a replacement starts.
/// Fetch and artwork retain their original independent finite time budgets.
pub(super) async fn run(
    socket_dir: &std::path::Path,
    images: &ImageDirectory,
    shared: &Mutex<Snapshot>,
    selection: &Selection,
    requests: &mut watch::Receiver<Wake>,
    timeout: Duration,
) -> Outcome {
    let mut queued_refresh = false;
    let result = async {
        let mut profile = wait(
            selection.source.fetch(socket_dir),
            shared,
            selection,
            requests,
            AsyncInstant::now() + timeout,
            &mut queued_refresh,
        )
        .await?
        .map_err(|_| Exit::Unavailable)?;
        wait(
            async {
                artwork::fill(images, artwork::profile_slots(profile.profile_mut())).await;
                profile
            },
            shared,
            selection,
            requests,
            AsyncInstant::now() + ARTWORK_BUDGET,
            &mut queued_refresh,
        )
        .await
    }
    .await;
    match result {
        Ok(profile) => Outcome::Complete(Ok(profile), queued_refresh),
        Err(Exit::Unavailable) => Outcome::Complete(Err(Failure::Unavailable), queued_refresh),
        Err(Exit::TimedOut) => Outcome::Complete(Err(Failure::TimedOut), queued_refresh),
        Err(Exit::Retired) => Outcome::Retired,
        Err(Exit::Stopped) => Outcome::Stopped,
    }
}

/// Polls a single retained phase until completion, its original deadline or account retirement.
async fn wait<T>(
    operation: impl Future<Output = T>,
    shared: &Mutex<Snapshot>,
    selection: &Selection,
    requests: &mut watch::Receiver<Wake>,
    deadline: AsyncInstant,
    queued_refresh: &mut bool,
) -> Result<T, Exit> {
    tokio::pin!(operation);
    loop {
        match requests.has_changed() {
            Ok(changed) => *queued_refresh |= changed,
            Err(_) => return Err(Exit::Stopped),
        }
        if *requests.borrow_and_update() == Wake::Stop {
            return Err(Exit::Stopped);
        }
        if !selection.current(&shared.lock().unwrap_or_else(|poison| poison.into_inner())) {
            return Err(Exit::Retired);
        }
        if AsyncInstant::now() >= deadline {
            return Err(Exit::TimedOut);
        }
        tokio::select! {
            changed = requests.changed() => {
                if changed.is_err() { return Err(Exit::Stopped); }
                *queued_refresh = true;
            },
            _ = tokio::time::sleep_until(deadline) => return Err(Exit::TimedOut),
            result = &mut operation => {
                return if AsyncInstant::now() >= deadline { Err(Exit::TimedOut) } else { Ok(result) };
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Waker};

    /// A refresh retains the original reply; terminal stop also defeats a later account callback.
    #[tokio::test]
    async fn refresh_preserves_pending_phase_and_late_account_wake_cannot_revive_stop() {
        let (sender, mut requests) = channel();
        requests.borrow_and_update();
        let shared = Mutex::new(Snapshot {
            profile_wake: Some(sender.clone()),
            ..Default::default()
        });
        let selection = Selection::capture(&shared);
        let mut queued_refresh = false;
        let (reply, response) = tokio::sync::oneshot::channel();
        let mut phase = Box::pin(wait(
            response,
            &shared,
            &selection,
            &mut requests,
            AsyncInstant::now() + RESPONSE_TIMEOUT,
            &mut queued_refresh,
        ));
        let mut context = Context::from_waker(Waker::noop());
        assert!(phase.as_mut().poll(&mut context).is_pending());
        assert!(refresh(&sender));
        assert!(phase.as_mut().poll(&mut context).is_pending());
        assert!(!reply.is_closed(), "refresh canceled the original reply");
        reply.send("original reply").unwrap();
        assert!(matches!(phase.await, Ok(Ok("original reply"))));
        assert!(queued_refresh);

        let (reply, response) = tokio::sync::oneshot::channel::<()>();
        let mut phase = Box::pin(wait(
            response,
            &shared,
            &selection,
            &mut requests,
            AsyncInstant::now() + RESPONSE_TIMEOUT,
            &mut queued_refresh,
        ));
        assert!(phase.as_mut().poll(&mut context).is_pending());
        stop(&sender);
        shared.lock().unwrap().retire_account_data();
        assert!(matches!(phase.await, Err(Exit::Stopped)));
        assert!(reply.is_closed(), "stop retained an active phase");
    }
}
