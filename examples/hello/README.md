# Hello

Adapted from Sandstorm `isolates-rebased` `isolate-hello`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/hello auth
target/debug/spki --project examples/hello dev --esbuild "$PWD/build/tools/esbuild"
```

Use a separate Preview grain for each example. Credentials are stored locally under the `hello` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

Returns JSON describing the request. Powerbox authority is acquired through the native RPC bridge, not a legacy `env.POWERBOX` binding.
