//! Sign in with `ChatGPT` against a sign-in service on the loopback interface: registration,
//! the browser's part played by the test, the code exchange, a signed ID token, renewal and
//! revocation. No network.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use corduroy_chatgpt::sign_in::Front;
use corduroy_chatgpt::{ChatGpt, Endpoints, PLAN_SCOPE, Record, SCOPE, Store, random};
use corduroy_test_support::scratch;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use serde_json::{Value, json};

/// A key made for these tests and used nowhere else: the service signs ID tokens with it.
const KEY: &str = include_str!("fixtures/signing-key.pk8.b64");

type Failure = Box<dyn std::error::Error>;
type Form = BTreeMap<String, String>;

/// What the service was asked, and how it is to answer.
#[derive(Default)]
struct Service {
    /// The `nonce` of the sign-in under way, which the ID token repeats.
    nonce: String,
    /// The forms posted to the token endpoint, in order.
    tokens: Vec<Form>,
    /// The forms posted to the revocation endpoint.
    revoked: Vec<Form>,
    /// Sign ID tokens so that the signature does not hold.
    forge: bool,
    /// Answer a renewal with this status and OAuth error.
    refuse: Option<(u16, &'static str)>,
}

fn pairs(text: &str) -> Form {
    text.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| {
            (
                corduroy_chatgpt::web::decode(name),
                corduroy_chatgpt::web::decode(value),
            )
        })
        .collect()
}

fn key() -> Result<RsaKeyPair, Failure> {
    let der = STANDARD.decode(KEY.replace('\n', ""))?;
    Ok(RsaKeyPair::from_pkcs8(&der).map_err(|error| error.to_string())?)
}

/// An ID token for `nonce`, signed with [`KEY`] (or not quite, if `forge`).
fn id_token(issuer: &str, nonce: &str, forge: bool) -> Result<String, Failure> {
    let encode = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let header = encode(&json!({"alg": "RS256", "kid": "test", "typ": "JWT"}));
    let claims = encode(&json!({
        "iss": issuer, "aud": "oaiapp_issued", "sub": "user-1", "nonce": nonce,
        "email": "person@example.com", "exp": 4_000_000_000_u64,
    }));
    let signed = if forge {
        format!("{header}.{claims}x")
    } else {
        format!("{header}.{claims}")
    };
    let key = key()?;
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|error| error.to_string())?;
    Ok(format!(
        "{header}.{claims}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

fn answer(
    issuer: &str,
    path: &str,
    form: &Form,
    service: &Mutex<Service>,
) -> Result<(u16, Value), Failure> {
    Ok(match path {
        "/.well-known/openid-configuration" => (
            200,
            json!({"jwks_uri": format!("{issuer}/jwks"),
                   "revocation_endpoint": format!("{issuer}/revoke")}),
        ),
        "/jwks" => {
            let public: RsaPublicKeyComponents<Vec<u8>> = key()?.public().into();
            let number = |bytes: &[u8]| URL_SAFE_NO_PAD.encode(bytes);
            (
                200,
                json!({"keys": [{
                    "kty": "RSA", "kid": "test", "alg": "RS256",
                    "n": number(&public.n), "e": number(&public.e),
                }]}),
            )
        }
        "/api/accounts/oauth/token" => {
            let renewal = form.get("grant_type").map(String::as_str) == Some("refresh_token");
            let (number, nonce, forge, refuse) = {
                let mut service = service.lock().map_err(|_| "poisoned")?;
                service.tokens.push(form.clone());
                let refuse = service.refuse.filter(|_| renewal);
                (
                    service.tokens.len(),
                    service.nonce.clone(),
                    service.forge,
                    refuse,
                )
            };
            match refuse {
                Some((status, error)) => (status, json!({"error": error})),
                None => (
                    200,
                    json!({
                        "access_token": format!("access-{number}"),
                        "refresh_token": format!("refresh-{number}"),
                        "id_token": id_token(issuer, &nonce, forge)?,
                        "token_type": "Bearer", "expires_in": 3600, "scope": SCOPE,
                    }),
                ),
            }
        }
        "/revoke" => {
            service
                .lock()
                .map_err(|_| "poisoned")?
                .revoked
                .push(form.clone());
            (200, Value::Null)
        }
        _ => (404, Value::Null),
    })
}

fn serve(stream: &TcpStream, issuer: &str, service: &Mutex<Service>) -> Result<(), Failure> {
    let mut reader = BufReader::new(stream);
    let (mut first, mut line, mut length) = (String::new(), String::new(), 0);
    reader.read_line(&mut first)?;
    while reader.read_line(&mut line)? > 0 && line != "\r\n" {
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse()?;
        }
        line.clear();
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let path = first.split_whitespace().nth(1).unwrap_or("");
    let (status, reply) = answer(
        issuer,
        path,
        &pairs(&String::from_utf8_lossy(&body)),
        service,
    )?;
    let reply = if reply.is_null() {
        String::new()
    } else {
        reply.to_string()
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{reply}",
        reply.len()
    );
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    Ok(())
}

/// A sign-in service on the loopback interface, and a `ChatGpt` that talks to it, keeping
/// its tokens under a scratch directory named for `test`.
/// A `ChatGpt` and the service it talks to.
type Running = (ChatGpt, Arc<Mutex<Service>>);

fn service(test: &str) -> Result<Running, Failure> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let issuer = format!("http://{}", listener.local_addr()?);
    let service = Arc::new(Mutex::new(Service::default()));
    let (kept, origin) = (Arc::clone(&service), issuer.clone());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _served = serve(&stream, &origin, &kept);
        }
    });
    let store = Store::named(&scratch(test), "plan");
    Ok((
        ChatGpt::new(store).with_endpoints(Endpoints::at(&issuer)),
        service,
    ))
}

