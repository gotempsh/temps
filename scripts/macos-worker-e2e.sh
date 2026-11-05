#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Manual end-to-end check of a Mac worker (ADR 090), for a real Mac with a
# Docker app. CI has no macOS runner with a Docker VM, so this is how the
# sidecar, launchd and the join are tested.
#
# Run it on the Mac AFTER joining it with the console's Mac command:
#
#   ~/.temps/bin/temps join <control plane URL> <token> && \
#   ~/.temps/bin/temps agent service install
#
# Then:
#
#   scripts/macos-worker-e2e.sh [--restart] [--network-change] [--app-url <url>]
#
#   --restart         restart the LaunchAgent and wait for the mesh to recover
#   --network-change  move the worker's WireGuard source port (what a NAT or
#                     Wi-Fi change looks like to the control plane) and wait
#                     for a fresh handshake
#   --app-url <url>   an app deployed to this worker, fetched through the
#                     control plane's proxy (e.g. https://app.example.com)
#
# Sleep is checked by hand: `pmset sleepnow`, wake the Mac, run this again.
# Every check prints PASS or FAIL; the exit status is the number of failures.

set -uo pipefail

TEMPS="${TEMPS_BIN:-$HOME/.temps/bin/temps}"
DATA_DIR="${TEMPS_DATA_DIR:-$HOME/.temps}"
SIDECAR=temps-mac-mesh
LABEL=sh.temps.agent
failures=0
restart=0
network_change=0
app_url=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --restart) restart=1 ;;
    --network-change) network_change=1 ;;
    --app-url) app_url="$2"; shift ;;
    -h|--help) sed -n '5,28p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() { local name="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi; }

[[ "$(uname -s)" == Darwin ]] || { echo "run this on a Mac" >&2; exit 2; }
config="$DATA_DIR/mac-worker.json"
[[ -f "$config" ]] || { echo "$config is missing: join this Mac first" >&2; exit 2; }

field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$config" "$1"; }
docker_cli=$(field docker_cli)
docker_host=$(field docker_host)
mesh_address=$(field mesh_address)
control_plane=$(field control_plane_address)
agent_port=$(field agent_port)
egress_port=$(field egress_port)
d() { "$docker_cli" --host "$docker_host" "$@"; }
in_sidecar() { d exec "$SIDECAR" "$@"; }

# Seconds since the control plane peer's last handshake, or a large number.
handshake_age() {
  local now latest
  now=$(date +%s)
  latest=$(in_sidecar wg show temps-wg0 latest-handshakes 2>/dev/null | awk '{print $2}' | sort -n | tail -1)
  [[ -n "$latest" && "$latest" != 0 ]] && echo $((now - latest)) || echo 999999
}

wait_for_handshake() {
  local limit="$1" since="$2"
  for _ in $(seq 1 "$limit"); do
    local latest
    latest=$(in_sidecar wg show temps-wg0 latest-handshakes 2>/dev/null | awk '{print $2}' | sort -n | tail -1)
    [[ -n "$latest" && "$latest" -gt "$since" ]] && return 0
    sleep 1
  done
  return 1
}

echo "Mac worker: mesh $mesh_address, control plane $control_plane, Docker $docker_host"

check "the LaunchAgent is loaded and running" \
  sh -c "launchctl print gui/$(id -u)/$LABEL | grep -q 'state = running'"
check "the mesh sidecar runs in the Docker app's VM" \
  sh -c "[ \"\$('$docker_cli' --host '$docker_host' inspect -f '{{.State.Running}}' $SIDECAR)\" = true ]"
check "the sidecar holds NET_ADMIN only and is not privileged" \
  sh -c "'$docker_cli' --host '$docker_host' inspect -f '{{.HostConfig.Privileged}} {{.HostConfig.CapAdd}} {{.HostConfig.CapDrop}}' $SIDECAR | grep -qxE 'false \[(CAP_)?NET_ADMIN\] \[ALL\]'"
check "temps-wg0 has this node's mesh address" \
  sh -c "'$docker_cli' --host '$docker_host' exec $SIDECAR ip -4 -o addr show dev temps-wg0 | grep -q ' $mesh_address/'"
