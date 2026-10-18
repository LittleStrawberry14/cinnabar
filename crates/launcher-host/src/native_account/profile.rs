//! Profile presentation from the selected account's existing leased social owner.

use super::*;
use bedrock_services::xbox::social::{PeopleList, ProfileDetail, Social, User};

mod achievements;
mod facets;
mod policy;
mod statistics;

/// A read remains tied to its original account while artwork is acquired off-frame.
/// Presentation survives ordinary credential rotation, but never a replacement sign-in or XUID.
pub(crate) struct NativeProfile {
    pub(crate) profile: bridge::Profile,
    pub(crate) partial: bool,
    account: Arc<NativeAccount>,
    generation: String,
    avatar: Option<bedrock_services::minecraft::profile::ImageBody>,
}

impl NativeProfile {
    /// Admits persona bytes only through the existing artwork cache on its off-frame worker.
    pub(crate) fn cache_avatar(&mut self, images: &crate::remote_images::ImageDirectory) {
        if let Some(avatar) = self.avatar.take() {
            if let Some(path) = images.store_service_image(&avatar.bytes) {
                self.profile.avatar.path = path.to_string_lossy().into_owned();
                self.profile.avatar_error = false;
            } else {
                self.partial = true;
            }
        }
    }

    /// Rejects a retired or currently unverifiable selection immediately before menu publication.
    /// The profile worker calls this off-frame; the short protected lease performs no authentication.
    pub(crate) fn into_current(self) -> Option<bridge::Profile> {
        let lease = self.account.store.try_native_lease().ok()?;
        let state = self.account.state.try_lock().ok()?;
        let cached = state.cached.as_ref()?;
        (!state.busy
            && cached.generation == self.generation
            && lease.generation() == self.generation
            && cached.binding == lease.binding()
            && lease
                .selected_id()
                .is_none_or(|selected| selected == self.profile.xuid)
            && self
                .account
                .selected_id
                .as_deref()
                .is_none_or(|selected| lease.selected_id() == Some(selected))
            && cached.identity.as_ref().is_some_and(|identity| {
                identity.current_credential && identity.xuid == self.profile.xuid
            }))
        .then_some(self.profile)
    }
}

impl NativeAccount {
    /// Reads overview and independent relationship counts through the selected account owner.
    /// The existing profile worker owns the original deadline and cancellation. Each phase
    /// rearms/persists the shared SDK owner and releases its lease before the next phase.
    /// Optional service failure retains the other successfully loaded facets.
    pub(crate) async fn read_profile(self: Arc<Self>) -> io::Result<NativeProfile> {
        let mut operation = Arc::clone(&self).acquire_verified().await?;
        let cached = operation.cached.as_ref().expect("verified account");
        let identity = cached.identity.as_ref().expect("verified identity");
        let generation = cached.generation.clone();
        let xuid = identity.xuid.clone();
        let user = operation.profile_social().await?.profile(&xuid).await;
        operation.persist()?;
        let identity = operation
            .cached
            .as_ref()
            .expect("account owner")
            .identity
            .as_ref()
            .expect("verified identity");
        let mut profile = bridge::Profile {
            gamertag: identity.gamertag.clone(),
            xuid,
            avatar_error: true,
            featured_screenshot_error: true,
            ..Default::default()
        };
        let mut partial = false;
        match user {
            Ok(user) => apply_user(&mut profile, user)?,
            Err(error) => {
                let error = service_error(error);
                if requires_sign_in(&error) {
                    return Err(error);
                }
                partial = true;
            }
        }
        drop(operation);

        profile.friends = self
            .profile_count(&generation, &profile.xuid, PeopleList::Friends)
            .await?;
        partial |= profile.friends.is_none();
        profile.followers = self
            .profile_count(&generation, &profile.xuid, PeopleList::Followers)
            .await?;
        partial |= profile.followers.is_none();
        Ok(NativeProfile {
            profile,
            partial,
            account: self,
            generation,
            avatar: None,
        })
    }

    /// Keeps independent counts on the original generation without holding an idle account lease.
    async fn profile_count(
        self: &Arc<Self>,
        generation: &str,
        xuid: &str,
        list: PeopleList,
    ) -> io::Result<Option<u32>> {
        let mut operation = Arc::clone(self)
            .acquire_verified_for(Some(generation))
            .await?;
        if operation
            .cached
            .as_ref()
            .expect("account owner")
            .identity
            .as_ref()
            .is_none_or(|identity| identity.xuid != xuid)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native profile identity changed",
            ));
        }
        let people = operation
            .profile_social()
            .await?
            .people(list, ProfileDetail::Basic)
            .await
            .map(|users| users.len());
        operation.persist()?;
        count(people)
    }
}

impl Operation {
    /// Revalidates the actual Xbox identity before each independent borrowed profile facet.
    async fn profile_social(&mut self) -> io::Result<Social<'_>> {
        self.profile_identity().await?;
        match &mut self.cached.as_mut().expect("account owner").owner {
            Owner::Xbox(account) => Ok(account.social()),
            Owner::Minecraft(services) => Ok(services.social()),
        }
    }

    /// Reuses the same proof's current default authorization before dispatching any profile facet.
    async fn profile_identity(&mut self) -> io::Result<()> {
        self.rearm()?;
        let authorization = self
            .cached
            .as_mut()
            .expect("account owner")
            .owner
            .xbox_authorization()
            .await?;
        self.verify_identity(authorization.xuid(), authorization.gamertag())
    }
}

/// Preserves absence for a failed list and zero only for a successfully decoded empty list.
fn count(result: Result<usize, bedrock_services::Error>) -> io::Result<Option<u32>> {
    match result {
        Ok(count) => Ok(u32::try_from(count).ok()),
        Err(error) => {
            let error = service_error(error);
            if requires_sign_in(&error) {
                Err(error)
            } else {
                Ok(None)
            }
        }
    }
}

/// Projects only fields supplied by PeopleHub, retaining the verified account as identity authority.
fn apply_user(profile: &mut bridge::Profile, user: User) -> io::Result<()> {
    if user.xuid != profile.xuid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native profile identity changed",
        ));
    }
    if profile.gamertag.is_empty() {
        profile.gamertag = user.gamertag.trim().to_owned();
    }
    profile.real_name = user
        .real_name
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_owned();
    profile.presence_text = user
        .presence_text
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_owned();
    profile.gamerscore = user.gamerscore.and_then(|score| i64::try_from(score).ok());
    profile.gamerpic.url = user
        .display_picture
        .filter(|url| crate::remote_images::LAUNCHER_ART.accepts(url))
        .unwrap_or_default();
    Ok(())
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