/// The browser's part: read the authorize URL, tell the service the nonce, and come back to
/// the redirect URI with `back` (to which the URL's own `state` is added unless given).
fn browser(
    service: &Arc<Mutex<Service>>,
    seen: &Arc<Mutex<Vec<Form>>>,
    back: &'static [(&'static str, &'static str)],
) -> impl Fn(&str) + use<> {
    let (service, seen) = (Arc::clone(service), Arc::clone(seen));
    move |url: &str| {
        let asked = pairs(url.split_once('?').map_or("", |(_, query)| query));
        if let Ok(mut service) = service.lock() {
            service.nonce = asked.get("nonce").cloned().unwrap_or_default();
        }
        let mut returned: Vec<(&str, &str)> = back.to_vec();
        let state = asked.get("state").cloned().unwrap_or_default();
        if !back.iter().any(|(name, _)| *name == "state") {
            returned.push(("state", &state));
        }
        let target = format!(
            "{}?{}",
            asked.get("redirect_uri").cloned().unwrap_or_default(),
            corduroy_chatgpt::web::query(&returned)
        );
        if let Ok(mut seen) = seen.lock() {
            seen.push(asked);
        }
        thread::spawn(move || {
            let address = target.trim_start_matches("http://");
            let (host, path) = address.split_once('/').unwrap_or((address, ""));
            if let Ok(mut stream) = TcpStream::connect(host) {
                let _sent = write!(stream, "GET /{path} HTTP/1.1\r\nHost: {host}\r\n\r\n");
                let _read = stream.read_to_end(&mut Vec::new());
            }
        });
    }
}

const ISSUED: &[(&str, &str)] = &[("code", "c0de"), ("client_id", "oaiapp_issued")];

fn sign_in(chatgpt: &ChatGpt, open: &dyn Fn(&str)) -> Result<Record, String> {
    let mut say = |_: &str| {};
    chatgpt.sign_in_within(
        &mut Front {
            open,
            say: &mut say,
        },
        Duration::from_secs(10),
    )
}

#[test]
fn a_first_sign_in_registers_a_client_and_a_later_one_uses_it() {
    let (chatgpt, service) = service("chatgpt-sign-in").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));

    let record = sign_in(&chatgpt, &browser(&service, &seen, ISSUED)).unwrap();
    assert_eq!(record.client_id, "oaiapp_issued");
    assert_eq!(record.subject, "user-1");
    assert_eq!(record.email.as_deref(), Some("person@example.com"));
    assert_eq!(
        (record.access_token.as_str(), record.refresh_token.as_str()),
        ("access-1", "refresh-1")
    );
    assert!(record.plan() && record.scopes.contains(&PLAN_SCOPE.to_owned()));
    assert_eq!(chatgpt.store().load().unwrap(), Some(record.clone()));

    // What the browser was sent with, and what the code was exchanged with.
    let asked = seen.lock().unwrap().first().unwrap().clone();
    assert_eq!(asked["client_id"], "dynamic_agent_client");
    assert_eq!(asked["agent_name_hint"], "Corduroy");
    assert_eq!(asked["ext_agent_host_id"], record.ext_agent_host_id);
    assert_eq!(
        (
            asked["scope"].as_str(),
            asked["code_challenge_method"].as_str()
        ),
        (SCOPE, "S256")
    );
    assert_eq!(asked["resource"], "https://api.openai.com/v1");
    assert!(
        asked["redirect_uri"].starts_with("http://127.0.0.1:"),
        "{asked:?}"
    );
    assert!(
        asked["redirect_uri"].ends_with("/auth/callback"),
        "{asked:?}"
    );
    let exchanged = service.lock().unwrap().tokens.first().unwrap().clone();
    assert_eq!(exchanged["client_id"], "oaiapp_issued");
    assert_eq!(exchanged["code"], "c0de");
    assert_eq!(exchanged["redirect_uri"], asked["redirect_uri"]);
    assert_eq!(
        random::challenge(&exchanged["code_verifier"]),
        asked["code_challenge"]
    );

    // Again: the issued client, the last ID token as a hint, no name, and the same host.
    let again = sign_in(&chatgpt, &browser(&service, &seen, &[("code", "c0de")])).unwrap();
    assert_eq!(again.client_id, "oaiapp_issued");
    let asked = seen.lock().unwrap().get(1).unwrap().clone();
    assert_eq!(asked["client_id"], "oaiapp_issued");
    assert_eq!(asked["id_token_hint"], record.id_token);
    assert!(!asked.contains_key("agent_name_hint"));
    assert_eq!(asked["ext_agent_host_id"], record.ext_agent_host_id);
}

