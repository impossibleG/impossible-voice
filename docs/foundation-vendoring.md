# Impossible Server foundation snapshot

Impossible Voice vendors the reviewed portable Impossible Server source snapshot under
`vendor/impossible-server`. The snapshot is produced from an explicit allowlist, contains exactly
17 manifested files, normalizes text deterministically, and records every file's byte size and
SHA-256 digest in `source-sync.json`.

Run the local verifier with:

```text
pwsh ./scripts/verify-vendored-impossible-server.ps1
```

The application workspace links the vendored `impossible-server-core` as a normal dependency and
the vendored `impossible-server-testkit` as a development dependency. The snapshot provides the
reviewed cancellation, request context, limits, health, shutdown, error, and test-support
primitives.

The snapshot does **not** contain an HTTP or gRPC server, WebSocket handling, voice protocols,
audio decoding or resampling, STT or TTS engines, model/runtime installation, packaging, or a
dashboard. Those remain product-owned implementation work. The larger Impossible Server repository
contains a template application, but that template is intentionally outside the reviewed portable
export and must not be described as inherited production functionality.

## Provisional provenance

The current `source-sync.json` sets `provisional` to `true` because the foundation's canonical
release identity has not been finalized. Development verification accepts that status, while
`pwsh ./scripts/verify-vendored-impossible-server.ps1 -Release` fails closed. Before an Impossible
Voice release, the foundation must publish non-provisional provenance or the product must document
and deliberately resolve that release gate.

Downloaded models, runtimes, Git history, build output, tool state, private paths, and product code
are excluded from the snapshot.
