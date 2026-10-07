//! The Mac app's ShortLinkTests and ShareXImportTests, with the same
//! definitions file and the same `.sxcu` fixtures.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use super::definition::{self, custom_template, Capabilities, Definition, Expiration, Request, ShortLinkSettings, CUSTOM_ID};
use super::engine::{
    self, build, expiry_values, Cleanup, Created, Engine, HttpRequest, HttpResponse, Operations, Placeholder, ShortLinkError, Stats,
    Target, Transport, Values,
};
use super::rules::{self, Decision, MovePlan, Shortening, Snapshot, Status};
use super::sharex::{self, SecretExtractor, SecretLocation, ShareXImportError};

// MARK: - Helpers

fn definition(id: &str) -> Definition {
    definition::definition(id).unwrap_or_else(|| panic!("No {id} definition")).clone()
}

fn settings(provider: &str) -> ShortLinkSettings {
    ShortLinkSettings::new(provider)
}

fn with_endpoint(provider: &str, endpoint: &str) -> ShortLinkSettings {
    ShortLinkSettings { endpoint: Some(endpoint.into()), ..settings(provider) }
}

fn values(pairs: &[(Placeholder, &str)]) -> Values {
    pairs.iter().map(|(key, value)| (*key, value.to_string())).collect()
}

fn snapshot(provider: &str, status: Status, provider_id: Option<&str>, created_at: i64, expires_at: Option<i64>) -> Snapshot {
    Snapshot {
        id: crate::util::new_id(),
        provider: provider.into(),
        provider_id: provider_id.map(str::to_string),
        domain: None,
        status,
        created_at,
        expires_at,
    }
}

fn active_link() -> Snapshot {
    snapshot("shlink", Status::Active, Some("code"), crate::util::now_millis(), None)
}

/// Answers each request with the last response set.
#[derive(Default)]
struct FakeTransport {
    response: Mutex<Option<Result<HttpResponse, String>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeTransport {
    fn respond(&self, status: u16, body: &str) {
        *self.response.lock().unwrap() = Some(Ok(HttpResponse { status, body: body.as_bytes().to_vec() }));
    }

    fn fail(&self, message: &str) {
        *self.response.lock().unwrap() = Some(Err(message.into()));
    }

    fn last(&self) -> HttpRequest {
        self.requests.lock().unwrap().last().cloned().unwrap()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Transport for &FakeTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        self.requests.lock().unwrap().push(request);
        self.response.lock().unwrap().clone().unwrap_or(Ok(HttpResponse { status: 200, body: Vec::new() }))
    }
}

fn engine<'a>(definition: Definition, settings: ShortLinkSettings, token: &str, transport: &'a FakeTransport) -> Engine<&'a FakeTransport> {
    Engine { definition, settings, token: Some(token.into()), transport }
}

/// Deletes and updates succeed, except for the id "fail".
struct FakeOperations {
    provider: String,
    capabilities: Capabilities,
    deleted: Mutex<Vec<String>>,
    updated: Mutex<Vec<(String, String)>>,
}

impl FakeOperations {
    fn new(provider: &str, capabilities: Capabilities) -> Self {
        Self { provider: provider.into(), capabilities, deleted: Mutex::default(), updated: Mutex::default() }
    }
}

impl Operations for FakeOperations {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn create(&self, _url: &str, _expires_at: Option<i64>) -> Result<Created, ShortLinkError> {
        Ok(Created { short_url: "https://s.example.com/new".into(), provider_id: Some("new".into()) })
    }

    async fn delete(&self, target: &Target) -> Result<(), ShortLinkError> {
        self.deleted.lock().unwrap().push(target.provider_id.clone());
        if target.provider_id == "fail" {
            return Err(ShortLinkError::Rejected { status: 422, message: None });
        }
        Ok(())
    }

    async fn update(&self, target: &Target, url: &str) -> Result<(), ShortLinkError> {
        self.updated.lock().unwrap().push((target.provider_id.clone(), url.into()));
        if target.provider_id == "fail" {
            return Err(ShortLinkError::Rejected { status: 500, message: None });
        }
        Ok(())
    }

    async fn stats(&self, _target: &Target) -> Result<Stats, ShortLinkError> {
        Ok(Stats { clicks: Some(1), last_click_at: None })
    }
}

// MARK: - Definitions

#[test]
fn bundled_definitions_are_the_docs_file() {
    // The copy here is the Mac repo's file, byte for byte, when it's next
    // to this one (a checkout of both).
    let mac = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mac/docs/short-link-providers.json");
    if let Ok(original) = std::fs::read(&mac) {
        let copy = include_bytes!("../../../docs/short-link-providers.json");
        assert!(original == copy, "docs/short-link-providers.json differs from {}: copy it over", mac.display());
    }
    let ids: Vec<&str> = definition::built_in().iter().map(|definition| definition.id.as_str()).collect();
    assert_eq!(ids, ["shlink", "yourls", "kutt", "dub", "shortio"]);
}

#[test]
fn capabilities_decode() {
    let shlink = definition("shlink");
    assert_eq!(shlink.capabilities.expiration, Expiration::Absolute);
    assert!(shlink.capabilities.update_destination);
    assert_eq!(definition("kutt").capabilities.expiration, Expiration::Relative);
    let yourls = definition("yourls");
    assert_eq!(yourls.capabilities.expiration, Expiration::None);
    assert!(!yourls.capabilities.delete);
    assert!(yourls.delete.is_none());
    assert_eq!(yourls.create.success_statuses, Some(vec![409]));
    assert!(definition("shortio").needs_domain);
    assert!(definition("dub").is_hosted());
    assert!(!definition("dub").uses_endpoint() && definition("kutt").uses_endpoint());
}