#[test]
fn a_sign_in_that_is_declined_or_not_ours_or_forged_keeps_nothing() {
    let (chatgpt, service) = service("chatgpt-refused").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let refused = |back| sign_in(&chatgpt, &browser(&service, &seen, back)).unwrap_err();

    assert!(refused(&[("error", "access_denied")]).contains("declined"));
    let other = refused(&[
        ("code", "c0de"),
        ("client_id", "oaiapp_issued"),
        ("state", "x"),
    ]);
    assert!(other.contains("not the answer to this sign-in"), "{other}");
    let unregistered = refused(&[("code", "c0de")]);
    assert!(
        unregistered.contains("no client was issued"),
        "{unregistered}"
    );
    let elsewhere = refused(&[
        ("code", "c"),
        ("client_id", "oaiapp_issued"),
        ("iss", "http://x"),
    ]);
    assert!(elsewhere.contains("another issuer"), "{elsewhere}");
    // Nothing was exchanged for any of those.
    assert_eq!(service.lock().unwrap().tokens.len(), 0);

    service.lock().unwrap().forge = true;
    let forged = refused(ISSUED);
    assert!(forged.contains("signature"), "{forged}");
    assert_eq!(chatgpt.store().load().unwrap(), None);
}

#[test]
fn the_token_is_renewed_as_it_runs_out_and_the_new_refresh_token_is_kept() {
    let (chatgpt, service) = service("chatgpt-renew").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let nobody = chatgpt.token().unwrap_err();
    assert!(nobody.contains("corduroy login"), "{nobody}");

    let record = sign_in(&chatgpt, &browser(&service, &seen, ISSUED)).unwrap();
    // Good for an hour: no request is made for it.
    assert_eq!(chatgpt.token().unwrap(), "access-1");
    assert_eq!(service.lock().unwrap().tokens.len(), 1);

    // Four minutes from running out: renewed, with the refresh token it had.
    let aged = |by: u64| Record {
        saved_at: record.saved_at - by,
        ..chatgpt.store().load().unwrap().unwrap()
    };
    chatgpt.store().save(&aged(3400)).unwrap();
    assert_eq!(chatgpt.token().unwrap(), "access-2");
    let renewal = service.lock().unwrap().tokens.last().unwrap().clone();
    assert_eq!(renewal["grant_type"], "refresh_token");
    assert_eq!(renewal["refresh_token"], "refresh-1");
    assert_eq!(renewal["client_id"], "oaiapp_issued");
    assert!(!renewal.contains_key("scope"));
    let kept = chatgpt.store().load().unwrap().unwrap();
    assert_eq!(kept.refresh_token, "refresh-2");
    assert!(kept.plan());

    // The service is down: a token that is still good stays in use, one that is not fails.
    service.lock().unwrap().refuse = Some((503, "temporarily_unavailable"));
    chatgpt.store().save(&aged(3400)).unwrap();
    assert_eq!(chatgpt.token().unwrap(), "access-2");
    chatgpt.store().save(&aged(4000)).unwrap();
    let down = chatgpt.token().unwrap_err();
    assert!(
        down.contains("could not be renewed") && down.contains("503"),
        "{down}"
    );
    assert!(chatgpt.store().load().unwrap().is_some());

    // The refresh token is spent: the person is sent to sign in again, and no token is in
    // what they are told.
    service.lock().unwrap().refuse = Some((400, "invalid_grant"));
    let spent = chatgpt.token().unwrap_err();
    assert!(
        spent.contains("corduroy login") && spent.contains("invalid_grant"),
        "{spent}"
    );
    assert!(
        !spent.contains("refresh-") && !spent.contains("access-"),
        "{spent}"
    );
}

#[test]
fn signing_out_revokes_the_refresh_token_and_forgets_the_tokens() {
    let (chatgpt, service) = service("chatgpt-sign-out").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    assert!(!chatgpt.sign_out().unwrap());
    sign_in(&chatgpt, &browser(&service, &seen, ISSUED)).unwrap();
    assert!(chatgpt.sign_out().unwrap());
    let revoked = service.lock().unwrap().revoked.first().unwrap().clone();
    assert_eq!(revoked["token"], "refresh-1");
    assert_eq!(revoked["token_type_hint"], "refresh_token");
    assert_eq!(revoked["client_id"], "oaiapp_issued");
    assert_eq!(chatgpt.store().load().unwrap(), None);
    // The host keeps its ID for the next sign-in.
    assert!(chatgpt.store().host_id().unwrap().starts_with("urn:uuid:"));
}
