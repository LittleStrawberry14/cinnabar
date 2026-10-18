//! Selection lifetime for the native account cached across retries and transfers.

use super::{MenuRuntime, SessionController};
use launcher_host::native_account::NativeAccount;
use std::{path::PathBuf, sync::Arc};

pub(super) struct SelectedAccount {
    path: PathBuf,
    identity: Option<String>,
    account: Arc<NativeAccount>,
}

impl SessionController {
    /// Reuses one SDK owner until the selected cache/identity changes, without frame I/O.
    pub(super) fn native_account_for(
        &mut self,
        menu: &mut MenuRuntime,
        path: PathBuf,
    ) -> Arc<NativeAccount> {
        let account = menu.native_account_for(path.clone());
        let identity = menu.selected_account_id().map(str::to_owned);
        if self.native_account.as_ref().is_none_or(|previous| {
            previous.path != path
                || previous.identity != identity
                || !Arc::ptr_eq(&previous.account, &account)
        }) {
            self.native_account = Some(SelectedAccount {
                path,
                identity,
                account,
            });
        }
        Arc::clone(
            &self
                .native_account
                .as_ref()
                .expect("selected native account owner")
                .account,
        )
    }
}
