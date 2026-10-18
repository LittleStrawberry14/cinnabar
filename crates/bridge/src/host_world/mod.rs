//! Private, capability-bound process protocol for the running Dragonfly world.
//! This endpoint carries no Xbox credential and grants no player/world authority.

pub mod contract;
mod peer;
mod process;
mod wire;
pub use peer::{ActiveBackendPeer, BackendWorldReady, ClosingBackendPeer, PendingBackendPeer};
pub use process::{HostProcess, PendingHostProcess};
pub use wire::{Frame, Reader, Writer, split};

#[cfg(test)]
mod tests;

use std::io;

/// Omits child endpoints, player metadata and capabilities from diagnostics.
pub(super) fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native world bridge state",
    )
}
