//! "Share to Another Device": one destination, keys included, moved to
//! another Aktar (Mac, Windows, iOS, Android) as a QR code or a copied
//! link, encrypted with a short transfer code that's typed on the other
//! device and is never part of the link. The format is shared with the
//! other apps:
//!
//! ```text
//! link = "aktar://import#" + base64url([0x01][salt 16][nonce 12][ciphertext + tag 16])
//! key  = PBKDF2-HMAC-SHA256(code, salt, 20000 iterations, 32 bytes)
//! AES-256-GCM, additional data "aktar-transfer-v1"
//! ```
//!
//! The sealed JSON carries `expiresAt` (Unix seconds, an hour after the
//! link was made); a link more than five minutes past it is refused, and
//! one without it (from older apps) is taken.
//!
//! Deriving the key takes a moment on purpose, so `seal` and `open` run
//! off the async runtime's threads. Nothing here is ever logged.

use aes_gcm::aead::{Aead, KeyInit, Payload as AeadPayload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::DecodePaddingMode;
use base64::{alphabet, Engine};
use rand::RngCore;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::credentials::StorageCredentials;
use crate::destinations::{DestinationConfig, ImageProcessing, ProviderPreset};
use crate::output::OutputMode;
use crate::watched::model::{Hook, HookKind};

pub const LINK_PREFIX: &str = "aktar://import#";
const FORMAT_VERSION: u8 = 1;
/// Crockford base32, which leaves out I, L, O and U.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LENGTH: usize = 12;
const ITERATIONS: u32 = 20_000;
const SALT_LENGTH: usize = 16;
const NONCE_LENGTH: usize = 12;
const TAG_LENGTH: usize = 16;
const HEADER_LENGTH: usize = 1 + SALT_LENGTH + NONCE_LENGTH;
const ADDITIONAL_DATA: &[u8] = b"aktar-transfer-v1";
const DEFAULT_OBJECT_PATH: &str = "{year}/{month}/{uuid}.{ext}";
/// How long a link can be imported: an hour. It goes in the sealed JSON as
/// `expiresAt` (Unix seconds), which apps that don't know it ignore.
const LIFETIME_SECONDS: i64 = 3600;
/// How far the two devices' clocks may disagree.
const CLOCK_SKEW_SECONDS: i64 = 300;

/// base64url without padding, also read with it.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_encode_padding(false).with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Why a transfer link couldn't be opened. The windows show their own text
/// for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferError {
    /// Not a link (or a broken one), or its contents aren't a destination.
    NotTransfer,
    /// Made by an app that writes a newer format.
    NewerVersion,
    /// The transfer code didn't decrypt it, or isn't a valid code at all.
    WrongCode,
    /// Made more than an hour ago (`expiresAt` has passed).
    Expired,
}

/// What a link carries. `custom_template` is the app-level template, sent
/// along when the destination copies with it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferPayload {
    pub destination: DestinationConfig,
    pub credentials: StorageCredentials,
    pub custom_template: Option<String>,
}

// MARK: - Transfer code

/// A new random code, 12 characters (60 bits). 32 divides 256, so masking
/// a random byte picks every character equally often.
pub fn generate_code() -> String {
    let mut bytes = [0u8; CODE_LENGTH];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| CODE_ALPHABET[(byte & 31) as usize] as char).collect()
}

/// "K7P2QX9M4TRW" as "K7P2-QX9M-4TRW".
pub fn display_code(code: &str) -> String {
    if code.len() != CODE_LENGTH {
        return code.to_string();
    }
    format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..])
}

/// What was typed, as the 12 characters the key is derived from, or none
/// if it can't be a code. Case, spaces and hyphens don't matter, and the
/// letters people mistake for digits count as those digits.
pub fn normalize_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    (code.len() == CODE_LENGTH && code.bytes().all(|byte| CODE_ALPHABET.contains(&byte))).then_some(code)
}

// MARK: - Sealing and opening

/// The link for `payload`, encrypted with `code` (from `generate_code`). A
/// fresh salt and nonce every time.
pub fn seal(payload: &TransferPayload, code: &str) -> Result<String, String> {
    let mut salt = [0u8; SALT_LENGTH];
    let mut nonce = [0u8; NONCE_LENGTH];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    seal_with(&encode_payload(payload, now_seconds()), code, &salt, &nonce)
}

fn now_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

fn seal_with(plaintext: &[u8], code: &str, salt: &[u8; SALT_LENGTH], nonce: &[u8; NONCE_LENGTH]) -> Result<String, String> {
    let sealed = cipher(code, salt)
        .encrypt(&Nonce::from(*nonce), AeadPayload { msg: plaintext, aad: ADDITIONAL_DATA })
        .map_err(|error| error.to_string())?;
    let mut envelope = Vec::with_capacity(HEADER_LENGTH + sealed.len());
    envelope.push(FORMAT_VERSION);
    envelope.extend_from_slice(salt);
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    Ok(format!("{LINK_PREFIX}{}", BASE64URL.encode(envelope)))
}

/// The destination in a scanned or pasted link (or just its base64url
/// part), decrypted with what the user typed as the code.
pub fn open(input: &str, code: &str) -> Result<TransferPayload, TransferError> {
    let envelope = envelope(input)?;
    let code = normalize_code(code).ok_or(TransferError::WrongCode)?;
    let salt = &envelope[1..1 + SALT_LENGTH];
    let nonce: [u8; NONCE_LENGTH] = envelope[1 + SALT_LENGTH..HEADER_LENGTH].try_into().map_err(|_| TransferError::NotTransfer)?;
    let plaintext = cipher(&code, salt)
        .decrypt(&Nonce::from(nonce), AeadPayload { msg: &envelope[HEADER_LENGTH..], aad: ADDITIONAL_DATA })
        .map_err(|_| TransferError::WrongCode)?;
    decode_payload(&plaintext, now_seconds())
}

