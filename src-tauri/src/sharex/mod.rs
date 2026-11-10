//! "Import from ShareX" (the Mac repo's docs/sharex-import.md): ShareX's own
//! settings become Aktar destinations, its URL-shortener custom uploaders
//! short link settings (through the `.sxcu` import), and its upload history
//! Aktar's history. Ported from ShareX's `develop` branch:
//! `PersonalPathManager.cs`, `SystemOptions.cs`, `SettingManager.cs`,
//! `AppPaths.cs`, `AmazonS3.cs`, `BackblazeB2.cs`, `UploadersConfig.cs`,
//! `DPAPIEncryptedStringValueProvider.cs` and `HistoryManagerSQLite.cs`.
//!
//! Secrets are never logged, and go straight to Credential Manager.

pub mod commands;
mod platform;

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::destinations::{DestinationConfig, ProviderPreset};
use crate::output::DEFAULT_OBJECT_PATH_TEMPLATE;
use crate::short_links::sharex::ShareXImport;
use crate::t;

/// The largest settings file (or backup) read.
pub const MAX_CONFIG_BYTES: u64 = 5 * 1024 * 1024;
/// ApplicationConfig.json is only read for two settings.
const MAX_APPLICATION_CONFIG_BYTES: u64 = 20 * 1024 * 1024;
const DPAPI_TAG: &str = "$DPAPIEncrypted$";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\ShareX_is1";
const SYSTEM_OPTIONS_KEY: &str = r"SOFTWARE\ShareX";
const CONFIG_FILE: &str = "UploadersConfig.json";
/// ShareX's default `ObjectPrefix` and `B2UploadPath`.
const DEFAULT_PREFIX: &str = "ShareX/%y/%mo";

// MARK: - Finding ShareX's folder

/// Where ShareX keeps things on this PC, as found in Windows (or made up
/// by a test).
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// Folders ShareX.exe may be in, most likely first.
    pub exe_folders: Vec<PathBuf>,
    /// `SystemOptions.PersonalPath`: HKLM's, else HKCU's.
    pub registry_personal_path: Option<String>,
    pub documents: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
}

impl Places {
    pub fn of_this_pc() -> Self {
        let mut exe_folders: Vec<PathBuf> = Vec::new();
        for local_machine in [false, true] {
            if let Some(location) = platform::registry_value(local_machine, UNINSTALL_KEY, "InstallLocation") {
                exe_folders.push(PathBuf::from(location.trim()));
            }
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            exe_folders.push(PathBuf::from(program_files).join("ShareX"));
        }
        exe_folders.dedup();
        let registry_personal_path = platform::registry_value(true, SYSTEM_OPTIONS_KEY, "PersonalPath")
            .or_else(|| platform::registry_value(false, SYSTEM_OPTIONS_KEY, "PersonalPath"));
        let documents = platform::documents_folder().or_else(|| std::env::var_os("USERPROFILE").map(|profile| PathBuf::from(profile).join("Documents")));
        Self { exe_folders, registry_personal_path, documents, local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from) }
    }

    /// The folder ShareX.exe is in, if it's in one of them.
    fn exe_folder(&self) -> Option<&Path> {
        self.exe_folders.iter().find(|folder| folder.join("ShareX.exe").is_file()).map(PathBuf::as_path)
    }
}

/// ShareX's personal folder, when it exists, in ShareX's own order
/// (`PersonalPathManager.Initialize`): a portable install's `ShareX`
/// folder, the registry's PersonalPath, PersonalPath.cfg (next to the exe,
/// in Documents\ShareX, or the older one in %LOCALAPPDATA%\ShareX), and
/// Documents\ShareX.
pub fn personal_folder(places: &Places) -> Option<PathBuf> {
    let exe = places.exe_folder();
    let resolve = |raw: &str| {
        let expanded = expand_folder_variables(raw.trim(), places);
        match exe {
            Some(exe) if !is_rooted(&expanded) => exe.join(expanded),
            _ => PathBuf::from(expanded),
        }
    };
    if let Some(exe) = exe.filter(|exe| exe.join("Portable").is_file()) {
        return existing_folder(exe.join("ShareX"));
    }
    if let Some(path) = places.registry_personal_path.as_deref().filter(|path| !path.trim().is_empty()) {
        return existing_folder(resolve(path));
    }
    let configs = [
        exe.map(|exe| exe.join("PersonalPath.cfg")),
        places.documents.as_ref().map(|documents| documents.join("ShareX").join("PersonalPath.cfg")),
        places.local_app_data.as_ref().map(|local| local.join("ShareX").join("PersonalPath.cfg")),
    ];
    for config in configs.into_iter().flatten() {
        let Ok(text) = read_text(&config, 64 * 1024) else { continue };
        if !text.trim().is_empty() {
            return existing_folder(resolve(&text));
        }
    }
    places.documents.as_ref().map(|documents| documents.join("ShareX")).and_then(existing_folder)
}

