#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
installer="${script_dir}/install-agent-service.sh"
fixture_root="$(mktemp -d)"
trap 'rm -rf "${fixture_root}"' EXIT

unit=""

# Report which assertion failed, and the unit it ran against, before exiting.
fail_assertion() {
  printf 'FAIL: %s\n' "$1" >&2
  if [[ -n "${unit}" ]]; then
    printf -- '--- generated unit ---\n%s\n--- end of unit ---\n' "${unit}" >&2
  fi
  exit 1
}

assert_line() {
  grep -Fqx -- "$1" <<<"${unit}" || fail_assertion "unit has no line '$1'"
}

assert_contains() {
  grep -Fq -- "$1" <<<"${unit}" || fail_assertion "unit does not contain '$1'"
}

assert_absent() {
  if grep -Fq -- "$1" <<<"${unit}"; then
    fail_assertion "unit must not contain '$1'"
  fi
}

# The same escaping the installer applies to values in the unit file.
systemd_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\%/%%}"
  printf '%s' "${value}"
}

data_dir="${fixture_root}/agent data%node"
mkdir -p "${data_dir}"
printf '{}\n' >"${data_dir}/agent.json"

# A fake Temps binary owned by the test, reached through a symlink, so the
# expected ExecStart path never depends on how the host lays out its own
# executables (on some distributions /bin/echo resolves into a multicall
# coreutils binary rather than /usr/bin/echo).
fake_bin_dir="${fixture_root}/bin"
mkdir -p "${fake_bin_dir}"
fake_binary="${fake_bin_dir}/temps-real"
printf '#!/bin/sh\nexit 0\n' >"${fake_binary}"
chmod 0755 "${fake_binary}"
ln -s "${fake_binary}" "${fake_bin_dir}/temps"

# The installer resolves --binary with `readlink -f`; the unit must reference
# that resolved absolute path, not the symlink it was given.
expected_binary="$(readlink -f "${fake_binary}")"
[[ "${expected_binary}" == /* ]] ||
  fail_assertion "readlink -f did not resolve the fake binary to an absolute path: '${expected_binary}'"

if ! unit="$("${installer}" install --dry-run --binary "${fake_bin_dir}/temps" --data-dir "${data_dir}")"; then
  fail_assertion "installer dry run exited non-zero for a joined data directory"
fi

assert_line '# Managed by Temps. Do not edit by hand.'
assert_line "ExecStart=\"$(systemd_escape "${expected_binary}")\" agent"
assert_absent "ExecStart=\"${fake_bin_dir}/temps\" agent"
assert_line 'Restart=on-failure'
assert_line 'UMask=0077'
assert_contains 'agent data%%node'
assert_absent 'TEMPS_AGENT_TOKEN'

unit=""
mkdir -p "${fixture_root}/not-joined"
if "${installer}" install --dry-run --binary "${fake_bin_dir}/temps" \
  --data-dir "${fixture_root}/not-joined" >/dev/null 2>&1; then
  fail_assertion "installer unexpectedly accepted a missing agent.json"
fi

"${installer}" --help >/dev/null || fail_assertion "installer --help exited non-zero"
echo "agent service installer tests passed"
