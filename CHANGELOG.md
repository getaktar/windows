# Changelog

All notable changes to Aktar for Windows are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-01

Catches up with Aktar for Mac 0.9.1.

### Added

- "Upload with Aktar" in File Explorer's right-click menu for files and
  folders (under "Show more options" on Windows 11), and Aktar in the "Send
  to" menu. Not in the Microsoft Store version yet, which can't add them
- Upload profiles: each destination keeps its own "Copy as" (URL, Markdown,
  HTML or custom), "Link", "Delete after", "Image metadata" and "Folders",
  set in the destination's Upload Defaults. The panel's "Delete after" and
  "Link" choices are saved for the selected destination. Duplicate in a
  destination's menu in Settings starts another profile on the same bucket
  with the same keys
- A "Link" choice in the panel and in Upload Defaults: copy the public URL,
  or a temporary link valid for 5 minutes, 15 minutes, 1 hour, 24 hours or
  7 days, which also works for private buckets
- Copy Temporary Link for uploads in the panel's recent list and in the
  Library, with the same durations as the bucket browser
- Image metadata: photos lose their GPS location before they're uploaded
  (the default), lose all metadata (camera, lens, date, location), or are
  uploaded as they are. Orientation and color profile are kept and the
  image isn't recompressed. If the metadata can't be removed, the upload
  stops instead of sharing the location. Covers JPEG, HEIC, PNG and TIFF,
  from every way of uploading
- Folder uploads: drop a folder on the panel, send it from File Explorer,
  or copy it and use the shortcut. It goes up as one ZIP (the default) or
  file by file with its subfolders under a new folder in the bucket, with
  all the links copied at once when it's done. Hidden files such as .env,
  .git, desktop.ini and Thumbs.db are left out either way, and photos lose
  their metadata inside a ZIP too. A folder dropped into the bucket browser
  keeps its structure, and the local API always zips
- Test Connection shows each step on its own line, with the HTTP status
  when the test file's public link doesn't open and what to do about it

## [0.1.2] - 2026-09-30

The first Windows versions, with the features of Aktar for Mac 0.4.1 and
its expiring uploads (Mac 0.5.0 and 0.5.1).

### Added

- Upload panel in the notification area: drag & drop, clipboard paste (files
  copied in File Explorer, or images), or file picker
- Global "paste & upload" shortcut (default Ctrl+Shift+Alt+U), customizable
  in Settings > General
- Bring-your-own S3-compatible storage (Amazon S3, Cloudflare R2, Backblaze B2,
  DigitalOcean Spaces, MinIO, or any other S3-compatible endpoint), with
  credentials kept in Windows Credential Manager
- "Use path-style addressing" for S3-compatible servers without a
  subdomain per bucket
- Multiple destinations, switchable per upload
- Upload progress, and cancelling or removing uploads from the panel and
  the Library
- Upload history with search, thumbnails, and previews (images, PDFs, text,
  Markdown)
- Copy link as URL, Markdown, HTML, or a custom template
- Bucket browser in the Library window: browse each destination's bucket
  folder by folder, search the whole bucket, preview files, copy links or
  temporary links, upload into a folder, create folders, rename or move
  files, and delete them
- Raycast integration: the same opt-in local API as the Mac app (127.0.0.1
  only, token-protected), and `aktar://` links (`upload-clipboard`,
  `library`, `settings`, `connect`)
- Expiring uploads: "Delete after" 1, 7, 14, or 30 days next to the
  destination picker, available once the destination's bucket has Aktar's
  lifecycle rules (set up from the picker or the destination's settings,
  keeping the bucket's other rules exactly as they are, including ones like
  R2's default multipart rule). They go under `tmp/{N}d/` and the bucket
  deletes them. "Turn Off..." in the destination's settings stops offering
  it, optionally removing Aktar's rules from the bucket. The local API takes
  `expires=`
- Automatic updates from GitHub releases, verified before installing, and
  installed while Aktar isn't in use
- Launch at sign-in
- A Microsoft Store (MSIX) package alongside the installer, updated by the
  Store
- English, Turkish, German, French, Spanish, Brazilian Portuguese, Japanese,
  and Simplified Chinese. The language can be changed in Settings without
  restarting
