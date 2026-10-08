# Changelog

All notable changes to Aktar for Windows are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0] - 2026-10-08

### Security

- Thumbnails of files in a bucket no longer use the file's name on disk:
  a name with `\`, `..` or a drive letter, which someone else with access
  to the bucket could give a file, could make Aktar write that file
  anywhere in your user folder. It's now downloaded under a fixed name in
  Aktar's temp folder, and only pictures, videos, documents and audio
  files are downloaded for a thumbnail at all
- Names in the bucket (the Library, the local API, the thumbnail folder)
  can no longer contain `\` or `:`
- Import from Another Device now shows where the destination sends things
  before it's saved (endpoint, bucket, public URL, short link, webhook
  hosts, "Use For" rules, custom template), also when updating one that's
  already here. Webhooks and "Use For" rules are only imported when you
  turn them on there
- Watched folders check again that a file is still inside the folder right
  before it goes to the Recycle Bin or the Uploaded folder, refuse an
  Uploaded folder that's a link to somewhere else, and never replace a
  file that appears in Uploaded at the same moment. A folder that's a
  link into Aktar's own or temp folders can't be watched
- `aktar://watch/resume` links now ask before resuming watched folders you
  paused
- Short links from a shortener must be http:// or https:// links, and
  links in HTML output are escaped
- Previews only download from your destinations' addresses and links in
  History
- The local API has a new `GET /v1/hello` that lets the CLI and the
  Raycast extension check they're talking to Aktar before they send your
  token

### Changed

- A PowerShell (`.ps1`) After Upload script gets an empty argument in
  place of a value that starts with `-`, which PowerShell would read as one
  of the script's parameters. The value is still in the `AKTAR_*`
  environment variables and the JSON input

## [0.10.0] - 2026-10-07

### Added

- Clean URLs: `{short}` in a path template becomes a random 7-character
  code (0-9, A-Z, a-z, from the system's secure random generator), for
  links like `files.example.com/A7kdP2x.png`. It never replaces a file:
  on Amazon S3 and Cloudflare R2 the upload is only written if the key is
  new (`If-None-Match: *`, also when completing a multipart upload), on
  other providers the key is checked first; a taken key gets a whole new
  code, and after 5 tries the upload fails with nothing overwritten.
  New destinations use `{year}/{month}/{short}.{ext}`; saved ones keep
  their path. Set Up Cloudflare R2 uses `{short}.{ext}` with a domain of
  your own and the new default with r2.dev
- A Presets menu next to the destination's path: Clean URL (Recommended),
  Short with Date and Original File Name
- A destination whose links use a domain of your own and whose path has
  no `{short}` suggests Clean URLs in its form, with an example on that
  domain; the note can be dismissed for each destination
- Short links with your own shortener: a destination's new Short Links
  section picks Shlink, YOURLS, Kutt, Dub, Short.io or a custom HTTP
  request, with its address, domain and API key (kept in Credential
  Manager), a Test button, "Only shorten links longer than" and "Also
  shorten temporary links" (for shorteners that can expire links; the
  short link expires with the temporary link). After an upload the short
  link is what's copied (URL, Markdown, HTML, and `{url}`, `{shortUrl}`
  and `{longUrl}` in a custom template) and what the QR code shows; an
  expiring upload's short link expires with it. If it can't be created
  the original link is copied and a notification offers Retry. http://
  addresses need "Allow insecure HTTP". Hosted shorteners are marked as
  seeing every link and click. The shorteners are described by the same
  definitions file as in the Mac app