/// The encrypted bytes of a link, checked before any code is asked for,
/// so a link that can't work is turned away right after it's pasted.
pub fn envelope(input: &str) -> Result<Vec<u8>, TransferError> {
    let trimmed = input.trim();
    let encoded = match trimmed.find('#') {
        Some(hash) if trimmed[..hash].eq_ignore_ascii_case("aktar://import") => &trimmed[hash + 1..],
        Some(_) => return Err(TransferError::NotTransfer),
        None if trimmed.contains(':') => return Err(TransferError::NotTransfer),
        None => trimmed,
    };
    let data = BASE64URL.decode(encoded).map_err(|_| TransferError::NotTransfer)?;
    if data.len() < HEADER_LENGTH + TAG_LENGTH {
        return Err(TransferError::NotTransfer);
    }
    match data[0] {
        FORMAT_VERSION => Ok(data),
        version if version > FORMAT_VERSION => Err(TransferError::NewerVersion),
        _ => Err(TransferError::NotTransfer),
    }
}

fn cipher(code: &str, salt: &[u8]) -> Aes256Gcm {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(code.as_bytes(), salt, ITERATIONS, &mut key);
    Aes256Gcm::new(&key.into())
}

// MARK: - Payload

/// The same field names destinations.json uses, without `isDefault` and
/// with unset fields left out, and when the link stops working (an hour
/// from `now`, Unix seconds).
fn encode_payload(payload: &TransferPayload, now: i64) -> Vec<u8> {
    let mut destination = serde_json::to_value(&payload.destination).unwrap_or(Value::Null);
    if let Value::Object(object) = &mut destination {
        object.remove("isDefault");
        // Kept as written: a custom body template's nulls are its own.
        let short_links = object.remove("shortLinks").filter(|value| !value.is_null());
        remove_nulls(object);
        if let Some(short_links) = short_links {
            object.insert("shortLinks".into(), short_links);
        }
        if object.get("accountID").and_then(Value::as_str).is_some_and(str::is_empty) {
            object.remove("accountID");
        }
    }
    let mut credentials = json!({
        "accessKeyId": payload.credentials.access_key_id,
        "secretAccessKey": payload.credentials.secret_access_key,
    });
    if let Some(token) = payload.credentials.session_token.as_deref().filter(|token| !token.is_empty()) {
        credentials["sessionToken"] = token.into();
    }
    if let Some(token) = payload.credentials.cloudflare_token.as_deref().filter(|token| !token.is_empty()) {
        credentials["cloudflareToken"] = token.into();
    }
    if let Some(token) = payload.credentials.short_link_token.as_deref().filter(|token| !token.is_empty()) {
        if payload.destination.short_links.is_some() {
            credentials["shortLinkToken"] = token.into();
        }
    }
    let mut root = json!({
        "v": FORMAT_VERSION,
        "destination": destination,
        "credentials": credentials,
        "expiresAt": now + LIFETIME_SECONDS,
    });
    if payload.destination.output_mode == Some(OutputMode::Custom) {
        if let Some(template) = &payload.custom_template {
            root["customTemplate"] = template.as_str().into();
        }
    }
    serde_json::to_vec(&root).unwrap_or_default()
}

fn remove_nulls(object: &mut Map<String, Value>) {
    object.retain(|_, value| !value.is_null());
    for value in object.values_mut() {
        if let Value::Object(inner) = value {
            remove_nulls(inner);
        }
    }
}

