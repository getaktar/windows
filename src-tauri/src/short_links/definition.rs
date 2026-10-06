//! A link shortener described as HTTP configuration, not code: the built-in
//! ones come from `docs/short-link-providers.json` (the Mac repo's file,
//! copied as is; a test checks the two are the same), a custom one is made
//! in the destination form or imported from ShareX. The schema is
//! deliberately narrow; see the Mac repo's docs/short-links.md.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// `ShortLinkSettings::provider_id` of a custom definition.
pub const CUSTOM_ID: &str = "custom";
/// The methods a custom create request can use.
pub const CUSTOM_METHODS: [&str; 4] = ["POST", "GET", "PUT", "PATCH"];

/// The definitions file, shared with the Mac app.
const PROVIDERS_JSON: &str = include_str!("../../../docs/short-link-providers.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    SelfHosted,
    Hosted,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// None: requests go to the destination's endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub needs_domain: bool,
    /// None for a custom definition, whose templates use `{token}` wherever
    /// the service wants it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    pub create: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<Request>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<Request>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Request>,
    /// Authenticated and read-only. A custom definition has none: its test
    /// shortens a link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<Request>,
    pub capabilities: Capabilities,
}

#[cfg(test)]
impl Definition {
    pub fn is_hosted(&self) -> bool {
        self.kind == Kind::Hosted
    }

    pub fn uses_endpoint(&self) -> bool {
        self.base_url.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthType {
    /// The token as the value of the header `name`.
    Header,
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// The token as the query parameter `name`.
    Query,
    /// The token is "user:password", sent as HTTP basic auth.
    Basic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(rename = "type")]
    pub kind: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyType {
    Json,
    Form,
}

/// One request of a definition. Values are templates with the placeholders
/// of `engine::Placeholder`. The extra paths only mean something on the
/// request they belong to: `short_url_path`/`id_path` on create,
/// `clicks_path`/`last_click_path` on stats.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub method: String,
    /// Relative to the base URL, or a whole URL (Short.io's statistics are
    /// on another host). May carry a query string.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_type: Option<BodyType>,
    /// Where the provider's own error message is in an error response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_path: Option<String>,
    /// Statuses outside 200-299 that still mean success, such as YOURLS's
    /// 409 for a URL it has already shortened, which comes with that link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_statuses: Option<Vec<u16>>,
    /// A field of the answer that says whether it worked, for providers
    /// that answer 200 to a failure (Short.io's delete answers
    /// `{"success": false, "error": ...}`). With it, the request succeeds
    /// only when the value there equals `success_value` (`true` when that's
    /// left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_url_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clicks_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_click_path: Option<String>,
}

impl Request {
    pub fn new(method: &str, path: &str) -> Self {
        Self { method: method.to_string(), path: path.to_string(), ..Self::default() }
    }
}

/// `false` in the JSON is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiration {
    Absolute,
    Relative,
    #[default]
    None,
}

impl Serialize for Expiration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Expiration::Absolute => serializer.serialize_str("absolute"),
            Expiration::Relative => serializer.serialize_str("relative"),
            Expiration::None => serializer.serialize_bool(false),
        }
    }
}

impl<'de> Deserialize<'de> for Expiration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(deserializer)?.as_str() {
            Some("absolute") => Expiration::Absolute,
            Some("relative") => Expiration::Relative,
            _ => Expiration::None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsCapabilities {
    pub clicks: bool,
    pub last_click: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub delete: bool,
    pub update_destination: bool,
    #[serde(default)]
    pub expiration: Expiration,
    #[serde(default)]
    pub custom_code: bool,
    #[serde(default)]
    pub custom_domain: bool,
    #[serde(default)]
    pub stats: StatsCapabilities,
}

impl Capabilities {
    pub fn supports_expiration(&self) -> bool {
        self.expiration != Expiration::None
    }

    pub fn has_stats(&self) -> bool {
        self.stats.clicks || self.stats.last_click
    }