#[test]
fn settings_round_trip_and_defaults() {
    let settings = ShortLinkSettings {
        endpoint: Some("https://s.example.com".into()),
        custom: Some(custom_template()),
        only_longer_than: 40,
        shorten_temporary_links: true,
        allow_insecure_http: true,
        ..settings(CUSTOM_ID)
    };
    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(json["allowInsecureHTTP"], true);
    assert_eq!(json["custom"]["capabilities"]["expiration"], false);
    assert_eq!(serde_json::from_value::<ShortLinkSettings>(json).unwrap(), settings);
    let minimal: ShortLinkSettings = serde_json::from_str(r#"{"providerId":"shlink"}"#).unwrap();
    assert_eq!(minimal.only_longer_than, 0);
    assert!(!minimal.shorten_temporary_links);
    assert!(!minimal.allow_insecure_http);
    assert_eq!(minimal.definition().map(|definition| definition.name.as_str()), Some("Shlink"));
    assert_eq!(serde_json::from_str::<ShortLinkSettings>(r#"{"providerId":"shlink","onlyLongerThan":-5}"#).unwrap().only_longer_than, 0);
}

#[test]
fn destinations_read_unusable_settings_as_off() {
    let destination = |short_links: Value| -> crate::destinations::DestinationConfig {
        serde_json::from_value(json!({
            "id": "D1", "name": "N", "preset": "minIO", "endpoint": "e", "region": "r", "bucket": "b",
            "publicBaseURL": "https://p", "objectPathTemplate": "{uuid}.{ext}", "forcePathStyle": true,
            "shortLinks": short_links,
        }))
        .unwrap()
    };
    assert_eq!(destination(json!({ "providerId": "kutt", "endpoint": "https://k" })).short_links.unwrap().provider_id, "kutt");
    // Unknown, unreadable, or custom without a definition: off, and the
    // destination still loads.
    assert!(destination(json!({ "providerId": "bitly-next" })).short_links.is_none());
    assert!(destination(json!({ "providerId": "shlink", "onlyLongerThan": "x" })).short_links.is_none());
    assert!(destination(json!({ "providerId": "custom" })).short_links.is_none());
    assert!(destination(Value::Null).short_links.is_none());
}

// MARK: - Requests

#[test]
fn shlink_requests() {
    let shlink = definition("shlink");
    let settings = with_endpoint("shlink", "s.example.com/");
    let create = build(&shlink.create, &shlink, &settings, &values(&[(Placeholder::Url, "https://files.example.com/a/b.png")]), Some("key")).unwrap();
    assert_eq!(create.url, "https://s.example.com/rest/v3/short-urls");
    assert_eq!(create.method, "POST");
    assert_eq!(create.header("X-Api-Key"), Some("key"));
    assert_eq!(create.header("Content-Type"), Some("application/json"));
    // No expiry and no domain: those keys aren't sent at all.
    assert_eq!(create.body_json(), Some(json!({ "longUrl": "https://files.example.com/a/b.png", "findIfExists": false })));

    let expiry = 1_791_892_800_000;
    let mut expiring_values = expiry_values(Some(expiry), expiry - 7 * 86_400_000);
    expiring_values.insert(Placeholder::Url, "https://files.example.com/a/b.png".into());
    let with_domain = ShortLinkSettings { domain: Some("go.example.com".into()), ..with_endpoint("shlink", "https://s.example.com") };
    let expiring = build(&shlink.create, &shlink, &with_domain, &expiring_values, Some("key")).unwrap();
    let body = expiring.body_json().unwrap();
    assert_eq!(body["validUntil"], "2026-10-13T12:00:00+00:00");
    assert_eq!(body["domain"], "go.example.com");

    // Shlink reads "?domain=" as the domain "", so it's left out.
    let delete = shlink.delete.as_ref().unwrap();
    let request = build(delete, &shlink, &settings, &values(&[(Placeholder::Id, "12C18")]), Some("key")).unwrap();
    assert_eq!(request.url, "https://s.example.com/rest/v3/short-urls/12C18");
    assert_eq!(request.method, "DELETE");
    assert!(request.body.is_none());
    let request = build(delete, &shlink, &settings, &values(&[(Placeholder::Id, "12C18"), (Placeholder::Domain, "go.example.com")]), Some("key")).unwrap();
    assert_eq!(request.url, "https://s.example.com/rest/v3/short-urls/12C18?domain=go.example.com");

    let stats = build(shlink.stats.as_ref().unwrap(), &shlink, &settings, &values(&[(Placeholder::Id, "12C18")]), Some("key")).unwrap();
    assert_eq!(stats.url, "https://s.example.com/rest/v3/short-urls/12C18/visits?itemsPerPage=1");
    let update = build(
        shlink.update.as_ref().unwrap(),
        &shlink,
        &settings,
        &values(&[(Placeholder::Id, "12C18"), (Placeholder::Url, "https://files.example.com/new.png")]),
        Some("key"),
    )
    .unwrap();
    assert_eq!(update.method, "PATCH");
    assert_eq!(update.body_json(), Some(json!({ "longUrl": "https://files.example.com/new.png" })));
}

#[test]
fn yourls_requests() {
    let yourls = definition("yourls");
    let settings = with_endpoint("yourls", "https://sho.rt");
    let create = build(&yourls.create, &yourls, &settings, &values(&[(Placeholder::Url, "https://files.example.com/a b+c.png?x=1&y=2")]), Some("sig")).unwrap();
    assert_eq!(create.url, "https://sho.rt/yourls-api.php?action=shorturl&format=json&signature=sig");
    assert_eq!(create.header("Content-Type"), Some("application/x-www-form-urlencoded"));
    // A + or & in the link survives the form body.
    assert_eq!(create.body_text(), "url=https%3A%2F%2Ffiles.example.com%2Fa%20b%2Bc.png%3Fx%3D1%26y%3D2");
    let stats = build(yourls.stats.as_ref().unwrap(), &yourls, &settings, &values(&[(Placeholder::Id, "3x")]), Some("sig")).unwrap();
    assert_eq!(stats.url, "https://sho.rt/yourls-api.php?action=url-stats&format=json&shorturl=3x&signature=sig");
}

#[test]
fn kutt_requests() {
    let kutt = definition("kutt");
    let settings = with_endpoint("kutt", "https://kutt.example.com");
    let plain = build(&kutt.create, &kutt, &settings, &values(&[(Placeholder::Url, "https://f.example.com/x.png")]), Some("k")).unwrap();
    assert_eq!(plain.header("X-API-KEY"), Some("k"));
    // " minutes" alone would fail validation, so expire_in is left out.
    assert_eq!(plain.body_json(), Some(json!({ "target": "https://f.example.com/x.png", "reuse": false })));

    let now = crate::util::now_millis();
    let mut expiring_values = expiry_values(Some(now + 7 * 86_400_000), now);
    expiring_values.insert(Placeholder::Url, "https://f.example.com/x.png".into());
    let expiring = build(&kutt.create, &kutt, &settings, &expiring_values, Some("k")).unwrap();
    assert_eq!(expiring.body_json().unwrap()["expire_in"], "10080 minutes");
    let stats = build(kutt.stats.as_ref().unwrap(), &kutt, &settings, &values(&[(Placeholder::Id, "6a7b")]), Some("k")).unwrap();
    assert_eq!(stats.url, "https://kutt.example.com/api/v2/links/6a7b/stats");
}

#[test]
fn dub_and_shortio_requests() {
    let dub = definition("dub");
    // Hosted: the endpoint is ignored.
    let dub_settings = with_endpoint("dub", "https://ignored.example.com");
    let create = build(&dub.create, &dub, &dub_settings, &values(&[(Placeholder::Url, "https://f.example.com/x.png")]), Some("dub_x")).unwrap();
    assert_eq!(create.url, "https://api.dub.co/links");
    assert_eq!(create.header("Authorization"), Some("Bearer dub_x"));
    assert_eq!(create.body_json(), Some(json!({ "url": "https://f.example.com/x.png" })));
    let stats = build(dub.stats.as_ref().unwrap(), &dub, &dub_settings, &values(&[(Placeholder::Id, "link_1")]), Some("dub_x")).unwrap();
    assert_eq!(stats.url, "https://api.dub.co/links/info?linkId=link_1");

    let shortio = definition("shortio");
    let missing = build(&shortio.create, &shortio, &settings("shortio"), &values(&[(Placeholder::Url, "https://f.example.com")]), Some("sk"));
    assert_eq!(missing.unwrap_err(), ShortLinkError::MissingDomain);
    let settings = ShortLinkSettings { domain: Some("short.gy".into()), ..settings("shortio") };
    let create = build(&shortio.create, &shortio, &settings, &values(&[(Placeholder::Url, "https://f.example.com/x.png")]), Some("sk")).unwrap();
    assert_eq!(create.header("Authorization"), Some("sk"));
    assert_eq!(create.body_json(), Some(json!({ "domain": "short.gy", "originalURL": "https://f.example.com/x.png", "allowDuplicates": true })));
    let stats = build(shortio.stats.as_ref().unwrap(), &shortio, &settings, &values(&[(Placeholder::Id, "lnk_1")]), Some("sk")).unwrap();
    assert_eq!(stats.url, "https://statistics.short.io/statistics/link/lnk_1?period=total");
}

#[test]
fn endpoint_and_token_checks() {
    let shlink = definition("shlink");
    let link = values(&[(Placeholder::Url, "https://f.example.com")]);
    let insecure = with_endpoint("shlink", "http://localhost:8081");
    assert_eq!(build(&shlink.create, &shlink, &insecure, &link, Some("k")).unwrap_err(), ShortLinkError::InsecureEndpoint);
    let allowed = ShortLinkSettings { allow_insecure_http: true, ..insecure };
    assert!(build(&shlink.create, &shlink, &allowed, &link, Some("k")).is_ok());
    assert_eq!(build(&shlink.create, &shlink, &settings("shlink"), &link, Some("k")).unwrap_err(), ShortLinkError::MissingEndpoint);
    assert_eq!(build(&shlink.create, &shlink, &with_endpoint("shlink", "ftp://x"), &link, Some("k")).unwrap_err(), ShortLinkError::InvalidEndpoint);
    let secure = with_endpoint("shlink", "https://s.example.com");
    assert_eq!(build(&shlink.create, &shlink, &secure, &link, Some(" ")).unwrap_err(), ShortLinkError::MissingToken);
    // A path needs its value.
    assert_eq!(
        build(shlink.delete.as_ref().unwrap(), &shlink, &secure, &Values::new(), Some("k")).unwrap_err(),
        ShortLinkError::MissingValue("id".into())
    );
}

#[test]
fn custom_definition_request() {
    let mut custom = custom_template();
    custom.create.path = "https://api.example.com/v1/shorten?key={token}&long={url}".into();
    custom.create.headers =
        Some([("Authorization".to_string(), "Bearer {token}".to_string()), ("X-Optional".to_string(), "{domain}".to_string())].into());
    let settings = ShortLinkSettings { custom: Some(custom.clone()), ..settings(CUSTOM_ID) };
    let request = build(&custom.create, &custom, &settings, &values(&[(Placeholder::Url, "https://f.example.com/a.png")]), Some("t0k")).unwrap();
    assert_eq!(request.url, "https://api.example.com/v1/shorten?key=t0k&long=https%3A%2F%2Ff.example.com%2Fa.png");
    assert_eq!(request.header("Authorization"), Some("Bearer t0k"));
    assert_eq!(request.header("X-Optional"), None);
    assert_eq!(request.body_json(), Some(json!({ "url": "https://f.example.com/a.png" })));
}

#[test]
fn expiry_values_round_down() {
    let now = 1_000_000_000;
    let values = expiry_values(Some(now + 299_900), now);
    assert_eq!(values[&Placeholder::ExpiresInSeconds], "299");
    // Rounded down, so the short link never outlives its target.
    assert_eq!(values[&Placeholder::ExpiresInMinutes], "4");
    assert_eq!(values[&Placeholder::ExpiresAtUnix], "1000299");
    assert!(expiry_values(Some(now + 59_000), now).is_empty());
    assert!(expiry_values(None, now).is_empty());
}

// MARK: - Answers

#[test]
fn response_extraction() {
    let json: Value = serde_json::from_str(
        r#"{"visits":{"data":[{"date":"2026-10-06T12:30:00+00:00"},{"date":"2026-10-05T08:00:00+00:00"}],"pagination":{"totalItems":12}},
            "link":{"clicks":"7"},"error":{"message":"Nope"},"ms":1791892800000,"empty":null}"#,
    )
    .unwrap();
    let json = Some(&json);
    assert_eq!(engine::int_at(Some("visits.pagination.totalItems"), json), Some(12));
    assert_eq!(engine::date_at(Some("visits.data.0.date"), json), Some(1_791_289_800_000));
    assert_eq!(engine::int_at(Some("link.clicks"), json), Some(7));
    assert_eq!(engine::string_at(Some("error.message"), json).as_deref(), Some("Nope"));
    assert_eq!(engine::date_at(Some("ms"), json), Some(1_791_892_800_000));
    assert!(engine::value_at(Some("visits.data.5.date"), json).is_none());
    assert!(engine::value_at(Some("empty"), json).is_none());
    assert!(engine::value_at(Some(""), json).is_none());
    assert_eq!(engine::parse_date("2026-10-06T12:00:00.000Z"), Some(1_791_288_000_000));
    assert_eq!(engine::parse_date("2026-10-06 12:00:00"), Some(1_791_288_000_000));
}

#[tokio::test]
async fn engine_create_and_errors() {
    let transport = FakeTransport::default();
    let engine = engine(definition("shlink"), with_endpoint("shlink", "https://s.example.com"), "secret-key", &transport);

    transport.respond(200, r#"{"shortCode":"12C18","shortUrl":"https://s.example.com/12C18"}"#);
    let created = engine.create("https://f.example.com/a.png", None).await.unwrap();
    assert_eq!(created, Created { short_url: "https://s.example.com/12C18".into(), provider_id: Some("12C18".into()) });

    transport.respond(422, r#"{"type":"invalid-short-url-deletion","detail":"Cannot delete, threshold secret-key reached"}"#);
    let target = Target { provider_id: "12C18".into(), domain: None };
    // The token never shows up in a message.
    assert_eq!(
        engine.delete(&target).await.unwrap_err(),
        ShortLinkError::Rejected { status: 422, message: Some("Cannot delete, threshold \u{2022}\u{2022}\u{2022} reached".into()) }
    );

    transport.respond(200, r#"{"nothing":"here"}"#);
    assert_eq!(engine.create("https://f.example.com/a.png", None).await.unwrap_err(), ShortLinkError::NoShortLink("shortUrl".into()));
    for short in ["javascript:alert(1)", "data:text/html,x", "file:///C:/x"] {
        transport.respond(200, &format!(r#"{{"shortUrl":"{short}"}}"#));
        assert_eq!(engine.create("https://f.example.com/a.png", None).await.unwrap_err(), ShortLinkError::NoShortLink("shortUrl".into()), "{short}");
    }

    transport.fail("operation timed out (secret-key)");
    let ShortLinkError::Network(message) = engine.create("https://f.example.com/a.png", None).await.unwrap_err() else { panic!() };
    assert!(!message.contains("secret-key"));
}

#[tokio::test]
async fn yourls_existing_url_counts_as_success() {
    let transport = FakeTransport::default();
    let engine = engine(definition("yourls"), with_endpoint("yourls", "https://sho.rt"), "sig", &transport);
    transport.respond(
        409,
        r#"{"status":"fail","code":"error:url","message":"https://f.example.com/a.png already exists in database","shorturl":"https://sho.rt/3x","url":{"keyword":"3x"},"statusCode":"409"}"#,
    );
    let created = engine.create("https://f.example.com/a.png", Some(crate::util::now_millis() + 86_400_000)).await.unwrap();
    assert_eq!(created, Created { short_url: "https://sho.rt/3x".into(), provider_id: Some("3x".into()) });
    // YOURLS can't expire links, so nothing about expiry is sent.
    assert!(!transport.last().body_text().contains("expire"));

    transport.respond(403, r#"{"message":"Please log in","errorCode":"403"}"#);
    assert_eq!(
        engine.create("https://f.example.com/a.png", None).await.unwrap_err(),
        ShortLinkError::Rejected { status: 403, message: Some("Please log in".into()) }
    );
}

#[tokio::test]
async fn success_path_fails_a_200() {
    let shortio = definition("shortio");
    assert_eq!(shortio.delete.as_ref().unwrap().success_path.as_deref(), Some("success"));
    assert_eq!(shortio.delete.as_ref().unwrap().success_value, Some(Value::Bool(true)));
    let transport = FakeTransport::default();
    let settings = ShortLinkSettings { domain: Some("short.gy".into()), ..settings("shortio") };
    let engine = engine(shortio, settings, "sk", &transport);
    let target = Target { provider_id: "lnk_1".into(), domain: None };
    transport.respond(200, r#"{"success":false,"error":"Link not found"}"#);
    assert_eq!(engine.delete(&target).await.unwrap_err(), ShortLinkError::Rejected { status: 200, message: Some("Link not found".into()) });
    // Lifecycle: the cleanup failed, so the link may still exist.
    let link = snapshot("shortio", Status::Active, Some("code"), 0, None);
    let statuses = rules::delete_all(std::slice::from_ref(&link), Some(&engine)).await;
    assert_eq!(statuses[&link.id], Status::Orphaned);
    transport.respond(200, r#"{"success":true,"idString":"lnk_1"}"#);
    engine.delete(&target).await.unwrap();
    // A 200 without the field fails too.
    transport.respond(200, "");
    assert!(engine.delete(&target).await.is_err());

    // A bool is never a number, and numbers compare by value.
    let request = |value: Value| Request { success_path: Some("ok".into()), success_value: Some(value), ..Request::new("GET", "/") };
    assert!(engine::is_success(200, Some(&json!({ "ok": true })), &request(json!(true))));
    assert!(!engine::is_success(200, Some(&json!({ "ok": 1 })), &request(json!(true))));
    assert!(engine::is_success(200, Some(&json!({ "ok": "ok" })), &request(json!("ok"))));
    assert!(engine::is_success(200, Some(&json!({ "ok": 1.0 })), &request(json!(1))));
}

#[tokio::test]
async fn custom_test_deletes_its_link() {
    let mut custom = custom_template();
    custom.create.path = "https://api.example.com/shorten".into();
    custom.create.id_path = Some("id".into());
    custom.delete = Some(Request {
        headers: Some([("Authorization".to_string(), "Bearer {token}".to_string())].into()),
        ..Request::new("DELETE", "/links/{id}")
    });
    custom.capabilities = Capabilities::custom(true);
    let transport = FakeTransport::default();
    let settings = ShortLinkSettings { endpoint: Some("https://api.example.com".into()), custom: Some(custom.clone()), ..settings(CUSTOM_ID) };
    let engine = engine(custom, settings, "t0k", &transport);
    transport.respond(200, r#"{"shortUrl":"https://s.example.com/x1","id":"x1"}"#);
    let result = engine.test().await.unwrap();
    assert_eq!(result.created.unwrap().short_url, "https://s.example.com/x1");
    assert_eq!(result.cleanup, Cleanup::Deleted);
    assert_eq!(transport.last().method, "DELETE");
    assert_eq!(transport.last().url, "https://api.example.com/links/x1");
    assert_eq!(transport.last().header("Authorization"), Some("Bearer t0k"));

    // Without a delete request nothing more is sent.
    let plain_settings = ShortLinkSettings { endpoint: Some("https://api.example.com".into()), ..self::settings(CUSTOM_ID) };
    let plain = self::engine(custom_template(), plain_settings, "t0k", &transport);
    transport.respond(200, r#"{"shortUrl":"https://s.example.com/x2"}"#);
    let count = transport.count();
    let result = plain.test().await.unwrap();
    assert_eq!(result.cleanup, Cleanup::None);
    assert_eq!(transport.count(), count + 1);
}

#[tokio::test]
async fn engine_stats() {
    let transport = FakeTransport::default();
    let engine = engine(definition("yourls"), with_endpoint("yourls", "https://sho.rt"), "sig", &transport);
    transport.respond(200, r#"{"statusCode":200,"link":{"shorturl":"https://sho.rt/3x","clicks":"12"}}"#);
    let stats = engine.stats(&Target { provider_id: "3x".into(), domain: None }).await.unwrap();
    assert_eq!(stats, Stats { clicks: Some(12), last_click_at: None });
}

// MARK: - Rules

#[test]
fn decide_rules() {
    let shlink = definition("shlink").capabilities;
    let yourls = definition("yourls").capabilities;
    let now = 1_000_000_000;
    let link = "https://files.example.com/2026/10/A7kdP2x.png";
    let length = link.chars().count() as i64;
    let mut settings = with_endpoint("shlink", "https://s.example.com");
    let plain = Shortening::default();

    assert_eq!(rules::decide(None, Some(&shlink), link, plain, now), Decision::Skip);
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, plain, now), Decision::Create { expires_at: None });

    // Only links longer than the limit, unless asked for by hand.
    settings.only_longer_than = length;
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, plain, now), Decision::Skip);
    let explicit = Shortening { explicit: true, ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, explicit, now), Decision::Create { expires_at: None });
    settings.only_longer_than = length - 1;
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, plain, now), Decision::Create { expires_at: None });
    settings.only_longer_than = 0;

    // The upload's expiry goes along when the provider can expire links.
    let in_7_days = now + 7 * 86_400_000;
    let expiring = Shortening { upload_expires_at: Some(in_7_days), ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, expiring, now), Decision::Create { expires_at: Some(in_7_days) });
    assert_eq!(rules::decide(Some(&settings), Some(&yourls), link, expiring, now), Decision::Create { expires_at: None });

    // Temporary links: only with the toggle and expiration support, and
    // never outliving the temporary link.
    let in_an_hour = now + 3_600_000;
    let temporary = Shortening { temporary_expires_at: Some(in_an_hour), ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, temporary, now), Decision::Skip);
    settings.shorten_temporary_links = true;
    let both = Shortening { temporary_expires_at: Some(in_an_hour), upload_expires_at: Some(in_7_days), ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, both, now), Decision::Create { expires_at: Some(in_an_hour) });
    assert_eq!(rules::decide(Some(&settings), Some(&yourls), link, temporary, now), Decision::Skip);
    let soon = Shortening { temporary_expires_at: Some(now + 30_000), ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, soon, now), Decision::Skip);
    assert!(!rules::can_shorten_temporary_links(Some(&yourls)));
    assert!(rules::can_shorten_temporary_links(Some(&shlink)));

    // One file of a folder uploaded with its structure.
    let folder_file = Shortening { is_folder_file: true, ..plain };
    assert_eq!(rules::decide(Some(&settings), Some(&shlink), link, folder_file, now), Decision::Skip);
}

#[test]
fn picks_the_active_link() {
    let now = 1_000_000_000;
    let old = snapshot("shlink", Status::Active, Some("a"), now - 300_000, None);
    let newer = snapshot("shlink", Status::Active, Some("b"), now - 100_000, None);
    let newest_expired = snapshot("shlink", Status::Active, Some("c"), now - 50_000, Some(now - 1_000));
    let orphaned = snapshot("shlink", Status::Orphaned, Some("d"), now, None);
    let links = [old, newer.clone(), newest_expired.clone(), orphaned.clone()];
    assert_eq!(rules::active(&links, now).map(|link| link.id.clone()), Some(newer.id));
    assert!(rules::active(&[orphaned, newest_expired.clone()], now).is_none());
    assert_eq!(rules::display_status(&newest_expired, now), Status::Expired);
}

#[tokio::test]
async fn delete_transitions() {
    let fake = FakeOperations::new("shlink", definition("shlink").capabilities);
    let active = active_link();
    let failing = snapshot("shlink", Status::Active, Some("fail"), 0, None);
    let expired = snapshot("shlink", Status::Expired, Some("fail"), 0, None);
    let other_provider = snapshot("kutt", Status::Active, Some("code"), 0, None);
    let already_deleted = snapshot("shlink", Status::Deleted, Some("code"), 0, None);
    let no_id = snapshot("shlink", Status::Active, None, 0, None);
    let links = [active.clone(), failing.clone(), expired.clone(), other_provider.clone(), already_deleted.clone(), no_id.clone()];
    let statuses = rules::delete_all(&links, Some(&fake)).await;
    assert_eq!(statuses[&active.id], Status::Deleted);
    assert_eq!(statuses[&failing.id], Status::Orphaned);
    assert_eq!(statuses[&expired.id], Status::Expired);
    assert_eq!(statuses[&other_provider.id], Status::Orphaned);
    assert_eq!(statuses[&no_id.id], Status::Orphaned);
    assert!(!statuses.contains_key(&already_deleted.id));
    assert_eq!(*fake.deleted.lock().unwrap(), ["code", "fail", "fail"]);

    // A provider without delete: the link may still exist.
    let yourls = FakeOperations::new("shlink", definition("yourls").capabilities);
    let no_delete = rules::delete_all(std::slice::from_ref(&active), Some(&yourls)).await;
    assert_eq!(no_delete[&active.id], Status::Orphaned);
    assert!(yourls.deleted.lock().unwrap().is_empty());
    // No shortener set up any more.
    let none = rules::delete_all(std::slice::from_ref(&active), None::<&FakeOperations>).await;
    assert_eq!(none[&active.id], Status::Orphaned);

    assert_eq!(rules::delete(&active, Some(&fake)).await, Ok(Status::Deleted));
    assert_eq!(rules::delete(&active, Some(&yourls)).await, Err(ShortLinkError::Unsupported));
    assert_eq!(rules::delete(&active, None::<&FakeOperations>).await, Err(ShortLinkError::NotConfigured));
}

#[tokio::test]
async fn move_transitions() {
    let now = crate::util::now_millis();
    let shlink = FakeOperations::new("shlink", definition("shlink").capabilities);
    let yourls = FakeOperations::new("yourls", definition("yourls").capabilities);
    let active = active_link();
    let failing = snapshot("shlink", Status::Active, Some("fail"), now, None);

    assert_eq!(rules::move_plan(&[], Some(&shlink), now), MovePlan::Nothing);
    assert_eq!(rules::move_plan(&[snapshot("shlink", Status::Orphaned, Some("code"), now, None)], Some(&shlink), now), MovePlan::Nothing);
    assert_eq!(rules::move_plan(std::slice::from_ref(&active), Some(&shlink), now), MovePlan::Update);
    assert_eq!(rules::move_plan(&[snapshot("yourls", Status::Active, Some("code"), now, None)], Some(&yourls), now), MovePlan::Warn);
    assert_eq!(rules::move_plan(std::slice::from_ref(&active), None::<&FakeOperations>, now), MovePlan::Warn);
    // Made with a provider that isn't set up any more.
    assert_eq!(rules::move_plan(&[snapshot("kutt", Status::Active, Some("code"), now, None)], Some(&shlink), now), MovePlan::Warn);

    let (statuses, all_updated) = rules::update_all(std::slice::from_ref(&active), "https://f.example.com/new.png", &shlink, now).await;
    assert!(all_updated);
    assert_eq!(statuses[&active.id], Status::Active);
    assert_eq!(shlink.updated.lock().unwrap()[0].1, "https://f.example.com/new.png");

    // An update that fails: the old object stays, the link is unknown.
    let (statuses, all_updated) = rules::update_all(&[active.clone(), failing.clone()], "https://f.example.com/new.png", &shlink, now).await;
    assert!(!all_updated);
    assert_eq!(statuses[&failing.id], Status::Unknown);
    assert_eq!(statuses[&active.id], Status::Active);

    // Moved anyway without update support.
    let expired = snapshot("shlink", Status::Expired, Some("code"), now, None);
    assert_eq!(rules::orphan_all(&[active.clone(), expired]), HashMap::from([(active.id.clone(), Status::Orphaned)]));
}

#[test]
fn expiry_transitions() {
    let active = active_link();
    let unknown = snapshot("shlink", Status::Unknown, Some("code"), 0, None);
    let deleted = snapshot("shlink", Status::Deleted, Some("code"), 0, None);
    assert_eq!(
        rules::expire_all(&[active.clone(), unknown.clone(), deleted]),
        HashMap::from([(active.id, Status::Expired), (unknown.id, Status::Expired)])
    );
}

// MARK: - Output

#[test]
fn output_uses_the_short_link() {
    use crate::output::{format, OutputMode};
    let long = "https://files.example.com/2026/10/A7kdP2x.png";
    let short = Some("https://s.example.com/12C18");
    assert_eq!(format(long, short, OutputMode::Url, "a.png", ""), "https://s.example.com/12C18");
    assert_eq!(format(long, short, OutputMode::Markdown, "a.png", ""), "![](https://s.example.com/12C18)");
    assert_eq!(format(long, short, OutputMode::Html, "a.pdf", ""), "<a href=\"https://s.example.com/12C18\">a.pdf</a>");
    assert_eq!(
        format(long, short, OutputMode::Custom, "a.png", "{url} {shortUrl} {longUrl}"),
        "https://s.example.com/12C18 https://s.example.com/12C18 https://files.example.com/2026/10/A7kdP2x.png"
    );
    assert_eq!(
        format(long, None, OutputMode::Custom, "a.png", "{shortUrl} {longUrl}"),
        "https://files.example.com/2026/10/A7kdP2x.png https://files.example.com/2026/10/A7kdP2x.png"
    );
}

// MARK: - History

/// The short links table joins an existing history database, and its
/// queries pick the same active link as `rules::active`.
#[test]
fn history_carries_the_active_short_link() {
    use crate::history::{History, NewRecord};
    let directory = std::env::temp_dir().join(format!("aktar-short-links-{}", crate::util::new_id()));
    std::fs::create_dir_all(&directory).unwrap();
    {
        // The table as versions before short links made it.
        let connection = rusqlite::Connection::open(directory.join("history.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE uploads (
                     id TEXT PRIMARY KEY NOT NULL, local_filename TEXT NOT NULL, object_key TEXT NOT NULL,
                     public_url TEXT NOT NULL, destination_id TEXT NOT NULL, destination_name TEXT NOT NULL,
                     mime_type TEXT NOT NULL, byte_size INTEGER NOT NULL, created_at INTEGER NOT NULL
                 );
                 INSERT INTO uploads VALUES ('A', 'a.png', 'a.png', 'https://f.example.com/a.png', 'D1', 'Main', 'image/png', 1, 1000);",
            )
            .unwrap();
    }
    let history = History::open(&directory, crate::thumbnails::LocalStore::open(directory.join("thumbnails"), None)).unwrap();
    let old = history.get("A").unwrap();
    assert_eq!((old.local_filename.as_str(), old.short_url.as_deref()), ("a.png", None));

    let now = crate::util::now_millis();
    let store = history.short_links();
    let link = |id: &str, created_at: i64, expires_at: Option<i64>, status: Status| super::store::ShortLink {
        id: id.into(),
        upload_id: "A".into(),
        provider: "shlink".into(),
        provider_name: "Shlink".into(),
        provider_id: Some(id.to_lowercase()),
        domain: None,
        short_url: format!("https://s.example.com/{id}"),
        target_url: "https://f.example.com/a.png".into(),
        created_at,
        expires_at,
        status,
        clicks: None,
        last_click_at: None,
        stats_checked_at: None,
    };
    store.insert(&link("OLD", now - 300_000, None, Status::Active));
    store.insert(&link("NEWER", now - 100_000, None, Status::Active));
    store.insert(&link("EXPIRED", now - 50_000, Some(now - 1_000), Status::Active));
    store.insert(&link("ORPHANED", now, None, Status::Orphaned));
    let record = history.get("A").unwrap();
    assert_eq!(record.short_url.as_deref(), Some("https://s.example.com/NEWER"));
    assert_eq!(record.short_link_id.as_deref(), Some("NEWER"));
    assert_eq!(record.short_provider.as_deref(), Some("shlink"));
    let snapshots: Vec<Snapshot> = store.for_upload("A").iter().map(super::store::ShortLink::snapshot).collect();
    assert_eq!(rules::active(&snapshots, now).map(|link| link.id.as_str()), Some("NEWER"));
    assert_eq!(history.all()[0].short_url, record.short_url);

    store.set_statuses(&HashMap::from([("NEWER".to_string(), Status::Deleted)]));
    assert_eq!(history.get("A").unwrap().short_url.as_deref(), Some("https://s.example.com/OLD"));
    store.set_stats("OLD", Some(3), Some(now), now);
    assert_eq!(store.get("OLD").map(|link| link.clicks), Some(Some(3)));

    // A new history entry has none, and Remove from History forgets them.
    let destination: crate::destinations::DestinationConfig = serde_json::from_value(json!({
        "id": "D1", "name": "Main", "preset": "minIO", "endpoint": "e", "region": "r", "bucket": "b",
        "publicBaseURL": "https://f.example.com", "objectPathTemplate": "{uuid}.{ext}", "forcePathStyle": true,
    }))
    .unwrap();
    let fresh = history.insert(NewRecord {
        local_filename: "b.png",
        object_key: "b.png",
        public_url: "https://f.example.com/b.png",
        destination: &destination,
        mime_type: "image/png",
        byte_size: 1,
        expire_after_days: None,
        content_hash: None,
        watched_folder: None,
    });
    assert_eq!(history.get(&fresh.id).unwrap().short_url, None);
    store.delete_for_upload("A");
    assert!(store.for_upload("A").is_empty());
    assert_eq!(history.get("A").unwrap().short_url, None);
    drop(history);
    let _ = std::fs::remove_dir_all(&directory);
}

// MARK: - ShareX

fn sxcu(name: &str) -> &'static str {
    match name {
        "bearer-form" => include_str!("../../tests/fixtures/sxcu/bearer-form.sxcu"),
        "file-uploader" => include_str!("../../tests/fixtures/sxcu/file-uploader.sxcu"),
        "insecure" => include_str!("../../tests/fixtures/sxcu/insecure.sxcu"),
        "prompt" => include_str!("../../tests/fixtures/sxcu/prompt.sxcu"),
        "regex" => include_str!("../../tests/fixtures/sxcu/regex.sxcu"),
        "response-transform" => include_str!("../../tests/fixtures/sxcu/response-transform.sxcu"),
        "shlink" => include_str!("../../tests/fixtures/sxcu/shlink.sxcu"),
        "two-secrets" => include_str!("../../tests/fixtures/sxcu/two-secrets.sxcu"),
        "yourls-legacy" => include_str!("../../tests/fixtures/sxcu/yourls-legacy.sxcu"),
        _ => panic!("No fixture {name}"),
    }
}

fn parse(name: &str, allow_insecure_http: bool) -> Result<sharex::ShareXImport, ShareXImportError> {
    sharex::parse(sxcu(name).as_bytes(), allow_insecure_http)
}

#[test]
fn sharex_fixtures_are_the_mac_ones() {
    let mac = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mac/Tests/Fixtures/sxcu");
    let Ok(entries) = std::fs::read_dir(&mac) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().trim_end_matches(".sxcu").to_string();
        assert_eq!(std::fs::read_to_string(entry.path()).unwrap(), sxcu(&name), "{name}.sxcu differs from the Mac repo's");
    }
}

