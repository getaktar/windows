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
- Multiple destinations, switchable per upload
- Upload history with search, thumbnails, and previews (images, PDFs, text,
  Markdown)
- Copy the link as a plain URL, Markdown, HTML, or a custom template
- Delete the remote file straight from the history view
- Browse each bucket folder by folder, including files uploaded elsewhere:
  search the whole bucket, preview, copy links or temporary links, upload
  into a folder, create folders, rename, move, and delete
- `aktar://` links and the same local API as the Mac app, for the Raycast
  extension (opt-in, see Settings > Integrations)
- Launch at sign-in, automatic updates, light and dark mode
- In English, Turkish, German, French, Spanish, Brazilian Portuguese,
  Japanese, and Simplified Chinese

## Privacy & security

Your storage credentials are kept in Windows Credential Manager and never
leave your PC except in direct requests to the S3-compatible endpoint you
configure. Aktar has no backend, no telemetry, and no account system. The
only other request it makes is the update check, which downloads
`latest.json` from this repository's latest GitHub release and sends no
information about you or your PC; you can turn it off in Settings. Every
update is verified against a public key built into the app before it's
installed.

If you turn on Settings > Integrations > Allow local connections (off by
default, and what the Raycast extension uses), Aktar also listens on
`127.0.0.1` for requests carrying a random token that is kept in Credential
Manager. It never accepts connections from other machines or from web pages.
See [SECURITY.md](SECURITY.md) for the disclosure policy.

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
pnpm tauri dev      # run the app
pnpm tauri build    # build the installer into src-tauri/target/release/bundle/nsis
```

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
