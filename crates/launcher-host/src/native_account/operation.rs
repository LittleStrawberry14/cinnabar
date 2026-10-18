//! One acquired SDK owner and cancellation-safe credential publication.

use super::*;

impl Operation {
    /// Binds the saved slot only after checking the acquired authorization's actual identity.
    pub(super) fn verify_identity(&mut self, xuid: &str, gamertag: &str) -> io::Result<()> {
        let lease = self.lease.as_ref().expect("acquired lease");
        let cached = self.cached.as_ref().expect("account owner");
        if lease.selected_id().is_some_and(|selected| selected != xuid)
            || cached
                .identity
                .as_ref()
                .is_some_and(|previous| previous.xuid != xuid)
        {
            self.rejected = true;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity does not match the selection",
            ));
        }
        self.cached.as_mut().expect("account owner").identity = Some(AccountIdentity {
            xuid: xuid.to_owned(),
            gamertag: gamertag.to_owned(),
            current_credential: true,
        });
        Ok(())
    }

    /// Allows service minting only after current credentials pass every application identity fence.
    pub(super) fn minecraft(&mut self) -> io::Result<&mut minecraft::Minecraft> {
        if self.rejected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity was rejected",
            ));
        }
        let cached = self
            .cached
            .as_mut()
            .ok_or_else(|| io::Error::other("native account owner unavailable"))?;
        if cached
            .identity
            .as_ref()
            .is_none_or(|identity| !identity.current_credential)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account identity is not verified",
            ));
        }
        match &mut cached.owner {
            Owner::Minecraft(services) => Ok(services),
            Owner::Xbox(_) => Err(io::Error::other("native Minecraft owner is not prepared")),
        }
    }

    /// Writes the newest refresh before success and updates the cached file revision.
    pub(super) fn persist(&mut self) -> io::Result<()> {
        let (Some(cached), Some(lease)) = (self.cached.as_mut(), self.lease.as_mut()) else {
            return Ok(());
        };
        if cached.generation != lease.generation() || cached.binding != lease.binding() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native account has not adopted the current leased credentials",
            ));
        }
        let credentials = cached.owner.credentials();
        let mut publication_error = None;
        if !credentials.access_token().is_empty() {
            let publication = oauth_expiry(credentials.expires_at()).and_then(|expiry| {
                lease.publish_oauth(
                    credentials.access_token(),
                    credentials.refresh_token(),
                    &expiry,
                )
            });
            if let Err(error) = publication {
                if !matches!(
                    error.kind(),
                    io::ErrorKind::FileTooLarge | io::ErrorKind::InvalidInput
                ) {
                    return Err(error);
                }
                // Fail this join, but preserve a rotated refresh when the access record cannot be shared with Go.
                lease.publish_oauth("", credentials.refresh_token(), REFRESH_ONLY_EXPIRY)?;
                self.rejected = true;
                publication_error = Some(error);
            }
        }
        // OAuth is already durable even if checkpoint encoding or saved-slot publication fails next.
        cached.binding = lease.binding();
        let checkpoint = cached.owner.checkpoint()?;
        if !self.rejected
            && let Some(identity) = &cached.identity
            && identity.current_credential
        {
            if let Err(error) =
                lease.publish_verified_identity(&identity.xuid, checkpoint.as_bytes())
            {
                lease.publish_active_checkpoint(checkpoint.as_bytes())?;
                return Err(error);
            }
        } else {
            lease.publish_active_checkpoint(checkpoint.as_bytes())?;
        }
        self.published = true;
        publication_error.map_or(Ok(()), Err)
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.published && self.persist().is_err() {
            // Neither a replaced generation nor a failed durable write may return a reusable owner.
            self.cached = None;
            tracing::warn!("native account credential persistence failed");
        }
        if self.rejected {
            self.cached = None;
        }
        if let Ok(mut state) = self.account.state.lock() {
            state.cached = self.cached.take();
            state.busy = false;
        }
    }
}
