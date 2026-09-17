# Security policy

## Reporting a vulnerability

Do not publish credentials, private audio, model data, or sensitive request text in an issue.
Use the repository host's private vulnerability-reporting channel when it is available and include
the affected revision, impact, and minimal synthetic reproduction steps.

## Security boundary

Impossible Voice is designed for local inference. Setup may contact only explicitly curated
artifact origins and must verify artifact identity before activation. Normal startup and inference
must work offline after setup. Network listeners default to loopback; exposing them remotely is an
operator decision and requires appropriate authentication, TLS termination, and network policy.

Request audio, generated speech, transcript text, credentials, model paths, host paths, and machine
details are excluded from normal logs, metrics, status responses, and public errors. Downloaded
artifacts and media inputs remain untrusted at every boundary.

## Supported versions

Before the first stable release, security fixes target the default branch. A version-support table
will be published with the first stable release.
