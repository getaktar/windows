//! Builds the HTTP requests of a definition and reads their answers. No
//! provider has code of its own: everything comes from the definition.
//! Nothing here is logged, and no error message ever contains the token.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::Serialize;
use serde_json::Value;

use super::definition::{AuthType, BodyType, Capabilities, Definition, Request, ShortLinkSettings};
use crate::t;

/// What a definition's templates can use. A query parameter, body value or
/// header that uses one with no value is left out entirely rather than
/// sent empty; a path that does can't be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Placeholder {
    Url,
    Id,
    Domain,
    ExpiresAt,
    ExpiresAtUnix,
    ExpiresInSeconds,
    ExpiresInMinutes,
    Token,
}

impl Placeholder {
    pub const ALL: [Placeholder; 8] = [
        Placeholder::Url,
        Placeholder::Id,
        Placeholder::Domain,
        Placeholder::ExpiresAt,
        Placeholder::ExpiresAtUnix,
        Placeholder::ExpiresInSeconds,
        Placeholder::ExpiresInMinutes,
        Placeholder::Token,
    ];

    pub fn raw_value(self) -> &'static str {
        match self {
            Placeholder::Url => "url",
            Placeholder::Id => "id",
            Placeholder::Domain => "domain",
            Placeholder::ExpiresAt => "expiresAt",
            Placeholder::ExpiresAtUnix => "expiresAtUnix",
            Placeholder::ExpiresInSeconds => "expiresInSeconds",
            Placeholder::ExpiresInMinutes => "expiresInMinutes",
            Placeholder::Token => "token",
        }
    }

    fn token(self) -> String {
        format!("{{{}}}", self.raw_value())
    }
}

pub type Values = BTreeMap<Placeholder, String>;

/// The expiry placeholders for `expires_at` (Unix milliseconds); none
/// without one, or once it's less than a minute away (the shortest a
/// relative expiry can be). Relative values are rounded down, so the short
/// link never outlives what it points at.
pub fn expiry_values(expires_at: Option<i64>, now: i64) -> Values {
    let Some(expires_at) = expires_at else { return Values::new() };
    let seconds = (expires_at - now).div_euclid(1000);
    if seconds < 60 {
        return Values::new();
    }
    Values::from([
        (Placeholder::ExpiresAt, iso8601(expires_at)),
        (Placeholder::ExpiresAtUnix, expires_at.div_euclid(1000).to_string()),
        (Placeholder::ExpiresInSeconds, seconds.to_string()),
        (Placeholder::ExpiresInMinutes, (seconds / 60).to_string()),
    ])
}

/// "2026-10-13T12:00:00+00:00": the form Shlink validates (ATOM), and ISO
/// 8601 for everyone else.
pub fn iso8601(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis).unwrap_or_default().format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortLinkError {
    NotConfigured,
    MissingEndpoint,
    InvalidEndpoint,
    InsecureEndpoint,
    MissingDomain,
    MissingToken,
    MissingValue(String),
    Unsupported,
    Network(String),
    Rejected { status: u16, message: Option<String> },
    NoShortLink(String),
}

impl std::fmt::Display for ShortLinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ShortLinkError::NotConfigured => t!("This destination has no link shortener set up."),
            ShortLinkError::MissingEndpoint => t!("Enter the address of your link shortener."),
            ShortLinkError::InvalidEndpoint => t!("The link shortener’s address isn’t valid."),
            ShortLinkError::InsecureEndpoint => {
                t!("The link shortener’s address uses http://. Turn on “Allow insecure HTTP” to use it anyway.")
            }
            ShortLinkError::MissingDomain => t!("Enter the short domain."),
            ShortLinkError::MissingToken => t!("Enter the API key."),
            ShortLinkError::MissingValue(name) => t!("The request needs a value for {{0}}.", name),
            ShortLinkError::Unsupported => t!("This link shortener can’t do that."),
            ShortLinkError::Network(message) => t!("The link shortener didn’t answer. {0}", message),
            ShortLinkError::Rejected { status, message: Some(message) } if !message.is_empty() => {
                t!("The link shortener refused the request (HTTP {0}): {1}", status, message)
            }
            ShortLinkError::Rejected { status, .. } => t!("The link shortener refused the request (HTTP {0}).", status),
            ShortLinkError::NoShortLink(path) => t!("The link shortener’s answer has no short link at “{0}”.", path),
        };
        formatter.write_str(&text)
    }
}

