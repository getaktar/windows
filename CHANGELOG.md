# Changelog

All notable changes to Aktar for Windows are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1] - 2026-10-03

### Fixed

- Deleting the files of a large batch that was waiting for "Upload" or
  "Skip" now withdraws the question. Before, the question stayed until
  Aktar restarted, and the folder held every new file back behind it
  instead of uploading it

## [0.4.0] - 2026-10-02

### Added

- Watched folders (Settings > Watched Folders): files that land in a folder
  you pick upload on their own. "Upload Screenshots Automatically" watches
  Windows' Screenshots folder and copies each screenshot's link. Each folder
  has its own destination, path, link and "Delete after", which files it
  takes (all, images, videos, or your own patterns, sizes, and subfolders),
  what happens when a file changes (ignore it, upload it again, or replace
  the upload and keep its link), what happens to the original afterwards
  (keep it, move it to the Recycle Bin, or into an "Uploaded" subfolder),
  and whether the link is copied and how you're notified. Aktar waits
  until a file is completely written, skips partial downloads and
  temporary files, notices renames, asks before uploading more than 50
  files at once, retries uploads that failed on the network, and picks up
  what arrived while it was closed or paused
- Pause watching for an hour, until tomorrow, or until you resume, from
  Settings, the panel, or the tray menu; or automatically on battery power
  or a metered network
- Webhooks and scripts for a watched folder, run after each upload with its
  details as JSON
- "When a file is deleted" for each watched folder: keep the uploaded file
  (the default), or delete it from the bucket too, a few seconds after the
  file is deleted from the folder. Not when the upload reused an earlier
  upload's link or another file still uses it, never for Aktar's own moves
  to the Recycle Bin or "Uploaded". "Ask before deleting" (on by default)
  asks about every deletion in the panel, Settings, and a notification, and
  nothing is deleted until you answer; with it off, deleting more than 50
  files at once still asks first. A file saved in place by deleting and
  writing it again, or renamed, keeps its upload
- "Watch with Aktar" in File Explorer's right-click menu for folders. The
  Microsoft Store version now has "Upload with Aktar" and "Watch with
  Aktar" in the menu too
- `{folder}` and `{subpath}` in the object path: the watched folder's name
  and the file's folder inside it
- The Library shows which watched folder an upload came from, with a
  filter for them
- Local API: `GET /v1/watched-folders`, `POST /v1/watched-folders/pause`,
  `/resume` and `/{id}` (`{"enabled": false}`), and `watching` in
  `/v1/status`. Links: `aktar://watch`, `aktar://watch/pause?minutes=60`,
  `aktar://watch/resume`

### Changed

- The local API's upload reply also has `reused` inside `upload`, where the Mac app puts it

## [0.3.0] - 2026-10-01

### Added

- Show QR Code for an upload, in the Library (a row's right-click menu,
  the details pane) and in the panel's recent uploads: a sharp QR code of
  the link that would be copied (a fresh temporary link when the
  destination copies those), with Copy Image and Save Image as PNG
- Reuse links for duplicate files (Settings > General, on by default):
  uploading a file that's already in the same destination, with the same
  "Delete after", copies the existing link instead of uploading it again.
  Not for folders keeping their structure, ZIPs made from folders, or
  uploads to a chosen name (the bucket browser, the local API's prefix). The
  local API's upload response says `"reused": true` when that happened
- `{md5}` and `{sha256}` in the object path: the file's checksum, for
  names that only change when the contents do
- Rename before upload: hold Alt while dropping files on the panel (or
  clicking Browse), or set the new "Rename and upload clipboard" shortcut
  in Settings, and name each upload before it goes up. The extension stays
- Image Processing for each destination: convert photos and screenshots to
  WebP or AVIF, recompress them (90%, 80% or 65%), and scale them down to a
  longest side of 3840 to 1024 px. Off by default. Covers JPEG, PNG, HEIC
  (with Windows' HEIF Image Extensions), WebP, TIFF and BMP; GIFs, SVGs
  and files inside ZIPs are left alone. The image metadata setting still
  decides what EXIF the new file keeps, and the color profile is kept
- Big files go up in parts, up to 4 at a time, so there's no 5 GB limit
  any more. A part that fails on a bad connection is tried again, Retry
  continues from the parts already sent, and so does uploading the same
  file again after Aktar was closed ("Resuming upload…"). Cancel throws the
  sent parts away; unfinished uploads are cleaned up after 7 days

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