fn existing_folder(path: PathBuf) -> Option<PathBuf> {
    path.is_dir().then_some(path)
}

/// A drive (`C:\`), UNC (`\\server`) or absolute path.
fn is_rooted(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('\\') || path.starts_with('/') || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// `FileHelpers.ExpandFolderVariables`: `%MyDocuments%` and the other
/// special folder names, then environment variables.
fn expand_folder_variables(text: &str, places: &Places) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            expanded.push_str(&rest[start..]);
            rest = "";
            break;
        };
        match folder_variable(&after[..end], places) {
            Some(value) => {
                expanded.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                expanded.push('%');
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    expanded
}

fn folder_variable(name: &str, places: &Places) -> Option<String> {
    let variable = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
    let profile = |folder: &str| variable("USERPROFILE").map(|profile| Path::new(&profile).join(folder).to_string_lossy().into_owned());
    match name.to_ascii_lowercase().as_str() {
        "" => None,
        "mydocuments" | "personal" => places.documents.as_ref().map(|documents| documents.to_string_lossy().into_owned()),
        "desktop" | "desktopdirectory" => profile("Desktop"),
        "mypictures" => profile("Pictures"),
        "myvideos" => profile("Videos"),
        "mymusic" => profile("Music"),
        "userprofile" => variable("USERPROFILE"),
        "applicationdata" => variable("APPDATA"),
        "localapplicationdata" => places.local_app_data.as_ref().map(|local| local.to_string_lossy().into_owned()).or_else(|| variable("LOCALAPPDATA")),
        "programfiles" => variable("ProgramFiles"),
        "programfilesx86" => variable("ProgramFiles(x86)"),
        "commonapplicationdata" => variable("ProgramData"),
        _ => variable(name),
    }
}

/// A small text file, read while ShareX may have it open, without a BOM.
fn read_text(path: &Path, max_bytes: u64) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > max_bytes {
        return Err(t!("This file is too large to be ShareX’s settings."));
    }
    let mut data = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut data).map_err(|error| error.to_string())?;
    Ok(text_without_bom(&data))
}

fn text_without_bom(data: &[u8]) -> String {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    String::from_utf8_lossy(data).into_owned()
}

/// What's in a ShareX personal folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub folder: PathBuf,
    /// The uploaders' settings, which may be in another folder.
    pub uploaders_config: PathBuf,
    /// History.db, else History.json; none when neither is there.
    pub history: Option<PathBuf>,
}

/// The settings and history in ShareX's personal folder: UploadersConfig
/// from `CustomUploadersConfigPath` when that folder has it, the
/// machine-specific `UploadersConfig-<machine>.json` when it exists
/// (unless ApplicationConfig turned it off), else `UploadersConfig.json`.
pub fn locate(folder: &Path, machine_name: Option<&str>, places: &Places) -> Option<Located> {
    let application: Value = read_text(&folder.join("ApplicationConfig.json"), MAX_APPLICATION_CONFIG_BYTES)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let machine_specific = application.get("UseMachineSpecificUploadersConfig").and_then(Value::as_bool);
    let config_in = |directory: &Path| -> Option<PathBuf> {
        let machine = machine_name
            .map(|name| sanitize_file_name(&name.to_lowercase()))
            .filter(|name| !name.is_empty() && machine_specific != Some(false))
            .map(|name| directory.join(format!("UploadersConfig-{name}.json")))
            .filter(|path| path.is_file());
        machine.or_else(|| Some(directory.join(CONFIG_FILE)).filter(|path| path.is_file()))
    };
    let custom = application
        .get("CustomUploadersConfigPath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(expand_folder_variables(path, places)));
    let uploaders_config = custom.as_deref().and_then(config_in).or_else(|| config_in(folder))?;
    let history = ["History.db", "History.json"].iter().map(|name| folder.join(name)).find(|path| path.is_file());
    Some(Located { folder: folder.to_path_buf(), uploaders_config, history })
}

/// `FileHelpers.SanitizeFileName`: trimmed, without the characters Windows
/// doesn't allow in a file name.
pub fn sanitize_file_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '\\' | '/')).collect()
}