/// Lenient: unknown fields are ignored, and an optional field with a value
/// this app doesn't know is left unset rather than failing the whole
/// import. Only what a destination can't work without is required. Text
/// fields are trimmed, as the destination form does, since the import
/// saves them as they come. A link past its `expiresAt` (by more than the
/// clock skew allowed) is refused; one without it, from an app before it,
/// is taken.
fn decode_payload(data: &[u8], now: i64) -> Result<TransferPayload, TransferError> {
    let root: Value = serde_json::from_slice(data).map_err(|_| TransferError::NotTransfer)?;
    if integer(root.get("v")).is_some_and(|version| version > FORMAT_VERSION as i64) {
        return Err(TransferError::NewerVersion);
    }
    if integer(root.get("expiresAt")).is_some_and(|expires_at| now > expires_at.saturating_add(CLOCK_SKEW_SECONDS)) {
        return Err(TransferError::Expired);
    }
    let object = root.get("destination").and_then(Value::as_object).ok_or(TransferError::NotTransfer)?;
    let keys = root.get("credentials").and_then(Value::as_object).ok_or(TransferError::NotTransfer)?;
    let required = |object: &Map<String, Value>, name: &str| trimmed(object.get(name)).ok_or(TransferError::NotTransfer);

    let id = uuid::Uuid::parse_str(&required(object, "id")?).map_err(|_| TransferError::NotTransfer)?;
    let preset: ProviderPreset = serde_json::from_value(Value::String(required(object, "preset")?)).map_err(|_| TransferError::NotTransfer)?;
    let mut destination = DestinationConfig {
        id: id.hyphenated().to_string().to_uppercase(),
        name: required(object, "name")?,
        preset,
        account_id: trimmed(object.get("accountID")),
        endpoint: required(object, "endpoint")?,
        region: trimmed(object.get("region")).unwrap_or_else(|| default_region(preset).into()),
        bucket: required(object, "bucket")?,
        public_base_url: required(object, "publicBaseURL")?,
        object_path_template: trimmed(object.get("objectPathTemplate")).unwrap_or_else(|| DEFAULT_OBJECT_PATH.into()),
        force_path_style: object.get("forcePathStyle").and_then(Value::as_bool).unwrap_or(preset == ProviderPreset::MinIO),
        is_default: false,
        output_mode: known(object.get("outputMode")),
        expiry_days: integer(object.get("expiryDays")).and_then(|days| u32::try_from(days).ok()),
        temporary_link: integer(object.get("temporaryLink")).and_then(|seconds| u64::try_from(seconds).ok()),
        image_metadata: known(object.get("imageMetadata")),
        folder_upload: known(object.get("folderUpload")),
        image_processing: image_processing(object.get("imageProcessing")),
        thumbnails: known(object.get("thumbnails")),
        // Normalized, and dropped when it can't be a folder, by `sanitize`.
        thumbnail_prefix: string(object.get("thumbnailPrefix")),
        use_for: object.get("useFor").and_then(crate::routing::FileRouting::from_value),
        short_cache: object.get("shortCache").and_then(Value::as_bool),
        // Dropped by `sanitize` when it isn't a zone ID.
        cloudflare_zone_id: trimmed(object.get("cloudflareZoneId")),
        hooks: hooks(object.get("hooks")),
        // Settings for a provider this app doesn't know (or a custom one it
        // can't read) leave short links off.
        short_links: object.get("shortLinks").and_then(crate::short_links::definition::from_value),
    };
    // Values this app doesn't offer (a "Delete after" or link duration)
    // are dropped, the same as in destinations.json.
    destination.sanitize();

    let credentials = StorageCredentials {
        access_key_id: required(keys, "accessKeyId")?,
        secret_access_key: required(keys, "secretAccessKey")?,
        session_token: string(keys.get("sessionToken")),
        cloudflare_token: trimmed(keys.get("cloudflareToken")),
        // Only along with the settings it's for.
        short_link_token: destination.short_links.as_ref().and_then(|_| trimmed(keys.get("shortLinkToken"))),
    };
    Ok(TransferPayload { destination, credentials, custom_template: string(root.get("customTemplate")) })
}

/// A destination's "After Upload" hooks: its webhooks. Scripts stay behind:
/// a program to run is only ever one picked on this PC (the Mac's are names
/// in its own scripts folder anyway), never one a link names. Anything
/// unreadable is left out.
fn hooks(value: Option<&Value>) -> Option<Vec<Hook>> {
    let hooks: Vec<Hook> = value?
        .as_array()?
        .iter()
        .filter_map(|hook| {
            let kind: HookKind = known(hook.get("kind"))?;
            let target = trimmed(hook.get("target"))?;
            let usable = kind == HookKind::Webhook && crate::watched::check_webhook_url(&target).is_ok();
            usable.then(|| Hook {
                id: crate::util::new_id(),
                kind,
                target,
                enabled: hook.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            })
        })
        .collect();
    (!hooks.is_empty()).then_some(hooks)
}

/// The region a provider's form starts with, as in the Mac app.
fn default_region(preset: ProviderPreset) -> &'static str {
    match preset {
        ProviderPreset::AmazonS3 => "us-east-1",
        _ => "auto",
    }
}

/// An unknown format drops the whole setting; a quality or size the
/// pickers don't offer is turned off on its own (by `sanitize`).
fn image_processing(value: Option<&Value>) -> Option<ImageProcessing> {
    let object = value?.as_object()?;
    Some(ImageProcessing {
        format: known(object.get("format"))?,
        quality: integer(object.get("quality")).and_then(|quality| u8::try_from(quality).ok()),
        max_long_edge: integer(object.get("maxLongEdge")).and_then(|size| u32::try_from(size).ok()),
    })
}

/// A string enum's value, or none for one this app doesn't know.
fn known<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> Option<T> {
    value.filter(|value| value.is_string()).and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_string)
}

