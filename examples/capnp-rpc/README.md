# Capnp Rpc

Adapted from Sandstorm `isolates-rebased` `isolate-capnp-rpc`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/capnp-rpc auth
target/debug/spki --project examples/capnp-rpc dev --esbuild "$PWD/build/tools/esbuild"
```

Use a separate Preview grain for each example. Credentials are stored locally under the `capnp-rpc` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

The root route exercises local typed RPC; `connect(request, env)` exports the same Greeter to browsers. Nested Greeting capabilities and browser callbacks are supported. The upstream durable `/export-greeter` token route is intentionally replaced with a live bootstrap capability.
