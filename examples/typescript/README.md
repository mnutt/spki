# Typescript

Adapted from Sandstorm `isolates-rebased` `isolate-typescript`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/typescript auth
target/debug/spki --project examples/typescript dev --esbuild "$PWD/build/tools/esbuild"
```

Use a separate Preview grain for each example. Credentials are stored locally under the `typescript` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

The worker is compiled directly from `worker.ts` by SPKI. `connect()` exports the TypedCounter, and the HTTP UI still exercises session metadata and increment validation. Stored values persist; the exported capability itself does not.

Run `npm ci --prefix examples/typescript` and `npm run typecheck --prefix examples/typescript` from the repository root for strict typechecking. SDK declarations are vendored under `types/`, and `typed-counter.d.ts` describes the checked-in schema. There is no dependency on Sandstorm source paths or a separately generated `worker.js`.
