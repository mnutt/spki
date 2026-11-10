# CI artifacts

Pushes to `main`, pull requests, `v*` tags, and manual workflow runs build:

- `spki` archives and SHA-256 checksums for Linux x86_64/ARM64 and macOS Intel/Apple Silicon.
- A Linux x86_64 Isolate Preview SPK, built in release mode and packaged and verified by the native Rust SPK writer. No `spk` command, VM, or running Sandstorm server is needed.

Download artifacts from the workflow run's Actions page. These workflows do not
install packages on Sandstorm or publish GitHub Releases.

Native Windows is not supported yet: credential permissions, filesystem metadata,
and the preview runtime use Unix APIs. Use the Linux build under WSL. The CLI
includes Rolldown 1.3.1 and Lightning CSS; no bundler executable is needed. It
includes the pure Rust fs-capnpc schema compiler (pinned in Cargo.lock). Binding
generation runs inside the binary using QuickJS and Oxc; capnp, Node.js, npm,
`CAPNP`, and `SPKI_SCHEMA_COMPILER` are no longer needed. The pinned isolate host and packaged apps remain
Linux x86_64 regardless of the CLI's host platform. Linux CLI builds require glibc
compatible with Ubuntu 24.04.

Existing projects using esbuild should replace their `compiler` setting with
`compiler = (name = "rolldown", version = "1.3.1")`. The `--esbuild` option has
been removed; `SPKI_ESBUILD` is no longer used. Bundles will change, so the first
deployment after migration creates a new revision.

The embedded schema generator is checked in under `crates/spki/schema-compiler`,
with its upstream license and hashes. Ordinary Cargo builds need no Node tools.
To regenerate it, maintainers run `npm ci --prefix tools/schema-compiler` and
`python3 tools/schema-compiler/generate-embedded.py`. The optional differential
test, `python3 scripts/check-schema-engine.py`, uses Node only to compare against
the previous generator; it runs the new CLI with no Node on PATH.

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

The schema compiler is linked into both the CLI and Cargo build scripts; normal
builds do not invoke a system `capnp`. Maintainers can compare compiled schema
nodes, project constants, and manifests against an installed official compiler:

```sh
cargo test -p spki capnp_compiler::tests -- --ignored
python3 scripts/check-schema-engine.py
```

The second check also requires the Node-based reference generator dependencies
(`npm ci --prefix tools/schema-compiler`); the CLI under test runs with no host
tools on PATH. These reference tools are test-only dependencies.

## Native previewer packaging

After staging `build/grain-root`, create a disposable test package with:

```sh
cargo run --locked --release -p spki --example package-preview -- \
  --test --output build/isolate-preview-test.spk
```

The helper shares `spki`'s SPK writer and verifier. It uses `capnp` to read the
existing `.sandstorm/sandstorm-pkgdef.capnp`, copies the staged runtime into a
temporary directory, writes the manifest, then signs, compresses, and verifies
the package in Rust. It does not upload or install it. Test builds get a fresh
identity unless `--keyring` and `--app-id` select an existing test identity.
CI sets the test app version to its run number so subsequent builds can upgrade.

For release packaging, omit `--test` and pass `--keyring PATH`. The existing
production identity and manifest versions are preserved; missing keys fail.
Existing output files are never overwritten.
