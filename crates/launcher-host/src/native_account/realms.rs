//! Account-borrowed Realms calls and the application's existing menu projection.

use super::*;
use bedrock_services::realms::{Join, NetworkProtocol, Realm, RealmAddress};
use launcher::menu::view::MenuRealmCard;
use prepare::check_preparation_deadline;
use std::num::NonZeroU64;

const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(45);

impl NativeAccount {
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
        let deadline = tokio::time::Instant::now() + RESOLUTION_TIMEOUT + ACQUISITION_TIMEOUT;
        let result = tokio::time::timeout_at(deadline, async {
            check_preparation_deadline(deadline)?;
            let mut operation = Arc::clone(&self).acquire_verified().await?;
            check_preparation_deadline(deadline)?;
            operation.promote()?;
            let resolution_deadline =
                deadline.min(tokio::time::Instant::now() + RESOLUTION_TIMEOUT);
            let route = tokio::time::timeout_at(resolution_deadline, async {
                check_preparation_deadline(resolution_deadline)?;
                let mut realms = operation.minecraft()?.realms().await.map_err(realm_error)?;
                check_preparation_deadline(resolution_deadline)?;
                let id = match selection {
                    RealmTarget::Id(id) => id,
                    RealmTarget::Invite(code) => {
                        realms.preview_invite(code).await.map_err(realm_error)?.id
                    }
                };
                loop {
                    check_preparation_deadline(resolution_deadline)?;
                    let joined = realms.join(id, &[]).await.map_err(realm_error)?;
                    check_preparation_deadline(resolution_deadline)?;
                    match joined {
                        Join::Pending { retry_after } => tokio::time::sleep(retry_after).await,
                        Join::Ready(address) => return resolved_route(address),
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
            check_preparation_deadline(resolution_deadline)?;
            operation.persist()?;
            check_preparation_deadline(deadline)?;
            let authentication = if let Some(peer) = route.messaging_peer {
                let (prepared, authentication) = self
                    .prepare_messaging_socket_borrowed(
                        &mut operation,
                        peer,
                        claims,
                        random_id,
                        deadline,
                    )
                    .await?;
                authentication.with_realm_messaging(prepared)?
            } else {
                self.prepare_borrowed(&mut operation, claims, random_id)
                    .await?
            };
            check_preparation_deadline(deadline)?;
            Ok((route.into_target(), authentication))
        })
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Realm account preparation timed out",
            )
        })?;
        check_preparation_deadline(deadline)?;
        result
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

/// Retains the actual join response until the same account finishes credential preparation.
/// This private transport projection is not a membership proof or a reusable authorization token.
struct ResolvedRealmRoute {
    response: RealmAddress,
    messaging_peer: Option<uuid::Uuid>,
}

impl ResolvedRealmRoute {
    /// Consumes service metadata only after the account-bound handoff has finished.
    fn into_target(self) -> protocol::NativeConnectionTarget {
        match self.messaging_peer {
            Some(peer) => protocol::NativeConnectionTarget {
                address: peer.to_string(),
                transport: protocol::NativeTransport::NetherNetMessaging,
            },
            None => protocol::NativeConnectionTarget {
                address: self.response.address,
                transport: protocol::NativeTransport::RakNet,
            },
        }
    }
}

/// Validates explicit service transport data; only authenticated join orchestration uses it for admission.
fn resolved_route(response: RealmAddress) -> io::Result<ResolvedRealmRoute> {
    let messaging_peer = match &response.network_protocol {
        NetworkProtocol::Default if !response.address.trim().is_empty() => None,
        NetworkProtocol::NetherNetJsonRpc => {
            let peer = uuid::Uuid::parse_str(&response.address)
                .ok()
                .filter(|peer| !peer.is_nil())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid Realm messaging recipient",
                    )
                })?;
            Some(peer)
        }
        NetworkProtocol::NetherNet => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native Realm WebSocket signaling is not supported",
            ));
        }
        NetworkProtocol::Default => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Realm returned no address",
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Realm advertised an unsupported transport",
            ));
        }
    };
    Ok(ResolvedRealmRoute {
        response,
        messaging_peer,
    })
}

/// Projects the pinned Go catalog's fields without resolving addresses or accepting membership.
pub(super) fn card(realm: Realm) -> MenuRealmCard {
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
pub(super) fn realm_error(error: bedrock_services::realms::Error) -> io::Error {
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
