use super::*;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use nethernet::identity::{MINECRAFT_AUDIENCE, jwt::Jws};
use p384::pkcs8::EncodePublicKey;
use rsa::{
    RsaPrivateKey, pkcs1v15,
    sha2::Sha256,
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use serde_json::Value;
use std::{
    sync::OnceLock,
    time::{Instant, UNIX_EPOCH},
};

const ISSUER: &str = "https://synthetic.example/tenant/";
const XUID: &str = "123";
const SERVICE_SECONDS: u64 = 1_900_000_000;

/// Generates an original synthetic issuer once; no real credential or private fixture is loaded.
fn issuer() -> &'static pkcs1v15::SigningKey<Sha256> {
    static ISSUER: OnceLock<pkcs1v15::SigningKey<Sha256>> = OnceLock::new();
    ISSUER.get_or_init(|| {
        pkcs1v15::SigningKey::new(RsaPrivateKey::new(&mut rand::rng(), 1024).unwrap())
    })
}

/// The connection key is independent of the issuer key.
fn key() -> SigningKey {
    SigningKey::from_slice(&[1; 48]).unwrap()
}

/// Builds bounded trusted public metadata from the synthetic issuer, retaining a non-retail issuer.
fn verifier() -> NativeTokenVerifier {
    let public = issuer().as_ref();
    let keys = serde_json::to_vec(&json!({"keys":[{"kty":"RSA","kid":"synthetic",
        "n":URL_SAFE_NO_PAD.encode(public.n().to_be_bytes()), "e":URL_SAFE_NO_PAD.encode(public.e().to_be_bytes())}]})).unwrap();
    NativeTokenVerifier::new(ISSUER.into(), &["RS256".into()], &keys).unwrap()
}

/// Keeps subject distinct from Xbox identity: the configured verifier does not equate these claims.
fn claims() -> Value {
    json!({"iss":ISSUER,"aud":MINECRAFT_AUDIENCE,"sub":"synthetic-subject","xid":XUID,
        "xname":"Synthetic","iat":SERVICE_SECONDS,"exp":SERVICE_SECONDS+3600,
        "cpk":STANDARD.encode(key().verifying_key().to_public_key_der().unwrap().as_bytes())})
}

/// Signs original public test claims through the existing RSA primitive.
fn signed(claims: Value, kid: &str) -> String {
    signed_header(claims, json!({"alg":"RS256","kid":kid}))
}

/// Signs issuer-authentic header-policy violations without conflating them with invalid signatures.
fn signed_header(claims: Value, header: Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let signature: pkcs1v15::Signature = issuer().sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_vec()))
}

/// Preserves the response's original observation rather than creating a remaining-lifetime TTL.
fn credential(token: String, observed: Instant, lifetime: Duration) -> NativeCredential {
    let service = UNIX_EPOCH + Duration::from_secs(SERVICE_SECONDS);
    NativeCredential::new(token, service, service + lifetime, observed).unwrap()
}

/// Exercises the production qualification entry point for other native Login tests.
pub(super) fn authentication() -> NativeAuthentication {
    authentication_at(Instant::now())
}