impl std::error::Error for ShortLinkError {}

/// A request ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    /// A header's value; names are compared without case.
    #[cfg(test)]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(existing, _)| existing.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    /// Sets a header, replacing one with the same name.
    fn set_header(&mut self, name: &str, value: String) {
        self.headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value));
    }

    #[cfg(test)]
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(self.body.as_deref().unwrap_or_default()).into_owned()
    }

    #[cfg(test)]
    pub fn body_json(&self) -> Option<Value> {
        serde_json::from_slice(self.body.as_deref()?).ok()
    }
}

/// Everything but unreserved characters is encoded, so a `+` or `&` in a
/// link survives a form body or query string.
const ENCODED: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

pub fn encode(text: &str) -> String {
    utf8_percent_encode(text, ENCODED).to_string()
}

/// The request `request` describes, with `values` filled in. `token` goes
/// where the definition's auth says, and wherever a template uses
/// `{token}`.
pub fn build(
    request: &Request,
    definition: &Definition,
    settings: &ShortLinkSettings,
    values: &Values,
    token: Option<&str>,
) -> Result<HttpRequest, ShortLinkError> {
    let mut values: Values = values.iter().filter(|(_, value)| !value.is_empty()).map(|(key, value)| (*key, value.clone())).collect();
    let token = token.unwrap_or_default().trim().to_string();
    if !token.is_empty() {
        values.insert(Placeholder::Token, token.clone());
    }
    if let Some(domain) = settings.trimmed_domain() {
        values.entry(Placeholder::Domain).or_insert(domain);
    }
    if definition.needs_domain && !values.contains_key(&Placeholder::Domain) {
        return Err(ShortLinkError::MissingDomain);
    }
    if definition.auth.is_some() && token.is_empty() {
        return Err(ShortLinkError::MissingToken);
    }

    // The path, with any query string it carries split off as templates.
    let mut path_template = request.path.clone();
    let mut query_templates = request.query.clone().unwrap_or_default();
    if let Some(mark) = path_template.find('?') {
        for pair in path_template[mark + 1..].split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            if !name.is_empty() {
                query_templates.insert(name.to_string(), value.to_string());
            }
        }
        path_template.truncate(mark);
    }
    let path = fill_path(&path_template, &values)?;

    let lowered = path.to_ascii_lowercase();
    let base = if lowered.starts_with("https://") || lowered.starts_with("http://") {
        path
    } else {
        let root = base_url(definition, settings)?.ok_or(ShortLinkError::MissingEndpoint)?;
        let root = root.strip_suffix('/').unwrap_or(&root);
        if path.starts_with('/') || path.is_empty() {
            format!("{root}{path}")
        } else {
            format!("{root}/{path}")
        }
    };
    check_scheme(&base, settings)?;

    let mut query: Vec<(String, String)> =
        query_templates.iter().filter_map(|(name, template)| fill(template, &values).map(|value| (name.clone(), value))).collect();
    if let Some(auth) = definition.auth.as_ref().filter(|auth| auth.kind == AuthType::Query) {
        query.push((auth.name.clone().unwrap_or_else(|| "token".into()), token.clone()));
    }
    let mut url = base.clone();
    if !query.is_empty() {
        let separator = if base.contains('?') { '&' } else { '?' };
        let pairs: Vec<String> = query.iter().map(|(name, value)| format!("{}={}", encode(name), encode(value))).collect();
        url = format!("{url}{separator}{}", pairs.join("&"));
    }
    if !url::Url::parse(&url).is_ok_and(|parsed| parsed.host_str().is_some_and(|host| !host.is_empty())) {
        return Err(ShortLinkError::InvalidEndpoint);
    }

    let mut built = HttpRequest { method: request.method.to_ascii_uppercase(), url, headers: Vec::new(), body: None };
    built.set_header("Accept", "application/json".into());
    if let Some(auth) = &definition.auth {
        let name = auth.name.clone().unwrap_or_else(|| "Authorization".into());
        match auth.kind {
            AuthType::Header => built.set_header(&name, token.clone()),
            AuthType::Bearer => built.set_header(&name, format!("Bearer {token}")),
            AuthType::Basic => {
                built.set_header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(token.as_bytes())))
            }
            AuthType::Query => {}
        }
    }
    for (name, template) in request.headers.iter().flatten() {
        if let Some(value) = fill(template, &values) {
            built.set_header(name, value);
        }
    }

    if let (Some(body), Some(body_type)) = (&request.body, request.body_type) {
        let filled = fill_value(body, &values).unwrap_or_else(|| Value::Object(Default::default()));
        match body_type {
            BodyType::Json => {
                built.body = Some(serde_json::to_vec(&filled).unwrap_or_default());
                built.set_header("Content-Type", "application/json".into());
            }
            BodyType::Form => {
                if let Value::Object(fields) = &filled {
                    let pairs: Vec<String> = fields
                        .iter()
                        .filter_map(|(name, value)| form_value(value).map(|value| format!("{}={}", encode(name), encode(&value))))
                        .collect();
                    built.body = Some(pairs.join("&").into_bytes());
                    built.set_header("Content-Type", "application/x-www-form-urlencoded".into());
                }
            }
        }
    }
    Ok(built)
}

