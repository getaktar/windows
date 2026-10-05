# Aktar for Windows

<p align="center">
  <img src="docs/logo.png" alt="Aktar logo" width="120">
</p>

<p align="center"><strong>Your files. Your storage. One shortcut away.</strong></p>

A Windows notification area app for uploading files to your own
S3-compatible storage. Drag a file onto the panel, paste from the clipboard,
or press a shortcut from anywhere, and get a public link back on your
clipboard. This is the Windows version of [Aktar for Mac](https://github.com/getaktar/mac).

## Features

- Lives in the notification area: click the icon for the upload panel,
  right-click for the menu
- Drag & drop, clipboard paste (files copied in File Explorer, or images
  like screenshots), or file picker to upload
- Global "paste & upload" shortcut (default `Ctrl+Shift+Alt+U`), customizable
- Bring your own storage: Amazon S3, Cloudflare R2, Backblaze B2,
  DigitalOcean Spaces, MinIO, or any other S3-compatible endpoint
- Multiple destinations, switchable per upload; each can claim kinds of
  files and extensions ("Use for"), so screenshots, builds and documents
  go to their own destination on their own, and can have its own shortcut
- Replace a file in place from History or the bucket view: same key, same
  link, with an optional one-minute cache time and Cloudflare cache purge
  so the new version shows up right away
- Webhooks and scripts after each upload to a destination ("After Upload")
- Upload history with search, thumbnails (photos, videos, PDFs, documents),
  and previews (images, PDFs, text, Markdown, video and audio playback)
- Thumbnails per destination: off, on this PC, or also in the bucket so
  other devices can show them
- Copy the link as a plain URL, Markdown, HTML, or a custom template
- Delete the remote file straight from the history view
- Browse each bucket folder by folder, including files uploaded elsewhere:
  search the whole bucket, preview, copy links or temporary links, upload
  into a folder, create folders, rename, move, and delete
- Watched folders: files that land in a folder you pick (or Windows'
  Screenshots folder) upload on their own, with that folder's destination,
  path, link, filters, and what happens to the original; webhooks and
  scripts can run after each upload
- `aktar://` links and the same local API as the Mac app, for the Raycast
  extension (opt-in, see Settings > Integrations)
- Launch at sign-in, automatic updates, light and dark mode
- In English, Turkish, German, French, Spanish, Brazilian Portuguese,
  Japanese, Simplified and Traditional Chinese, Korean, Italian, Dutch, Polish,
  Russian, Ukrainian, Indonesian, and Vietnamese

## Privacy & security

Your storage credentials are kept in Windows Credential Manager and never
leave your PC except in direct requests to the S3-compatible endpoint you
configure. Aktar has no backend, no telemetry, and no account system. The
only other request it makes is the update check, which downloads
`latest.json` from this repository's latest GitHub release and sends no
information about you or your PC; you can turn it off in Settings. Every
update is verified against a public key built into the app before it's
installed. A watched folder's or destination's webhooks only send upload details to
the URLs you add to it, and the optional Cloudflare token (Zone > Cache
Purge only) is kept in Credential Manager with the destination's keys and
only sent to Cloudflare's API to clear a replaced file's link.

If you turn on Settings > Integrations > Allow local connections (off by
default, and what the Raycast extension uses), Aktar also listens on
`127.0.0.1` for requests carrying a random token that is kept in Credential
Manager. It never accepts connections from other machines or from web pages.
See [SECURITY.md](SECURITY.md) for the disclosure policy.

## Automation

The command line and the local API are Aktar's automation surface on
Windows. Turn on Settings > Integrations > Allow local connections, copy the
token, and install the CLI (`npm install -g @getaktar/cli`, then
`aktar login`), or call the API on `http://127.0.0.1:47913/v1/` with
`Authorization: Bearer <token>`:

| Request | What it does |
|---|---|
| `POST /v1/uploads?filename=` | Uploads the request body. Without `destinationId`, it goes where the file's kind or extension says ("Use for"), else to the default destination |
| `POST /v1/uploads/clipboard` | Uploads the clipboard, routed the same way |
| `POST /v1/uploads/{id}/replace?filename=` | Replaces an upload's file in place; the link stays |
| `PUT /v1/destinations/{id}/objects?key=&filename=` | Replaces the file at a key in the bucket |
| `GET /v1/uploads`, `GET /v1/destinations` | History and destinations (with `useFor`, `shortCache`, `hasCloudflarePurge`, `hooks`) |

Every upload reply has the links in all copy formats (`formats.url`,
`formats.markdown`...).

**Power Automate Desktop**: add a *Run PowerShell script* action, for
example to upload every PDF in a folder and collect the links:

```powershell
Get-ChildItem "$env:USERPROFILE\Documents\Invoices\*.pdf" | ForEach-Object {
  aktar upload $_.FullName --json | ConvertFrom-Json | Select-Object -ExpandProperty url
}
```

**Task Scheduler**: a task that runs `powershell.exe` with
`-NoProfile -Command "aktar upload C:\Reports\daily.csv"` uploads the day's
report on a schedule; replacing it in place keeps one link that always shows
the latest version:

```powershell
$token = "<token from Settings > Integrations>"
$headers = @{ Authorization = "Bearer $token" }
Invoke-RestMethod -Method Put -Headers $headers -InFile C:\Reports\daily.csv `
  -Uri "http://127.0.0.1:47913/v1/destinations/<destination ID>/objects?key=reports/daily.csv"
```

## Requirements

- Windows 10 (1809 or later) or Windows 11, x64
- Microsoft Edge WebView2 (built into Windows 11; the installer adds it on
  Windows 10 if it's missing)

## Building

Aktar for Windows is a [Tauri 2](https://tauri.app) app: the core (uploads,
storage, credentials, history, the local API) is Rust in `src-tauri/`, and
the windows are React with [Fluent UI](https://react.fluentui.dev) in `src/`.

You need [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 22 or
later, and [pnpm](https://pnpm.io). On Windows, also the "Desktop
development with C++" workload from the Visual Studio Build Tools.

```powershell
pnpm install
pnpm tauri dev           # run the app
pwsh scripts/build.ps1   # build the installer into src-tauri/target/release/bundle/nsis
```

Build anything you'll share with `scripts/build.ps1` rather than a plain
`pnpm tauri build`: Rust embeds source paths in the binary, and the script
maps your home folder out of them, then checks that `aktar.exe` doesn't
contain your user name.

### Microsoft Store package

The Store gets an MSIX of the same `aktar.exe`. Inside the package, Aktar
leaves updates to the Store, uses the package's startup task for "Launch at
login", and shows notifications under the package's identity.

```powershell
pwsh scripts/pack_msix.ps1 -Build            # unsigned, for Partner Center
pwsh scripts/pack_msix.ps1 -SignForTesting   # test-signed, to install locally
```

The package lands in `src-tauri/target/msix`. The manifest is
`packaging/msix/AppxManifest.xml`; the identity it's built with (from
Partner Center > Product identity) is in `packaging/msix/identity.json`,
and the logos come from `scripts/make_msix_assets.py`.

The UI can also be worked on in a regular browser on any OS, against a
fake backend with sample data:

```bash
pnpm dev:mock       # then open http://localhost:1420/#/library
```

Routes are `#/panel`, `#/library`, `#/settings`, `#/onboarding`, and
`#/update`; add `?lang=tr` (or any other language code) before the `#` to
preview a translation.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and our [Code of Conduct](CODE_OF_CONDUCT.md).
Changes are tracked in [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)

## Other platforms

macOS, iOS, and Android clients: see the
[getaktar organization](https://github.com/getaktar).
