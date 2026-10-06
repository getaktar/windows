//! A ShareX custom uploader (`.sxcu`) for a URL shortener, read into a
//! custom definition (the Mac repo's docs/short-links.md, "Custom HTTP and
//! .sxcu"). Only what maps onto the definition schema one to one is taken:
//! anything else refuses the import rather than being guessed at. Secrets
//! found in the headers, parameters and body move into the token, and the
//! definition says `{token}` there instead, so they never end up in the
//! destination's settings.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::definition::{BodyType, Capabilities, Definition, Kind, Request, CUSTOM_ID, CUSTOM_METHODS};
use crate::t;

/// Where a secret was found, for the consent screen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "camelCase")]
pub enum SecretLocation {
    Header(String),
    Query(String),
    Body(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareXImport {
    /// The configuration's own name, if it has one.
    pub name: Option<String>,
    /// A custom definition; its create path is the whole request URL
    /// without the query, which is in `query`.
    pub definition: Definition,
    /// The secret taken out of the configuration, if any.
    pub token: Option<String>,
    pub secret_locations: Vec<SecretLocation>,
    /// The host the token (and every link) is sent to.
    pub host: String,
    pub method: String,
    /// The request URL with its query, secrets as {token}.
    pub endpoint: String,
    #[serde(rename = "usesHTTP")]
    pub uses_http: bool,
    /// It has a DeletionURL that isn't a simple request, so links made with
    /// it can't be deleted from Aktar.
    pub deletion_skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareXImportError {
    Invalid,
    NotShortener,
    MissingRequestUrl,
    ResponseNotJson,
    Insecure,
    MultipleSecrets,
    /// What it uses, as ShareX writes it or described.
    Unsupported(String),
}

impl std::fmt::Display for ShareXImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ShareXImportError::Invalid => t!("This isn’t a ShareX custom uploader (.sxcu) file."),
            ShareXImportError::NotShortener => t!("This ShareX configuration isn’t a URL shortener."),
            ShareXImportError::MissingRequestUrl => t!("This ShareX configuration has no valid request URL."),
            ShareXImportError::ResponseNotJson => t!("Aktar can only read the short link as one value of a JSON answer, such as {json:link}."),
            ShareXImportError::Insecure => {
                t!("This configuration sends requests over http://. Turn on “Allow insecure HTTP” to import it anyway.")
            }
            ShareXImportError::MultipleSecrets => t!("This configuration has more than one secret. Aktar keeps one token per destination."),
            ShareXImportError::Unsupported(feature) => t!("Aktar can’t import this configuration: it uses {0}.", feature),
        };
        formatter.write_str(&text)
    }
}

/// Larger than any real configuration.
pub const MAX_FILE_SIZE: usize = 256 * 1024;

/// ShareX's syntax functions (`{name}` or `{name:argument}`, and `$name$`
/// in files from before ShareX 13). Other braces and dollar signs are just
/// text, as they are to ShareX.
const SYNTAX_NAMES: [&str; 17] = [
    "input", "json", "xml", "regex", "response", "responseurl", "header", "filename", "name", "random", "select", "prompt",
    "inputbox", "outputbox", "base64", "link", "file",
];

