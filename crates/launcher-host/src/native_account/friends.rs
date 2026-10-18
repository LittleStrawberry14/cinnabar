//! Narrow projection of Xbox activity observations into the existing friend-world cards.
//!
//! Listing neither establishes session membership nor authorizes a connection. Unknown
//! or malformed observations are skipped without retaining private admission metadata.

use bedrock_services::xbox::social::Activity;
use launcher::menu::view::{CatalogFriend, MenuFriendCard};
use serde_json::{Map, Value};
use std::{borrow::Cow, collections::HashSet};
use uuid::Uuid;

const NETHERNET_TRANSPORT: i64 = 2;
const WEBSOCKET_SIGNALING: i64 = 3;
const JSON_RPC_SIGNALING: i64 = 7;

/// Preserves service order and the current menu's one-row-per-host/world de-duplication.
pub(super) fn cards(activities: Vec<Activity>, self_xuid: &str) -> Vec<MenuFriendCard> {
    let mut seen = HashSet::new();
    let mut skipped = 0_usize;
    let values = activities
        .into_iter()
        .filter_map(|activity| {
            let Some(card) = card(activity, self_xuid) else {
                skipped += 1;
                return None;
            };
            seen.insert((card.xuid.clone(), card.world_name.clone()))
                .then_some(card)
        })
        .collect();
    if skipped != 0 {
        tracing::debug!(
            skipped,
            "ignored unavailable friend-world activity observations"
        );
    }
    values
}

/// Applies the pinned catalog's listed-world and usable-signaling rules to fields this menu consumes.
fn card(activity: Activity, self_xuid: &str) -> Option<MenuFriendCard> {
    // The pinned activity owner defaults an absent/null closed flag to false.
    if activity.related_info?.closed.unwrap_or(false) {
        return None;
    }
    let custom = activity.custom_properties?;
    let world = custom.as_object()?;
    let host = text(world, "hostName")?;
    let owner = text(world, "ownerId")?;
    let name = text(world, "worldName")?;
    let members = integer(world, "MemberCount")?;
    let maximum = integer(world, "MaxMemberCount")?;
    if host.is_empty()
        || owner.is_empty()
        || owner == self_xuid
        || members <= 0
        || maximum < 0
        || integer(world, "RealmId")? != 0
        || !uuid(world, "ExperienceId")?.is_nil()
        || !uuid(world, "ExperienceWorldId")?.is_nil()
        || !text(world, "FriendId")?.is_empty()
        || !matches!(integer(world, "BroadcastSetting")?, 2..=4)
        || integer(world, "TransportLayer")? != NETHERNET_TRANSPORT
    {
        return None;
    }
    let Value::Array(connections) = field(world, "SupportedConnections")? else {
        return None;
    };
    let mut supported = false;
    for value in connections {
        supported |= usable_connection(value)?;
    }
    if !supported {
        return None;
    }
    Some(
        CatalogFriend {
            gamertag: host.into(),
            world_name: name.into(),
            xuid: owner.into(),
            members: i32::try_from(members).ok()?,
            max_members: i32::try_from(maximum).ok()?,
        }
        .into(),
    )
}

/// Accepts an advertised signaling method without interpreting it as an admitted native join.
/// Direct WebSocket uses RakNetGUID on the wire; JSON-RPC requires its own ID plus a nonnil PmsgId.
fn usable_connection(value: &Value) -> Option<bool> {
    if value.is_null() {
        return Some(false);
    }
    let connection = value.as_object()?;
    let kind = integer(connection, "ConnectionType")?;
    // Decode the recognized fields even on an auxiliary method, matching malformed-record rejection.
    let legacy = text(connection, "RakNetGUID")?;
    let messaging = uuid(connection, "PmsgId")?;
    let network = match field(connection, "NetherNetId") {
        None | Some(Value::Null) => Cow::Borrowed(""),
        Some(Value::String(value)) => Cow::Borrowed(value.as_str()),
        Some(value) => Cow::Owned(value.to_string()),
    };
    Some(match kind {
        WEBSOCKET_SIGNALING => usable_id(legacy),
        JSON_RPC_SIGNALING => usable_id(&network) && !messaging.is_nil(),
        _ => false,
    })
}

/// Accepts canonical nonzero decimal network IDs or UUID text, preserving the baseline's opaque-ID rule.
fn usable_id(value: &str) -> bool {
    value
        .parse::<u64>()
        .is_ok_and(|id| id != 0 && id.to_string() == value)
        || Uuid::parse_str(value).is_ok()
}

/// Go's title metadata decoding compares JSON field names without ASCII case sensitivity.
fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

/// Missing/null title strings retain their zero value; a supplied wrong type rejects this observation.
fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    match field(object, name) {
        None | Some(Value::Null) => Some(""),
        Some(value) => value.as_str(),
    }
}

/// Reads the signed integer metadata representation, without coercing strings or fractions.
fn integer(object: &Map<String, Value>, name: &str) -> Option<i64> {
    match field(object, name) {
        None | Some(Value::Null) => Some(0),
        Some(value) => value.as_i64(),
    }
}

/// Retains missing/null UUID zero values while rejecting malformed supplied UUIDs.
fn uuid(object: &Map<String, Value>, name: &str) -> Option<Uuid> {
    match field(object, name) {
        None | Some(Value::Null) => Some(Uuid::nil()),
        Some(Value::String(value)) => Uuid::parse_str(value).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