/// The text of a picked `UploadersConfig.json`, or of the one at the root
/// of a `.sxb` backup (a ZIP).
pub fn read_picked(path: &Path) -> Result<String, String> {
    let invalid = || t!("This isn’t ShareX’s UploadersConfig.json or a ShareX backup (.sxb).");
    let is_backup = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("sxb"));
    if !is_backup {
        return read_text(path, MAX_CONFIG_BYTES);
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > 64 * MAX_CONFIG_BYTES {
        return Err(invalid());
    }
    let mut archive = zip::ZipArchive::new(file).map_err(|_| invalid())?;
    let name = archive.file_names().find(|name| name.eq_ignore_ascii_case(CONFIG_FILE)).map(str::to_string).ok_or_else(invalid)?;
    let entry = archive.by_name(&name).map_err(|_| invalid())?;
    if entry.size() > MAX_CONFIG_BYTES {
        return Err(t!("This file is too large to be ShareX’s settings."));
    }
    let mut data = Vec::new();
    entry.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut data).map_err(|_| invalid())?;
    Ok(text_without_bom(&data))
}

// MARK: - Destinations

/// A secret key as ShareX keeps it.
#[derive(Clone, PartialEq, Eq)]
pub enum Secret {
    Known(String),
    /// `$DPAPIEncrypted$` that can't be decrypted here (another user's or
    /// PC's file).
    Unreadable,
    Missing,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Secret::Known(_) => "Known(..)",
            Secret::Unreadable => "Unreadable",
            Secret::Missing => "Missing",
        })
    }
}

/// A `[JsonEncrypt]` field: plain text in older files, else
/// `$DPAPIEncrypted$` + base64 of `CryptProtectData`'s output.
pub fn read_secret(value: Option<&Value>, unprotect: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Secret {
    use base64::Engine;
    let Some(text) = value.and_then(Value::as_str).filter(|text| !text.is_empty()) else { return Secret::Missing };
    let Some(encoded) = text.strip_prefix(DPAPI_TAG) else { return Secret::Known(text.trim().to_string()) };
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()
        .and_then(|sealed| unprotect(&sealed))
        .and_then(|plain| String::from_utf8(plain).ok())
        .map(|plain| plain.trim().to_string())
        .filter(|plain| !plain.is_empty())
        .map_or(Secret::Unreadable, Secret::Known)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UploaderKind {
    AmazonS3,
    BackblazeB2,
}

/// A destination found in ShareX's settings, before the review.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub review_id: String,
    pub kind: UploaderKind,
    /// A new ID; the endpoint is empty for B2 until `b2_authorize_account`
    /// answered.
    pub config: DestinationConfig,
    pub access_key_id: String,
    pub secret: Secret,
    /// ShareX's base URL for its uploads here (`GetObjectBaseURL`), which
    /// history entries are matched against; none when it can't be known.
    pub sharex_base_url: Option<String>,
    pub notes: Vec<String>,
}

/// Something in ShareX's settings that isn't imported, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

/// A URL shortener custom uploader, read like a `.sxcu` file.
#[derive(Debug, Clone)]
pub struct ShortenerCandidate {
    pub review_id: String,
    pub import: ShareXImport,
}

/// Everything found in an UploadersConfig.json.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub destinations: Vec<Candidate>,
    pub shorteners: Vec<ShortenerCandidate>,
    pub skipped: Vec<Skipped>,
}

