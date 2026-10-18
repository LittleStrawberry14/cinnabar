//! Account-core title presence policy and explicit retirement outcomes.

use super::*;

impl LauncherCoreSlot {
    /// Requests the sole publisher policy for the next account-core incarnation.
    /// Changing policy retires the old core while idle, before any replacement starts.
    /// Native publication still requires its own account-generation and cleanup owner.
    pub fn set_title_presence_owner(&mut self, owner: TitlePresenceOwner) {
        if self.title_presence_owner != owner {
            self.title_presence_owner = owner;
            self.failed = None;
            self.restart_at = None;
        }
    }

    /// Observes the actual spawned policy; a request alone does not change the current core.
    /// This does not grant authority to publish for an account or skip native cleanup.
    pub fn title_presence_owner(&self) -> Option<TitlePresenceOwner> {
        self.core.as_ref().map(|core| core.title_presence_owner)
    }

    /// Reports the last observed stop outcome, including an unreaped child that blocks replacement.
    /// A missing outcome is not proof that an old publisher or cleanup request has ended.
    pub fn last_core_retirement(&self) -> Option<CoreStopOutcome> {
        self.last_retirement
    }

    /// Keeps replacement and account mutation blocked until an actual successful stop result arrives.
    pub(super) fn retirement_complete(&mut self) -> bool {
        if self.retirement_blocked {
            return false;
        }
        let Some(retiring) = &self.retiring else {
            return true;
        };
        let outcome = match retiring.try_recv() {
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => None,
            Ok(outcome) => Some(outcome),
        };
        self.retiring = None;
        self.last_retirement = outcome;
        self.retirement_blocked = matches!(outcome, None | Some(CoreStopOutcome::Unreaped));
        if self.retirement_blocked {
            bevy::log::warn!("account core exit is unconfirmed; replacement remains blocked");
        }
        !self.retirement_blocked
    }

    /// Runs the existing stop reaper, allowing a credential mutation only after confirmed exit.
    pub(super) fn begin_retirement(
        &mut self,
        mut old: LauncherCore,
        after_exit: impl FnOnce() + Send + 'static,
    ) {
        let (done, retired) = crossbeam_channel::bounded(1);
        self.retiring = Some(retired);
        self.last_retirement = None;
        let mut guard = std::mem::take(&mut old._guard);
        guard.stop_detached_with_outcome(move |outcome| {
            drop(old);
            if outcome != CoreStopOutcome::Unreaped {
                after_exit();
            }
            let _ = done.send(outcome);
        });
    }

    /// Detaches clients before the old account core can complete or a new one can attach.
    pub(super) fn detach_clients(
        &self,
        commands: &mut Commands,
        menu: &mut MenuRuntime,
        worlds: Option<&mut LocalWorlds>,
    ) {
        commands.remove_resource::<LauncherAccount>();
        menu.close_realm_membership();
        if let Some(worlds) = worlds {
            worlds.detach();
        }
        menu.control_auth = None;
        menu.accounts.skip_control = true;
        menu.feeds.profile = Default::default();
    }
}