#[test]
fn sharex_shortener_with_header_secret() {
    let imported = parse("shlink", false).unwrap();
    assert_eq!(imported.name.as_deref(), Some("Shlink (s.example.com)"));
    assert_eq!(imported.host, "s.example.com");
    assert_eq!(imported.method, "POST");
    assert_eq!(imported.endpoint, "https://s.example.com/rest/v3/short-urls");
    assert_eq!(imported.token.as_deref(), Some("c0ffee-1234-secret"));
    assert_eq!(imported.secret_locations, [SecretLocation::Header("X-Api-Key".into())]);
    assert!(!imported.uses_http);
    assert!(!imported.deletion_skipped);

    let create = &imported.definition.create;
    assert_eq!(create.path, "https://s.example.com/rest/v3/short-urls");
    assert_eq!(create.headers, Some([("X-Api-Key".to_string(), "{token}".to_string())].into()));
    assert_eq!(create.body_type, Some(definition::BodyType::Json));
    assert_eq!(create.body, Some(json!({ "longUrl": "{url}", "findIfExists": true })));
    assert_eq!(create.short_url_path.as_deref(), Some("shortUrl"));
    assert_eq!(create.id_path.as_deref(), Some("shortCode"));
    assert_eq!(create.error_path.as_deref(), Some("detail"));
    // The deletion URL is opened, as ShareX does, without the headers.
    let delete = imported.definition.delete.as_ref().unwrap();
    assert_eq!(delete.method, "GET");
    assert_eq!(delete.path, "https://s.example.com/admin/delete/{id}");
    assert!(delete.headers.is_none());
    assert!(imported.definition.capabilities.delete);

    // The secret is nowhere in the definition that gets saved.
    assert!(!serde_json::to_string(&imported.definition).unwrap().contains("c0ffee"));

    // And the request it makes puts the token back where it was.
    let settings = ShortLinkSettings { custom: Some(imported.definition.clone()), ..settings(CUSTOM_ID) };
    let request =
        build(create, &imported.definition, &settings, &values(&[(Placeholder::Url, "https://f.example.com/a.png")]), imported.token.as_deref()).unwrap();
    assert_eq!(request.url, "https://s.example.com/rest/v3/short-urls");
    assert_eq!(request.header("X-Api-Key"), Some("c0ffee-1234-secret"));
}

