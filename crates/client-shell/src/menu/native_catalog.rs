//! The opt-in Realms and friend-world catalog has one selected account and one caller-driven request owner.

use super::*;
use launcher::menu::CATALOG_REFRESH_INTERVAL;
use launcher_host::native_account::{CatalogCards, CatalogWorker, NativeAccount};
use std::{sync::Arc, time::Instant};

const FAILURE_MESSAGE: &str = "Realms and friends: Refresh to try again.";

#[derive(Default)]
pub(super) struct Controller {
    selected: Option<Selection>,
    request: Option<Request>,
    epoch: u64,
    next_refresh: Option<Instant>,
}

struct Selection {
    path: PathBuf,
    identity: Option<String>,
    account: Arc<NativeAccount>,
}
struct Request {
    epoch: u64,
    worker: CatalogWorker,
}

impl std::fmt::Debug for Controller {
    /// Contains no account or network identifiers in diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeCatalog")
    }
}

impl Controller {
    /// Cancels the prior request before allowing another operation on the same SDK owner.
    pub(super) fn pause(&mut self) {
        self.request = None;
    }

    /// Retires pending results and requests a fresh catalog on the next eligible frame.
    pub(super) fn refresh(&mut self) {
        self.pause();
        self.epoch = self.epoch.wrapping_add(1);
        self.next_refresh = None;
    }

    /// Releases the selected owner on account mutations; the old worker may only finish protected cleanup.
    fn retire(&mut self) {
        self.refresh();
        self.selected = None;
    }

    /// Reuses one account across listing, Realm resolution and final key mint without filesystem access.
    fn account(
        &mut self,
        path: PathBuf,
        identity: Option<String>,
        device: PathBuf,
    ) -> (Arc<NativeAccount>, bool) {
        let changed = self
            .selected
            .as_ref()
            .is_none_or(|selection| selection.path != path || selection.identity != identity);
        if changed {
            self.retire();
            self.selected = Some(Selection {
                account: Arc::new(NativeAccount::new(
                    launcher::accounts::AccountStore::new(path.clone()),
                    device,
                    identity.clone(),
                )),
                path,
                identity,
            });
        }
        (
            Arc::clone(&self.selected.as_ref().expect("selected account").account),
            changed,
        )
    }
}

impl MenuRuntime {
    /// Takes the catalog's concrete account for a join after cancelling its request.
    /// The acquisition future waits off-frame until the old Operation finishes durable cleanup.
    pub fn native_account_for(&mut self, path: PathBuf) -> Arc<NativeAccount> {
        self.native_catalog.pause();
        let identity = self.selected_account_id().map(str::to_owned);
        let device = self.layout.device_profile_file();
        let (account, changed) = self.native_catalog.account(path, identity, device);
        if changed {
            self.realms.replace(Vec::new());
            self.friends.replace(Vec::new());
            self.feeds.selected_realm = None;
        }
        account
    }

    /// Replaces successful sections independently and retains rows when one service fails.
    fn apply_native_catalog(&mut self, cards: CatalogCards) {
        let mut failed = false;
        match cards.realms {
            Ok(cards) => {
                let selected = self
                    .feeds
                    .selected_realm
                    .and_then(|index| self.realms.get(index))
                    .map(|realm| realm.target.clone());
                self.realms.replace(cards);
                self.feeds.selected_realm = selected
                    .and_then(|target| self.realms.iter().position(|realm| realm.target == target));
            }
            Err(_) => failed = true,
        }
        match cards.friends {
            Ok(cards) => self.friends.replace(cards),
            Err(_) => failed = true,
        }
        if failed {
            self.catalog_message = Some(FAILURE_MESSAGE.into());
        } else if self.catalog_message.as_deref() == Some(FAILURE_MESSAGE) {
            self.catalog_message = None;
        }
    }

    /// Advances listing after account observations; Go catalog/control results never own this native feed.
    pub(super) fn poll_native_catalog(&mut self, now: Instant) {
        if !self.native_sign_in {
            return;
        }
        let Some(path) = self
            .launcher_auth_cache()
            .filter(|_| !self.account_change_pending())
        else {
            if self.native_catalog.selected.is_some()
                || !self.realms.is_empty()
                || !self.friends.is_empty()
            {
                self.native_catalog.retire();
                self.realms.replace(Vec::new());
                self.friends.replace(Vec::new());
                self.feeds.selected_realm = None;
            }
            return;
        };
        if !self.visible || self.is_connecting() {
            self.native_catalog.pause();
            return;
        }
        let identity = self.selected_account_id().map(str::to_owned);
        let device = self.layout.device_profile_file();
        let (account, changed) = self.native_catalog.account(path, identity, device);
        if changed {
            self.realms.replace(Vec::new());
            self.friends.replace(Vec::new());
            self.feeds.selected_realm = None;
        }
        if let Some(result) = self
            .native_catalog
            .request
            .as_mut()
            .and_then(|request| request.worker.poll())
        {
            let request = self
                .native_catalog
                .request
                .take()
                .expect("completed request");
            if request.epoch == self.native_catalog.epoch {
                match result
                    .ok()
                    .and_then(|listing| listing.into_current_cards(&account))
                {
                    Some(cards) => self.apply_native_catalog(cards),
                    None => self.catalog_message = Some(FAILURE_MESSAGE.into()),
                }
            }
            self.native_catalog.next_refresh = Some(now + CATALOG_REFRESH_INTERVAL);
        }
        if self.native_catalog.request.is_none()
            && self
                .native_catalog
                .next_refresh
                .is_none_or(|next| now >= next)
        {
            match CatalogWorker::start(account) {
                Ok(worker) => {
                    self.native_catalog.request = Some(Request {
                        epoch: self.native_catalog.epoch,
                        worker,
                    })
                }
                Err(_) => {
                    self.catalog_message = Some(FAILURE_MESSAGE.into());
                    self.native_catalog.next_refresh = Some(now + CATALOG_REFRESH_INTERVAL);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