/// The definition's base URL, or the destination's endpoint (https://
/// assumed without a scheme). None when there's neither.
pub fn base_url(definition: &Definition, settings: &ShortLinkSettings) -> Result<Option<String>, ShortLinkError> {
    if let Some(base) = &definition.base_url {
        return Ok(Some(base.clone()));
    }
    let endpoint = settings.endpoint.as_deref().unwrap_or_default().trim();
    if endpoint.is_empty() {
        return Ok(None);
    }
    let lowered = endpoint.to_ascii_lowercase();
    if lowered.starts_with("https://") || lowered.starts_with("http://") {
        return Ok(Some(endpoint.to_string()));
    }
    if endpoint.contains("://") {
        return Err(ShortLinkError::InvalidEndpoint);
    }
    Ok(Some(format!("https://{endpoint}")))
}

/// http:// only when the user allowed it; nothing but http(s).
pub fn check_scheme(url: &str, settings: &ShortLinkSettings) -> Result<(), ShortLinkError> {
    let lowered = url.to_ascii_lowercase();
    if lowered.starts_with("https://") {
        Ok(())
    } else if lowered.starts_with("http://") {
        if settings.allow_insecure_http {
            Ok(())
        } else {
            Err(ShortLinkError::InsecureEndpoint)
        }
    } else {
        Err(ShortLinkError::InvalidEndpoint)
    }
}

/// `template` with the placeholders that have values filled in; none if it
/// uses one that has none. Braces that aren't a placeholder stay.
pub fn fill(template: &str, values: &Values) -> Option<String> {
    let mut result = template.to_string();
    for placeholder in Placeholder::ALL {
        let token = placeholder.token();
        if result.contains(&token) {
            result = result.replace(&token, values.get(&placeholder)?);
        }
    }
    Some(result)
}

/// A body template filled in: a string with a placeholder that has no
/// value drops out of its object or array (none at the top).
pub fn fill_value(template: &Value, values: &Values) -> Option<Value> {
    match template {
        Value::String(text) => fill(text, values).map(Value::String),
        Value::Object(fields) => {
            Some(Value::Object(fields.iter().filter_map(|(key, value)| fill_value(value, values).map(|value| (key.clone(), value))).collect()))
        }
        Value::Array(items) => Some(Value::Array(items.iter().filter_map(|item| fill_value(item, values)).collect())),
        other => Some(other.clone()),
    }
}

