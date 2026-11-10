# Browser Capnp

Adapted from Sandstorm `isolates-rebased` `isolate-browser-capnp`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/browser-capnp auth
target/debug/spki --project examples/browser-capnp dev --esbuild "$PWD/build/tools/esbuild"
```

Use a separate Preview grain for each example. Credentials are stored locally under the `browser-capnp` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

The browser now uses `await connectApplication(BrowserCounter)` and the worker exposes `connect(request, env)`. No `/counter-capability` HTTP handoff is needed. The value resets on worker replacement.
