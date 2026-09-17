# Privacy and data handling

Audio inputs, generated speech, transcripts, credentials, model paths, and private diagnostics are
sensitive data.

The server follows these rules:

- Request audio, output audio, and transcript text are not logged by default.
- Telemetry is limited to opaque request identifiers, sizes, timings, status codes, and aggregate
  counters.
- Local paths are normalized or redacted before diagnostic output.
- No telemetry leaves the host unless an operator explicitly configures an exporter.
- Curated artifacts are installed only into documented local storage.
- Tests use synthetic media and never depend on contributor files, identities, or hardware facts.
- Repository automation scans tracked files for common credentials, private paths, machine markers,
  and attribution leaks.

The repository scan is a guardrail rather than a substitute for review. Operators remain
responsible for access controls, storage permissions, retention, transport security, and backups in
their deployment environment.