/// Reads a `.sxcu` file. `allow_insecure_http`: the user turned on "Allow
/// insecure HTTP"; without it an http:// request is refused.
pub fn parse(data: &[u8], allow_insecure_http: bool) -> Result<ShareXImport, ShareXImportError> {
    if data.len() > MAX_FILE_SIZE {
        return Err(ShareXImportError::Invalid);
    }
    let Ok(Value::Object(raw)) = serde_json::from_slice::<Value>(data) else { return Err(ShareXImportError::Invalid) };
    // ShareX writes these keys in PascalCase; case doesn't matter here.
    let root: Map<String, Value> = raw.into_iter().map(|(key, value)| (key.to_lowercase(), value)).collect();
    let text = |key: &str| root.get(key).and_then(Value::as_str);

    let types: Vec<String> = text("destinationtype").unwrap_or_default().split(',').map(|kind| kind.trim().to_lowercase()).collect();
    if !types.iter().any(|kind| kind == "urlshortener") {
        return Err(ShareXImportError::NotShortener);
    }
    if root.get("regexlist").and_then(Value::as_array).is_some_and(|list| !list.is_empty()) {
        return Err(ShareXImportError::Unsupported(t!("regular expressions")));
    }
    if text("fileformname").is_some_and(|field| !field.is_empty()) {
        return Err(ShareXImportError::Unsupported(t!("a file form field")));
    }
    if let Some(response_type) = text("responsetype").filter(|kind| !["", "text", "json"].contains(&kind.to_lowercase().as_str())) {
        return Err(ShareXImportError::Unsupported(response_type.to_string()));
    }

    let method = root.get("requestmethod").or_else(|| root.get("requesttype")).and_then(Value::as_str).unwrap_or("POST").to_uppercase();
    if !CUSTOM_METHODS.contains(&method.as_str()) {
        return Err(ShareXImportError::Unsupported(method));
    }

    // The request URL, with any query it carries as parameters.
    let raw_url = text("requesturl").map(str::trim).filter(|url| !url.is_empty()).ok_or(ShareXImportError::MissingRequestUrl)?;
    let mut base = map(raw_url)?;
    let mut query: BTreeMap<String, String> = BTreeMap::new();
    if let Some(mark) = base.find('?') {
        query.extend(form_pairs(&base[mark + 1..]));
        base.truncate(mark);
    }
    if let Some(hash) = base.find('#') {
        base.truncate(hash);
    }
    // A placeholder can be in the path, never in the host.
    let (scheme, host) = scheme_and_host(&base.replace("{url}", "aktarplaceholder")).ok_or(ShareXImportError::MissingRequestUrl)?;
    let uses_http = scheme == "http";
    if uses_http && !allow_insecure_http {
        return Err(ShareXImportError::Insecure);
    }

    for (name, value) in strings(root.get("parameters"), "Parameters")? {
        query.insert(name, map(&value)?);
    }
    let headers: BTreeMap<String, String> =
        strings(root.get("headers"), "Headers")?.into_iter().map(|(name, value)| Ok((name, map(&value)?))).collect::<Result<_, _>>()?;

    // The body.
    let arguments: BTreeMap<String, String> =
        strings(root.get("arguments"), "Arguments")?.into_iter().map(|(name, value)| Ok((name, map(&value)?))).collect::<Result<_, _>>()?;
    let data_text = text("data");
    let (body, body_type) = match text("body").unwrap_or_default().to_lowercase().as_str() {
        "json" => {
            let body = match data_text.filter(|data| !data.trim().is_empty()) {
                Some(data) => match serde_json::from_str::<Value>(data) {
                    Ok(value @ Value::Object(_)) => map_value(&value)?,
                    _ => return Err(ShareXImportError::Unsupported(t!("a JSON body that isn’t an object"))),
                },
                None => Value::Object(arguments.iter().map(|(name, value)| (name.clone(), Value::String(value.clone()))).collect()),
            };
            (Some(body), Some(BodyType::Json))
        }
        "formurlencoded" => {
            let mut fields = arguments.clone();
            if let Some(data) = data_text.filter(|data| !data.is_empty()) {
                for (name, value) in form_pairs(data) {
                    fields.insert(name, map(&value)?);
                }
            }
            (Some(Value::Object(fields.into_iter().map(|(name, value)| (name, Value::String(value))).collect())), Some(BodyType::Form))
        }
        "" | "none" => {
            // Older configurations sent their arguments in the query.
            query.extend(arguments.clone());
            (None, None)
        }
        _ => return Err(ShareXImportError::Unsupported(text("body").unwrap_or_default().to_string())),
    };

    // The answer.
    let short_url_path = response_path(text("url"))?.ok_or(ShareXImportError::ResponseNotJson)?;
    let error_path = response_path(text("errormessage")).ok().flatten();

    // Secrets out, {token} in.
    let mut extractor = SecretExtractor::default();
    let mut safe_headers = BTreeMap::new();
    for (name, value) in &headers {
        safe_headers.insert(name.clone(), extractor.take(value, name, SecretLocation::Header(name.clone()))?);
    }
    let mut safe_query = BTreeMap::new();
    for (name, value) in &query {
        safe_query.insert(name.clone(), extractor.take(value, name, SecretLocation::Query(name.clone()))?);
    }
    let safe_body = body.as_ref().map(|body| extractor.take_value(body, None)).transpose()?;

    // The deletion URL, when it's the same request for every link with the
    // link's id from the answer in it.
    let mut delete = None;
    let mut id_path = None;
    let mut deletion_skipped = false;
    if let Some(deletion) = text("deletionurl").map(str::trim).filter(|url| !url.is_empty()) {
        let mut attempt = extractor.clone();
        match simple_deletion(deletion, &mut attempt) {
            Ok((request, path)) => {
                extractor = attempt;
                delete = Some(request);
                id_path = Some(path);
            }
            Err(_) => deletion_skipped = true,
        }
    }

    let mut create = Request::new(&method, &base);
    create.query = (!safe_query.is_empty()).then(|| safe_query.clone());
    create.headers = (!safe_headers.is_empty()).then_some(safe_headers);
    create.body = safe_body;
    create.body_type = body_type;
    create.error_path = error_path;
    create.short_url_path = Some(short_url_path);
    create.id_path = id_path;
    let definition = Definition {
        id: CUSTOM_ID.into(),
        name: t!("Custom HTTP"),
        kind: Kind::Custom,
        base_url: None,
        needs_domain: false,
        auth: None,
        create,
        capabilities: Capabilities::custom(delete.is_some()),
        delete,
        update: None,
        stats: None,
        test: None,
    };
    let shown_query: Vec<String> = safe_query.iter().map(|(name, value)| format!("{name}={value}")).collect();
    let name = text("name").map(str::trim).filter(|name| !name.is_empty()).map(str::to_string);
    Ok(ShareXImport {
        name,
        definition,
        token: extractor.token,
        secret_locations: extractor.locations,
        host,
        method,
        endpoint: if shown_query.is_empty() { base } else { format!("{base}?{}", shown_query.join("&")) },
        uses_http,
        deletion_skipped,
    })
}