/// Reads an UploadersConfig.json. `unprotect` is DPAPI on Windows.
pub fn parse_uploaders_config(text: &str, unprotect: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Result<Parsed, String> {
    let root: Value = serde_json::from_str(text).map_err(|_| t!("This isn’t ShareX’s UploadersConfig.json or a ShareX backup (.sxb)."))?;
    let root = root.as_object().ok_or_else(|| t!("This isn’t ShareX’s UploadersConfig.json or a ShareX backup (.sxb)."))?;
    let mut parsed = Parsed::default();
    if let Some(candidate) = root.get("AmazonS3Settings").and_then(|settings| s3_candidate(settings, unprotect)) {
        parsed.destinations.push(candidate);
    }
    if let Some(candidate) = b2_candidate(root, unprotect) {
        parsed.destinations.push(candidate);
    }
    let not_storage = t!("Aktar uploads only to S3-compatible storage.");
    let configured = |key: &str| root.get(key).is_some_and(|value| value.is_object() || value.as_array().is_some_and(|list| !list.is_empty()));
    for (key, name) in [
        ("FTPAccountList", "FTP"),
        ("ImgurOAuth2Info", "Imgur"),
        ("FlickrOAuthInfo", "Flickr"),
        ("GistOAuth2Info", "GitHub Gist"),
        ("DropboxOAuth2Info", "Dropbox"),
        ("OneDriveV2OAuth2Info", "OneDrive"),
        ("GoogleDriveOAuth2Info", "Google Drive"),
        ("BoxOAuth2Info", "Box"),
        ("GoogleCloudStorageOAuth2Info", "Google Cloud Storage"),
        ("LocalhostAccountList", "Shared folder"),
    ] {
        if configured(key) {
            parsed.skipped.push(Skipped { name: name.into(), reason: not_storage.clone() });
        }
    }
    for (key, name) in [("OwnCloudHost", "ownCloud / Nextcloud"), ("SeafileAPIURL", "Seafile")] {
        if root.get(key).and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()) {
            parsed.skipped.push(Skipped { name: name.into(), reason: not_storage.clone() });
        }
    }
    for (index, uploader) in root.get("CustomUploadersList").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let name = uploader.get("Name").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).map_or_else(|| t!("Custom uploader {0}", index + 1), str::to_string);
        if !is_shortener(uploader) {
            parsed.skipped.push(Skipped { name, reason: t!("Custom uploaders to other web services aren’t supported.") });
            continue;
        }
        let mut uploader = uploader.clone();
        if let Some(object) = uploader.as_object_mut() {
            // A number (flags) in some versions; the .sxcu reader wants names.
            object.insert("DestinationType".into(), Value::String("URLShortener".into()));
        }
        let data = serde_json::to_vec(&uploader).unwrap_or_default();
        match crate::short_links::sharex::parse(&data, true) {
            Ok(import) => parsed.shorteners.push(ShortenerCandidate { review_id: format!("shortener-{index}"), import }),
            Err(error) => parsed.skipped.push(Skipped { name, reason: error.to_string() }),
        }
    }
    Ok(parsed)
}

/// `CustomUploaderDestinationType` includes `URLShortener`: a name in a
/// comma-separated list, or the flag 8.
fn is_shortener(uploader: &Value) -> bool {
    match uploader.get("DestinationType") {
        Some(Value::String(types)) => types.split(',').any(|kind| kind.trim().eq_ignore_ascii_case("URLShortener")),
        Some(Value::Number(flags)) => flags.as_u64().is_some_and(|flags| flags & 8 != 0),
        _ => false,
    }
}

fn text<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

