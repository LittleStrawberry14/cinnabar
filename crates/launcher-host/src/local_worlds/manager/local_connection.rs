//! Original local-player endpoints from the actual manager opening, independent of Core routing.

use super::*;
use std::{io, net::SocketAddr};

/// A weak capability for one actual local server opening; it never starts or owns another world.
#[derive(Clone)]
pub struct NativeLocalWorldSelection {
    opening: HostOpening,
    changed: tokio::sync::watch::Receiver<()>,
}

impl std::fmt::Debug for NativeLocalWorldSelection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeLocalWorldSelection")
            .finish_non_exhaustive()
    }
}

impl Manager {
    /// Captures only this selected running world's original numeric loopback route.
    ///
    /// # Errors
    /// Rejects replacement or closed worlds; transport support is checked before connection.
    pub fn native_local_world(&self, id: &str) -> io::Result<NativeLocalWorldSelection> {
        let changed = self.inner.native_changed.subscribe();
        let opening = HostOpening::capture(self)?;
        if opening.world().id != id {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "selected local world was replaced",
            ));
        }
        opening.validate()?;
        Ok(NativeLocalWorldSelection { opening, changed })
    }
}

impl NativeLocalWorldSelection {
    /// Stops only this original process on the existing worlds worker, preserving replacements.
    /// This may write the process pipe and must not run on the frame or reactor thread.
    pub fn request_close(&self) {
        self.opening.request_close();
    }

    /// Rechecks this original process/manager generation without following a newer world.
    pub fn validate(&self) -> io::Result<()> {
        self.opening.validate()
    }

    /// Returns only the original still-running numeric endpoint; no DNS/display-text lookup occurs.
    pub fn connection_target(&self) -> io::Result<protocol::NativeConnectionTarget> {
        self.validate()?;
        let local = self.opening.target();
        if (self.opening.world().backend, local.transport)
            == (Backend::Bds, Transport::NetherNetLan)
        {
            let address = local
                .lan_address
                .as_deref()
                .and_then(|value| value.parse::<SocketAddr>().ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "local world returned an invalid LAN endpoint",
                    )
                })?;
            let level = local
                .level_name
                .as_deref()
                .filter(|level| *level == self.world_id())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "local world returned another LAN level",
                    )
                })?;
            return protocol::NativeConnectionTarget::local_lan(
                address,
                level.to_owned(),
                std::time::Duration::from_millis(u64::from(
                    bridge::contract::LOCAL_LAN_DISCOVERY_TIMEOUT_MS,
                )),
            );
        }
        let transport = match (self.opening.world().backend, local.transport) {
            (Backend::Dragonfly, Transport::RakNet) => protocol::NativeTransport::RakNet,
            (Backend::Bds, Transport::NetherNet) => protocol::NativeTransport::LocalNetherNetHttp,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "local world advertises an unsupported native transport",
                ));
            }
        };
        let address = local
            .address
            .parse::<SocketAddr>()
            .ok()
            .filter(|address| address.ip().is_loopback() && address.port() != 0)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "local world returned an invalid loopback endpoint",
                )
            })?;
        Ok(protocol::NativeConnectionTarget::new(
            address.to_string(),
            transport,
        ))
    }

    /// Borrows the stable original world ID, solely for local presentation/session identity.
    pub fn world_id(&self) -> &str {
        &self.opening.world().id
    }

    /// Borrows presentation metadata captured with the original process.
    pub fn world_name(&self) -> &str {
        &self.opening.world().name
    }

    /// Retains the same coalesced manager wake across canceled borrowed waits.
    /// The manager/process remains weakly held, so its real owner can still shut it down.
    pub async fn changed(&mut self) -> io::Result<()> {
        self.changed.borrow_and_update();
        self.validate()?;
        self.changed.changed().await.map_err(|_| {
            io::Error::new(io::ErrorKind::NotConnected, "local world manager retired")
        })?;
        self.validate()
    }
}

#[cfg(test)]
mod tests;