check "the mesh lockdown is installed in the VM" \
  in_sidecar nft list table inet temps_mesh
age=$(handshake_age)
if [[ "$age" -lt 180 ]]; then pass "handshake with the control plane ${age}s ago"; else fail "no handshake in the last 180s"; fi
check "the control plane answers over the mesh (ping $control_plane)" \
  in_sidecar ping -c 1 -W 3 "$control_plane"
check "the agent listens on the Mac's loopback" \
  sh -c "lsof -nP -iTCP:$agent_port -sTCP:LISTEN | grep -q '127.0.0.1:$agent_port'"
check "the egress forwarder is reachable on the Mac's loopback" \
  nc -z -w 3 127.0.0.1 "$egress_port"
if lsof -nP -iTCP:"$agent_port" -sTCP:LISTEN | grep -qE '\*:|0\.0\.0\.0:'; then
  fail "the agent port is open on all interfaces"
else
  pass "nothing listens on the agent port beyond loopback"
fi
# Without the control plane's client certificate, the agent refuses TLS.
if curl -sk --max-time 5 "https://127.0.0.1:$agent_port/health" >/dev/null 2>&1; then
  fail "the agent API answered without a client certificate"
else
  pass "the agent API refuses a client without the control plane's certificate"
fi

if [[ -n "$app_url" ]]; then
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$app_url")
  if [[ "$code" =~ ^(2|3) ]]; then pass "app answers through the control plane ($code)"; else fail "app at $app_url answered $code"; fi
fi

if [[ "$restart" -eq 1 ]]; then
  echo "Restarting the LaunchAgent..."
  old_pid=$(launchctl print "gui/$(id -u)/$LABEL" | awk '/^\tpid = /{print $3}')
  launchctl kickstart -k "gui/$(id -u)/$LABEL"
  new_pid=""
  for _ in $(seq 1 60); do
    new_pid=$(launchctl print "gui/$(id -u)/$LABEL" | awk '/^\tpid = /{print $3}')
    [[ -n "$new_pid" && "$new_pid" != "$old_pid" ]] && lsof -nP -iTCP:"$agent_port" -sTCP:LISTEN >/dev/null 2>&1 && break
    sleep 2
  done
  if [[ -n "$new_pid" && "$new_pid" != "$old_pid" ]]; then pass "the LaunchAgent restarted (pid $old_pid -> $new_pid)"; else fail "the LaunchAgent did not restart"; fi
  check "the agent listens again after the restart" \
    sh -c "lsof -nP -iTCP:$agent_port -sTCP:LISTEN | grep -q '127.0.0.1:$agent_port'"
  check "the control plane answers over the mesh after the restart" in_sidecar ping -c 1 -W 3 "$control_plane"
  if [[ -n "$app_url" ]]; then
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$app_url")
    if [[ "$code" =~ ^(2|3) ]]; then pass "app answers after the restart ($code)"; else fail "app answered $code after the restart"; fi
  fi
fi

if [[ "$network_change" -eq 1 ]]; then
  # A new WireGuard source port is what a new NAT mapping or Wi-Fi network
  # looks like to the control plane: it must roam to it.
  old_port=$(in_sidecar wg show temps-wg0 listen-port)
  if [[ "$old_port" -lt 64000 ]]; then new_port=$((old_port + 1000)); else new_port=$((old_port - 1000)); fi
  echo "Moving the worker's WireGuard port $old_port -> $new_port..."
  before=$(date +%s)
  in_sidecar wg set temps-wg0 listen-port "$new_port"
  if wait_for_handshake 90 "$before"; then pass "a fresh handshake from the new port"; else fail "no handshake from the new port"; fi
  check "the control plane answers after the change" in_sidecar ping -c 1 -W 3 "$control_plane"
  if [[ -n "$app_url" ]]; then
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$app_url")
    if [[ "$code" =~ ^(2|3) ]]; then pass "app answers after the change ($code)"; else fail "app answered $code after the change"; fi
  fi
fi

echo "$failures failure(s)"
exit "$failures"