fn flag(object: &Value, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `URLHelpers.FixPrefix`: https:// when there's no scheme.
fn with_scheme(url: &str) -> String {
    let url = url.trim();
    if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

/// `URLHelpers.CombineURL` of two parts.
fn combine_url(left: &str, right: &str) -> String {
    match (left.trim_end_matches('/'), right.trim_start_matches('/')) {
        ("", right) => right.to_string(),
        (left, "") => left.to_string(),
        (left, right) => format!("{left}/{right}"),
    }
}

/// `AmazonS3.GetRegion` for an empty Region: `s3.<region>.amazonaws.com`
/// or `s3-<region>.amazonaws.com`, else us-east-1.
pub fn region_from_endpoint(endpoint: &str) -> String {
    const DEFAULT: &str = "us-east-1";
    let mut url = endpoint;
    if let Some(index) = url.find("//") {
        url = &url[index + 2..];
    }
    let url = url.strip_suffix('/').unwrap_or(url);
    let Some(aws) = url.find(".amazonaws.com") else { return DEFAULT.into() };
    let service_and_region = &url[..aws];
    let service_and_region = match service_and_region.strip_prefix("s3-") {
        Some(rest) => format!("s3.{rest}"),
        None => service_and_region.to_string(),
    };
    match service_and_region.rfind('.') {
        Some(separator) => service_and_region[separator + 1..].to_string(),
        None => DEFAULT.into(),
    }
}

/// The region in a B2 S3 endpoint (`s3.<region>.backblazeb2.com`).
fn b2_region(host: &str) -> Option<String> {
    let rest = host.strip_prefix("s3.")?.strip_suffix(".backblazeb2.com")?;
    (!rest.is_empty() && !rest.contains('.')).then(|| rest.to_string())
}

fn host_of(endpoint: &str) -> String {
    url::Url::parse(endpoint).ok().and_then(|url| url.host_str().map(str::to_ascii_lowercase)).unwrap_or_default()
}

/// The provider a ShareX endpoint is on.
pub fn preset_for(host: &str) -> ProviderPreset {
    let is = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if host.ends_with(".r2.cloudflarestorage.com") {
        ProviderPreset::CloudflareR2
    } else if b2_region(host).is_some() {
        ProviderPreset::BackblazeB2
    } else if is("digitaloceanspaces.com") {
        ProviderPreset::DigitalOceanSpaces
    } else if is("amazonaws.com") {
        ProviderPreset::AmazonS3
    } else {
        ProviderPreset::CustomS3
    }
}

fn new_destination(name: String, preset: ProviderPreset) -> DestinationConfig {
    DestinationConfig {
        id: crate::util::new_id(),
        name,
        preset,
        account_id: None,
        endpoint: String::new(),
        region: String::new(),
        bucket: String::new(),
        public_base_url: String::new(),
        object_path_template: DEFAULT_OBJECT_PATH_TEMPLATE.into(),
        force_path_style: false,
        is_default: false,
        output_mode: None,
        expiry_days: None,
        temporary_link: None,
        image_metadata: None,
        folder_upload: None,
        image_processing: None,
        thumbnails: None,
        thumbnail_prefix: None,
        use_for: None,
        short_cache: None,
        cloudflare_zone_id: None,
        hooks: None,
        short_links: None,
        share_page: None,
        encryption: None,
        request_cors_origin: None,
    }
}

/// A custom domain or URL with ShareX syntax in it (`$prompt$`, `{...}`)
/// can't be a fixed base URL.
fn has_sharex_syntax(url: &str) -> bool {
    url.contains('$') || url.contains('{')
}

/// The template notes for a review, and the template.
fn template_with_notes(prefix: &str, notes: &mut Vec<String>) -> String {
    let (template, dropped) = convert_prefix(prefix);
    if !dropped.is_empty() {
        notes.push(t!("ShareX’s path {0} uses {1}, which Aktar doesn’t have, so Aktar’s default path is used.", prefix, dropped.join(" ")));
    }
    template
}

fn secret_notes(secret: &Secret, notes: &mut Vec<String>) {
    if *secret == Secret::Unreadable {
        notes.push(t!("ShareX encrypted this key for another Windows user or PC. Paste the secret key from your provider."));
    }
}

/// The Amazon S3 uploader, when `AccessKeyID`, `Endpoint` and `Bucket`
/// are set.
fn s3_candidate(settings: &Value, unprotect: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Option<Candidate> {
    let access_key_id = text(settings, "AccessKeyID");
    let raw_endpoint = text(settings, "Endpoint");
    let bucket = text(settings, "Bucket");
    if access_key_id.is_empty() || raw_endpoint.is_empty() || bucket.is_empty() {
        return None;
    }
    let endpoint = with_scheme(raw_endpoint).trim_end_matches('/').to_string();
    let host = host_of(&endpoint);
    let preset = preset_for(&host);
    let mut notes = Vec::new();
    let mut config = new_destination(format!("ShareX: {bucket}"), preset);
    config.region = match text(settings, "Region") {
        "" if preset == ProviderPreset::CloudflareR2 => "auto".into(),
        // B2's S3 API signs with its own region.
        "" if preset == ProviderPreset::BackblazeB2 => b2_region(&host).unwrap_or_else(|| "us-east-1".into()),
        "" => region_from_endpoint(raw_endpoint),
        region => region.to_string(),
    };
    if preset == ProviderPreset::CloudflareR2 {
        config.account_id = host.split('.').next().map(str::to_string).filter(|account| !account.is_empty());
    }
    config.endpoint = endpoint;
    config.bucket = bucket.to_string();
    config.force_path_style = flag(settings, "UsePathStyle");
    // `GetObjectBaseURL`.
    let custom_domain = text(settings, "CustomDomain");
    let sharex_base_url = if flag(settings, "UseCustomCNAME") && !custom_domain.is_empty() {
        if has_sharex_syntax(custom_domain) {
            notes.push(t!("The custom domain {0} uses ShareX syntax. Enter the public URL in the destination’s settings.", custom_domain));
            None
        } else {
            Some(with_scheme(custom_domain).trim_end_matches('/').to_string())
        }
    } else {
        Some(with_scheme(&combine_url(raw_endpoint, bucket)))
    };
    config.public_base_url = sharex_base_url.clone().unwrap_or_default();
    let prefix = settings.get("ObjectPrefix").and_then(Value::as_str).unwrap_or(DEFAULT_PREFIX);
    config.object_path_template = template_with_notes(prefix, &mut notes);
    if ["RemoveExtensionImage", "RemoveExtensionVideo", "RemoveExtensionText"].iter().any(|key| flag(settings, key)) {
        notes.push(t!("ShareX removed file extensions here; Aktar keeps them."));
    }
    let secret = read_secret(settings.get("SecretAccessKey"), unprotect);
    secret_notes(&secret, &mut notes);
    Some(Candidate {
        review_id: "s3".into(),
        kind: UploaderKind::AmazonS3,
        config,
        access_key_id: access_key_id.to_string(),
        secret,
        sharex_base_url,
        notes,
    })
}

/// The Backblaze B2 uploader, when its key ID and bucket name are set. Its
/// S3 endpoint (and the public URL, without a custom one) come from
/// `b2_authorize_account` (`apply_b2_authorization`).
fn b2_candidate(root: &serde_json::Map<String, Value>, unprotect: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Option<Candidate> {
    let settings = Value::Object(root.clone());
    let key_id = text(&settings, "B2ApplicationKeyId");
    let bucket = text(&settings, "B2BucketName");
    if key_id.is_empty() || bucket.is_empty() {
        return None;
    }
    let mut notes = Vec::new();
    let mut config = new_destination(format!("ShareX: {bucket} (B2)"), ProviderPreset::BackblazeB2);
    config.bucket = bucket.to_string();
    let custom_url = text(&settings, "B2CustomUrl");
    let mut sharex_base_url = None;
    if flag(&settings, "B2UseCustomUrl") && !custom_url.is_empty() {
        if has_sharex_syntax(custom_url) {
            notes.push(t!("The custom domain {0} uses ShareX syntax. Enter the public URL in the destination’s settings.", custom_url));
        } else {
            sharex_base_url = Some(with_scheme(custom_url).trim_end_matches('/').to_string());
            config.public_base_url = sharex_base_url.clone().unwrap_or_default();
        }
    }
    let prefix = settings.get("B2UploadPath").and_then(Value::as_str).unwrap_or(DEFAULT_PREFIX);
    config.object_path_template = template_with_notes(prefix, &mut notes);
    let secret = read_secret(settings.get("B2ApplicationKey"), unprotect);
    secret_notes(&secret, &mut notes);
    Some(Candidate {
        review_id: "b2".into(),
        kind: UploaderKind::BackblazeB2,
        config,
        access_key_id: key_id.to_string(),
        secret,
        sharex_base_url,
        notes,
    })
}

/// What `b2_authorize_account` (v3) says about the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct B2Authorization {
    /// `https://s3.<region>.backblazeb2.com`.
    pub s3_api_url: String,
    /// `https://f<NNN>.backblazeb2.com`.
    pub download_url: String,
}

pub fn parse_b2_authorization(body: &Value) -> Option<B2Authorization> {
    let storage = body.get("apiInfo")?.get("storageApi")?;
    let s3_api_url = storage.get("s3ApiUrl")?.as_str()?.trim().trim_end_matches('/').to_string();
    let download_url = storage.get("downloadUrl").and_then(Value::as_str).unwrap_or_default().trim().trim_end_matches('/').to_string();
    (!s3_api_url.is_empty()).then_some(B2Authorization { s3_api_url, download_url })
}

/// Fills in a B2 destination from its account's authorization: the S3
/// endpoint and region, and the public URL ShareX uses without a custom one
/// (`<downloadUrl>/file/<bucket>`).
pub fn apply_b2_authorization(candidate: &mut Candidate, authorization: &B2Authorization) {
    let config = &mut candidate.config;
    config.endpoint = authorization.s3_api_url.clone();
    config.region = b2_region(&host_of(&config.endpoint)).unwrap_or_else(|| "us-east-1".into());
    if candidate.sharex_base_url.is_none() && config.public_base_url.is_empty() && !authorization.download_url.is_empty() {
        // B2 bucket names are letters, digits and hyphens: nothing to encode.
        let base = format!("{}/file/{}", authorization.download_url, config.bucket);
        candidate.sharex_base_url = Some(base.clone());
        config.public_base_url = base;
    }
}

/// `GET https://api.backblazeb2.com/b2api/v3/b2_authorize_account` with the
/// key. Nothing about the key is logged.
pub async fn authorize_b2(key_id: &str, key: &str) -> Result<B2Authorization, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|error| error.to_string())?;
    let response = client
        .get("https://api.backblazeb2.com/b2api/v3/b2_authorize_account")
        .basic_auth(key_id, Some(key))
        .send()
        .await
        .map_err(|_| t!("Backblaze couldn’t be reached."))?;
    if !response.status().is_success() {
        return Err(t!("Backblaze didn’t accept this key ({0}).", response.status().as_u16()));
    }
    let data = response.bytes().await.map_err(|_| t!("Backblaze couldn’t be reached."))?;
    let body: Value = serde_json::from_slice(&data).map_err(|_| t!("Backblaze couldn’t be reached."))?;
    parse_b2_authorization(&body).ok_or_else(|| t!("Backblaze couldn’t be reached."))
}

/// An endpoint typed in the review: https:// when it has no scheme, and
/// the region from it for B2.
pub fn apply_typed_endpoint(candidate: &mut Candidate, endpoint: &str) {
    let endpoint = with_scheme(endpoint).trim_end_matches('/').to_string();
    let host = host_of(&endpoint);
    candidate.config.region = b2_region(&host).unwrap_or_else(|| region_from_endpoint(&endpoint));
    candidate.config.endpoint = endpoint;
}

/// The endpoint as compared for "already in Aktar": no scheme, no trailing
/// slash, lowercase.
pub fn comparable_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim().to_ascii_lowercase();
    let endpoint = endpoint.split_once("://").map_or(endpoint.as_str(), |(_, rest)| rest);
    endpoint.trim_end_matches('/').to_string()
}

