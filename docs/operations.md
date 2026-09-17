# Installation and operations

## Source checkout

Prerequisites are Git, Rust 1.85, and PowerShell 7 on Windows or Bash on Linux. From a clean clone,
the setup scripts compile the release binary, download the pinned runtime and models, verify exact
sizes and SHA-256 digests, and activate them atomically.

PowerShell:

```powershell
./scripts/setup.ps1
./scripts/serve.ps1
```

Bash:

```bash
./scripts/setup.sh
./scripts/serve.sh
```

The first command requires network access. The second command does not download. Both use the
ignored `runtime-artifacts` directory by default. Set `IMPOSSIBLE_VOICE_ARTIFACT_ROOT` on Bash, or
pass `-ArtifactRoot` on PowerShell, to select another local store.

## Native release archive

Extract the archive, then use the same two commands from its root. The packaged scripts detect the
included native binary, so Rust is not required. Windows and Linux archives intentionally exclude
the native runtime, models, caches, generated audio, and local diagnostics; setup acquires them from
their pinned upstream publishers.

## Offline verification and restart

After one successful setup, verify the store without network access:

```text
impossible-voice setup --offline
impossible-voice doctor
impossible-voice status
```

An offline restart is the normal `serve` command. If verification detects a missing or corrupted
object, readiness remains false. Run online setup again to stage and activate a verified replacement.

## Runtime endpoints

HTTP, WebSocket, and MCP bind to `127.0.0.1:8080`; gRPC binds to `127.0.0.1:50051`. Non-loopback
binds are rejected. Use `GET /health/live`, `GET /health/ready`, and `GET /v1/capabilities` for
process, engine, and contract status. Detailed examples are in [`api.md`](api.md).

The service is local-only by design. If another machine must reach it, place an authenticated,
TLS-terminating reverse proxy in front instead of weakening the loopback guard.