/// The URL's scheme (http or https) and host, which must be there.
fn scheme_and_host(url: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(url).ok()?;
    let scheme = parsed.scheme().to_string();
    let host = parsed.host_str()?.to_string();
    (["http", "https"].contains(&scheme.as_str()) && !host.is_empty() && !host.contains("aktarplaceholder")).then_some((scheme, host))
}

// MARK: - ShareX syntax

/// The ShareX syntax in `value`: each function as written, and its name.
pub fn syntax(value: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (open, close) in [('{', '}'), ('$', '$')] {
        let chars: Vec<char> = value.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            if chars[index] != open {
                index += 1;
                continue;
            }
            match function_at(&chars, index, open, close) {
                Some((end, name)) => {
                    if SYNTAX_NAMES.contains(&name.to_lowercase().as_str()) {
                        found.push((chars[index..=end].iter().collect(), name.to_lowercase()));
                    }
                    index = end + 1;
                }
                None => index += 1,
            }
        }
    }
    found
}

/// `{name}` or `{name:argument}` (with `$` for both delimiters in the old
/// syntax) starting at `start`: where it ends, and its name. The argument
/// can't hold a delimiter.
fn function_at(chars: &[char], start: usize, open: char, close: char) -> Option<(usize, String)> {
    let mut index = start + 1;
    while index < chars.len() && chars[index].is_ascii_alphabetic() {
        index += 1;
    }
    if index == start + 1 || index >= chars.len() {
        return None;
    }
    let name: String = chars[start + 1..index].iter().collect();
    if chars[index] == close {
        return Some((index, name));
    }
    if chars[index] != ':' {
        return None;
    }
    index += 1;
    while index < chars.len() {
        let c = chars[index];
        if c == close {
            return Some((index, name));
        }
        if c == open || (open == '{' && c == '}') {
            return None;
        }
        index += 1;
    }
    None
}

/// ShareX's input (`{input}`, or `$input$` in older files) as `{url}`. Any
/// other function of its syntax (`{filename}`, `{random}`, `{select}`,
/// `{prompt}`, `{response}`, `{regex}`, `{json}` in a request ...) means a
/// request Aktar can't make the same way.
pub fn map(value: &str) -> Result<String, ShareXImportError> {
    if let Some((token, _)) = syntax(value).into_iter().find(|(token, _)| !["{input}", "$input$"].contains(&token.to_lowercase().as_str())) {
        return Err(ShareXImportError::Unsupported(token));
    }
    Ok(replace_ignoring_case(&replace_ignoring_case(value, "{input}", "{url}"), "$input$", "{url}"))
}

fn replace_ignoring_case(text: &str, pattern: &str, replacement: &str) -> String {
    let lowered = text.to_lowercase();
    // Lowercasing can change lengths outside ASCII; then only exact matches.
    if lowered.len() != text.len() {
        return text.replace(pattern, replacement);
    }
    let mut result = String::new();
    let mut rest = 0;
    while let Some(found) = lowered[rest..].find(pattern) {
        result.push_str(&text[rest..rest + found]);
        result.push_str(replacement);
        rest += found + pattern.len();
    }
    result.push_str(&text[rest..]);
    result
}

/// "a=1&b=2", percent-decoded.
fn form_pairs(text: &str) -> Vec<(String, String)> {
    let decode = |part: &str| percent_encoding::percent_decode_str(part).decode_utf8().map(|decoded| decoded.into_owned()).unwrap_or_else(|_| part.to_string());
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').map_or((decode(pair), String::new()), |(name, value)| (decode(name), decode(value)));
            (!name.is_empty()).then_some((name, value))
        })
        .collect()
}

