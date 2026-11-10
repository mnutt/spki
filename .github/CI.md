# CI artifacts

Pushes to `main`, pull requests, `v*` tags, and manual workflow runs build:

- `spki` archives and SHA-256 checksums for Linux x86_64/ARM64 and macOS Intel/Apple Silicon.
- A Linux x86_64 Isolate Preview SPK, built in release mode and packaged with spktool in Lima.

Download artifacts from the workflow run's Actions page. These workflows do not
install packages on Sandstorm or publish GitHub Releases.

Native Windows is not supported yet: credential permissions, filesystem metadata,
and the preview runtime use Unix APIs. Use the Linux build under WSL. The CLI
requires `capnp` and esbuild 0.25.12 on PATH for project compilation. Schema
bindings also require Node.js and a checkout of `tools/schema-compiler`: run
`npm ci --prefix tools/schema-compiler`, then set `SPKI_SCHEMA_COMPILER` to the
absolute path of its `compile.mjs`. The pinned isolate host and packaged apps remain
Linux x86_64 regardless of the CLI's host platform. Linux CLI builds require glibc
compatible with Ubuntu 24.04.

## SPK signing

PRs use fresh disposable test identities. Main and manual builds do too unless
both of the following are configured in repository Actions settings:

- Secret `SANDSTORM_TEST_KEYRING_B64`: base64-encoded test signing keyring.
- Variable `SANDSTORM_TEST_APP_ID`: the matching test app ID.

As in Davros, release signing is separate. `v*` tags require the secret
`SANDSTORM_KEYRING_B64`, containing the base64-encoded keyring for the app ID in
`.sandstorm/sandstorm-pkgdef.capnp`. Missing release keys fail the build. Bump
`appVersion` and `appMarketingVersion` in that file before tagging a release;
the workflow preserves them. Never commit keyrings. Fork PRs need no secrets.
