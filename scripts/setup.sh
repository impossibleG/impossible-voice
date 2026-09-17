#!/usr/bin/env bash
set -euo pipefail

repository_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
artifact_root="${IMPOSSIBLE_VOICE_ARTIFACT_ROOT:-${repository_root}/runtime-artifacts}"
packaged_binary="${repository_root}/impossible-voice"

if [[ -x "${packaged_binary}" ]]; then
  exec "${packaged_binary}" setup --artifact-root "${artifact_root}" "$@"
fi

cd "${repository_root}"
exec cargo run --locked --release --bin impossible-voice -- setup --artifact-root "${artifact_root}" "$@"
