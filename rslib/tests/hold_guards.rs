// Copyright: Ankitects Pty Ltd and contributors
// License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html

//! Hold guards: behaviour the checksums and the sync server must keep when
//! their dependencies move (sha1, pbkdf2 and the typed sync header). The
//! golden values were taken by running this crate's own code paths at
//! a078820fd4fec3068ed7863344cd50effddabc6b, before any of those moves.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::LazyLock;

use anki::collection::CollectionBuilder;
use anki::error::AnkiError;
use anki::error::SyncError;
use anki::error::SyncErrorKind;
use anki::media::MediaManager;
use anki::prelude::DeckId;
use anki::sync::http_server::default_ip_header;
use anki::sync::http_server::SimpleServer;
use anki::sync::http_server::SyncServerConfig;
use anki::sync::login::sync_login;
use anki::sync::login::HostKeyRequest;
use anki::sync::request::SyncRequest;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::extract::FromRequest;
use axum::http::HeaderValue;
use axum::http::Request;
use axum::http::StatusCode;
use axum_client_ip::ClientIpSource;
use tempfile::tempdir;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

/// field_checksum() of the first field, as stored in the notes table.
const FIELD_CHECKSUMS: [(&str, i64); 3] = [
    ("test", 2840236005),
    ("今日", 1464653051),
    ("<b>hold</b> guard", 3503506758),
];

/// sha1_of_data() of a file's bytes, as stored in the media database.
const MEDIA_SHA1S: [(&[u8], &str); 2] = [
    (b"hello", "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"),
    (
        "hold guard 今日".as_bytes(),
        "38063321d201f665987d48eff96e91bfd3a730d8",
    ),
];

/// derive_hkey() of a SYNC_USER value, as returned by a host key login.
const HOST_KEYS: [(&str, &str, &str); 2] = [
    ("user", "pass", "b2619aa1529dfdc4248e6edbf3c1b2a2b014cf6d"),
    (
        "holdguard",
        "p@ss:wörd 今日",
        "b99cfc7d73a7b5135dd9ba01554df72300a9edbd",
    ),
];

/// A PHC string the sync server's password hash path produced for
/// PHC_PASSWORD at a078820, with its fixed salt.
const PHC_FIXTURE: &str = include_str!("support/sync-server-a078820.phc");
const PHC_PASSWORD: &str = "fixture-password";
const PHC_WRONG_PASSWORD: &str = "fixture-passwore";

const SYNC_HEADER: &str = r#"{"v":11,"k":"hkey-golden","c":"client-golden","s":"skey-golden"}"#;

/// The server reads SYNC_USER* and PASSWORDS_HASHED from the environment, so
/// tests that start one take turns.
static SERVER_ENV: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn stored_field_checksum(text: &str) -> i64 {
    let dir = tempdir().unwrap();
    let path = dir.path().join("col.anki2");
    let mut col = CollectionBuilder::new(&path).build().unwrap();
    let nt = col.get_notetype_by_name("Basic").unwrap().unwrap();
    let mut note = nt.new_note();
    note.set_field(0, text).unwrap();
    col.add_note(&mut note, DeckId(1)).unwrap();
    let nid = note.id.0;
    col.close(None).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.query_row("select csum from notes where id = ?", [nid], |row| {
        row.get(0)
    })
    .unwrap()
}

fn stored_media_sha1(data: &[u8]) -> String {
    let dir = tempdir().unwrap();
    let mgr = MediaManager::new(dir.path().join("media"), dir.path().join("media.db")).unwrap();
    let name = mgr.add_file("golden.txt", data).unwrap().to_string();
    let mut get = mgr.checksum_getter();
    hex::encode(get(&name).unwrap().unwrap())
}

async fn start_server(
    users: &[String],
    passwords_hashed: bool,
) -> (String, TempDir, MutexGuard<'static, ()>) {
    let guard = SERVER_ENV.lock().await;
    for idx in 1..=4 {
        std::env::remove_var(format!("SYNC_USER{idx}"));
    }
    for (idx, user) in users.iter().enumerate() {
        std::env::set_var(format!("SYNC_USER{}", idx + 1), user);
    }
    if passwords_hashed {
        std::env::set_var("PASSWORDS_HASHED", "1");
    } else {
        std::env::remove_var("PASSWORDS_HASHED");
    }
    let base = tempdir().unwrap();
    let (addr, fut) = SimpleServer::make_server(SyncServerConfig {
        host: "127.0.0.1".parse().unwrap(),
        port: 0,
        base_folder: base.path().into(),
        ip_header: default_ip_header(),
    })
    .await
    .unwrap();
    tokio::spawn(fut);
    (format!("http://{addr}/"), base, guard)
}

async fn login(user: &str, pass: &str, endpoint: &str) -> Result<String, AnkiError> {
    sync_login(
        user,
        pass,
        Some(endpoint.to_string()),
        reqwest::Client::new(),
    )
    .await
    .map(|auth| auth.hkey)
}

