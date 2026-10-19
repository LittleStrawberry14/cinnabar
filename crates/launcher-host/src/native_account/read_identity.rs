//! Protected final identity checks for account-borrowed presentation results.

use super::*;

pub(super) struct ReadIdentity {
    pub(super) account: Arc<NativeAccount>,
    pub(super) generation: String,
    pub(super) xuid: String,
}

impl ReadIdentity {
    /// Captures the actual verified owner while its original credential lease is still held.
    pub(super) fn capture(operation: &Operation) -> Self {
        let cached = operation.cached.as_ref().expect("verified account");
        Self {
            account: Arc::clone(&operation.account),
            generation: cached.generation.clone(),
            xuid: cached
                .identity
                .as_ref()
                .expect("verified identity")
                .xuid
                .clone(),
        }
    }

    /// Rechecks selection after asynchronous work without authenticating or replacing its proof.
    pub(super) fn current(&self) -> bool {
        self.current_status().unwrap_or(false)
    }

    /// Retains a completed result through contention; its caller owns cancellation and the cutoff.
    pub(super) async fn wait_current(&self) -> bool {
        loop {
            if let Some(current) = self.current_status() {
                return current;
            }
            tokio::time::sleep(LEASE_RETRY).await;
        }
    }

    /// An unavailable lease or owner is not evidence that its verified identity was replaced.
    fn current_status(&self) -> Option<bool> {
        let lease = match self.account.store.try_native_lease() {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
            Err(_) => return Some(false),
        };
        let state = match self.account.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(_)) => return Some(false),
        };
        if state.busy {
            return None;
        }
        let Some(cached) = state.cached.as_ref() else {
            return Some(false);
        };
        Some(
            cached.generation == self.generation
                && lease.generation() == self.generation
                && cached.binding == lease.binding()
                && lease
                    .selected_id()
                    .is_none_or(|selected| selected == self.xuid)
                && self
                    .account
                    .selected_id
                    .as_deref()
                    .is_none_or(|selected| lease.selected_id() == Some(selected))
                && cached.identity.as_ref().is_some_and(|identity| {
                    identity.current_credential && identity.xuid == self.xuid
                }),
        )
    }
}