/// Simulates holding an already-qualified handoff without sleeping or changing a production clock.
fn authentication_at(observed: Instant) -> NativeAuthentication {
    let client =
        RawValue::from_string(r#"{"ThirdPartyName":"stale","DeviceOS":1}"#.into()).unwrap();
    NativeAuthentication::new(
        key(),
        credential(
            signed(claims(), "synthetic"),
            observed,
            Duration::from_secs(60),
        ),
        &client,
        &verifier(),
        XUID,
        observed,
    )
    .unwrap()
}

/// The actual public Login entry rejects a delayed handoff before any endpoint work.
#[tokio::test]
async fn native_provider_expired_handoff_cannot_start_a_connection() {
    let settings = crate::LoginSettings::default();
    let observed = Instant::now().checked_sub(Duration::from_secs(61)).unwrap();
    let result = LoginSequence::connect_native_authenticated(
        &NativeConnectionTarget {
            address: "unresolved.invalid:0".into(),
            transport: NativeTransport::RakNet,
        },
        SessionLogin {
            display_name: "Synthetic",
            client_random_id: crate::ClientRandomId::new(1).unwrap(),
            cache: None,
            skin: None,
            settings: &settings,
            tap: None,
            target: None,
        },
        authentication_at(observed),
        |_, _| (),
    )
    .await;
    assert!(
        matches!(result, Err(ProtocolError::Authentication(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
}

/// Signature, issuer, account/key identity and temporal claims are all part of the real handoff.
#[test]
fn native_provider_rejects_untrusted_identity_and_temporal_contracts() {
    let observed = Instant::now();
    let client = RawValue::from_string("{}".into()).unwrap();
    let verifier = verifier();
    for (field, value) in [
        ("iss", json!("https://another.example/")),
        ("aud", json!("another-service")),
        ("aud", json!([MINECRAFT_AUDIENCE, 7])),
        ("sub", json!("")),
        ("sub", Value::Null),
        ("xid", json!("456")),
        ("cpk", json!("not-a-key")),
        ("exp", json!(SERVICE_SECONDS)),
        ("exp", Value::Null),
        ("nbf", json!(SERVICE_SECONDS + 61)),
        ("iat", json!(SERVICE_SECONDS + 61)),
        ("leguuid", json!("invalid")),
        ("xname", json!("1Invalid")),
    ] {
        let mut claims = claims();
        claims[field] = value;
        let result = NativeAuthentication::new(
            key(),
            credential(
                signed(claims, "synthetic"),
                observed,
                Duration::from_secs(60),
            ),
            &client,
            &verifier,
            XUID,
            observed,
        );
        assert!(result.is_err(), "accepted invalid {field}");
    }
    let token = signed(claims(), "not-published");
    assert!(!verifier.recognizes(&token).unwrap());
    assert!(
        NativeAuthentication::new(
            key(),
            credential(token, observed, Duration::from_secs(60)),
            &client,
            &verifier,
            XUID,
            observed
        )
        .is_err()
    );
    let token = signed(claims(), "synthetic");
    let (input, signature) = token.rsplit_once('.').unwrap();
    let mut bytes = URL_SAFE_NO_PAD.decode(signature).unwrap();
    bytes[0] ^= 1;
    let untrusted = format!("{input}.{}", URL_SAFE_NO_PAD.encode(bytes));
    assert!(
        NativeAuthentication::new(
            key(),
            credential(untrusted, observed, Duration::from_secs(60)),
            &client,
            &verifier,
            XUID,
            observed
        )
        .is_err()
    );
    let other = SigningKey::from_slice(&[2; 48]).unwrap();
    assert!(
        NativeAuthentication::new(
            other,
            credential(token, observed, Duration::from_secs(60)),
            &client,
            &verifier,
            XUID,
            observed
        )
        .is_err()
    );
    let mut optional = claims();
    optional["nbf"] = Value::Null;
    optional["iat"] = Value::Null;
    optional["leguuid"] = Value::Null;
    assert!(
        NativeAuthentication::new(
            key(),
            credential(signed(optional, ""), observed, Duration::from_secs(60)),
            &client,
            &verifier,
            XUID,
            observed
        )
        .is_ok()
    );
    for header in [
        json!({"alg":"RS256","kid":"synthetic","crit":["unknown"]}),
        json!({"alg":"RS256","kid":"synthetic","b64":false}),
        json!({"alg":"HS256","kid":"synthetic"}),
    ] {
        let service = UNIX_EPOCH + Duration::from_secs(SERVICE_SECONDS);
        assert!(
            NativeCredential::new(
                signed_header(claims(), header),
                service,
                service + Duration::from_secs(60),
                observed
            )
            .is_err()
        );
    }
}

/// Provider/transport delay cannot extend the mint's hard validity or change the Login proof key.
#[test]
fn native_provider_preserves_hard_expiry_and_the_actual_login_token_and_key() {
    let observed = Instant::now();
    let client = RawValue::from_string(r#"{"ThirdPartyName":"stale"}"#.into()).unwrap();
    let token = signed(claims(), "synthetic");
    let authentication = NativeAuthentication::new(
        key(),
        credential(token.clone(), observed, Duration::from_secs(5)),
        &client,
        &verifier(),
        XUID,
        observed + Duration::from_secs(4),
    )
    .unwrap();
    assert_eq!(authentication.expires, observed + Duration::from_secs(5));
    assert_eq!(authentication.claims["ThirdPartyName"], "Synthetic");
    assert!(
        authentication::ensure_current(authentication.expires, observed + Duration::from_secs(5))
            .is_err()
    );
    assert!(
        NativeAuthentication::new(
            key(),
            credential(token.clone(), observed, Duration::from_secs(5)),
            &client,
            &verifier(),
            XUID,
            observed + Duration::from_secs(5)
        )
        .is_err()
    );

    let client = serde_json::value::to_raw_value(&authentication.claims).unwrap();
    let request = encode_authenticated_request(
        &authentication.key,
        &authentication.token,
        &client,
        bedrock_net::BatchLimits::default().max_decoded_bytes,
    )
    .unwrap();
    let length = u32::from_le_bytes(request[..4].try_into().unwrap()) as usize;
    let envelope: Value = serde_json::from_slice(&request[4..4 + length]).unwrap();
    assert_eq!(envelope["Token"], token);
    let rest = &request[4 + length..];
    let length = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
    let client = Jws::parse(std::str::from_utf8(&rest[4..4 + length]).unwrap()).unwrap();
    client.verify_es384(key().verifying_key()).unwrap();
    assert!(
        client
            .verify_es384(SigningKey::from_slice(&[2; 48]).unwrap().verifying_key())
            .is_err()
    );
}
