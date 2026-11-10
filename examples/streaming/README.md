# HTTP streaming

Run `spki --project examples/streaming auth`, then
`spki --project examples/streaming dev` against a matching Preview grain.
The page demonstrates incremental downloads, uploaded byte counts, and a delayed
roundtrip response. The roundtrip route buffers its upload in JavaScript; the
runtime streams it to the worker as it arrives.

Application responses use Sandstorm's response stream, with bounded queues and
backpressure. POST and PUT support streaming request bodies, including duplex
traffic through the native WebSession RPC interface. Disconnecting the consumer
or replacing the worker cancels unfinished responses. Blocked stream reads and writes time out after 60 seconds. Declared lengths are checked for truncation and overflow.

Legacy callers without a response stream, control routes, and non-content HTTP
statuses still use bounded buffering. Responses that keep native JavaScript RPC
capabilities alive retain the SDK's existing 1 MiB buffer. Unknown-length failures
may appear as EOF through the legacy Sandstorm HTTP gateway; declare a length
when clients need to detect truncation. The runtime never sends `done()` after a
failed stream.

Imported from Sandstorm `isolates-rebased`; see `../upstream.json` for provenance.