#[test]
fn sharex_legacy_syntax_and_query_secret() {
    let imported = parse("yourls-legacy", false).unwrap();
    assert_eq!(imported.method, "GET");
    assert_eq!(imported.token.as_deref(), Some("5f3c2a1b9e"));
    assert_eq!(imported.secret_locations, [SecretLocation::Query("signature".into())]);
    let create = &imported.definition.create;
    assert!(create.body_type.is_none());
    let query: Vec<(&str, &str)> = create.query.as_ref().unwrap().iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
    assert_eq!(query, [("action", "shorturl"), ("format", "json"), ("signature", "{token}"), ("url", "{url}")]);
    assert_eq!(create.short_url_path.as_deref(), Some("shorturl"));
    assert_eq!(imported.endpoint, "https://sho.rt/yourls-api.php?action=shorturl&format=json&signature={token}&url={url}");
    assert!(imported.definition.delete.is_none());
}

#[test]
fn sharex_bearer_form_and_json_path() {
    let imported = parse("bearer-form", false).unwrap();
    // The same secret in two places is one token.
    assert_eq!(imported.token.as_deref(), Some("abc.def.ghi"));
    let mut locations = imported.secret_locations.clone();
    locations.sort_by_key(|location| format!("{location:?}"));
    assert_eq!(locations, [SecretLocation::Body("api_key".into()), SecretLocation::Header("Authorization".into())]);
    let create = &imported.definition.create;
    assert_eq!(
        create.headers,
        Some([("Authorization".to_string(), "Bearer {token}".to_string()), ("Accept".to_string(), "application/json".to_string())].into())
    );
    assert_eq!(create.query, Some([("client".to_string(), "sharex".to_string())].into()));
    assert_eq!(create.body_type, Some(definition::BodyType::Form));
    assert_eq!(create.body, Some(json!({ "link": "{url}", "api_key": "{token}" })));
    assert_eq!(create.short_url_path.as_deref(), Some("data.0.short_link"));
    // A deletion URL that comes whole from the answer can't be stored.
    assert!(imported.deletion_skipped);
    assert!(imported.definition.delete.is_none());
    assert!(!imported.definition.capabilities.delete);
}

