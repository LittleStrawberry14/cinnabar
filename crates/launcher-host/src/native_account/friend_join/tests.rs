use super::*;
use bedrock_services::xbox::social;
use serde_json::{Value, json};

/// Supplies original synthetic title metadata and real member identities, without service traffic.
fn snapshot() -> Value {
    json!({
        "members":{
            "me":{"constants":{"system":{"xuid":"123"}}},
            "0":{"constants":{"system":{"xuid":"456"}}}
        },
        "properties":{"system":{"closed":false}, "custom":{
            "ownerId":"456", "protocol":protocol::PROTOCOL_VERSION,
            "TransportLayer":super::super::friends::NETHERNET_TRANSPORT,
            "SupportedConnections":[
                {"ConnectionType":999},
                {"ConnectionType":super::super::friends::JSON_RPC_SIGNALING,
                    "NetherNetId":42, "PmsgId":Uuid::from_u128(7)}
            ],
            "nonces":{"123":"synthetic-player-admission", "456":"another-player"}
        }}
    })
}

/// Builds the captured application identity, with no OAuth or proof material.
fn scope() -> Scope {
    Scope {
        generation: "synthetic-generation".into(),
        xuid: "123".into(),
    }
}

#[test]
fn joined_snapshot_requires_the_selected_host_and_actual_local_membership() {
    let description = serde_json::from_value(snapshot()).unwrap();
    let admission = world::admission(&description, &scope(), "456")
        .unwrap()
        .unwrap();
    assert_eq!(
        admission.into_parts(),
        (Uuid::from_u128(7), "synthetic-player-admission".into())
    );

    let mut value = snapshot();
    value["members"]["me"] = Value::Null;
    assert!(world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456").is_err());
    let mut value = snapshot();
    value["properties"]["custom"]["ownerId"] = json!("789");
    assert!(world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456").is_err());
    let mut value = snapshot();
    value["properties"]["custom"]["protocol"] = json!(protocol::PROTOCOL_VERSION + 1);
    assert!(world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456").is_err());
}

#[test]
fn nonce_readiness_never_borrows_another_players_admission_or_accepts_an_empty_nonce() {
    let mut value = snapshot();
    value["properties"]["custom"]["nonces"]
        .as_object_mut()
        .unwrap()
        .remove("123");
    assert!(
        world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456")
            .unwrap()
            .is_none()
    );
    let mut value = snapshot();
    value["properties"]["custom"]["nonces"]["123"] = json!("");
    assert!(world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456").is_err());
    let mut value = snapshot();
    value["properties"]["custom"]["SupportedConnections"][1]["PmsgId"] = json!(Uuid::nil());
    assert!(world::admission(&serde_json::from_value(value).unwrap(), &scope(), "456").is_err());
}

#[test]
fn selection_keeps_the_actual_service_handle_and_reference_without_admitting_advertised_credentials()
 {
    let activity: social::Activity = serde_json::from_value(json!({
        "id":Uuid::from_u128(10), "type":"activity", "ownerXuid":"456",
        "sessionRef":{"scid":bedrock_services::minecraft::SERVICE_CONFIG_ID,
            "templateName":"MinecraftLobby", "name":"synthetic-world"},
        "customProperties":{"ownerId":"456", "nonces":{"123":"unjoined-nonce"}}
    }))
    .unwrap();
    let selected = FriendSelection::activity(&activity).unwrap();
    assert_eq!(selected.handle, Uuid::from_u128(10));
    assert_eq!(selected.host, "456");
    assert_eq!(selected.reference.name(), "synthetic-world");

    let invitation: mpsd::Invitation = serde_json::from_value(json!({
        "id":Uuid::from_u128(11), "senderXuid":"789", "invitedXuid":"123",
        "expiration":"2099-01-01T00:00:00Z",
        "sessionRef":{"scid":bedrock_services::minecraft::SERVICE_CONFIG_ID,
            "templateName":"MinecraftLobby", "name":"invited-world"}
    }))
    .unwrap();
    let selected = FriendSelection::invitation(&invitation, "456").unwrap();
    assert_eq!(selected.host, "456");
    assert_eq!(selected.recipient.as_deref(), Some("123"));
    assert_eq!(selected.reference.name(), "invited-world");
}