/// Path placeholders are percent-encoded as one segment each; a missing
/// value can't be left out of a path.
fn fill_path(template: &str, values: &Values) -> Result<String, ShortLinkError> {
    let mut result = template.to_string();
    for placeholder in Placeholder::ALL {
        let token = placeholder.token();
        if result.contains(&token) {
            let value = values.get(&placeholder).ok_or_else(|| ShortLinkError::MissingValue(placeholder.raw_value().into()))?;
            result = result.replace(&token, &encode(value));
        }
    }
    Ok(result)
}

fn form_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Object(_) | Value::Array(_) => None,
    }
}

// MARK: - Answers

/// The value at a dot path with array indexes (`visits.data.0.date`).
pub fn value_at<'a>(path: Option<&str>, json: Option<&'a Value>) -> Option<&'a Value> {
    let path = path.filter(|path| !path.is_empty())?;
    let mut current = json?;
    for component in path.split('.') {
        current = match current {
            Value::Object(object) => object.get(component)?,
            Value::Array(items) => items.get(component.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    (!current.is_null()).then_some(current)
}

/// A string, or a number written out (YOURLS sends clicks as "12").
pub fn string_at(path: Option<&str>, json: Option<&Value>) -> Option<String> {
    match value_at(path, json)? {
        Value::String(text) => (!text.is_empty()).then(|| text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(if *flag { "1" } else { "0" }.into()),
        _ => None,
    }
}

pub fn int_at(path: Option<&str>, json: Option<&Value>) -> Option<i64> {
    match value_at(path, json)? {
        Value::Number(number) => number.as_i64().or_else(|| number.as_f64().map(|float| float as i64)),
        Value::String(text) => text.trim().parse().ok(),
        Value::Bool(flag) => Some(i64::from(*flag)),
        _ => None,
    }
}

/// Unix milliseconds from ISO 8601 with or without fractions, "2026-10-06
/// 12:00:00" (UTC), or epoch seconds or milliseconds.
pub fn date_at(path: Option<&str>, json: Option<&Value>) -> Option<i64> {
    match value_at(path, json)? {
        Value::Number(number) => {
            let value = number.as_f64()?;
            Some(if value > 1e12 { value as i64 } else { (value * 1000.0) as i64 })
        }
        Value::String(text) => parse_date(text),
        _ => None,
    }
}

pub fn parse_date(text: &str) -> Option<i64> {
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(date.timestamp_millis());
    }
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok().map(|date| date.and_utc().timestamp_millis())
}

/// Whether `actual` is `expected`; numbers compare by value, and a bool is
/// never a number.
fn matches(expected: &Value, actual: Option<&Value>) -> bool {
    match (expected, actual) {
        (Value::Null, None | Some(Value::Null)) => true,
        (Value::Number(expected), Some(Value::Number(actual))) => expected.as_f64() == actual.as_f64(),
        (Value::Array(expected), Some(Value::Array(actual))) => {
            expected.len() == actual.len() && expected.iter().zip(actual).all(|(expected, actual)| matches(expected, Some(actual)))
        }
        (Value::Object(expected), Some(Value::Object(actual))) => {
            expected.len() == actual.len() && expected.iter().all(|(key, value)| matches(value, actual.get(key)))
        }
        (expected, Some(actual)) => expected == actual,
        _ => false,
    }
}

/// A 2xx status or one of `success_statuses`, and, when the request has a
/// `success_path`, `success_value` (or `true`) there.
pub fn is_success(status: u16, json: Option<&Value>, request: &Request) -> bool {
    if !(200..300).contains(&status) && !request.success_statuses.as_ref().is_some_and(|statuses| statuses.contains(&status)) {
        return false;
    }
    let Some(path) = request.success_path.as_deref().filter(|path| !path.is_empty()) else { return true };
    let expected = request.success_value.clone().unwrap_or(Value::Bool(true));
    matches(&expected, value_at(Some(path), json))
}

/// The provider's message at the request's `error_path`, with the token
/// taken out in case it's echoed back.
pub fn error(status: u16, json: Option<&Value>, request: &Request, token: Option<&str>) -> ShortLinkError {
    let message = string_at(request.error_path.as_deref(), json).map(|message| redact(&message, token).chars().take(300).collect());
    ShortLinkError::Rejected { status, message }
}

pub fn redact(text: &str, token: Option<&str>) -> String {
    let Some(token) = token.filter(|token| token.chars().count() >= 4) else { return text.to_string() };
    text.replace(token, "\u{2022}\u{2022}\u{2022}").replace(&encode(token), "\u{2022}\u{2022}\u{2022}")
}

// MARK: - Operations

/// A short link the provider made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Created {
    pub short_url: String,
    /// The provider's id or code, for delete, update and stats; none when
    /// the definition doesn't say where it is.
    pub provider_id: Option<String>,
}

/// What Test did: nothing to show for a read-only test, otherwise the short
/// link it made and whether that was deleted again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub created: Option<Created>,
    pub cleanup: Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum Cleanup {
    /// Nothing to delete with (no delete request), or nothing made.
    None,
    Deleted,
    /// The message, without the token.
    Failed(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub clicks: Option<i64>,
    /// Unix milliseconds.
    pub last_click_at: Option<i64>,
}

/// Where a short link is, for the requests about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub provider_id: String,
    pub domain: Option<String>,
}

/// What a shortener can be asked; `Engine` does it over HTTP, and tests
/// use fakes.
pub trait Operations: Send + Sync {
    /// `Definition::id` (or "custom"): links made by another provider
    /// aren't this one's to change.
    fn provider(&self) -> &str;
    fn capabilities(&self) -> &Capabilities;
    fn create(&self, url: &str, expires_at: Option<i64>) -> impl Future<Output = Result<Created, ShortLinkError>> + Send;
    fn delete(&self, target: &Target) -> impl Future<Output = Result<(), ShortLinkError>> + Send;
    fn update(&self, target: &Target, url: &str) -> impl Future<Output = Result<(), ShortLinkError>> + Send;
    fn stats(&self, target: &Target) -> impl Future<Output = Result<Stats, ShortLinkError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

pub trait Transport: Send + Sync {
    /// The answer, or why there was none.
    fn send(&self, request: HttpRequest) -> impl Future<Output = Result<HttpResponse, String>> + Send;
}

/// A client that doesn't follow redirects (a redirect could carry the
/// token to another host), keeps no cookies, and gives up after 20 seconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpTransport;

impl Transport for HttpTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, String> {
        static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
        let client = CLIENT.get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default()
        });
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| error.to_string())?;
        let mut builder = client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        // The URL can carry the token (a query parameter), so it's left out.
        let response = builder.send().await.map_err(|error| error.without_url().to_string())?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|error| error.without_url().to_string())?;
        Ok(HttpResponse { status, body: body.to_vec() })
    }
}