#[test]
fn sharex_http_needs_the_toggle() {
    assert_eq!(parse("insecure", false).unwrap_err(), ShareXImportError::Insecure);
    let imported = parse("insecure", true).unwrap();
    assert!(imported.uses_http);
    assert_eq!(imported.host, "localhost");
    assert_eq!(imported.token.as_deref(), Some("kutt-local-key"));
    assert_eq!(imported.definition.delete.as_ref().map(|delete| delete.path.as_str()), Some("http://localhost:8082/api/v2/links/{id}"));
    assert_eq!(imported.definition.create.id_path.as_deref(), Some("id"));
}

#[test]
fn sharex_unsupported_features_are_refused() {
    assert!(matches!(parse("regex", false), Err(ShareXImportError::Unsupported(_))));
    assert_eq!(parse("response-transform", false).unwrap_err(), ShareXImportError::Unsupported("{response}".into()));
    assert_eq!(parse("prompt", false).unwrap_err(), ShareXImportError::Unsupported("{prompt:Slug}".into()));
    assert_eq!(parse("file-uploader", false).unwrap_err(), ShareXImportError::NotShortener);
    assert_eq!(parse("two-secrets", false).unwrap_err(), ShareXImportError::MultipleSecrets);
    assert_eq!(sharex::parse(b"not json", false).unwrap_err(), ShareXImportError::Invalid);
    // The messages say what's wrong without any secret.
    assert_eq!(ShareXImportError::Unsupported("{select:a|b}".into()).to_string(), "Aktar can’t import this configuration: it uses {select:a|b}.");
}