- History shows an upload's short link with its clicks and last click,
  and offers Copy Short Link, Copy Original Link, Create Short Link and
  Delete Short Link (also in the panel's recent uploads); earlier short
  links are listed with their status
- Deleting a file deletes its short links too; one that can't be deleted
  is marked "may still exist" and you're told. Renaming or moving a file
  in the bucket view points its short link at the new path when the
  shortener can (and keeps the old file if that fails); otherwise it
  warns first. A reused duplicate reuses its short link, Replace File
  keeps it, and Remove from History forgets an upload's short link
  records (the short links themselves are left alone)
- Import ShareX Configuration (.sxcu) in the Short Links section reads a
  ShareX custom URL shortener into a custom HTTP request. It first shows
  where your token will be sent (host, method, endpoint, and which
  headers or parameters carry it); secrets found in the file are moved to
  Credential Manager. Configurations that use regular expressions,
  `{response}`, file fields, `{select}`, `{prompt}` or other ShareX
  syntax are refused with the reason, and http:// needs "Allow insecure
  HTTP". Testing a custom HTTP shortener with a delete request deletes
  the test link again, and says whether that worked
- Local API: uploads have `shortUrl` (null without one) and their formats
  use it; `short=1` or `short=0` on an upload overrides the destination
  for that upload; `GET` and `POST /v1/uploads/{id}/short-link` read
  (with clicks) or create an upload's short link; moving an object
  reports `shortLinkStatus`, and deleting one cleans up its uploads'
  short links
- Webhooks and scripts after an upload get `upload.shortUrl` (null
  without one), also for watched folders
- Share to Another Device carries the destination's short link settings
  and token, in the same format as the Mac app

### Fixed

- Import from Another Device keeps the destination's Cloudflare API token
  that came with it

## [0.9.0] - 2026-10-06

### Added

- Set Up Cloudflare R2: opens Cloudflare's token page with the permissions
  filled in, and with the pasted token creates the bucket (or uses an
  existing one), turns on public links (the r2.dev address or a domain on
  the Cloudflare account), saves the destination with keys made from the
  token, and tests it. The token itself isn't stored. It's the main button
  in Settings > Destinations when there's no destination yet

## [0.8.0] - 2026-10-05

### Added

- Nine more languages: Traditional Chinese, Korean, Italian, Dutch, Polish,
  Russian, Ukrainian, Indonesian and Vietnamese

### Fixed

- The confirmation for an `aktar://upload-clipboard` link no longer says
  "from the clipboard" twice when the clipboard holds an image

## [0.7.0] - 2026-10-05

### Added

- "Use for" in a destination's settings: pick the kinds of files (images,
  videos, audio, documents, archives) and extensions that go there. An
  upload that doesn't name a destination (the clipboard shortcut, the
  panel, File Explorer, Send to, the local API) goes to the destination
  that claims the file's extension, else its kind, else the default one.
  Files of one drop that land in different destinations have their links
  copied together, and the panel says where files go under the destination
  picker
- A keyboard shortcut for each destination that uploads the clipboard
  straight there
- Replace File in History and the bucket view: a new file goes to the same
  key, so its link keeps working. The history entry keeps its date and gets
  the new file's size and thumbnail, with "Replaced" and when. A file under
  a "Delete after" folder starts its days again
- Short cache time for a destination: every upload there is sent with a
  one-minute cache time, so a replaced file shows up everywhere within about
  a minute. And an optional Cloudflare zone ID and API token (Zone > Cache
  Purge) that clear a replaced file from Cloudflare's cache right away, also
  for a watched folder that replaces its upload to keep the link
- After Upload in a destination's settings: webhooks and scripts that run
  after each upload there and each replace, like a watched folder's
  Automation
- Local API: uploads without `destinationId` follow "Use for";
  `POST /v1/uploads/{id}/replace` and `PUT /v1/destinations/{id}/objects`
  replace a file in place; destinations list `useFor`, `shortCache`,
  `hasCloudflarePurge` and `hooks`. The README shows automation with the
  CLI, Power Automate Desktop and Task Scheduler
