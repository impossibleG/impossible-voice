#!/usr/bin/env bash
set -euo pipefail

repository_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
archive="$(find "${repository_root}/dist" -maxdepth 1 -type f -name 'impossible-voice-*-linux-x86_64.tar.gz' -print -quit)"

if [[ -z "${archive}" || ! -f "${archive}.sha256" ]]; then
  echo 'Linux archive or checksum is missing.' >&2
  exit 1
fi

(
  cd "$(dirname -- "${archive}")"
  sha256sum --check "$(basename -- "${archive}.sha256")"
)

entries="$(tar -tzf "${archive}")"
if grep -Eqi '(runtime-artifacts|\.shame|target/|\.wav$|\.onnx$|\.dll$|\.so$)' <<<"${entries}"; then
  echo 'Linux archive contains a forbidden runtime, model, build, or audio artifact.' >&2
  exit 1
fi

test_root="${repository_root}/.shame/linux-package-test"
case "${test_root}" in
  "${repository_root}"/.shame/*) ;;
  *) echo 'Unsafe package-test directory.' >&2; exit 1 ;;
esac
trap 'rm -rf -- "${test_root}"' EXIT
mkdir -p -- "${test_root}"
tar -xzf "${archive}" -C "${test_root}"
package_root="$(find "${test_root}" -mindepth 1 -maxdepth 1 -type d -name 'impossible-voice-*-linux-x86_64' -print -quit)"

test -x "${package_root}/impossible-voice"
test -x "${package_root}/scripts/setup.sh"
test -x "${package_root}/scripts/serve.sh"
test -f "${package_root}/docs/assets/impossible-voice-header.png"
"${package_root}/impossible-voice" --version
bash -n "${package_root}/scripts/setup.sh" "${package_root}/scripts/serve.sh"

set +e
offline_output="$(cd "${package_root}" && PATH=/usr/bin:/bin ./scripts/setup.sh --offline 2>&1)"
offline_status=$?
set -e
if [[ ${offline_status} -eq 0 ]] || ! grep -Fq 'offline setup requires a complete verified artifact profile' <<<"${offline_output}"; then
  echo 'Packaged setup script did not select the included binary in offline mode.' >&2
  exit 1
fi

echo 'Linux archive modes, checksum, binary, and packaged script detection passed.'