// MARK: - Path template

/// ShareX's NameParser tokens, longest first where one starts another.
const TOKENS: [&str; 37] = [
    "radjective", "ranimal", "remoji", "height", "width", "unix", "guid", "mon2", "iAa", "uln", "mon", "rna", "w2", "wy", "yy", "mo", "ms", "mi", "pm",
    "pn", "ia", "ib", "ix", "rn", "ra", "rx", "rf", "un", "cn", "y", "d", "h", "s", "t", "w", "i", "n",
];

/// ShareX's `ObjectPrefix` (or `B2UploadPath`) as an Aktar template, and
/// the parts that can't be (then the template is Aktar's default). The
/// key is the prefix plus `/{filename}.{ext}`.
pub fn convert_prefix(prefix: &str) -> (String, Vec<String>) {
    let prefix = prefix.trim().trim_matches('/');
    if prefix.is_empty() {
        return ("{filename}.{ext}".into(), Vec::new());
    }
    let mut converted = String::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut rest = prefix;
    while let Some(c) = rest.chars().next() {
        if c != '%' {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
                converted.push(c);
            } else if !dropped.contains(&c.to_string()) {
                dropped.push(c.to_string());
            }
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let after = &rest[1..];
        let Some(token) = TOKENS.iter().find(|token| after.starts_with(**token)) else {
            dropped.push("%".into());
            rest = after;
            continue;
        };
        rest = &after[token.len()..];
        let random = matches!(*token, "ra" | "rna" | "rx" | "rn");
        // %ra{10}: a count for the random ones.
        if random && rest.starts_with('{') {
            if let Some(end) = rest.find('}').filter(|end| rest[1..*end].bytes().all(|byte| byte.is_ascii_digit())) {
                rest = &rest[end + 1..];
            }
        }
        match *token {
            "y" => converted.push_str("{year}"),
            "mo" => converted.push_str("{month}"),
            "d" => converted.push_str("{day}"),
            "guid" => converted.push_str("{uuid}"),
            _ if random => converted.push_str("{random}"),
            other => {
                let part = format!("%{other}");
                if !dropped.contains(&part) {
                    dropped.push(part);
                }
            }
        }
    }
    if dropped.is_empty() {
        (format!("{converted}/{{filename}}.{{ext}}"), dropped)
    } else {
        (DEFAULT_OBJECT_PATH_TEMPLATE.into(), dropped)
    }
}

