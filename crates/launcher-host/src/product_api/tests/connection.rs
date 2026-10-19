use super::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use bedrock_services::{minecraft, test_support::HostedAuthority};
use futures::{SinkExt, StreamExt};
use p384::{
    ecdsa::{Signature, VerifyingKey, signature::Verifier},
    pkcs8::DecodePublicKey,
};
use std::num::NonZeroU32;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_websockets::{Message, ServerBuilder};

/// Real selected-account mint/proof, original WS exchanges and account retirement share one production owner.
#[tokio::test]
async fn acquired_account_drives_product_wire_and_retires_its_pending_lookup() {
    tokio::time::timeout(crate::native_account::ACQUISITION_TIMEOUT,async {
        let HostedAuthority {account:services,identity,peer}=HostedAuthority::acquire_activation(
            crate::native_account::authentication_policy(),
            minecraft::Config::desktop(protocol::GAME_VERSION,u32::try_from(protocol::PROTOCOL_VERSION).unwrap(),uuid::Uuid::from_u128(1)),
            NonZeroU32::new(2).unwrap(),bridge::contract::PRESENCE_TITLE_ID.to_string(),"product-fixture-audience".into(),
        ).await;
        let namespace=tempfile::tempdir().unwrap();
        let (account,store)=crate::native_account::NativeAccount::product_api_fixture_account(namespace.path(),services,&identity);
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("ws://{}/v1/connect",listener.local_addr().unwrap());
        let api=Client::default();
        let initial=config();
        api.configure(initial.clone());
        let (pending_seen,pending_ready)=oneshot::channel();
        let uuid="00112233-4455-6677-8899-aabbccddeeff";
        let server=async {
            let (stream,_)=listener.accept().await.unwrap();
            let (request,mut socket)=ServerBuilder::new().accept(stream).await.unwrap();
            assert_eq!(request.uri().path(),"/v1/connect");
            assert_eq!(request.headers()["user-agent"],"cinnabar/1.0.0");
            assert!(!request.headers().contains_key("authorization"));
            let hello:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().as_text().unwrap()).unwrap();
            let schema:Value=serde_json::from_str(include_str!("../../../../../core/cinnabarapi/schema/v1.json")).unwrap();
            assert_eq!(hello["type"],"hello");
            assert_eq!(hello["protocol"],schema["x-protocol"]);
            assert_eq!(hello["client_build"],initial.client_build);
            assert_eq!(hello["channel"],initial.channel);
            assert_eq!(hello["visible"],true);
            let token=hello["token"].as_str().unwrap();
            let claims:Value=serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split('.').nth(1).unwrap()).unwrap()).unwrap();
            assert_eq!(claims["xid"],identity.xuid());
            let minted_der=STANDARD.decode(claims["cpk"].as_str().unwrap()).unwrap();
            let key=VerifyingKey::from_public_key_der(&minted_der).unwrap();
            let nonce=URL_SAFE_NO_PAD.encode([7;32]);
            socket.send(Message::text(json!({"type":"challenge","nonce":nonce}).to_string())).await.unwrap();
            let proof:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().as_text().unwrap()).unwrap();
            assert_eq!(proof["type"],"proof");
            let signature=Signature::from_slice(&URL_SAFE_NO_PAD.decode(proof["signature"].as_str().unwrap()).unwrap()).unwrap();
            let prefix=schema["$defs"]["Proof"]["properties"]["signature"]["description"].as_str().unwrap().split('"').nth(1).unwrap();
            key.verify(format!("{prefix}{nonce}").as_bytes(),&signature).unwrap();
            socket.send(Message::text(json!({"type":"welcome","xuid":identity.xuid(),"lookup_batch_max":2,"lookup_bucket_capacity":4,"cache_ttl_ms":500,"loadout_revision":30}).to_string())).await.unwrap();
            let first:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().as_text().unwrap()).unwrap();
            assert_eq!(first["type"],"lookup");
            assert_eq!(first["xuids"],json!(["11"]));
            assert_eq!(first["uuids"],json!([uuid]));
            socket.send(Message::text(json!({"type":"lookup_result","id":first["id"],"cinnabar":["11","99"],"cinnabar_uuids":[uuid]}).to_string())).await.unwrap();
            let (mut visibility,mut loadout,mut playback)=(false,false,false);
            while !(visibility && loadout && playback) {
                let message:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().as_text().unwrap()).unwrap();
                match message["type"].as_str().unwrap() {
                    "visibility"=>{assert_eq!(message["visible"],false);visibility=true;}
                    "loadout"=>{assert!(message["revision"].as_u64().unwrap()>30);assert_eq!(message["slots"]["head"],"fixture:item");loadout=true;}
                    "emote_state"=>{assert_eq!(message["emote_id"],"fixture:clip");assert_eq!(message["xuids"],json!(["11"]));playback=true;}
                    _=>{},
                }
            }
            for message in [
                json!({"type":"emote","xuid":"11","uuid":uuid,"emote_id":"fixture:remote","elapsed_ms":42,"valid_for_ms":15000}),
                json!({"type":"loadout","xuid":"11","uuid":uuid,"revision":3,"slots":{"head":"fixture:remote-item"}}),
                json!({"type":"release","channel":initial.channel,"version":"2.0.0","build":99,"published_at":"2026-10-19T00:00:00.000Z"}),
            ] {socket.send(Message::text(message.to_string())).await.unwrap();}
            let second:Value=serde_json::from_str(socket.next().await.unwrap().unwrap().as_text().unwrap()).unwrap();
            assert_eq!(second["type"],"lookup");
            assert_eq!(second["xuids"],json!(["22"]));
            pending_seen.send(()).unwrap();
            let terminal=socket.next().await;
            let retired=match terminal {
                None=>true,
                Some(Err(tokio_websockets::Error::Io(error)))=>error.kind()==std::io::ErrorKind::UnexpectedEof,
                _=>false,
            };
            assert!(retired,"original account socket did not reach its actual EOF retirement");
            minted_der
        };
        let driver=async {
            loop {
                let changed=api.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if api.with(|state| state.live.is_some()) {break;}
                changed.await;
            }
            let answer=api.lookup(&["11".into(),uuid.into()]).await.unwrap();
            assert_eq!(answer.resolved,["11"]);
            assert_eq!(answer.resolved_uuids,[uuid]);
            assert_eq!(answer.cinnabar,["11"]);
            assert_eq!(answer.cinnabar_uuids,[uuid]);
            assert_eq!(answer.cache_ttl_ms,500);
            let mut invisible=initial.clone();invisible.visible=false;api.configure(invisible);
            let local=CinnabarPlayerState{xuids:vec!["11".into()],uuids:vec![uuid.into()],emote_id:"fixture:clip".into(),elapsed_ms:12,playback_id:1,slots:vec![CinnabarCosmeticSlot{slot:"head".into(),item_id:"fixture:item".into()}]};
            let remote=loop {
                if let Some(remote)=api.sync(local.clone()) && !remote.emotes.is_empty() && !remote.loadouts.is_empty() {break remote;}
                // Drive the actual nonblocking exchange while the original socket delivers its finite frames.
                tokio::task::yield_now().await;
            };
            let release=loop {
                if let Some(release)=api.take_release() {break release;}
                tokio::task::yield_now().await;
            };
            let pending_ids=["22".into()];
            let mut pending=Box::pin(api.lookup(&pending_ids));
            tokio::select! {biased;
                _=pending_ready=>{},
                _=&mut pending=>panic!("original pending lookup completed without a service reply"),
            }
            store.sign_out().unwrap();
            let retired=pending.await.is_err();
            api.finished().await;
            (remote,release,retired)
        };
        let owner=async {api.run_at(Arc::clone(&account),&url).await;api.finish();};
        let ((),minted_der,(remote,release,retired))=tokio::join!(owner,server,driver);
        let observed=peer.abort_activation().await;
        assert_eq!(observed.verified_http_requests,9);
        assert_eq!(observed.minted_public_key,minted_der);
        assert_eq!(remote.emotes[0].emote_id,"fixture:remote");
        assert_eq!(remote.loadouts[0].slots[0].item_id,"fixture:remote-item");
        assert_eq!(release.version,"2.0.0");
        assert!(retired);
        assert!(store.try_native_lease().is_err());
    }).await.expect("finite product API fixture stalled");
}