fn map_value(value: &Value) -> Result<Value, ShareXImportError> {
    Ok(match value {
        Value::String(text) => Value::String(map(text)?),
        Value::Object(fields) => Value::Object(fields.iter().map(|(key, field)| Ok((key.clone(), map_value(field)?))).collect::<Result<_, _>>()?),
        Value::Array(items) => Value::Array(items.iter().map(map_value).collect::<Result<_, _>>()?),
        other => other.clone(),
    })
}

/// `{json:data.link}` or `$json:data.link$` as `data.link`; none for none
/// at all. JSONPath's `$.` and `[0]` are read as dot paths. Any other
/// answer (plain text, a link put together around the value) can't be
/// read.
pub fn response_path(value: Option<&str>) -> Result<Option<String>, ShareXImportError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else { return Ok(None) };
    // {response}, {regex:...} and the like transform the answer.
    if let Some((token, _)) = syntax(value).into_iter().find(|(_, name)| name != "json") {
        return Err(ShareXImportError::Unsupported(token));
    }
    let prefix = value.get(..6).map(str::to_ascii_lowercase);
    let wrapped = match prefix.as_deref() {
        Some("{json:") => value.ends_with('}'),
        Some("$json:") => value.ends_with('$'),
        _ => false,
    };
    if !wrapped || value.len() <= 7 {
        return Err(ShareXImportError::ResponseNotJson);
    }
    let inner = &value[6..value.len() - 1];
    dot_path(inner).map(Some).ok_or(ShareXImportError::ResponseNotJson)
}

pub fn dot_path(json_path: &str) -> Option<String> {
    let mut path = json_path.trim();
    if let Some(rest) = path.strip_prefix("$.") {
        path = rest;
    } else if let Some(rest) = path.strip_prefix('$') {
        path = rest;
    }
    // [0] and ['key'] as .0 and .key.
    let chars: Vec<char> = path.chars().collect();
    let mut converted = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '[' {
            if let Some((end, inner)) = bracket_at(&chars, index) {
                converted.push('.');
                converted.push_str(&inner);
                index = end + 1;
                continue;
            }
        }
        converted.push(chars[index]);
        index += 1;
    }
    let path = converted.trim_start_matches('.');
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-');
    (!path.is_empty() && !path.contains("..") && path.chars().all(allowed)).then(|| path.to_string())
}

/// `[12]` or `['key']` at `start`: where it ends, and what's inside.
fn bracket_at(chars: &[char], start: usize) -> Option<(usize, String)> {
    let mut index = start + 1;
    let digits: String = chars[index..].iter().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        index += digits.chars().count();
        return (chars.get(index) == Some(&']')).then_some((index, digits));
    }
    if !matches!(chars.get(index), Some('\'' | '"')) {
        return None;
    }
    index += 1;
    let key: String = chars[index..].iter().take_while(|c| !matches!(c, '\'' | '"' | ']')).collect();
    index += key.chars().count();
    (!key.is_empty() && matches!(chars.get(index), Some('\'' | '"')) && chars.get(index + 1) == Some(&']')).then_some((index + 1, key))
}

/// A dictionary of strings (Headers, Parameters, Arguments); numbers and
/// booleans are written out.
fn strings(value: Option<&Value>, what: &str) -> Result<BTreeMap<String, String>, ShareXImportError> {
    let Some(value) = value.filter(|value| !value.is_null()) else { return Ok(BTreeMap::new()) };
    let Value::Object(object) = value else { return Err(ShareXImportError::Invalid) };
    object
        .iter()
        .map(|(key, item)| match item {
            Value::String(text) => Ok((key.clone(), text.clone())),
            Value::Number(number) => Ok((key.clone(), number.to_string())),
            Value::Bool(flag) => Ok((key.clone(), flag.to_string())),
            _ => Err(ShareXImportError::Unsupported(what.to_string())),
        })
        .collect()
}

