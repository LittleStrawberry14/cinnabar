//! Connection-key minting and one-time Minecraft service promotion.

use super::*;

impl NativeAccount {
    /// Acquires account credentials and mints for one fresh connection key.
    /// The caller polls this on its session runtime and cancels by dropping the future.
    /// It never falls back to a signed-out identity when authentication fails.
    pub async fn prepare(
        self: Arc<Self>,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        tokio::time::timeout(ACQUISITION_TIMEOUT, self.prepare_inner(claims, random_id))
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native account preparation timed out",
                )
            })?
    }

    /// Serializes one acquisition, adopting a different process's current OAuth revision before HTTP.
    async fn prepare_inner(
        self: Arc<Self>,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        let mut operation = Arc::clone(&self).acquire_verified().await?;
        operation.promote()?;
        self.prepare_owned(operation, claims, random_id).await
    }

    /// Mints the final connection key without releasing or replacing the acquired service owner.
    pub(super) async fn prepare_owned(
        &self,
        mut operation: Operation,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        self.prepare_borrowed(&mut operation, claims, random_id)
            .await
    }

    /// Retains the same account lease for a subsequent protected signaling upgrade.
    pub(super) async fn prepare_borrowed(
        &self,
        operation: &mut Operation,
        mut claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<protocol::NativeAuthentication> {
        operation.rearm()?;
        let fresh = serde_json::to_vec(&metadata::DeviceProfile::fresh())
            .map_err(|_| io::Error::other("serialize native device profile"))?;
        let profile = loop {
            match operation
                .lease
                .as_ref()
                .expect("acquired lease")
                .device_profile(&self.device_path, &fresh, |bytes| {
                    metadata::DeviceProfile::parse(bytes).is_ok()
                }) {
                Ok(bytes) => break metadata::DeviceProfile::parse(&bytes)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(LEASE_RETRY).await
                }
                Err(error) => return Err(error),
            }
        };
        let key = SigningKey::try_generate_from_rng(&mut SysRng)
            .map_err(|_| io::Error::other("native connection key generation failed"))?;
        let (credential, issuer) = {
            let services = operation.minecraft()?;
            let token = services
                .multiplayer_token(key.verifying_key())
                .await
                .map_err(service_error)?;
            let observed = std::time::Instant::now();
            let credential = protocol::NativeCredential::new(
                token.as_str().to_owned(),
                services.service_time(),
                token.expires_at(),
                observed,
            )?;
            (
                credential,
                services
                    .authorization_issuer()
                    .map_err(service_error)?
                    .to_owned(),
            )
        };
        profile.apply(
            &mut claims,
            random_id,
            &operation
                .cached
                .as_ref()
                .expect("account owner")
                .identity
                .as_ref()
                .expect("verified identity")
                .gamertag,
        );
        let raw = serde_json::value::to_raw_value(&claims)
            .map_err(|_| io::Error::other("serialize native client data"))?;
        let cached = operation.cached.as_mut().expect("account owner");
        let same_provider = cached
            .provider
            .as_ref()
            .map(|provider| provider.matches(&issuer))
            .transpose()?
            .unwrap_or(false);
        if !same_provider {
            cached.provider = Some(provider::Provider::new(&issuer)?);
        }
        let prepared = cached
            .provider
            .as_mut()
            .expect("account provider")
            .authentication(
                key,
                credential,
                &raw,
                &cached.identity.as_ref().expect("verified identity").xuid,
            )
            .await?;
        operation.persist()?;
        Ok(prepared)
    }
}

impl Operation {
    /// Creates Minecraft services once, retaining proof, credentials and Session-Id for borrowed service calls.
    pub(super) fn promote(&mut self) -> io::Result<()> {
        if matches!(
            self.cached.as_ref().expect("account owner").owner,
            Owner::Xbox(_)
        ) {
            let identity = self
                .cached
                .as_ref()
                .expect("account owner")
                .identity
                .as_ref()
                .expect("verified identity");
            let version = u32::try_from(protocol::PROTOCOL_VERSION)
                .map_err(|_| io::Error::other("invalid application protocol version"))?;
            let config = minecraft::Config::desktop(
                protocol::GAME_VERSION,
                version,
                metadata::service_device_id(&identity.xuid)?,
            );
            self.persist()?;
            let Cached {
                generation,
                binding,
                checkpoint_revision,
                owner,
                identity,
                provider,
            } = self.cached.take().expect("account owner");
            let Owner::Xbox(account) = owner else {
                unreachable!("checked Xbox owner")
            };
            let services = minecraft::Minecraft::new(*account, config).map_err(service_error)?;
            self.cached = Some(Cached {
                generation,
                binding,
                checkpoint_revision,
                owner: Owner::Minecraft(Box::new(services)),
                identity,
                provider,
            });
            self.published = false;
        }
        Ok(())
    }
}
