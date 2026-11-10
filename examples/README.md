# SPKI examples

Runnable standalone projects live directly under this directory. Each has a
`sandstorm-pkgdef.capnp` and a distinct workspace project ID. They share the
`default` local credential profile. The imported
examples come from Sandstorm's `isolates-rebased` branch; `upstream.json` records
the source commit and original file hashes. No Sandstorm checkout is needed to
build or run them. Upstream signing keys and generated worker bundles are omitted.

| Example | What it demonstrates |
| --- | --- |
| [hello](hello/) | Request metadata and runtime bindings |
| [streaming](streaming/) | Incremental downloads and streamed uploads |
| [service-binding](service-binding/) | Native text, JSON, binary data, and loopback service bindings |
| [storage-counter](storage-counter/) | Persistent key/value storage, JSON and text imports |
| [app-skeleton](app-skeleton/) | HTTP routes, session identity, validation, and storage |
| [counter](counter/) | Direct application bootstrap and bidirectional browser callbacks |
| [browser-capnp](browser-capnp/) | Upstream counter UI adapted to `connectApplication()` |
| [capnp-rpc](capnp-rpc/) | Returning capabilities, multiple results, and accepting callbacks |
| [object-store](object-store/) | Typed object listing and read-only native `WebSession` capabilities |
| [typescript](typescript/) | Strict TypeScript types and a typed persistent counter |
| [powerbox-http](powerbox-http/) | Real Powerbox HTTP grants passed from the browser to the worker |
| [api-powerbox](api-powerbox/) | Save imported HTTP grants, restore after restart, and drop saved references |
| [capability-provider](capability-provider/) | Durable route-backed WebSession/ApiSession; legacy servers cannot drop saved app references |

## Run an example

Requires Rust to build `spki`. Schema compilation, binding generation, and bundling
are embedded; capnp, Node.js, and npm are not required to build examples.
From the repository root:

```sh
cargo build --locked --workspace
# Create one Preview grain, then paste its Developer webkey once:
target/debug/spki auth
target/debug/spki --project examples/counter dev
```

The first `dev` creates that project inside your Preview workspace. Further
projects use the same saved credential. Open `/__spki/workspace` inside the grain
and select a project; SPKI also prints its opening path. A persistent toolbar
provides a project selector and an app-only **Reload** button. The app runs in
an iframe at `/`, preserving absolute and relative URLs. Each browser session
has its own selection, so separate Sandstorm tabs can preview different projects.
Sandstorm refreshes preserve the selected project; apps using `setPath` also
preserve their current route. The shell relays Powerbox and other standard
Sandstorm messages from the app frame. Apps that explicitly pin their parent
message origin must account for this same-origin workspace frame.

Each project has its own worker host process, storage directory, deployment
revision, native RPC state, and captured logs. Replacing one project does not
replace the others. `dev` shows only that project's logs. Projects load on demand
after restart; this first implementation permits up to 16 projects per workspace.
They share the grain's Sandstorm permissions and may share browser-origin state,
so use a workspace for projects you trust together. It is not a security boundary
between applications.

New `spki init` projects use the default profile and receive a stable project ID.
To opt an existing project into the workspace, change its configuration to:

```capnp
preview = (profile = "default", project = "my-project")
```

The project ID selects its persistent code and data; changing it selects a
different project. Do not reuse an ID for unrelated applications. Existing
configurations without `preview.project` retain the legacy single-project mode,
and existing grain data remains available through the workspace's **Open legacy
preview** link. Opting into a named project starts with separate storage; it does
not move legacy data. Multiple workspaces can still use separate auth profiles.

Replace `counter` with another example name. The Preview installation must match
this CLI's runtime and SDK; examples do not install or upgrade Preview
automatically. `dev` bundles and deploys the example, then watches for edits.
`build` compiles offline. Generated bundles go to each example's ignored `dist/`
directory. Production SPKs still contain one application, not a workspace.

The TypeScript example also supports strict checking:

```sh
npm ci --prefix examples/typescript
npm run typecheck --prefix examples/typescript
```

Preview currently exposes no application-specific permission roles. Shared
sessions can use the same app operations; Preview's deployment permission is not
an application permission. The counter example is intentionally in-memory;
`storage-counter` and `typescript` persist their values. `capability-provider`
persists runtime-owned routes across restarts; JS-exported objects in the counter
and RPC examples remain live capabilities.

`api-powerbox` demonstrates durable imported HTTP grants: save, restore after
a grain restart, and drop the saved reference. Its tokens stay server-side.

## Checks

```sh
python3 scripts/check-examples.py
# Build/verify a bindings SPK, without uploading:
python3 scripts/check-bindings.py
# Against the dedicated SPKI test VM with `spktool dev` running:
node scripts/check-workspace-gateway.cjs /path/to/playwright
node scripts/check-examples-gateway.cjs /path/to/playwright
node scripts/check-powerbox-gateway.cjs /path/to/playwright
```

The browser checks create and trash their own temporary test grain; they do not
upload an SPK. The Powerbox check explicitly grants its test grain access to
`https://example.com` using Sandstorm's HTTP provider.