- Share to Another Device carries "Use for", the cache settings, the
  Cloudflare token and the webhooks (scripts stay on the PC they were
  picked on; shortcuts aren't shared)

### Fixed

- Files uploaded through the local API, from the Raycast extension or the
  CLI, now get thumbnails. Before, they were made without the file's
  extension, so none could be made

## [0.6.0] - 2026-10-05

### Added

- Thumbnails for videos, PDFs, RAW and HEIC photos, Office documents and
  more, made by the same Windows thumbnailer Explorer uses, in History, the
  tray panel and the bucket view. Files already in a bucket get one when
  they're shown (up to 25 MB), and a file's details show its thumbnail when
  there's no other preview
- A Thumbnails setting for each destination: Off (nothing is made or
  downloaded), On This PC (the default) or In the Bucket, which also saves
  them to a folder of your choice in the bucket so your other devices can
  show them. A thumbnail in the bucket is deleted, renamed, moved and
  expires together with its file, and its folder is hidden in the bucket
  view and the local API. Leaving that mode asks whether to delete the
  thumbnails already there. Shared with Share to Another Device
- Videos and audio files play right in a file's details in History and the
  bucket view, streamed from the bucket (private buckets too) without
  downloading them first. Nothing loads until you click Play
- Local API: `GET /v1/uploads/{id}/thumbnail` and
  `GET /v1/destinations/{id}/thumbnail?key=` return a file's thumbnail as
  a PNG (`px` sets its size, `generate=0` only returns one that's at hand),
  for the Raycast extension's icons and previews
- Settings > General shows how much space thumbnails take on this PC, with
  Clear to remove them all (they're made again when shown)

### Changed

- Thumbnails are sharper (512 pixels instead of 320) yet take about a tenth
  of the space, as WebP instead of PNG

## [0.5.1] - 2026-10-04

### Fixed

- "Reuse links for duplicate files" no longer hands out a link that now
  serves a different file. With a path like `{filename}.{ext}`, a file
  uploaded under a name another file has since taken is uploaded again,
  and so is one whose file in the bucket changed size or was written
  after it was uploaded
- A large upload that's picked up where it stopped no longer keeps its old
  name in the bucket after you rename the file or change the destination's
  path. It starts over under the new name, and the unfinished upload is
  removed from the bucket
- "Delete Remote File" no longer reports success while the file stays in
  the bucket when Credential Manager can't be read. The entry is kept and
  the error shown, so you can try again
- "Remove location" and "Remove all" now also clean WebP, AVIF and GIF
  images. A photo that can't be cleaned isn't uploaded, as before. The
  destination form now says that videos keep their location
- Requests to your bucket (listing, deleting, renaming, checking an
  upload) now give up after a minute instead of waiting forever on a
  connection that stopped answering
- Uploads into a folder from the Library or the local API no longer
  overwrite a file that appeared under the same name in the meantime, on
  Amazon S3 and Cloudflare R2. They take the next free name instead, and a
  name with a random suffix after " 999"
- Opening several items in File Explorer's "Upload with Aktar" now uploads
  the ones that are files even when one of them isn't (a library, a
  phone's folder)
- File names in HTML and Markdown output are now escaped, so a name with
  `<`, `&` or brackets no longer breaks the copied markup

### Security

- Images over 100 megapixels are no longer converted, compressed, resized
  or given a thumbnail, so a small file claiming huge dimensions can't use
  up all memory. They go up as they are, still without the metadata the
  destination removes
- "Save Image…" for a QR code now saves only where you pick in its save
  dialog, which Aktar opens itself
- HTML, SVG, XML and JavaScript files are now uploaded to download when
  their link is opened, instead of running as a page on your bucket's
  domain. They still show in an `<img>` where that applies
- `aktar://upload-clipboard` and `aktar://watch/pause` links now ask first,
  saying what would be uploaded and where. Pause minutes from links and
  the local API must be a whole number, up to a year
- Scripts for watched folders can only be picked in Aktar's own file
  dialog, and webhooks only use plain http:// for this PC or your local
  network. Webhooks no longer follow redirects to another host. Scripts
  you already set up keep working
- The Library and the local API no longer accept names with `.` or `..`
  folders, empty folder names, a leading `/` or control characters, and
  file names can no longer add folders to a generated path
- Transfer links now expire after an hour. Links made by older versions
  still open
- A transfer link copied from "Share to Another Device" is cleared from
  the clipboard when the window closes, and it and the local API token
  are kept out of Windows' clipboard history
- Previews download at most 25 MB, only follow redirects on the same
  host, and skip larger files up front
- The local API refuses bodies over 5 GB and drops a connection that sends
  nothing for a minute. Windows can't upload Aktar's own data files

## [0.5.0] - 2026-10-03

### Added

- Move a destination to another device: "Share to Another Device…" in a
  destination's menu shows a QR code and a transfer code; on the other
  device (Mac, Windows, iPhone or Android), "Import from Another Device…"
  scans the code with the camera or takes the pasted link, asks for the
  transfer code (formatted as XXXX-XXXX-XXXX as you type), and adds the
  destination right away, keys included. If it's already there, you pick
  "Update Existing" or "Add as Copy", with a warning when the update would
  upload somewhere else. The connection is then tested on its own and the
  result shown, with "Edit" to open the destination form; a failed test
  leaves the destination saved. The keys are encrypted with the transfer
  code, which is never part of the QR code or the link, and the window
  closes itself after 10 minutes. Also on the Welcome window, and through
  `aktar://import` links, which never import anything without the code

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
