//! An opaque identity for the actual running server opening, without owning or stopping it.

use super::*;
use std::{io, sync::Weak, time::Instant};

/// Holds weak ownership so retained remote cleanup cannot keep its local server manager alive.
#[derive(Clone)]
pub(crate) struct HostOpening {
    manager: Weak<Inner>,
    instance: Weak<dyn Instance>,
    generation: u64,
    world: LocalWorldInfo,
    max_players: u32,
}

impl HostOpening {
    /// Captures the manager's actual live generation and server instance under its lifecycle lock.
    pub(super) fn capture(manager: &Manager) -> io::Result<Self> {
        let life = lock(&manager.inner.life);
        let instance = life
            .instance
            .as_ref()
            .filter(|_| life.state == WorldState::Running)
            .ok_or_else(unavailable)?;
        if !matches!(instance.exited().try_recv(), Err(TryRecvError::Empty)) {
            return Err(unavailable());
        }
        let world = life.world.as_ref().ok_or_else(unavailable)?;
        Ok(Self {
            manager: Arc::downgrade(&manager.inner),
            instance: Arc::downgrade(instance),
            generation: life.generation,
            world: LocalWorldInfo {
                id: world.id.clone(),
                name: world.name.clone(),
                game_mode: world.game_mode,
                backend: world.backend,
            },
            max_players: instance
                .max_players()
                .unwrap_or(bridge::contract::HOSTED_WORLD_MAX_PLAYERS)
                .min(bridge::contract::HOSTED_WORLD_MAX_PLAYERS),
        })
    }

    /// Denies closed, failed, replaced and reopened servers, including reuse of the same world/address.
    pub(crate) fn validate(&self) -> io::Result<()> {
        let manager = self.manager.upgrade().ok_or_else(unavailable)?;
        let life = lock(&manager.life);
        let instance = life
            .instance
            .as_ref()
            .filter(|_| life.state == WorldState::Running)
            .ok_or_else(unavailable)?;
        if life.generation != self.generation
            || !self.instance.ptr_eq(&Arc::downgrade(instance))
            || life
                .world
                .as_ref()
                .is_none_or(|world| world.id != self.world.id)
            || !matches!(instance.exited().try_recv(), Err(TryRecvError::Empty))
        {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Matches exact opening ownership without accepting display text as a locator.
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.manager.ptr_eq(&other.manager)
            && self.instance.ptr_eq(&other.instance)
            && self.generation == other.generation
    }

    /// Borrows the metadata captured from this same actual local-world opening.
    pub(crate) fn world(&self) -> &LocalWorldInfo {
        &self.world
    }

    /// Returns the running instance's finite supported player cap, including its local host.
    pub(crate) fn max_players(&self) -> u32 {
        self.max_players
    }
}

impl Manager {
    /// Reserves this manager's sole unadvertised native host directory owner on the caller's runtime.
    /// No server process is started/stopped; publication, service progress and cleanup are explicit.
    /// The selected account must already have been verified through its existing account operation.
    pub fn native_hosted_world(
        &self,
        account: Arc<crate::native_account::NativeAccount>,
        deadline: Instant,
    ) -> io::Result<crate::native_account::NativeHostedWorld> {
        self.inner
            .native_host
            .open(account, HostOpening::capture(self)?, deadline)
    }

    /// Resumes exact unresolved remote cleanup without requiring the old local server to remain open.
    /// Another host cannot reserve the slot until this retained obligation closes successfully.
    pub fn native_host_cleanup(
        &self,
    ) -> io::Result<Option<crate::native_account::NativeHostedWorld>> {
        self.inner.native_host.cleanup()
    }
}

/// Keeps local server identifiers and loopback destinations out of diagnostics.
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "native hosted world opening is no longer running",
    )
}