/// One shortener over HTTP: a definition, the destination's settings and
/// its token.
#[derive(Debug, Clone)]
pub struct Engine<T: Transport = HttpTransport> {
    pub definition: Definition,
    pub settings: ShortLinkSettings,
    pub token: Option<String>,
    pub transport: T,
}

impl Engine<HttpTransport> {
    pub fn new(definition: Definition, settings: ShortLinkSettings, token: Option<String>) -> Self {
        Self { definition, settings, token, transport: HttpTransport }
    }
}

impl<T: Transport> Engine<T> {
    /// The Test button: the definition's read-only request. A custom
    /// definition has none, so it shortens https://getaktar.com/ and gives
    /// back the short link, then deletes it again when it has a delete
    /// request (which tests that too).
    pub async fn test(&self) -> Result<TestResult, ShortLinkError> {
        if let Some(request) = &self.definition.test {
            self.perform(request, &Values::new()).await?;
            return Ok(TestResult { created: None, cleanup: Cleanup::None });
        }
        let created = self.create(TEST_URL, None).await?;
        if self.definition.delete.is_none() || !self.definition.capabilities.delete {
            return Ok(TestResult { created: Some(created), cleanup: Cleanup::None });
        }
        let Some(id) = created.provider_id.clone() else {
            let message = ShortLinkError::MissingValue("id".into()).to_string();
            return Ok(TestResult { created: Some(created), cleanup: Cleanup::Failed(message) });
        };
        let cleanup = match self.delete(&Target { provider_id: id, domain: self.settings.trimmed_domain() }).await {
            Ok(()) => Cleanup::Deleted,
            Err(error) => Cleanup::Failed(redact(&error.to_string(), self.token.as_deref())),
        };
        Ok(TestResult { created: Some(created), cleanup })
    }