    /// A custom definition's: delete when it has a delete request, nothing
    /// else.
    pub fn custom(can_delete: bool) -> Self {
        Self {
            delete: can_delete,
            update_destination: false,
            expiration: Expiration::None,
            custom_code: false,
            custom_domain: false,
            stats: StatsCapabilities::default(),
        }
    }
}

/// `DestinationConfig::short_links`: a destination's shortener; none is
/// off. The token is in Credential Manager with the destination's keys
/// (`StorageCredentials::short_link_token`), never here. Same keys as the
/// Mac app's destinations, so Share to Another Device carries it as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLinkSettings {
    /// A built-in definition's id, or `CUSTOM_ID`.
    pub provider_id: String,
    /// The base URL of a self-hosted or custom shortener; ignored for
    /// hosted ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The short domain; required for Short.io, optional elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The definition when `provider_id` is custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<Definition>,
    /// Shorten only links longer than this many characters; 0 always.
    #[serde(default, deserialize_with = "non_negative")]
    pub only_longer_than: i64,
    /// Temporary links are shortened too (only with providers that can
    /// expire a link), and the short link expires with them.
    #[serde(default)]
    pub shorten_temporary_links: bool,
    /// The user chose to send requests to an http:// endpoint.
    #[serde(default, rename = "allowInsecureHTTP")]
    pub allow_insecure_http: bool,
}

fn non_negative<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    Ok(i64::deserialize(deserializer)?.max(0))
}

impl ShortLinkSettings {
    #[cfg(test)]
    pub fn new(provider_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            endpoint: None,
            domain: None,
            custom: None,
            only_longer_than: 0,
            shorten_temporary_links: false,
            allow_insecure_http: false,
        }
    }

    /// The definition these settings use, none for an unknown provider.
    pub fn definition(&self) -> Option<&Definition> {
        if self.provider_id == CUSTOM_ID {
            self.custom.as_ref()
        } else {
            definition(&self.provider_id)
        }
    }

    pub fn trimmed_domain(&self) -> Option<String> {
        self.domain.as_deref().map(str::trim).filter(|domain| !domain.is_empty()).map(str::to_string)
    }
}

/// Settings from destinations.json or a transfer: any that can't be read,
/// or name a provider this app doesn't know, are none (short links off),
/// rather than failing the whole destination.
pub fn lenient<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ShortLinkSettings>, D::Error> {
    Ok(from_value(&Value::deserialize(deserializer)?))
}

pub fn from_value(value: &Value) -> Option<ShortLinkSettings> {
    let settings: ShortLinkSettings = serde_json::from_value(value.clone()).ok()?;
    settings.definition().is_some().then_some(settings)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvidersFile {
    #[allow(dead_code)]
    schema_version: u32,
    providers: Vec<Definition>,
}

/// The built-in definitions, in the order the destination form lists them.
pub fn built_in() -> &'static [Definition] {
    static PROVIDERS: OnceLock<Vec<Definition>> = OnceLock::new();
    PROVIDERS.get_or_init(|| decode(PROVIDERS_JSON).unwrap_or_default())
}

pub fn decode(json: &str) -> serde_json::Result<Vec<Definition>> {
    Ok(serde_json::from_str::<ProvidersFile>(json)?.providers)
}

pub fn definition(id: &str) -> Option<&'static Definition> {
    built_in().iter().find(|definition| definition.id == id)
}

/// What a new custom definition starts as: a JSON POST with the link and a
/// bearer token.
pub fn custom_template() -> Definition {
    let mut create = Request::new("POST", "/api/shorten");
    create.headers = Some(BTreeMap::from([("Authorization".to_string(), "Bearer {token}".to_string())]));
    create.body = Some(serde_json::json!({ "url": "{url}" }));
    create.body_type = Some(BodyType::Json);
    create.short_url_path = Some("shortUrl".into());
    Definition {
        id: CUSTOM_ID.into(),
        name: crate::t!("Custom HTTP"),
        kind: Kind::Custom,
        base_url: None,
        needs_domain: false,
        auth: None,
        create,
        delete: None,
        update: None,
        stats: None,
        test: None,
        capabilities: Capabilities::custom(false),
    }
}