#[test]
fn sharex_syntax_helpers() {
    assert_eq!(sharex::map("u={input}&x=$input$").unwrap(), "u={url}&x={url}");
    // Braces and dollars that aren't ShareX functions are just text.
    assert_eq!(sharex::map("pa$word$x {abc}").unwrap(), "pa$word$x {abc}");
    assert!(sharex::map("{filename}").is_err());
    assert!(sharex::map("{random:a|b}").is_err());
    assert_eq!(sharex::response_path(Some("$json:data.link$")).unwrap().as_deref(), Some("data.link"));
    assert_eq!(sharex::response_path(Some("{json:$.items[2].url}")).unwrap().as_deref(), Some("items.2.url"));
    assert_eq!(sharex::response_path(Some("{json:$['data'][0]}")).unwrap().as_deref(), Some("data.0"));
    assert_eq!(sharex::response_path(None).unwrap(), None);
    assert!(sharex::response_path(Some("https://x.example/{json:code}")).is_err());
    assert_eq!(sharex::dot_path("a..b"), None);
    assert_eq!(sharex::dot_path("items[*].url"), None);
    assert!(SecretExtractor::is_secret_name("X-Api-Key"));
    assert!(SecretExtractor::is_secret_name("access_token"));
    assert!(!SecretExtractor::is_secret_name("keyword"));
    assert!(!SecretExtractor::is_secret_name("format"));
}

