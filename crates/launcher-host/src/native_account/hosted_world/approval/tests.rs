use super::*;
use serde_json::json;

/// Creates actual typed complete directory metadata with stable member slot/connection identities.
fn snapshot(peers: &[(&str, u128, bool)]) -> mpsd::Description {
    let mut members = serde_json::Map::new();
    members.insert(
        "me".into(),
        json!({"constants":{"system":{"xuid":"123"}},"properties":{"system":{"active":true}}}),
    );
    for (index, (owner, connection, active)) in peers.iter().enumerate() {
        members.insert(index.to_string(), json!({"constants":{"system":{"xuid":owner}},"properties":{"system":{"active":active,"connection":Uuid::from_u128(*connection),"subscription":{"id":Uuid::from_u128(50)}}}}));
    }
    serde_json::from_value(json!({"members":members})).unwrap()
}

/// Publishes this pure owner's exact desired nonce document at its current membership revision.
fn acknowledge(approvals: &mut Approvals) -> Value {
    let document = approvals.document().unwrap();
    approvals
        .acknowledge(approvals.revision(), &document)
        .unwrap();
    document
}

#[test]
fn staying_members_keep_their_nonce_and_departed_or_reentered_members_cannot_replay_it() {
    let mut approvals = Approvals::new("123".into(), 3).unwrap();
    let joined = snapshot(&[("456", 2, true)]);
    approvals.observe(&joined).unwrap();
    assert_eq!(approvals.missing(), ["456"]);
    approvals.issue("456", &[1; NONCE_BYTES]).unwrap();
    let document = approvals.document().unwrap();
    let old = document["456"].as_str().unwrap();
    assert!(
        !approvals.accepts("456", old),
        "intent is not service acknowledgment"
    );
    acknowledge(&mut approvals);
    assert!(approvals.accepts("456", old));
    approvals.observe(&joined).unwrap();
    assert_eq!(approvals.document().unwrap(), document);
    assert!(
        approvals.accepts("456", old),
        "unchanged membership must keep its nonce epoch"
    );

    approvals.observe(&snapshot(&[])).unwrap();
    assert!(!approvals.accepts("456", old));
    assert_eq!(approvals.document().unwrap(), json!({}));
    approvals.observe(&joined).unwrap();
    assert_eq!(approvals.missing(), ["456"]);
    approvals.issue("456", &[2; NONCE_BYTES]).unwrap();
    let replacement = acknowledge(&mut approvals);
    assert_ne!(replacement["456"], document["456"]);
    assert!(!approvals.accepts("456", old));
    assert!(approvals.accepts("456", replacement["456"].as_str().unwrap()));
    assert!(!approvals.accepts("123", replacement["456"].as_str().unwrap()));
    assert!(!approvals.accepts("789", replacement["456"].as_str().unwrap()));
}

#[test]
fn a_replaced_rta_connection_revokes_the_nonce_even_without_an_observed_absence() {
    let mut approvals = Approvals::new("123".into(), 2).unwrap();
    approvals.observe(&snapshot(&[("456", 2, true)])).unwrap();
    approvals.issue("456", &[1; NONCE_BYTES]).unwrap();
    let document = acknowledge(&mut approvals);
    approvals.observe(&snapshot(&[("456", 3, true)])).unwrap();
    assert_eq!(approvals.missing(), ["456"]);
    assert!(!approvals.accepts("456", document["456"].as_str().unwrap()));
    assert!(
        approvals.document().is_err(),
        "a new epoch must receive fresh edge entropy"
    );
}

#[test]
fn wrong_or_old_acknowledgments_cannot_authorize_desired_nonce_state() {
    let mut approvals = Approvals::new("123".into(), 3).unwrap();
    approvals.observe(&snapshot(&[("456", 2, true)])).unwrap();
    approvals.issue("456", &[1; NONCE_BYTES]).unwrap();
    let revision = approvals.revision();
    let document = approvals.document().unwrap();
    assert!(
        approvals
            .acknowledge(revision, &json!({"456":"unacknowledged"}))
            .is_err()
    );
    assert!(!approvals.accepts("456", document["456"].as_str().unwrap()));
    approvals
        .observe(&snapshot(&[("456", 2, true), ("789", 3, true)]))
        .unwrap();
    approvals.issue("789", &[2; NONCE_BYTES]).unwrap();
    let current = approvals.document().unwrap();
    assert!(approvals.acknowledge(revision, &current).is_err());
    assert!(!approvals.acknowledged());
    acknowledge(&mut approvals);
    assert!(approvals.accepts("456", current["456"].as_str().unwrap()));
    approvals.invalidate();
    assert!(
        !approvals.accepts("456", current["456"].as_str().unwrap()),
        "canceled or failed publication must revoke unproven approval"
    );
}

#[test]
fn inactive_invite_records_receive_no_nonce_and_capacity_overflow_revokes_approval() {
    let mut approvals = Approvals::new("123".into(), 2).unwrap();
    approvals.observe(&snapshot(&[("456", 2, false)])).unwrap();
    assert!(approvals.missing().is_empty());
    assert!(approvals.issue("456", &[1; NONCE_BYTES]).is_err());
    approvals.observe(&snapshot(&[("456", 2, true)])).unwrap();
    approvals.issue("456", &[1; NONCE_BYTES]).unwrap();
    let document = acknowledge(&mut approvals);
    assert!(
        approvals
            .observe(&snapshot(&[("456", 2, true), ("789", 3, true)]))
            .is_err()
    );
    assert!(!approvals.accepts("456", document["456"].as_str().unwrap()));
    assert!(
        !approvals.desired.contains_key("789"),
        "overflow must not retain another nonce owner"
    );
}

#[test]
fn missing_host_or_ambiguous_member_bindings_cannot_supply_approval() {
    let mut approvals = Approvals::new("123".into(), 3).unwrap();
    let mut description = snapshot(&[("456", 2, true)]);
    description.members.remove("me");
    assert!(approvals.observe(&description).is_err());
    assert!(
        approvals
            .observe(&snapshot(&[("456", 2, true), ("456", 3, true)]))
            .is_err()
    );
    assert!(approvals.observe(&snapshot(&[("123", 2, true)])).is_err());
    let mut description = snapshot(&[("456", 2, true)]);
    description
        .members
        .get_mut("0")
        .unwrap()
        .as_mut()
        .unwrap()
        .properties
        .as_mut()
        .unwrap()
        .system
        .as_mut()
        .unwrap()["subscription"] = json!("supplied-malformed-binding");
    assert!(approvals.observe(&description).is_err());
    assert!(!approvals.acknowledged());
}