// MARK: - History

/// One upload in ShareX's history, as far as Aktar needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub file_name: String,
    /// Unix milliseconds.
    pub date_time: Option<i64>,
    pub url: String,
}

/// History.db (copied, with its -wal and -shm, to a temporary folder
/// first: ShareX may be running) or History.json.
pub fn read_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    let is_db = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("db"));
    if !is_db {
        let data = std::fs::read(path).map_err(|error| error.to_string())?;
        return Ok(parse_history_json(&text_without_bom(&data)));
    }
    let folder = std::env::temp_dir().join(format!("aktar-sharex-{}", crate::util::new_id()));
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let result = (|| {
        let copy = folder.join("History.db");
        std::fs::copy(path, &copy).map_err(|error| error.to_string())?;
        for suffix in ["-wal", "-shm"] {
            let mut side = path.as_os_str().to_owned();
            side.push(suffix);
            let side = PathBuf::from(side);
            if side.is_file() {
                std::fs::copy(&side, folder.join(format!("History.db{suffix}"))).map_err(|error| error.to_string())?;
            }
        }
        read_history_db(&copy).map_err(|error| error.to_string())
    })();
    let _ = std::fs::remove_dir_all(&folder);
    result
}

fn read_history_db(path: &Path) -> rusqlite::Result<Vec<HistoryEntry>> {
    let connection = rusqlite::Connection::open(path)?;
    let mut statement = connection.prepare("SELECT FileName, DateTime, URL FROM History")?;
    let rows = statement.query_map([], |row| {
        let column = |index: usize| -> rusqlite::Result<String> {
            Ok(match row.get_ref(index)? {
                rusqlite::types::ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
                _ => String::new(),
            })
        };
        Ok(HistoryEntry { file_name: column(0)?, date_time: parse_sharex_date(&column(1)?), url: column(2)? })
    })?;
    Ok(rows.filter_map(Result::ok).filter(|entry| !entry.url.trim().is_empty()).collect())
}