fn is_auth_failed(err: &AnkiError) -> bool {
    matches!(
        err,
        AnkiError::SyncError {
            source: SyncError {
                kind: SyncErrorKind::AuthFailed,
                ..
            }
        }
    )
}

fn sync_request(header: Option<HeaderValue>, body: Vec<u8>) -> Request<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri("/sync/hostKey")
        .body(Body::from(body))
        .unwrap();
    if let Some(value) = header {
        req.headers_mut().insert("anki-sync", value);
    }
    req.extensions_mut().insert(ClientIpSource::ConnectInfo);
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 27701))));
    req
}

fn host_key_body() -> (Vec<u8>, Vec<u8>) {
    let json = serde_json::to_vec(&HostKeyRequest {
        username: "user".into(),
        password: "pass".into(),
    })
    .unwrap();
    let body = zstd::encode_all(json.as_slice(), 0).unwrap();
    (json, body)
}

#[test]
fn field_checksum_goldens_hold_in_stored_notes() {
    for (text, golden) in FIELD_CHECKSUMS {
        assert_eq!(stored_field_checksum(text), golden, "csum of {text:?}");
    }
}

#[test]
fn sha1_of_data_goldens_hold_in_the_media_db() {
    for (data, golden) in MEDIA_SHA1S {
        assert_eq!(stored_media_sha1(data), golden, "sha1 of {data:?}");
    }
}

#[tokio::test]
async fn derive_hkey_goldens_hold_in_host_key_logins() {
    let users: Vec<String> = HOST_KEYS
        .iter()
        .map(|(user, pass, _)| format!("{user}:{pass}"))
        .collect();
    let (endpoint, _base, _guard) = start_server(&users, false).await;
    for (user, pass, golden) in HOST_KEYS {
        assert_eq!(login(user, pass, &endpoint).await.unwrap(), golden, "{user}");
    }
    let err = login("user", "wrong", &endpoint).await.unwrap_err();
    assert!(is_auth_failed(&err), "a wrong password must be refused: {err:?}");
}

#[tokio::test]
async fn a078820_phc_fixture_verifies_and_a_wrong_password_is_refused() {
    let phc = PHC_FIXTURE.trim();
    assert!(phc.starts_with("$pbkdf2-sha256$"), "{phc}");
    let (endpoint, _base, _guard) = start_server(&[format!("user:{phc}")], true).await;
    let verified = login("user", PHC_PASSWORD, &endpoint).await;
    assert!(
        verified.is_ok(),
        "the a078820 PHC fixture must verify its password: {verified:?}"
    );
    let err = login("user", PHC_WRONG_PASSWORD, &endpoint)
        .await
        .unwrap_err();
    assert!(is_auth_failed(&err), "a wrong password must be refused: {err:?}");
}

#[tokio::test]
async fn sync_header_decodes_as_before() {
    let (json, body) = host_key_body();
    let req = sync_request(Some(HeaderValue::from_static(SYNC_HEADER)), body);
    let Ok(parsed) =
        <SyncRequest<HostKeyRequest> as FromRequest<()>>::from_request(req, &()).await
    else {
        panic!("a well-formed anki-sync header must decode");
    };
    assert_eq!(parsed.sync_version.0, 11);
    assert_eq!(parsed.sync_key, "hkey-golden");
    assert_eq!(parsed.client_version, "client-golden");
    assert_eq!(parsed.session_key, "skey-golden");
    assert_eq!(parsed.ip, IpAddr::from([127, 0, 0, 1]));
    assert_eq!(parsed.data, json);
    let decoded = parsed.json().unwrap();
    assert_eq!(decoded.username, "user");
    assert_eq!(decoded.password, "pass");
}

#[tokio::test]
async fn a_malformed_sync_header_is_a_400_bad_sync_header() {
    let cases = [
        ("not json", HeaderValue::from_static("not json")),
        ("missing fields", HeaderValue::from_static(r#"{"v":11}"#)),
        ("opaque bytes", HeaderValue::from_bytes(b"\xfa\xfb").unwrap()),
    ];
    for (case, value) in cases {
        let (_, body) = host_key_body();
        let req = sync_request(Some(value), body);
        match <SyncRequest<HostKeyRequest> as FromRequest<()>>::from_request(req, &()).await {
            Ok(_) => panic!("{case}: a malformed anki-sync header must not decode"),
            Err(err) => {
                assert_eq!(err.code, StatusCode::BAD_REQUEST, "{case}");
                assert_eq!(err.context, "bad sync header", "{case}");
            }
        }
    }
}

#[tokio::test]
async fn an_absent_sync_header_takes_the_multipart_path() {
    let (_, body) = host_key_body();
    let req = sync_request(None, body);
    match <SyncRequest<HostKeyRequest> as FromRequest<()>>::from_request(req, &()).await {
        Ok(_) => panic!("a request with no anki-sync header and no form must not decode"),
        Err(err) => {
            assert_eq!(err.code, StatusCode::BAD_REQUEST);
            assert_eq!(err.context, "multipart");
        }
    }
}
