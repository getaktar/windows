# Security Policy

Aktar stores your storage-provider credentials (access key / secret key)
in Windows Credential Manager only. They are never written to the settings
or destinations files, the local upload history database, or logs, and are
never sent anywhere except directly to the S3-compatible endpoint you
configure.

The optional local API (Settings > Integrations, off by default) listens on
127.0.0.1 only, refuses requests from web pages (any request with an
`Origin` header, or an unexpected `Host` header), and requires a random
token that is also kept in Credential Manager.

Updates are downloaded from this repository's GitHub releases and verified
against a public key built into the app before they're installed.

## Reporting a Vulnerability

If you believe you've found a security issue in Aktar, please report it
privately rather than opening a public issue:

- Email **security@getaktar.com** with a description of the issue and steps
  to reproduce it.
- Please give us a reasonable amount of time to investigate and release a
  fix before any public disclosure.
- Only test against your own accounts/data. Do not attempt denial of
  service, data destruction, or social engineering against maintainers.

We'll acknowledge your report and keep you updated as we work on a fix.

## Supported Versions

Only the latest released version of Aktar is supported with security fixes.