/// History.json: a JSON array, or (older ShareX) its objects without the
/// outer brackets.
pub fn parse_history_json(text: &str) -> Vec<HistoryEntry> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let wrapped = if trimmed.starts_with('[') { trimmed.to_string() } else { format!("[{trimmed}]") };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&wrapped) else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let url = item.get("URL").and_then(Value::as_str).unwrap_or_default().trim().to_string();
            (!url.is_empty()).then(|| HistoryEntry {
                file_name: item.get("FileName").and_then(Value::as_str).unwrap_or_default().to_string(),
                date_time: item.get("DateTime").and_then(Value::as_str).and_then(parse_sharex_date),
                url,
            })
        })
        .collect()
}

/// A `DateTime.ToString("o")`: with an offset (or Z), or without one, in
/// local time.
pub fn parse_sharex_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(date.timestamp_millis());
    }
    let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    use chrono::TimeZone;
    chrono::Local.from_local_datetime(&naive).earliest().map(|date| date.timestamp_millis())
}

/// ShareX's `TryGetObjectKey`: the object key of `url` when it lies under
/// `base_url` (same scheme, host and port, and the path starts with the
/// base's path plus `/`), percent-decoded.
pub fn object_key_under(base_url: &str, url: &str) -> Option<String> {
    let base = url::Url::parse(base_url).ok()?;
    let object = url::Url::parse(url.trim()).ok()?;
    if !base.scheme().eq_ignore_ascii_case(object.scheme())
        || !base.host_str()?.eq_ignore_ascii_case(object.host_str()?)
        || base.port_or_known_default() != object.port_or_known_default()
    {
        return None;
    }
    let base_path = base.path().trim_end_matches('/');
    let escaped = object.path().strip_prefix(base_path)?.strip_prefix('/')?;
    let key = percent_encoding::percent_decode_str(escaped).decode_utf8().ok()?.into_owned();
    (!key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests;
