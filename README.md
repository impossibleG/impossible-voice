# Impossible Voice

Impossible Voice is a ready-made, self-hosted speech-to-text and text-to-speech server. The first
release targets one curated local STT model and one curated local TTS voice with automatic,
checksum-verified installation and offline operation after setup.

The public v0.1 promise is frozen in [`docs/product-contract.md`](docs/product-contract.md). The
implementation is under active development and is not yet a release.

## Development

The repository is a Rust workspace. The current tree establishes the public contract and crate
boundaries; speech engines and network transports will arrive in later, feature-specific commits.

```text
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
pwsh ./scripts/test-privacy-scan.ps1
pwsh ./scripts/privacy-scan.ps1
```

## License

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.