/// `string` without the spaces around it.
fn trimmed(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// A whole number, also when it's written as 30.0.
fn integer(value: Option<&Value>) -> Option<i64> {
    let number = value?.as_number()?;
    number.as_i64().or_else(|| number.as_f64().filter(|float| float.fract() == 0.0 && float.abs() < 1e15).map(|float| float as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destinations::{FolderUploadMode, ImageFormat, ImageMetadataPolicy};

    // From the shared test vectors (vectors.json), made with Node's WebCrypto.
    const CODE: &str = "K7P2QX9M4TRW";
    const FULL: &str = "aktar://import#AQABAgMEBQYHCAkKCwwNDg-goaKjpKWmp6ipqqtb3kuNh7HmTZAAsoxnF4uSJvWbwAcgdjc8HykCZWgeykwGbFBmIzNl1Q3viSEVqd1NN5aRoH7z0vbPMhXSMtczXgNXHOZ76srLl2yVJFMp5QJQGdIkCtC5G7UPaOVIAKIqQ5npKkPWys4iGdM3lyb5puIHiV-LTGQlCm02n9N14hgmLpJGW1m2lVFisMr3qI03gEcxYSQ4ESH5OuyV2nHLx-IyxauI9iUhCEHUMZSXsJwsf3cNt8WzDgjVuNJOUmdKqD5I9k2Tw5hpHzqzxwxGJARrI0R9T8ZqSRkWWVl7LOY0tVK17CRjT_uU_c4gYZ6UVxxmYMDdVQSv5d4dFz2Otic8pfGW0jNiGSwCMTR5h8rHCn07FCNKpK_mziP0-CfUSrsdobas_LGisvfm69M3loMmmX6USbCzjLm0S0ua9AmKSank57zEvmRk2p-3BWG3LT6W0gOCa6_1MqQfwBKYMhqK2E3r-3BSV5LwjcznsOPY42_8BkPDfsacI-06LYa7ecKWkPdlIEwx7WE-HnS2TA-tfNSvxevlf6vPOA_YRECyQ1A2OHoQHj7vTAhCtsBt9QhzKCytGb3qV7uZZH1OxSwXBEeu03_MlEjbexFL_2ebcnhlgT7fu_iIpjkWchk35aNl9VjtW8wd3b9i75x-SHhX7C-_lGqjuulZFMVfLXxcFyrL2hKvag5O8kvtJ1xANK22lAK1uoFYIswA5JxXXyIppkcQo5l2PdKJCrzYToVbBRXH6FHYzVWPc3txe3wv9-2fZxZ23LRtzoUTgwoM-sdCxR2xuO-qXyREli016h8jeZMg7dGGjIV3LhSA2q1a1042lrGK25LKBuypulofHAD_3M9pqeL3yBxd9-7HvoC4qeFMbnuX_kDEQ96vnfD7YfR8fakpFLrB_5rKwvUwqXDjk-4dGN_YnFtWVmmKnDpnYrmD3SMLEwCvjun68bE6xlWak2TOWYEoAKeKxBy2PJjUrbL8cqnYpw12";
    const MINIMAL: &str = "aktar://import#AQABAgMEBQYHCAkKCwwNDg-goaKjpKWmp6ipqqtb3kuNh7HmTZAAsoxnF4uSJvWbwAcgdjc8HykCY2sexEkIGCNmWERsoA2f-1YQqaZAM-KR1X_z2vLMQxulRNBFXgNXHOZ76srLl3KfOH8DqV0aBtQyW5nvSf1IdulSa9cqDNfjMUPAycY-CKM_l2Kvs_FeyQXUAR9PGWEsgdNp4BwpIZVOUhr41EdisZOp9Jw5lwR1dHg4ADawbqib1DHbyu0n3uDcoHJrRkbBIZfGppFzOiRT7ZLEWUyI1OFgEzkNpnoNtESI2Z9nFS3jk1cMcEx0YUxqHJo1EwgAWVdoLeZi9gK76SsoT-CpvpZqR56eTh9pNp_dDlOg_4lYUSLVrikhpa6O3GJsGSoSdkg6g9f2Em00M2ADtYjB5y3stTrUTr4a1vbn__r09ean6d4l2d9olT2WQbjB0vS4TFKM_hO9Cuf3kb_GvGVk2tivHSupPTjVjFzcZaP2NaQOnwfbdxLPnkOq7XBsVpun04vHtvTX4h2nQ1a8KNyCI6tlJZO-a8vJkKJrdl0n-kkiCVrxD2SkP5zmzO38eOaCfgfJWgvsGGcyIntXUEy8A09FoOZ441goeDiIHrruFOXHOyNclHP2dLtVPlMGzKCIDra0JXr1";
    const NEWER_VERSION: &str = "aktar://import#AgABAgMEBQYHCAkKCwwNDg-goaKjpKWmp6ipqqtb3kuNh7HmTZAAsoxnF4uSJvWbwAcgdjc8HykCZWgeykwGbFBmIzNl1Q3viSEVqd1NN5aRoH7z0vbPMhXSMtczXgNXHOZ76srLl2yVJFMp5QJQGdIkCtC5G7UPaOVIAKIqQ5npKkPWys4iGdM3lyb5puIHiV-LTGQlCm02n9N14hgmLpJGW1m2lVFisMr3qI03gEcxYSQ4ESH5OuyV2nHLx-IyxauI9iUhCEHUMZSXsJwsf3cNt8WzDgjVuNJOUmdKqD5I9k2Tw5hpHzqzxwxGJARrI0R9T8ZqSRkWWVl7LOY0tVK17CRjT_uU_c4gYZ6UVxxmYMDdVQSv5d4dFz2Otic8pfGW0jNiGSwCMTR5h8rHCn07FCNKpK_mziP0-CfUSrsdobas_LGisvfm69M3loMmmX6USbCzjLm0S0ua9AmKSank57zEvmRk2p-3BWG3LT6W0gOCa6_1MqQfwBKYMhqK2E3r-3BSV5LwjcznsOPY42_8BkPDfsacI-06LYa7ecKWkPdlIEwx7WE-HnS2TA-tfNSvxevlf6vPOA_YRECyQ1A2OHoQHj7vTAhCtsBt9QhzKCytGb3qV7uZZH1OxSwXBEeu03_MlEjbexFL_2ebcnhlgT7fu_iIpjkWchk35aNl9VjtW8wd3b9i75x-SHhX7C-_lGqjuulZFMVfLXxcFyrL2hKvag5O8kvtJ1xANK22lAK1uoFYIswA5JxXXyIppkcQo5l2PdKJCrzYToVbBRXH6FHYzVWPc3txe3wv9-2fZxZ23LRtzoUTgwoM-sdCxR2xuO-qXyREli016h8jeZMg7dGGjIV3LhSA2q1a1042lrGK25LKBuypulofHAD_3M9pqeL3yBxd9-7HvoC4qeFMbnuX_kDEQ96vnfD7YfR8fakpFLrB_5rKwvUwqXDjk-4dGN_YnFtWVmmKnDpnYrmD3SMLEwCvjun68bE6xlWak2TOWYEoAKeKxBy2PJjUrbL8cqnYpw12";
    const NEWER_PAYLOAD: &str = "aktar://import#AQABAgMEBQYHCAkKCwwNDg-goaKjpKWmp6ipqqtb3kuNh7LmTZAAsoxnF4uSJvWbwAcgdjc8HykCZWgeykwGbFBmIzNl1Q3viSEVqd1NN5aRoH7z0vbPMhXSMtczXgNXHOZ76srLl2yVJFMp5QJQGdIkCtC5G7UPaOVIAKIqQ5npKkPWys4iGdM3lyb5puIHiV-LTGQlCm02n9N14hgmLpJGW1m2lVFisMr3qI03gEcxYSQ4ESH5OuyV2nHLx-IyxauI9iUhCEHUMZSXsJwsf3cNt8WzDgjVuNJOUmdKqD5I9k2Tw5hpHzqzxwxGJARrI0R9T8ZqSRkWWVl7LOY0tVK17CRjT_uU_c4gYZ6UVxxmYMDdVQSv5d4dFz2Otic8pfGW0jNiGSwCMTR5h8rHCn07FCNKpK_mziP0-CfUSrsdobas_LGisvfm69M3loMmmX6USbCzjLm0S0ua9AmKSank57zEvmRk2p-3BWG3LT6W0gOCa6_1MqQfwBKYMhqK2E3r-3BSV5LwjcznsOPY42_8BkPDfsacI-06LYa7ecKWkPdlIEwx7WE-HnS2TA-tfNSvxevlf6vPOA_YRECyQ1A2OHoQHj7vTAhCtsBt9QhzKCytGb3qV7uZZH1OxSwXBEeu03_MlEjbexFL_2ebcnhlgT7fu_iIpjkWchk35aNl9VjtW8wd3b9i75x-SHhX7C-_lGqjuulZFMVfLXxcFyrL2hKvag5O8kvtJ1xANK22lAK1uoFYIswA5JxXXyIppkcQo5l2PdKJCrzYToVbBRXH6FHYzVWPc3txe3wv9-2fZxZ23LRtzoUTgwoM-sdCxR2xuO-qXyREli016h8jeZMg7dGGjIV3LhSA2q1a1042lrGK25LKBuypulofHAD_3M9pqeL3yBxd9-7HvoC4qeFMbnuX_kDEQ96vnfD7YfR8fakpFLrB_5rKwvUwqXDjk-4dGN_YnFtWVmmKnDpnYrmD3SMLEwCvjun68bE6xlWak2TOWYEoAKfyYzVVEwpjS1uRXX1MUQGF";

    #[test]
    fn opens_the_full_vector() {
        let payload = open(FULL, "K7P2-QX9M-4TRW").unwrap();
        let destination = payload.destination;
        assert_eq!(destination.id, "6F9619FF-8B86-D011-B42D-00C04FC964FF");
        assert_eq!(destination.name, "Screenshots");
        assert_eq!(destination.preset, ProviderPreset::CloudflareR2);
        assert_eq!(destination.account_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(destination.endpoint, "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com");
        assert_eq!(destination.region, "auto");
        assert_eq!(destination.bucket, "shots");
        assert_eq!(destination.public_base_url, "https://files.example.com");
        assert_eq!(destination.object_path_template, "{year}/{month}/{uuid}.{ext}");
        assert!(!destination.force_path_style);
        assert!(!destination.is_default);
        assert_eq!(destination.output_mode, Some(OutputMode::Markdown));
        assert_eq!(destination.expiry_days, Some(30));
        assert_eq!(destination.temporary_link, Some(3600));
        assert_eq!(destination.image_metadata, Some(ImageMetadataPolicy::RemoveAll));
        assert_eq!(destination.folder_upload, Some(FolderUploadMode::KeepStructure));
        assert_eq!(
            destination.image_processing,
            Some(ImageProcessing { format: ImageFormat::Webp, quality: Some(80), max_long_edge: Some(2560) })
        );
        assert_eq!(payload.credentials.access_key_id, "EXAMPLEACCESSKEYID000");
        assert_eq!(payload.credentials.secret_access_key, "example-secret-not-real-0000000000000000");
        assert_eq!(payload.credentials.session_token, None);
        assert_eq!(payload.custom_template.as_deref(), Some("![{filename}]({url})"));
    }

    #[test]
    fn opens_the_minimal_vector_leniently() {
        let payload = open(MINIMAL, CODE).unwrap();
        let destination = payload.destination;
        assert_eq!(destination.id, "0E984725-C51C-4BF4-9960-E1C80E27ABA0");
        assert_eq!(destination.preset, ProviderPreset::MinIO);
        assert_eq!(destination.account_id, None);
        assert_eq!(destination.region, "us-east-1");
        assert_eq!(destination.object_path_template, "{uuid}.{ext}");
        assert!(destination.force_path_style);
        // "bogus" isn't an output mode, and "heic" isn't a format.
        assert_eq!(destination.output_mode, None);
        assert_eq!(destination.image_processing, None);
        assert_eq!(destination.expiry_days, None);
        assert_eq!(destination.temporary_link, None);
        assert_eq!(payload.credentials.access_key_id, "minioadmin");
        assert_eq!(payload.custom_template, None);
    }

    #[test]
    fn reports_why_a_link_fails() {
        assert_eq!(open(FULL, "K7P2QX9M4TRX").unwrap_err(), TransferError::WrongCode);
        assert_eq!(open(FULL, "K7P2-QX9M").unwrap_err(), TransferError::WrongCode);
        assert_eq!(envelope(NEWER_VERSION).unwrap_err(), TransferError::NewerVersion);
        assert_eq!(open(NEWER_VERSION, CODE).unwrap_err(), TransferError::NewerVersion);
        assert_eq!(open(NEWER_PAYLOAD, CODE).unwrap_err(), TransferError::NewerVersion);
        for input in ["https://example.com", "aktar://import#", "aktar://import#AAAA", "hello", "", "aktar://other#AQAB"] {
            assert_eq!(envelope(input).unwrap_err(), TransferError::NotTransfer, "{input}");
        }
        // Version 0 is garbage, not an old format.
        let mut zero = envelope(FULL).unwrap();
        zero[0] = 0;
        assert_eq!(envelope(&BASE64URL.encode(zero)).unwrap_err(), TransferError::NotTransfer);
    }

    #[test]
    fn expires_after_an_hour() {
        let payload = open(FULL, CODE).unwrap();
        let created = 1_790_000_000;
        let sealed = seal_with(&encode_payload(&payload, created), CODE, &[7; SALT_LENGTH], &[9; NONCE_LENGTH]).unwrap();
        let plaintext = |link: &str| {
            let envelope = envelope(link).unwrap();
            let nonce: [u8; NONCE_LENGTH] = envelope[1 + SALT_LENGTH..HEADER_LENGTH].try_into().unwrap();
            cipher(CODE, &envelope[1..1 + SALT_LENGTH])
                .decrypt(&Nonce::from(nonce), AeadPayload { msg: &envelope[HEADER_LENGTH..], aad: ADDITIONAL_DATA })
                .unwrap()
        };
        let data = plaintext(&sealed);
        // Within the hour, and the five minutes the clocks may be apart.
        assert!(decode_payload(&data, created).is_ok());
        assert!(decode_payload(&data, created + 3600 + 300).is_ok());
        assert_eq!(decode_payload(&data, created + 3600 + 301).unwrap_err(), TransferError::Expired);
        // Links from apps before `expiresAt` have none, and still open.
        assert!(decode_payload(&plaintext(FULL), i64::MAX).is_ok());
        // A fresh link opens right away.
        assert!(open(&seal(&payload, CODE).unwrap(), CODE).is_ok());
    }

    #[test]
    fn accepts_the_link_in_any_form() {
        let encoded = &FULL[LINK_PREFIX.len()..];
        assert!(envelope(encoded).is_ok());
        assert!(envelope(&format!("  AKTAR://Import#{encoded}\n")).is_ok());
    }

    #[test]
    fn normalizes_codes() {
        assert_eq!(normalize_code("k7p2 qx9m 4trw").as_deref(), Some("K7P2QX9M4TRW"));
        assert_eq!(normalize_code("K7P2-QX9M-4TRW").as_deref(), Some("K7P2QX9M4TRW"));
        assert_eq!(normalize_code("OIL0-0111-11AB").as_deref(), Some("0110011111AB"));
        assert_eq!(normalize_code("K7P2-QX9M"), None);
        assert_eq!(normalize_code("K7P2-QX9M-4TRWX"), None);
        assert_eq!(normalize_code("K7P2-QX9M-4TRU"), None);
        assert_eq!(display_code("K7P2QX9M4TRW"), "K7P2-QX9M-4TRW");
        let code = generate_code();
        assert_eq!(normalize_code(&code).as_deref(), Some(code.as_str()));
    }

    #[test]
    fn trims_what_it_saves() {
        let mut destination = open(FULL, CODE).unwrap().destination;
        destination.name = " Screenshots ".into();
        destination.endpoint = " https://s3.example.com/ ".into();
        destination.bucket = " shots\n".into();
        destination.public_base_url = "  https://files.example.com ".into();
        destination.region = " ".into();
        destination.object_path_template = " {uuid}.{ext} ".into();
        let payload = TransferPayload {
            destination,
            credentials: StorageCredentials {
                access_key_id: " AKID ".into(),
                secret_access_key: "secret ".into(),
                session_token: None,
                cloudflare_token: None,
                short_link_token: None,
            },
            custom_template: None,
        };
        let opened = open(&seal(&payload, CODE).unwrap(), CODE).unwrap();
        assert_eq!(opened.destination.name, "Screenshots");
        assert_eq!(opened.destination.endpoint, "https://s3.example.com/");
        assert_eq!(opened.destination.bucket, "shots");
        assert_eq!(opened.destination.public_base_url, "https://files.example.com");
        assert_eq!(opened.destination.region, "auto");
        assert_eq!(opened.destination.object_path_template, "{uuid}.{ext}");
        assert_eq!(opened.credentials.access_key_id, "AKID");
        assert_eq!(opened.credentials.secret_access_key, "secret");
    }

    #[test]
    fn reads_thumbnail_settings_leniently() {
        let decoded = |thumbnails: Value, prefix: Value| {
            let data = serde_json::to_vec(&json!({
                "v": 1,
                "destination": {
                    "id": "0F0E8A57-4D8C-4C61-9B26-7C3F7E0E9A11", "name": "A", "preset": "customS3",
                    "endpoint": "https://s3.example.com", "bucket": "b", "publicBaseURL": "https://files.example.com",
                    "thumbnails": thumbnails, "thumbnailPrefix": prefix,
                },
                "credentials": { "accessKeyId": "id", "secretAccessKey": "secret" },
            }))
            .unwrap();
            decode_payload(&data, 0).unwrap().destination
        };
        let valid = decoded(json!("bucket"), json!("/thumbs"));
        assert_eq!(valid.thumbnails, Some(crate::thumbnails::ThumbnailMode::Bucket));
        assert_eq!(valid.thumbnail_prefix.as_deref(), Some("thumbs/"));
        // Unknown values are left unset, never fail the import.
        let unknown = decoded(json!("cloud"), json!("tmp/7d/thumbs/"));
        assert_eq!(unknown.thumbnails, None);
        assert_eq!(unknown.thumbnail_prefix, None);
        assert_eq!(decoded(json!(3), json!("a/../b")).thumbnail_prefix, None);
    }

    #[test]
    fn reads_automation_settings_leniently() {
        let decoded = |destination: Value| {
            let mut object = json!({
                "id": "0F0E8A57-4D8C-4C61-9B26-7C3F7E0E9A11", "name": "A", "preset": "customS3",
                "endpoint": "https://s3.example.com", "bucket": "b", "publicBaseURL": "https://files.example.com",
            });
            object.as_object_mut().unwrap().extend(destination.as_object().unwrap().clone());
            let data = serde_json::to_vec(&json!({
                "v": 1,
                "destination": object,
                "credentials": { "accessKeyId": "id", "secretAccessKey": "secret", "cloudflareToken": " t " },
            }))
            .unwrap();
            decode_payload(&data, 0).unwrap()
        };
        let payload = decoded(json!({
            "useFor": { "kinds": ["video", "hologram"], "extensions": ["DMG", "no.pe"] },
            "shortCache": true,
            "cloudflareZoneId": " 0123456789ABCDEF0123456789ABCDEF ",
            "hooks": [
                { "kind": "webhook", "target": "https://hooks.example.com/x" },
                { "kind": "webhook", "target": "http://hooks.example.com/plain" },
                { "kind": "script", "target": "notify.sh", "enabled": false },
                { "kind": "script", "target": "C:\\Scripts\\notify.ps1", "enabled": false },
                { "kind": "carrier-pigeon", "target": "x" },
            ],
        }));
        let destination = payload.destination;
        assert_eq!(
            destination.use_for,
            Some(crate::routing::FileRouting { kinds: vec![crate::routing::FileKind::Video], extensions: vec!["dmg".into()] })
        );
        assert_eq!(destination.short_cache, Some(true));
        assert_eq!(destination.cloudflare_zone_id.as_deref(), Some("0123456789abcdef0123456789abcdef"));
        // Webhooks that can be used come along; scripts never do.
        let hooks = destination.hooks.unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].target, "https://hooks.example.com/x");
        assert!(hooks[0].enabled);
        assert_eq!(payload.credentials.cloudflare_token.as_deref(), Some("t"));

        // Wrong types and values leave each setting unset.
        let destination = decoded(json!({ "useFor": "image", "shortCache": "yes", "cloudflareZoneId": "zone", "hooks": {} })).destination;
        assert_eq!((destination.use_for, destination.short_cache, destination.cloudflare_zone_id, destination.hooks), (None, None, None, None));
    }

    #[test]
    fn short_links_travel() {
        use crate::short_links::definition::{custom_template, ShortLinkSettings};
        let mut destination = open(FULL, CODE).unwrap().destination;
        let mut custom = custom_template();
        custom.create.path = "https://api.example.com/shorten".into();
        custom.create.body = Some(json!({ "url": "{url}", "note": null }));
        destination.short_links = Some(ShortLinkSettings {
            custom: Some(custom),
            only_longer_than: 30,
            allow_insecure_http: true,
            ..ShortLinkSettings::new("custom")
        });
        let credentials = StorageCredentials {
            access_key_id: "id".into(),
            secret_access_key: "secret".into(),
            session_token: None,
            cloudflare_token: None,
            short_link_token: Some("short-secret".into()),
        };
        let payload = TransferPayload { destination: destination.clone(), credentials, custom_template: None };
        let plaintext = encode_payload(&payload, 1_000);
        let root: Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(root["credentials"]["shortLinkToken"], "short-secret");
        let sent = &root["destination"]["shortLinks"];
        assert_eq!(sent["providerId"], "custom");
        assert_eq!(sent["onlyLongerThan"], 30);
        assert_eq!(sent["allowInsecureHTTP"], true);
        assert_eq!(sent["custom"]["kind"], "custom");
        assert_eq!(sent["custom"]["needsDomain"], false);
        assert_eq!(sent["custom"]["capabilities"]["expiration"], false);
        // A body template's own null stays; unset fields are left out.
        assert!(sent["custom"]["create"]["body"]["note"].is_null() && sent["custom"]["create"]["body"].get("note").is_some());
        assert!(sent.get("endpoint").is_none() && sent["custom"].get("baseUrl").is_none());
        // The token is never in the settings.
        assert!(!sent.to_string().contains("short-secret"));

        let decoded = decode_payload(&plaintext, 1_000).unwrap();
        assert_eq!(decoded.destination.short_links, destination.short_links);
        assert_eq!(decoded.credentials.short_link_token.as_deref(), Some("short-secret"));

        // The Mac app's JSON for a built-in provider.
        let mut mac = root.clone();
        mac["destination"]["shortLinks"] = json!({ "providerId": "shlink", "endpoint": "https://s.example.com", "onlyLongerThan": 0, "shortenTemporaryLinks": false, "allowInsecureHTTP": false });
        let from_mac = decode_payload(&serde_json::to_vec(&mac).unwrap(), 1_000).unwrap();
        assert_eq!(from_mac.destination.short_links.as_ref().map(|settings| settings.provider_id.as_str()), Some("shlink"));
        assert_eq!(from_mac.credentials.short_link_token.as_deref(), Some("short-secret"));

        // A provider this app doesn't know leaves short links off, and its
        // token isn't kept; the rest of the destination imports.
        let mut unknown = root.clone();
        unknown["destination"]["shortLinks"] = json!({ "providerId": "bitly-next", "onlyLongerThan": "x" });
        let lenient = decode_payload(&serde_json::to_vec(&unknown).unwrap(), 1_000).unwrap();
        assert_eq!(lenient.destination.short_links, None);
        assert_eq!(lenient.credentials.short_link_token, None);
        assert_eq!(lenient.destination.name, destination.name);
        // A custom provider without a definition too.
        unknown["destination"]["shortLinks"] = json!({ "providerId": "custom" });
        assert_eq!(decode_payload(&serde_json::to_vec(&unknown).unwrap(), 1_000).unwrap().destination.short_links, None);

        // No short links: neither field is sent.
        let mut without = payload.clone();
        without.destination.short_links = None;
        let json: Value = serde_json::from_slice(&encode_payload(&without, 1_000)).unwrap();
        assert!(json["destination"].get("shortLinks").is_none());
        assert!(json["credentials"].get("shortLinkToken").is_none());
    }

    #[test]
    fn round_trips() {
        let mut destination = open(FULL, CODE).unwrap().destination;
        destination.output_mode = Some(OutputMode::Custom);
        destination.is_default = true;
        destination.image_processing = Some(ImageProcessing { format: ImageFormat::Avif, quality: None, max_long_edge: Some(1920) });
        destination.thumbnails = Some(crate::thumbnails::ThumbnailMode::Bucket);
        destination.thumbnail_prefix = Some("previews/".into());
        destination.use_for = crate::routing::FileRouting {
            kinds: vec![crate::routing::FileKind::Image],
            extensions: vec!["dmg".into()],
        }
        .sanitized();
        destination.short_cache = Some(true);
        destination.cloudflare_zone_id = Some("0123456789abcdef0123456789abcdef".into());
        destination.hooks = Some(vec![Hook {
            id: "H1".into(),
            kind: HookKind::Webhook,
            target: "https://hooks.example.com/x".into(),
            enabled: true,
        }]);
        let payload = TransferPayload {
            destination: destination.clone(),
            credentials: StorageCredentials {
                access_key_id: "AKID".into(),
                secret_access_key: "secret".into(),
                session_token: Some("token".into()),
                cloudflare_token: Some("cf-token".into()),
                short_link_token: None,
            },
            custom_template: Some("<{url}>".into()),
        };
        let json: Value = serde_json::from_slice(&encode_payload(&payload, 1_000)).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["expiresAt"], 4_600);
        assert!(json["destination"].get("isDefault").is_none());
        assert!(json["destination"]["imageProcessing"].get("quality").is_none());
        assert_eq!(json["destination"]["thumbnails"], "bucket");
        assert_eq!(json["destination"]["thumbnailPrefix"], "previews/");
        assert_eq!(json["destination"]["useFor"], json!({ "kinds": ["image"], "extensions": ["dmg"] }));
        assert_eq!(json["destination"]["shortCache"], true);
        assert_eq!(json["destination"]["cloudflareZoneId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(json["destination"]["hooks"][0]["kind"], "webhook");
        assert_eq!(json["credentials"]["cloudflareToken"], "cf-token");

        let code = generate_code();
        let link = seal(&payload, &code).unwrap();
        assert!(link.starts_with(LINK_PREFIX));
        // A fresh salt and nonce every time.
        assert_ne!(link, seal(&payload, &code).unwrap());
        let opened = open(&link, &display_code(&code).to_lowercase()).unwrap();
        // Hooks get IDs of their own on the receiving device.
        let received_hooks = opened.destination.hooks.clone().unwrap();
        assert_eq!((received_hooks[0].kind, received_hooks[0].target.as_str()), (HookKind::Webhook, "https://hooks.example.com/x"));
        assert_eq!(
            DestinationConfig { hooks: None, ..opened.destination },
            DestinationConfig { is_default: false, hooks: None, ..destination }
        );
        assert_eq!(opened.credentials.cloudflare_token.as_deref(), Some("cf-token"));
        assert_eq!(opened.credentials.session_token.as_deref(), Some("token"));
        assert_eq!(opened.custom_template.as_deref(), Some("<{url}>"));

        // The template only goes along with the custom output mode.
        let mut plain = payload.clone();
        plain.destination.output_mode = None;
        let json: Value = serde_json::from_slice(&encode_payload(&plain, 1_000)).unwrap();
        assert!(json.get("customTemplate").is_none());
    }
}
