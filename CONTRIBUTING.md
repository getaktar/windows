# Contributing

Please read our [Code of Conduct](CODE_OF_CONDUCT.md) before participating.

## Setup

```bash
pnpm install
pre-commit install   # optional: gitleaks scan of staged changes
```

`pnpm tauri dev` runs the app. Most UI work can happen in a browser on any
OS with `pnpm dev:mock` (a fake backend with sample data, see
`src/dev/mockBackend.ts`).

## Layout

- `src-tauri/src/` - the Rust core: uploads (`uploads.rs`), storage
  (`storage.rs`), credentials (`credentials.rs`), history (`history.rs`),
  the tray and panel (`tray.rs`, `panel.rs`), the local API
  (`local_api/`), `aktar://` links (`deeplink.rs`), updates (`updater.rs`),
  and the commands the windows call (`commands.rs`)
- `src/` - the windows, in React with Fluent UI; `src/lib/api.ts` has typed
  wrappers for every command
- `src/locales/` - UI strings, shared by the frontend and Rust

## Translations

Strings are keyed by their English text. The tables in `src/locales/` are
generated from the Mac app's String Catalog, so both apps share one set of
translations, plus `scripts/windows_strings.json` for strings that only
exist on Windows. With the Mac repo checked out next to this one
(`../mac`), regenerate them after changing either:

```bash
pnpm locales
```

New Windows-only strings need all eight languages in
`scripts/windows_strings.json`; the script refuses to run otherwise.

## Tests

```bash
cd src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
```

The storage tests talk to a real S3-compatible server and are skipped by
default. To run them, start one (for example
[moto](https://github.com/getmoto/moto): `moto_server -p 9100`), create a
bucket named `aktar-test`, and run:

```bash
AKTAR_TEST_S3_ENDPOINT=http://127.0.0.1:9100 cargo test storage -- --ignored
```

## Pull requests

- Keep PRs focused on one change.
- Never commit real credentials, certificates, or signing keys, even in
  tests or fixtures - use obviously-fake placeholder values.
- Make sure `pnpm build` and the Rust tests pass before opening a PR.
- Add a line under `[Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for any
  user-facing change.

## Releasing (maintainers)

Releases are built by `.github/workflows/release.yml` when a `v<version>`
tag is pushed. It needs one repository secret,
`TAURI_SIGNING_PRIVATE_KEY`: the updater's minisign private key (the
contents of the key file, whose public half is `plugins.updater.pubkey` in
`src-tauri/tauri.conf.json`).

To try a build on a Windows PC without releasing it, run the "Installer"
workflow from the Actions tab and download its artifact.

Losing the private key means installed copies can never be updated again,
so keep a backup somewhere safe.
