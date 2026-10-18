//! Account-borrowed Realms calls and the application's existing menu projection.

use super::*;
use bedrock_services::realms::{Join, NetworkProtocol, Realm, RealmAddress};
use launcher::menu::view::MenuRealmCard;
use std::num::NonZeroU64;

const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(45);

/// A list acquired and persisted under one verified sign-in generation/revision.
/// The menu also checks its request epoch and selected account before publishing these cards.
pub struct RealmListing {
    cards: Vec<MenuRealmCard>,
    generation: String,
    binding: [u8; 32],
}

impl RealmListing {
    /// Moves the existing launcher model out only when the concrete account still owns this result.
    pub fn into_current_cards(self, account: &NativeAccount) -> Option<Vec<MenuRealmCard>> {
        let state = account.state.try_lock().ok()?;
        let cached = state.cached.as_ref()?;
        (!state.busy
            && cached.generation == self.generation
            && cached.binding == self.binding
            && cached
                .identity
                .as_ref()
                .is_some_and(|identity| identity.current_credential))
        .then_some(self.cards)
    }
}

impl NativeAccount {
    /// Lists accessible Realms on the caller's runtime, retaining the same Minecraft/session owner.
    /// Dropping the future cancels HTTP and persists any acquired OAuth rotation through Operation.
    pub async fn list_realms(self: Arc<Self>) -> io::Result<RealmListing> {
        tokio::time::timeout(ACQUISITION_TIMEOUT, async {
            let mut operation = Arc::clone(&self).acquire_verified().await?;
            operation.promote()?;
            let values = operation
                .minecraft()?
                .realms()
                .await
                .map_err(realm_error)?
                .list()
                .await
                .map_err(realm_error)?;
            operation.persist()?;
            let cached = operation.cached.as_ref().expect("account owner");
            let listing = RealmListing {
                cards: values.into_iter().map(card).collect(),
                generation: cached.generation.clone(),
                binding: cached.binding,
            };
            // Operation returns its owner before the result can be consumed by the menu.
            drop(operation);
            Ok(listing)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Realms listing timed out"))?
    }

    /// Resolves a selected Realm and mints on the same mutable Minecraft owner, with no Go fallback.
    /// Direct targets retain their existing preparation; the returned target is the only endpoint dialled.
    pub async fn prepare_connection(
        self: Arc<Self>,
        target: protocol::NativeConnectionTarget,
        claims: Value,
        random_id: protocol::ClientRandomId,
    ) -> io::Result<(
        protocol::NativeConnectionTarget,
        protocol::NativeAuthentication,
    )> {
        if target.transport != protocol::NativeTransport::RakNet {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "authenticated native preparation requires RakNet",
            ));
        }
        let Some(selection) = realm_target(&target.address)? else {
            let authentication = self.prepare(claims, random_id).await?;
            return Ok((target, authentication));
        };
        if !claims.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native client data must be a JSON object",
            ));
        }
        tokio::time::timeout(RESOLUTION_TIMEOUT + ACQUISITION_TIMEOUT, async {
            let mut operation = Arc::clone(&self).acquire_verified().await?;
            operation.promote()?;
            let target = tokio::time::timeout(RESOLUTION_TIMEOUT, async {
                let mut realms = operation.minecraft()?.realms().await.map_err(realm_error)?;
                let id = match selection {
                    RealmTarget::Id(id) => id,
                    RealmTarget::Invite(code) => {
                        realms.preview_invite(code).await.map_err(realm_error)?.id
                    }
                };
                loop {
                    match realms.join(id, &[]).await.map_err(realm_error)? {
                        Join::Pending { retry_after } => tokio::time::sleep(retry_after).await,
                        Join::Ready(address) => return raknet_target(address),
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::Unsupported,
                                "unsupported Realms join state",
                            ));
                        }
                    }
                }
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Realm resolution timed out"))??;
            let authentication = self.prepare_owned(operation, claims, random_id).await?;
            Ok((target, authentication))
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Realm account preparation timed out",
            )
        })?
    }
}

enum RealmTarget<'a> {
    Id(NonZeroU64),
    Invite(&'a str),
}

/// Recognizes the existing Realm namespaces without treating them as ordinary server hosts.
pub fn is_realm_target(target: &str) -> bool {
    realm_suffix(target, bridge::contract::REALM_TARGET_PREFIX).is_some()
        || realm_suffix(target, bridge::contract::REALM_CODE_PREFIX).is_some()
}

/// Borrows a namespace's payload using the pinned Go owner's case-insensitive prefix admission.
fn realm_suffix<'a>(target: &'a str, prefix: &str) -> Option<&'a str> {
    let target = target.trim();
    target
        .get(..prefix.len())
        .filter(|value| value.eq_ignore_ascii_case(prefix))?;
    Some(target.get(prefix.len()..)?.trim())
}

/// Parses service-owned target identities without accepting invitations or guessing membership policy.
fn realm_target(target: &str) -> io::Result<Option<RealmTarget<'_>>> {
    if let Some(id) = realm_suffix(target, bridge::contract::REALM_TARGET_PREFIX) {
        return id
            .parse::<NonZeroU64>()
            .map(|id| Some(RealmTarget::Id(id)))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid Realm identifier"));
    }
    if let Some(code) = realm_suffix(target, bridge::contract::REALM_CODE_PREFIX) {
        if code.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty Realm invitation target",
            ));
        }
        return Ok(Some(RealmTarget::Invite(code)));
    }
    Ok(None)
}

/// Admits only the service's explicit default transport, preserving the exact address including IPv6.
fn raknet_target(address: RealmAddress) -> io::Result<protocol::NativeConnectionTarget> {
    match address.network_protocol {
        NetworkProtocol::Default if !address.address.trim().is_empty() => {
            Ok(protocol::NativeConnectionTarget {
                address: address.address,
                transport: protocol::NativeTransport::RakNet,
            })
        }
        NetworkProtocol::NetherNet | NetworkProtocol::NetherNetJsonRpc => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this Realm requires authenticated NetherNet signaling, which is not yet supported natively",
        )),
        NetworkProtocol::Default => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Realm returned no address",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Realm advertised an unsupported transport",
        )),
    }
}

/// Projects the pinned Go catalog's fields without resolving addresses or accepting membership.
fn card(realm: Realm) -> MenuRealmCard {
    let name = realm.name.trim();
    MenuRealmCard {
        name: if name.is_empty() {
            "Realm".into()
        } else {
            name.into()
        },
        state: realm.state,
        target: format!("{}{}", bridge::contract::REALM_TARGET_PREFIX, realm.id),
        address: String::new(),
        owner: realm.owner.trim().into(),
        online_players: u32::try_from(realm.players.iter().filter(|player| player.online).count())
            .unwrap_or(u32::MAX),
        max_players: realm.max_players,
        days_left: realm.days_left,
        expired: realm.expired,
        member: realm.member,
    }
}

/// Retains actionable access failures without exposing response bodies or service URLs.
fn realm_error(error: bedrock_services::realms::Error) -> io::Error {
    match error {
        bedrock_services::realms::Error::NotFound => {
            io::Error::new(io::ErrorKind::NotFound, "Realm was not found")
        }
        bedrock_services::realms::Error::NotMember => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "account cannot access this Realm",
        ),
        _ => io::Error::other("Realms service request failed; retry or sign in again"),
    }
}

#[cfg(test)]
mod tests;
