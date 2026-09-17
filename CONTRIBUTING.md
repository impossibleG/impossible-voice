# Contributing

Contributions must be focused, tested, and free of machine-specific data. Do not commit model
weights, runtime archives, credentials, request audio or text, absolute home-directory paths,
benchmark machine details, or generated native libraries.

Run the formatting, build, lint, test, snapshot-integrity, and privacy commands documented in the
README before submitting a change. New behavior requires focused tests. Changes to the advertised
v0.1 behavior must update `docs/product-contract.md` deliberately and must not silently reduce its
requirements.

Dependencies must be necessary, maintained, pinned by the lockfile, and compatible with the
dual-license policy.
