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
        let Ok(lease) = self.account.store.try_native_lease() else {
            return false;
        };
        let Ok(state) = self.account.state.try_lock() else {
            return false;
        };
        let Some(cached) = state.cached.as_ref() else {
            return false;
        };
        !state.busy
            && cached.generation == self.generation
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
            && cached
                .identity
                .as_ref()
                .is_some_and(|identity| identity.current_credential && identity.xuid == self.xuid)
    }
}
