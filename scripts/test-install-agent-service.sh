#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
installer="${script_dir}/install-agent-service.sh"
fixture_root="$(mktemp -d)"
trap 'rm -rf "${fixture_root}"' EXIT

data_dir="${fixture_root}/agent data%node"
mkdir -p "${data_dir}"
printf '{}\n' >"${data_dir}/agent.json"

unit="$(${installer} install --dry-run --binary /bin/echo --data-dir "${data_dir}")"

grep -Fqx '# Managed by Temps. Do not edit by hand.' <<<"${unit}"
grep -Fq 'ExecStart="/usr/bin/echo" agent' <<<"${unit}"
grep -Fq 'Restart=on-failure' <<<"${unit}"
grep -Fq 'UMask=0077' <<<"${unit}"
grep -Fq 'agent data%%node' <<<"${unit}"

if grep -Fq 'TEMPS_AGENT_TOKEN' <<<"${unit}"; then
  echo "generated unit must not expose the agent token" >&2
  exit 1
fi

mkdir -p "${fixture_root}/not-joined"
if "${installer}" install --dry-run --binary /bin/echo \
  --data-dir "${fixture_root}/not-joined" >/dev/null 2>&1; then
  echo "installer unexpectedly accepted a missing agent.json" >&2
  exit 1
fi

"${installer}" --help >/dev/null
echo "agent service installer tests passed"
