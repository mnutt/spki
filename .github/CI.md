# CI artifacts

Pushes to `main`, pull requests, `v*` tags, and manual workflow runs build:

- `spki` archives and SHA-256 checksums for Linux x86_64/ARM64 and macOS Intel/Apple Silicon.
- A Linux x86_64 Isolate Preview SPK, built in release mode and packaged and verified by the native Rust SPK writer. No `spk` command, VM, or running Sandstorm server is needed.

Download artifacts from the workflow run's Actions page. Pushing a `v*` tag
matching the Cargo workspace version runs the same builds, then publishes the
four CLI archives, test SPK, and checksums together in a GitHub Release. Tags with
a prerelease suffix produce a prerelease. No workflow installs packages on Sandstorm.

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

Tagged releases also use a **test** SPK identity. They do not require or use a
production signing key. Without the optional test identity above, each workflow
uses a fresh disposable identity; those packages install as separate apps.
A configured test identity permits upgrades, with the commit's Unix timestamp
used as the numeric `appVersion` consistently across workflows.

Keep `appMarketingVersion` in `.sandstorm/sandstorm-pkgdef.capnp` equal to
`[workspace.package].version` in `Cargo.toml`, and refresh `Cargo.lock` when bumping
versions. Packaging rejects mismatches. The displayed SPK version equals
`spki --version`. The tag must be `v` followed by that version. Never commit keys.

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

## Embedded runtime and packaging

Release CLI binaries embed the same compressed Linux x86_64 runtime, built once
from the tagged checkout by `runtime.yml`. The payload includes the pinned
isolate host, release supervisor, libraries, and licenses. It is validated by
version, runtime fingerprint, file hashes, and executable format when unpacked.
`spki pack` therefore works offline with no `--runtime-root` or helper tools.
A signing key is still required; no signing keys are embedded.

For a development build with an embedded runtime:

```sh
python3 scripts/fetch-host.py
cargo build --locked --release -p isolate-preview --bin isolate-preview
SPKI_PREVIEW_BINARY=target/release/isolate-preview python3 scripts/stage-preview.py
python3 scripts/bundle-runtime.py
SPKI_RUNTIME_BUNDLE="$PWD/build/runtime.xz" cargo build --locked --release -p spki
```

The bundling script refuses to overwrite an existing payload; use `--output`
for a new filename on subsequent builds. `SPKI_RUNTIME_BUNDLE` is a build-time
input, not a runtime environment requirement. Ordinary development Cargo builds
omit the payload and can use `spki pack --runtime-root build/grain-root` instead.
Cargo does not download or rebuild the runtime implicitly.

Packing streams the runtime into a private staging directory beside the output
SPK. The archive is serialized into a seekable temporary file, hashed, and
compressed through fixed-size buffers. Verification authenticates the decompressed
archive on disk, then reads metadata through a read-only mapping. No full archive
is allocated on the heap. Compression dictionaries, metadata, and mapped pages
still consume memory; this is not a promise of zero RAM usage. Temporary files
are removed on success and on ordinary errors.

Packing needs temporary free space for the staged runtime and an archive spool,
plus the compressed output. It uses the output filesystem rather than `/tmp`,
which may be RAM-backed. Standalone `spki verify` uses the system temporary
directory; set `TMPDIR` to a disk-backed location when needed.

To create a disposable previewer test package with an embedded build:

```sh
SPKI_RUNTIME_BUNDLE="$PWD/build/runtime.xz" cargo run --locked --release -p spki --example package-preview -- \
  --test --output build/isolate-preview-test.spk
```

Alternatively, pass `--runtime-root build/grain-root` to a development packager.
The helper shares the CLI's embedded schema compiler, disk-backed SPK writer,
and verifier. It does not upload or install packages. Test builds get a fresh
identity unless `--keyring` and `--app-id` select an existing test identity.
For manual production packaging, omit `--test` and pass `--keyring PATH`; the
configured production identity and manifest counter are preserved.

Host-only checks: `python3 scripts/check-host.py`. Add `--embedded-runtime` to
exercise offline packing with an embedded CLI; set `SPKI_TEST_BINARY` to select
the binary. Linux Cargo tests include a subprocess RSS regression that packs and
verifies a 256 MiB file with a peak-RSS ceiling of 128 MiB.