// MARK: - A real Shlink

/// Runs against a Shlink instance with
/// `AKTAR_TEST_SHLINK_URL=http://localhost:8081 AKTAR_TEST_SHLINK_KEY=... cargo test short_links -- --ignored`.
fn live_shlink() -> Option<(String, String)> {
    Some((std::env::var("AKTAR_TEST_SHLINK_URL").ok()?, std::env::var("AKTAR_TEST_SHLINK_KEY").ok()?))
}

#[tokio::test]
#[ignore]
async fn real_shlink() {
    let Some((endpoint, key)) = live_shlink() else { return eprintln!("AKTAR_TEST_SHLINK_URL and AKTAR_TEST_SHLINK_KEY aren't set") };
    let settings = ShortLinkSettings { endpoint: Some(endpoint.clone()), allow_insecure_http: true, ..settings("shlink") };
    let engine = Engine::new(definition("shlink"), settings.clone(), Some(key.clone()));
    assert_eq!(engine.test().await.unwrap().created, None);
    let target_url = format!("https://getaktar.com/?aktar-test={}", crate::util::new_id());
    let created = engine.create(&target_url, Some(crate::util::now_millis() + 7 * 86_400_000)).await.unwrap();
    let id = created.provider_id.clone().unwrap();
    assert!(created.short_url.ends_with(&format!("/{id}")));
    let target = Target { provider_id: id.clone(), domain: None };

    // A visit, then the stats.
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let _ = client.get(&created.short_url).header("User-Agent", "Mozilla/5.0 (Windows NT 10.0) AktarTest").send().await;
    let mut stats = Stats::default();
    for _ in 0..10 {
        stats = engine.stats(&target).await.unwrap();
        if stats.clicks.unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert_eq!(stats.clicks, Some(1));
    assert!(stats.last_click_at.is_some());

    engine.update(&target, &format!("{target_url}&moved=1")).await.unwrap();
    engine.delete(&target).await.unwrap();
    assert!(matches!(engine.stats(&target).await, Err(ShortLinkError::Rejected { status: 404, .. })));

    // A wrong key is refused, with Shlink's own message.
    let wrong = Engine::new(definition("shlink"), settings, Some("wrong-key".into()));
    let Err(ShortLinkError::Rejected { status: 401, message }) = wrong.test().await else { panic!("Expected a 401") };
    assert!(message.is_some());
}

/// A ShareX configuration for Shlink, imported and used against the real
/// thing.
#[tokio::test]
#[ignore]
async fn real_shlink_from_sharex() {
    let Some((endpoint, key)) = live_shlink() else { return eprintln!("AKTAR_TEST_SHLINK_URL and AKTAR_TEST_SHLINK_KEY aren't set") };
    let configuration = json!({
        "Version": "16.1.0",
        "DestinationType": "URLShortener",
        "RequestMethod": "POST",
        "RequestURL": format!("{endpoint}/rest/v3/short-urls"),
        "Headers": { "X-Api-Key": key },
        "Body": "JSON",
        "Data": r#"{"longUrl":"{input}","findIfExists":false}"#,
        "URL": "{json:shortUrl}",
        "ErrorMessage": "{json:detail}",
    });
    let imported = sharex::parse(configuration.to_string().as_bytes(), endpoint.starts_with("http://")).unwrap();
    assert_eq!(imported.token.as_deref(), Some(key.as_str()));
    let settings = ShortLinkSettings { custom: Some(imported.definition.clone()), allow_insecure_http: imported.uses_http, ..settings(CUSTOM_ID) };
    let engine = Engine::new(imported.definition.clone(), settings.clone(), imported.token.clone());
    let created = engine.create(&format!("https://getaktar.com/?aktar-sxcu={}", crate::util::new_id()), None).await.unwrap();
    assert!(created.short_url.starts_with("http"));

    // Cleaned up with the built-in definition.
    let shlink = Engine::new(
        definition("shlink"),
        ShortLinkSettings { endpoint: Some(endpoint), allow_insecure_http: true, ..self::settings("shlink") },
        Some(key),
    );
    let code = created.short_url.rsplit('/').next().unwrap().to_string();
    shlink.delete(&Target { provider_id: code, domain: None }).await.unwrap();

    // A wrong token: Shlink's own message, and not the token.
    let wrong = Engine::new(imported.definition, settings, Some("wrong-token-123".into()));
    let Err(ShortLinkError::Rejected { status: 401, message }) = wrong.create("https://getaktar.com/", None).await else { panic!("Expected a 401") };
    assert!(!message.unwrap_or_default().contains("wrong-token-123"));
}
