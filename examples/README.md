# SPKI examples

Runnable standalone projects live directly under this directory. Each has a
`sandstorm-pkgdef.capnp` and a separate local credential profile. The imported
examples come from Sandstorm's `isolates-rebased` branch; `upstream.json` records
the source commit and original file hashes. No Sandstorm checkout is needed to
build or run them. Upstream signing keys and generated worker bundles are omitted.

| Example | What it demonstrates |
| --- | --- |
| [hello](hello/) | Request metadata and runtime bindings |
| [storage-counter](storage-counter/) | Persistent key/value storage, JSON and text imports |
| [app-skeleton](app-skeleton/) | HTTP routes, session identity, validation, and storage |
| [counter](counter/) | Direct application bootstrap and bidirectional browser callbacks |
| [browser-capnp](browser-capnp/) | Upstream counter UI adapted to `connectApplication()` |
| [capnp-rpc](capnp-rpc/) | Returning capabilities, multiple results, and accepting callbacks |
| [object-store](object-store/) | Typed object listing and read-only native `WebSession` capabilities |
| [typescript](typescript/) | Strict TypeScript types and a typed persistent counter |
| [powerbox-http](powerbox-http/) | Real Powerbox HTTP grants passed from the browser to the worker |
| [api-powerbox](api-powerbox/) | Save imported HTTP grants, restore after restart, and drop saved references |

## Run an example

Requires Rust, Node.js/npm, and the Cap’n Proto `capnp` compiler.
From the repository root:

```sh
cargo build --locked --workspace
python3 scripts/fetch-esbuild.py
npm ci --prefix tools/schema-compiler
# Create a Preview grain, then paste its Developer webkey when prompted:
target/debug/spki --project examples/counter auth
target/debug/spki --project examples/counter dev --esbuild "$PWD/build/tools/esbuild"
```

Use a separate Preview grain for each example. Replace `counter` with another
runnable directory name. The Preview installation must match this CLI's runtime
and SDK; examples do not install or upgrade Preview automatically. `dev` bundles
and deploys the example, then watches for edits. `build` compiles offline instead.
Generated bundles go to each example's ignored `dist/` directory.

The TypeScript example also supports strict checking:

```sh
npm ci --prefix examples/typescript
npm run typecheck --prefix examples/typescript
```

Preview currently exposes no application-specific permission roles. Shared
sessions can use the same app operations; Preview's deployment permission is not
an application permission. The counter example is intentionally in-memory;
`storage-counter` and `typescript` persist their values. None of these examples
claims to persist exported capabilities across restarts.

`api-powerbox` demonstrates durable imported HTTP grants: save, restore after
a grain restart, and drop the saved reference. Its tokens stay server-side.

## Pending upstream examples

Reference source is retained under `pending/`, with no runnable SPKI package
configuration. These need runtime work before they can be ported faithfully:

| Example | Missing support |
| --- | --- |
| [capability-provider](pending/capability-provider/) | Route-backed capability factories and capability persistence |
| [service-binding](pending/service-binding/) | Custom text, JSON, data, and service binding declarations |
| [streaming](pending/streaming/) | Streaming WebSession uploads and incremental HTTP responses |

The runnable object store implements a read-only `WebSession` directly in JS.
It does not exercise the pending route-backed factory or durable capability tokens.
The runtime's HTTP adapter currently buffers bounded bodies, so the upstream
streaming demo is not advertised as runnable.

## Checks

```sh
python3 scripts/check-examples.py
# Against the dedicated SPKI test VM with `spktool dev` running:
node scripts/check-examples-gateway.cjs /path/to/playwright
node scripts/check-powerbox-gateway.cjs /path/to/playwright
```

The browser checks create and trash their own temporary test grain; they do not
upload an SPK. The Powerbox check explicitly grants its test grain access to
`https://example.com` using Sandstorm's HTTP provider.
