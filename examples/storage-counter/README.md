# Storage Counter

Adapted from Sandstorm `isolates-rebased` `isolate-counter`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/storage-counter auth
target/debug/spki --project examples/storage-counter dev
```

Use a separate Preview grain for each example. Credentials are stored locally under the `storage-counter` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

Every request increments the stored value. Reload the grain or restart it to see that the value persists. The worker imports `metadata.json` and `help.txt`; the CLI bundles both.
