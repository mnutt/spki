# Route-backed capability provider

`api.webSession({pathPrefix: "/shared"})` and
`api.apiSession({pathPrefix: "/api/v1"})` create capabilities that forward HTTP
calls to those application routes. They carry no browser identity or deployment
authority. The runtime rejects path traversal and private runtime routes.

Run using `spki --project examples/capability-provider auth`, then
`spki --project examples/capability-provider dev`. Set `--esbuild` if needed.

The two buttons exercise live calls, saving with SandstormApi, restoration, and
an attempted drop. Saved routes survive grain restarts and use the active worker
after redeployment. Choose `persistent: false` to prohibit saving a capability.

**Legacy Sandstorm limitation:** the supervisor currently rejects dropping
app-owned references with “Supervisor can only drop wakelocks for now.” The
example reports this in `dropSaved`; the token remains valid. Repeated runs can
therefore leave saved references until the test grain is deleted. No app-level
revocation workaround is installed. A core fix can enable the standard drop path
without changing the saved route IDs.

This supports runtime-owned route capabilities. Arbitrary JS objects with custom
MainView restore/drop hooks are a separate feature.