/// `https://s.example.com/delete/{json:id}`: a GET (ShareX opens it in the
/// browser) to a fixed URL with one value of the answer in it, which
/// becomes the link's id. The headers aren't sent along.
fn simple_deletion(value: &str, extractor: &mut SecretExtractor) -> Result<(Request, String), ShareXImportError> {
    let matches = json_functions(value);
    let [(start, end, captured)] = matches.as_slice() else { return Err(ShareXImportError::Invalid) };
    let id_path = dot_path(captured).ok_or(ShareXImportError::Invalid)?;
    let mut template = format!("{}{{id}}{}", &value[..*start], &value[*end..]);
    map(&template.replace("{id}", ""))?;
    let mut query = BTreeMap::new();
    if let Some(mark) = template.find('?') {
        for (name, value) in form_pairs(&template[mark + 1..]) {
            let safe = extractor.take(&value, &name, SecretLocation::Query(name.clone()))?;
            query.insert(name, safe);
        }
        template.truncate(mark);
    }
    scheme_and_host(&template.replace("{id}", "aktarplaceholder")).ok_or(ShareXImportError::Invalid)?;
    let mut request = Request::new("GET", &template);
    request.query = (!query.is_empty()).then_some(query);
    Ok((request, id_path))
}

/// Each `{json:path}` or `$json:path$` (any case) in `value`: its byte
/// range and the path.
fn json_functions(value: &str) -> Vec<(usize, usize, String)> {
    let lowered = value.to_lowercase();
    if lowered.len() != value.len() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut index = 0;
    while index < value.len() {
        let rest = &lowered[index..];
        let (open, close) = if rest.starts_with("{json:") {
            ('{', '}')
        } else if rest.starts_with("$json:") {
            ('$', '$')
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let inner_start = index + 6;
        let inner_end = value[inner_start..]
            .char_indices()
            .find(|(_, c)| *c == close || (open == '{' && *c == '{'))
            .map(|(offset, c)| (inner_start + offset, c));
        match inner_end {
            Some((end, c)) if c == close && end > inner_start => {
                found.push((index, end + 1, value[inner_start..end].to_string()));
                index = end + 1;
            }
            _ => index += 1,
        }
    }
    found
}

/// Finds values that look like secrets and swaps them for `{token}`. A
/// definition has one token, so a second, different secret refuses the
/// import.
#[derive(Debug, Clone, Default)]
pub struct SecretExtractor {
    pub token: Option<String>,
    pub locations: Vec<SecretLocation>,
}

const SCHEMES: [&str; 4] = ["bearer ", "basic ", "token ", "bot "];

impl SecretExtractor {
    /// Whether a header, parameter or field called `name` holds a secret.
    pub fn is_secret_name(name: &str) -> bool {
        let name = name.to_lowercase();
        if ["token", "secret", "signature", "password", "passwd", "auth"].iter().any(|word| name.contains(word)) {
            return true;
        }
        name.contains("key") && !name.contains("keyword")
    }

    pub fn take(&mut self, value: &str, name: &str, location: SecretLocation) -> Result<String, ShareXImportError> {
        let trimmed = value.trim_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.contains("{url}") || trimmed.contains("{token}") {
            return Ok(value.to_string());
        }
        let lowered = trimmed.to_lowercase();
        let scheme = SCHEMES.iter().find(|scheme| lowered.starts_with(*scheme) && trimmed.chars().count() > scheme.len());
        if scheme.is_none() && !Self::is_secret_name(name) {
            return Ok(value.to_string());
        }
        let prefix: String = scheme.map(|scheme| trimmed.chars().take(scheme.len()).collect()).unwrap_or_default();
        let secret = trimmed.chars().skip(prefix.chars().count()).collect::<String>().trim_matches([' ', '\t']).to_string();
        if secret.is_empty() {
            return Ok(value.to_string());
        }
        if self.token.as_ref().is_some_and(|token| *token != secret) {
            return Err(ShareXImportError::MultipleSecrets);
        }
        self.token = Some(secret);
        if !self.locations.contains(&location) {
            self.locations.push(location);
        }
        Ok(format!("{prefix}{{token}}"))
    }

    /// The string values of a body, by their key (`data.apiKey` for a
    /// nested one).
    pub fn take_value(&mut self, value: &Value, path: Option<&str>) -> Result<Value, ShareXImportError> {
        Ok(match value {
            Value::String(text) => match path {
                None => value.clone(),
                Some(path) => {
                    let name = path.rsplit('.').next().unwrap_or(path);
                    Value::String(self.take(text, name, SecretLocation::Body(path.to_string()))?)
                }
            },
            Value::Object(fields) => {
                let mut result = Map::new();
                for (key, field) in fields {
                    let nested = path.map_or_else(|| key.clone(), |path| format!("{path}.{key}"));
                    result.insert(key.clone(), self.take_value(field, Some(&nested))?);
                }
                Value::Object(result)
            }
            Value::Array(items) => Value::Array(items.iter().map(|item| self.take_value(item, path)).collect::<Result<_, _>>()?),
            other => other.clone(),
        })
    }
}
