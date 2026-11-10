use super::*;

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("aktar-sharex-test-{name}-{}", crate::util::new_id()));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn no_dpapi(_: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Stands in for DPAPI: "sealed" bytes are the plaintext reversed.
fn fake_dpapi(sealed: &[u8]) -> Option<Vec<u8>> {
    (sealed.first() == Some(&b'!')).then(|| sealed[1..].iter().rev().copied().collect())
}

fn fake_sealed(plain: &str) -> String {
    use base64::Engine;
    let mut sealed = vec![b'!'];
    sealed.extend(plain.bytes().rev());
    format!("$DPAPIEncrypted${}", base64::engine::general_purpose::STANDARD.encode(sealed))
}

#[test]
fn finds_the_personal_folder_in_sharex_order() {
    let root = temp_folder("places");
    let exe = root.join("Program Files").join("ShareX");
    let documents = root.join("Documents");
    std::fs::create_dir_all(&exe).unwrap();
    std::fs::write(exe.join("ShareX.exe"), b"").unwrap();
    std::fs::create_dir_all(documents.join("ShareX")).unwrap();
    let mut places = Places { exe_folders: vec![root.join("missing"), exe.clone()], registry_personal_path: None, documents: Some(documents.clone()), local_app_data: None };

    // Nothing set: Documents\ShareX.
    assert_eq!(personal_folder(&places), Some(documents.join("ShareX")));

    // PersonalPath.cfg in Documents\ShareX, relative to the exe's folder.
    std::fs::create_dir_all(exe.join("Custom")).unwrap();
    std::fs::write(documents.join("ShareX").join("PersonalPath.cfg"), "\u{feff}  Custom \r\n").unwrap();
    assert_eq!(personal_folder(&places), Some(exe.join("Custom")));

    // Next to the exe wins over Documents.
    let next_to_exe = root.join("Next");
    std::fs::create_dir_all(&next_to_exe).unwrap();
    std::fs::write(exe.join("PersonalPath.cfg"), next_to_exe.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(personal_folder(&places), Some(next_to_exe.clone()));

    // The registry wins over PersonalPath.cfg.
    let registry = root.join("Registry");
    std::fs::create_dir_all(&registry).unwrap();
    places.registry_personal_path = Some(registry.to_string_lossy().into_owned());
    assert_eq!(personal_folder(&places), Some(registry));
    // ...and %MyDocuments% is expanded.
    places.registry_personal_path = Some("%MyDocuments%/ShareX".into());
    assert_eq!(personal_folder(&places), Some(PathBuf::from(format!("{}/ShareX", documents.to_string_lossy()))));

    // A portable install wins over everything.
    std::fs::write(exe.join("Portable"), b"").unwrap();
    assert_eq!(personal_folder(&places), None, "its ShareX folder doesn't exist yet");
    std::fs::create_dir_all(exe.join("ShareX")).unwrap();
    assert_eq!(personal_folder(&places), Some(exe.join("ShareX")));

    // No ShareX at all.
    let empty = Places { documents: Some(root.join("nowhere")), ..Places::default() };
    assert_eq!(personal_folder(&empty), None);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn locates_the_settings_and_history() {
    let folder = temp_folder("locate");
    let places = Places::default();
    assert_eq!(locate(&folder, Some("DESKTOP-1"), &places), None);
    std::fs::write(folder.join("UploadersConfig.json"), "{}").unwrap();
    let located = locate(&folder, Some("DESKTOP-1"), &places).unwrap();
    assert_eq!(located.uploaders_config, folder.join("UploadersConfig.json"));
    assert_eq!(located.history, None);

    // The machine-specific file, lowercase and sanitized, when it's there.
    std::fs::write(folder.join("UploadersConfig-desktop-1.json"), "{}").unwrap();
    assert_eq!(locate(&folder, Some("DESKTOP-1"), &places).unwrap().uploaders_config, folder.join("UploadersConfig-desktop-1.json"));
    // ...unless ApplicationConfig turned it off.
    std::fs::write(folder.join("ApplicationConfig.json"), r#"{"UseMachineSpecificUploadersConfig": false}"#).unwrap();
    assert_eq!(locate(&folder, Some("DESKTOP-1"), &places).unwrap().uploaders_config, folder.join("UploadersConfig.json"));

    // CustomUploadersConfigPath, when that folder has the file.
    let custom = temp_folder("locate-custom");
    let config = format!("\u{feff}{{\"CustomUploadersConfigPath\": {}}}", serde_json::to_string(&custom.to_string_lossy()).unwrap());
    std::fs::write(folder.join("ApplicationConfig.json"), config).unwrap();
    assert_eq!(locate(&folder, Some("DESKTOP-1"), &places).unwrap().uploaders_config, folder.join("UploadersConfig-desktop-1.json"));
    std::fs::write(custom.join("UploadersConfig.json"), "{}").unwrap();
    assert_eq!(locate(&folder, None, &places).unwrap().uploaders_config, custom.join("UploadersConfig.json"));

    // History.db before History.json.
    std::fs::write(folder.join("History.json"), "").unwrap();
    assert_eq!(locate(&folder, None, &places).unwrap().history, Some(folder.join("History.json")));
    std::fs::write(folder.join("History.db"), "").unwrap();
    assert_eq!(locate(&folder, None, &places).unwrap().history, Some(folder.join("History.db")));
    std::fs::remove_dir_all(folder).unwrap();
    std::fs::remove_dir_all(custom).unwrap();
}

#[test]
fn sanitizes_machine_names() {
    assert_eq!(sanitize_file_name(" pc:1/a*b?\"<>|\\ "), "pc1ab");
    assert_eq!(sanitize_file_name("desktop-abc"), "desktop-abc");
}

#[test]
fn reads_secrets() {
    assert_eq!(read_secret(None, &no_dpapi), Secret::Missing);
    assert_eq!(read_secret(Some(&Value::String(String::new())), &no_dpapi), Secret::Missing);
    assert_eq!(read_secret(Some(&Value::String(" plain ".into())), &no_dpapi), Secret::Known("plain".into()));
    let sealed = Value::String(fake_sealed("s3cr3t"));
    assert_eq!(read_secret(Some(&sealed), &fake_dpapi), Secret::Known("s3cr3t".into()));
    assert_eq!(read_secret(Some(&sealed), &no_dpapi), Secret::Unreadable);
    assert_eq!(read_secret(Some(&Value::String("$DPAPIEncrypted$not base64!".into())), &fake_dpapi), Secret::Unreadable);
    // Never printed.
    assert_eq!(format!("{:?}", Secret::Known("s3cr3t".into())), "Known(..)");
}

#[test]
fn works_out_regions_like_sharex() {
    assert_eq!(region_from_endpoint("s3.amazonaws.com"), "us-east-1");
    assert_eq!(region_from_endpoint("s3.eu-west-1.amazonaws.com"), "eu-west-1");
    assert_eq!(region_from_endpoint("https://s3-ap-southeast-2.amazonaws.com/"), "ap-southeast-2");
    assert_eq!(region_from_endpoint("https://acc.r2.cloudflarestorage.com"), "us-east-1");
    assert_eq!(region_from_endpoint("minio.local:9000"), "us-east-1");
}

#[test]
fn converts_path_prefixes() {
    let ok = |prefix: &str| {
        let (template, dropped) = convert_prefix(prefix);
        assert!(dropped.is_empty(), "{prefix}: {dropped:?}");
        template
    };
    assert_eq!(ok("ShareX/%y/%mo"), "ShareX/{year}/{month}/{filename}.{ext}");
    assert_eq!(ok("/uploads/%y-%mo-%d/"), "uploads/{year}-{month}-{day}/{filename}.{ext}");
    assert_eq!(ok("%guid"), "{uuid}/{filename}.{ext}");
    assert_eq!(ok("img/%ra{10}_%rna{4}.%rx{8}/%rn"), "img/{random}_{random}.{random}/{random}/{filename}.{ext}");
    assert_eq!(ok("%ra"), "{random}/{filename}.{ext}");
    assert_eq!(ok(""), "{filename}.{ext}");
    assert_eq!(ok("//"), "{filename}.{ext}");

    let dropped = |prefix: &str| convert_prefix(prefix);
    assert_eq!(dropped("ShareX/%y/%h"), (DEFAULT_OBJECT_PATH_TEMPLATE.to_string(), vec!["%h".to_string()]));
    // Longest token first: %mon isn't %mo + "n", %yy isn't %y twice.
    assert_eq!(dropped("%mon/%yy/%mi").1, vec!["%mon", "%yy", "%mi"]);
    assert_eq!(dropped("%unix %pn").1, vec!["%unix", " ", "%pn"]);
    assert_eq!(dropped("%zz").1, vec!["%"]);
}

#[test]
fn reads_s3_settings() {
    let config = serde_json::json!({
        "AmazonS3Settings": {
            "AccessKeyID": "AKIA1",
            "SecretAccessKey": fake_sealed("secret1"),
            "Endpoint": "s3.eu-west-1.amazonaws.com",
            "Region": "",
            "UsePathStyle": false,
            "Bucket": "shots",
            "ObjectPrefix": "ShareX/%y/%mo",
            "UseCustomCNAME": false,
            "CustomDomain": "",
            "RemoveExtensionImage": true
        }
    });
    let parsed = parse_uploaders_config(&config.to_string(), &fake_dpapi).unwrap();
    assert_eq!(parsed.destinations.len(), 1);
    let s3 = &parsed.destinations[0];
    assert_eq!(s3.kind, UploaderKind::AmazonS3);
    assert_eq!(s3.config.name, "ShareX: shots");
    assert_eq!(s3.config.preset, ProviderPreset::AmazonS3);
    assert_eq!(s3.config.endpoint, "https://s3.eu-west-1.amazonaws.com");
    assert_eq!(s3.config.region, "eu-west-1");
    assert_eq!(s3.config.public_base_url, "https://s3.eu-west-1.amazonaws.com/shots");
    assert_eq!(s3.sharex_base_url.as_deref(), Some("https://s3.eu-west-1.amazonaws.com/shots"));
    assert_eq!(s3.config.object_path_template, "ShareX/{year}/{month}/{filename}.{ext}");
    assert_eq!(s3.access_key_id, "AKIA1");
    assert_eq!(s3.secret, Secret::Known("secret1".into()));
    assert_eq!(s3.notes.len(), 1, "{:?}", s3.notes);
    assert!(parsed.skipped.is_empty());

    // R2 with a custom domain.
    let r2 = serde_json::json!({ "AmazonS3Settings": {
        "AccessKeyID": "k", "SecretAccessKey": "plain", "Endpoint": "https://0123abcd.r2.cloudflarestorage.com/", "Bucket": "files",
        "UseCustomCNAME": true, "CustomDomain": "cdn.example.com/", "ObjectPrefix": "%h"
    }});
    let parsed = parse_uploaders_config(&r2.to_string(), &no_dpapi).unwrap();
    let r2 = &parsed.destinations[0];
    assert_eq!(r2.config.preset, ProviderPreset::CloudflareR2);
    assert_eq!(r2.config.account_id.as_deref(), Some("0123abcd"));
    assert_eq!(r2.config.region, "auto");
    assert_eq!(r2.config.endpoint, "https://0123abcd.r2.cloudflarestorage.com");
    assert_eq!(r2.config.public_base_url, "https://cdn.example.com");
    assert_eq!(r2.config.object_path_template, DEFAULT_OBJECT_PATH_TEMPLATE);
    assert_eq!(r2.secret, Secret::Known("plain".into()));
    assert_eq!(r2.notes.len(), 1);

    // ShareX syntax in the domain, a key from another PC.
    let syntax = serde_json::json!({ "AmazonS3Settings": {
        "AccessKeyID": "k", "SecretAccessKey": fake_sealed("x"), "Endpoint": "nyc3.digitaloceanspaces.com", "Bucket": "b",
        "UseCustomCNAME": true, "CustomDomain": "https://$prompt$.example.com", "UsePathStyle": true
    }});
    let parsed = parse_uploaders_config(&syntax.to_string(), &no_dpapi).unwrap();
    let spaces = &parsed.destinations[0];
    assert_eq!(spaces.config.preset, ProviderPreset::DigitalOceanSpaces);
    assert!(spaces.config.force_path_style);
    assert_eq!(spaces.config.public_base_url, "");
    assert_eq!(spaces.sharex_base_url, None);
    assert_eq!(spaces.secret, Secret::Unreadable);
    assert_eq!(spaces.notes.len(), 2);
    // ShareX's default prefix when it has none written.
    assert_eq!(spaces.config.object_path_template, "ShareX/{year}/{month}/{filename}.{ext}");

    // Not set up: nothing.
    let empty = serde_json::json!({ "AmazonS3Settings": { "ObjectPrefix": "ShareX/%y/%mo" } });
    assert!(parse_uploaders_config(&empty.to_string(), &no_dpapi).unwrap().destinations.is_empty());
    assert!(parse_uploaders_config("not json", &no_dpapi).is_err());
    assert!(parse_uploaders_config("[]", &no_dpapi).is_err());
}

#[test]
fn picks_presets() {
    assert_eq!(preset_for("acc.r2.cloudflarestorage.com"), ProviderPreset::CloudflareR2);
    assert_eq!(preset_for("s3.us-west-004.backblazeb2.com"), ProviderPreset::BackblazeB2);
    assert_eq!(preset_for("fra1.digitaloceanspaces.com"), ProviderPreset::DigitalOceanSpaces);
    assert_eq!(preset_for("s3.amazonaws.com"), ProviderPreset::AmazonS3);
    assert_eq!(preset_for("minio.example.com"), ProviderPreset::CustomS3);
    assert_eq!(preset_for("notamazonaws.com"), ProviderPreset::CustomS3);
}

#[test]
fn reads_b2_settings() {
    let config = serde_json::json!({
        "B2ApplicationKeyId": "0041",
        "B2ApplicationKey": fake_sealed("K004"),
        "B2BucketName": "my-shots",
        "B2UploadPath": "ShareX/%y/%mo",
        "B2UseCustomUrl": false,
        "B2CustomUrl": "https://example.com"
    });
    let parsed = parse_uploaders_config(&config.to_string(), &fake_dpapi).unwrap();
    let mut b2 = parsed.destinations[0].clone();
    assert_eq!(b2.kind, UploaderKind::BackblazeB2);
    assert_eq!(b2.config.name, "ShareX: my-shots (B2)");
    assert_eq!(b2.config.preset, ProviderPreset::BackblazeB2);
    assert_eq!(b2.config.endpoint, "");
    assert_eq!(b2.secret, Secret::Known("K004".into()));
    let answer = serde_json::json!({ "accountId": "a", "apiInfo": { "storageApi": {
        "apiUrl": "https://api004.backblazeb2.com", "downloadUrl": "https://f004.backblazeb2.com", "s3ApiUrl": "https://s3.us-west-004.backblazeb2.com"
    }}});
    let authorization = parse_b2_authorization(&answer).unwrap();
    apply_b2_authorization(&mut b2, &authorization);
    assert_eq!(b2.config.endpoint, "https://s3.us-west-004.backblazeb2.com");
    assert_eq!(b2.config.region, "us-west-004");
    assert_eq!(b2.config.public_base_url, "https://f004.backblazeb2.com/file/my-shots");
    assert_eq!(b2.sharex_base_url.as_deref(), Some("https://f004.backblazeb2.com/file/my-shots"));
    assert_eq!(parse_b2_authorization(&serde_json::json!({ "apiInfo": {} })), None);

    // A custom URL stays, whatever Backblaze says.
    let custom = serde_json::json!({ "B2ApplicationKeyId": "1", "B2BucketName": "b", "B2UseCustomUrl": true, "B2CustomUrl": "files.example.com/" });
    let mut b2 = parse_uploaders_config(&custom.to_string(), &no_dpapi).unwrap().destinations[0].clone();
    assert_eq!(b2.config.public_base_url, "https://files.example.com");
    assert_eq!(b2.secret, Secret::Missing);
    apply_b2_authorization(&mut b2, &authorization);
    assert_eq!(b2.config.public_base_url, "https://files.example.com");

    // An endpoint typed in the review.
    apply_typed_endpoint(&mut b2, "s3.eu-central-003.backblazeb2.com/");
    assert_eq!(b2.config.endpoint, "https://s3.eu-central-003.backblazeb2.com");
    assert_eq!(b2.config.region, "eu-central-003");
}

#[test]
fn lists_what_is_skipped_and_reads_shorteners() {
    let config = serde_json::json!({
        "FTPAccountList": [{ "Name": "Home" }],
        "ImgurOAuth2Info": { "Client_ID": "x" },
        "DropboxOAuth2Info": null,
        "LocalhostAccountList": [],
        "OwnCloudHost": "",
        "CustomUploadersList": [
            {
                "Name": "My shortener",
                "DestinationType": "URLShortener",
                "RequestMethod": "POST",
                "RequestURL": "https://s.example.com/api/shorten",
                "Headers": { "Authorization": "Bearer abc123" },
                "Body": "JSON",
                "Data": "{\"url\":\"{input}\"}",
                "URL": "{json:link}"
            },
            { "Name": "My host", "DestinationType": "ImageUploader, FileUploader", "RequestURL": "https://up.example.com" },
            { "Name": "Flags", "DestinationType": 8, "RequestURL": "https://t.example.com/new?url={input}", "URL": "{json:short}" },
            { "Name": "Broken", "DestinationType": "URLShortener" }
        ]
    });
    let parsed = parse_uploaders_config(&config.to_string(), &no_dpapi).unwrap();
    let skipped: Vec<&str> = parsed.skipped.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(skipped, ["FTP", "Imgur", "My host", "Broken"]);
    assert_eq!(parsed.shorteners.len(), 2);
    let shortener = &parsed.shorteners[0];
    assert_eq!(shortener.review_id, "shortener-0");
    assert_eq!(shortener.import.name.as_deref(), Some("My shortener"));
    assert_eq!(shortener.import.host, "s.example.com");
    assert_eq!(shortener.import.token.as_deref(), Some("abc123"));
    assert_eq!(parsed.shorteners[1].review_id, "shortener-2");
}

#[test]
fn matches_history_urls_like_try_get_object_key() {
    let base = "https://s3.eu-west-1.amazonaws.com/shots";
    assert_eq!(object_key_under(base, "https://s3.eu-west-1.amazonaws.com/shots/ShareX/2026/11/a%20b.png").as_deref(), Some("ShareX/2026/11/a b.png"));
    assert_eq!(object_key_under(base, "HTTPS://S3.EU-WEST-1.AMAZONAWS.COM/shots/x.png").as_deref(), Some("x.png"));
    assert_eq!(object_key_under(base, "https://s3.eu-west-1.amazonaws.com:443/shots/x.png").as_deref(), Some("x.png"));
    assert_eq!(object_key_under(base, "http://s3.eu-west-1.amazonaws.com/shots/x.png"), None);
    assert_eq!(object_key_under(base, "https://s3.eu-west-1.amazonaws.com:8443/shots/x.png"), None);
    assert_eq!(object_key_under(base, "https://s3.eu-west-1.amazonaws.com/shotsx/x.png"), None);
    assert_eq!(object_key_under(base, "https://s3.eu-west-1.amazonaws.com/shots/"), None);
    assert_eq!(object_key_under(base, "https://i.imgur.com/abc.png"), None);
    assert_eq!(object_key_under("https://cdn.example.com/", "https://cdn.example.com/2026/%C3%A7.png").as_deref(), Some("2026/ç.png"));
    assert_eq!(object_key_under("https://cdn.example.com", "not a url"), None);
}

#[test]
fn reads_history_json_and_dates() {
    let older = "{\r\n  \"FileName\": \"a.png\",\r\n  \"DateTime\": \"2026-11-09T12:00:00.1234567+03:00\",\r\n  \"URL\": \"https://cdn.example.com/a.png\"\r\n},\r\n{\r\n  \"FileName\": \"b.txt\",\r\n  \"URL\": \"\"\r\n}";
    let entries = parse_history_json(older);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file_name, "a.png");
    assert_eq!(entries[0].date_time, Some(1_794_214_800_123));
    assert_eq!(parse_history_json("[{\"URL\":\"https://x/y\"}]").len(), 1);
    assert!(parse_history_json("").is_empty());
    assert!(parse_history_json("{broken").is_empty());
    assert_eq!(parse_sharex_date("2026-11-09T09:00:00Z"), Some(1_794_214_800_000));
    assert!(parse_sharex_date("2026-11-09T09:00:00.5").is_some());
    assert_eq!(parse_sharex_date("yesterday"), None);
}

#[test]
fn reads_history_db_with_its_wal() {
    let folder = temp_folder("history");
    let path = folder.join("History.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS History (Id INTEGER PRIMARY KEY AUTOINCREMENT, FileName TEXT, FilePath TEXT, DateTime TEXT, Type TEXT, Host TEXT, URL TEXT, ThumbnailURL TEXT, DeletionURL TEXT, ShortenedURL TEXT, Tags TEXT);
             INSERT INTO History (FileName, DateTime, URL, Tags) VALUES ('a.png', '2026-11-09T09:00:00.0000000Z', 'https://cdn.example.com/a.png', '{}');
             INSERT INTO History (FileName, DateTime, URL) VALUES ('none', '2026-11-09T09:00:00Z', NULL);",
        )
        .unwrap();
    // Still open (as when ShareX runs): the rows may be only in the -wal.
    let entries = read_history(&path).unwrap();
    assert_eq!(entries, vec![HistoryEntry { file_name: "a.png".into(), date_time: Some(1_794_214_800_000), url: "https://cdn.example.com/a.png".into() }]);
    drop(connection);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn reads_picked_files_and_backups() {
    let folder = temp_folder("picked");
    let json = folder.join("UploadersConfig.json");
    std::fs::write(&json, "\u{feff}{\"a\":1}").unwrap();
    assert_eq!(read_picked(&json).unwrap(), "{\"a\":1}");

    let backup = folder.join("ShareX-backup.sxb");
    {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&backup).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("ApplicationConfig.json", options).unwrap();
        std::io::Write::write_all(&mut writer, b"{}").unwrap();
        writer.start_file("UploadersConfig.json", options).unwrap();
        std::io::Write::write_all(&mut writer, b"{\"b\":2}").unwrap();
        writer.finish().unwrap();
    }
    assert_eq!(read_picked(&backup).unwrap(), "{\"b\":2}");
    let not_zip = folder.join("other.sxb");
    std::fs::write(&not_zip, "nope").unwrap();
    assert!(read_picked(&not_zip).is_err());

    let big = folder.join("big.json");
    std::fs::write(&big, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
    assert!(read_picked(&big).is_err());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn compares_endpoints() {
    assert_eq!(comparable_endpoint("https://S3.Example.com/"), "s3.example.com");
    assert_eq!(comparable_endpoint("s3.example.com"), "s3.example.com");
}