    fn values_for(target: &Target) -> Values {
        let mut values = Values::from([(Placeholder::Id, target.provider_id.clone())]);
        if let Some(domain) = target.domain.as_ref().filter(|domain| !domain.is_empty()) {
            values.insert(Placeholder::Domain, domain.clone());
        }
        values
    }

    /// Sends the request; the parsed JSON answer on success.
    async fn perform(&self, request: &Request, values: &Values) -> Result<Option<Value>, ShortLinkError> {
        let token = self.token.as_deref();
        let built = build(request, &self.definition, &self.settings, values, token)?;
        let response = self.transport.send(built).await.map_err(|message| ShortLinkError::Network(redact(&message, token)))?;
        let json: Option<Value> = if response.body.is_empty() { None } else { serde_json::from_slice(&response.body).ok() };
        if !is_success(response.status, json.as_ref(), request) {
            return Err(error(response.status, json.as_ref(), request, token));
        }
        Ok(json)
    }
}

/// What Test shortens for a custom definition.
pub const TEST_URL: &str = "https://getaktar.com/";

impl<T: Transport> Operations for Engine<T> {
    fn provider(&self) -> &str {
        &self.settings.provider_id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.definition.capabilities
    }

    async fn create(&self, url: &str, expires_at: Option<i64>) -> Result<Created, ShortLinkError> {
        let expires_at = expires_at.filter(|_| self.definition.capabilities.supports_expiration());
        let mut values = expiry_values(expires_at, crate::util::now_millis());
        values.insert(Placeholder::Url, url.to_string());
        let json = self.perform(&self.definition.create, &values).await?;
        let path = self.definition.create.short_url_path.clone().unwrap_or_default();
        let short_url = string_at(Some(&path), json.as_ref())
            // Only a web link: it's copied, pasted and opened as one.
            .filter(|short| url::Url::parse(short).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https")))
            .ok_or_else(|| ShortLinkError::NoShortLink(path.clone()))?;
        Ok(Created { short_url, provider_id: string_at(self.definition.create.id_path.as_deref(), json.as_ref()) })
    }

    async fn delete(&self, target: &Target) -> Result<(), ShortLinkError> {
        let request = self.definition.delete.as_ref().filter(|_| self.definition.capabilities.delete).ok_or(ShortLinkError::Unsupported)?;
        self.perform(request, &Self::values_for(target)).await.map(drop)
    }

    async fn update(&self, target: &Target, url: &str) -> Result<(), ShortLinkError> {
        let request =
            self.definition.update.as_ref().filter(|_| self.definition.capabilities.update_destination).ok_or(ShortLinkError::Unsupported)?;
        let mut values = Self::values_for(target);
        values.insert(Placeholder::Url, url.to_string());
        self.perform(request, &values).await.map(drop)
    }

    async fn stats(&self, target: &Target) -> Result<Stats, ShortLinkError> {
        let capabilities = self.definition.capabilities;
        let request = self.definition.stats.as_ref().filter(|_| capabilities.has_stats()).ok_or(ShortLinkError::Unsupported)?;
        let json = self.perform(request, &Self::values_for(target)).await?;
        Ok(Stats {
            clicks: capabilities.stats.clicks.then(|| int_at(request.clicks_path.as_deref(), json.as_ref())).flatten(),
            last_click_at: capabilities.stats.last_click.then(|| date_at(request.last_click_path.as_deref(), json.as_ref())).flatten(),
        })
    }
}
