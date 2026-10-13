#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Network plumbing for tools/e2e-wireguard-cluster. Brings up a kernel
# WireGuard tunnel, then execs the unmodified tools/dev-cluster role script
# passed as $1, so `temps serve` / `temps join` / `temps agent` run exactly as
# they would on real machines joined by a WireGuard network.
#
#   WG_ROLE=hub    control plane: firewall the wan interface down to
#                  WireGuard, listen on WG_LISTEN_PORT, add spokes as peers as
#                  their keys appear, and forward spoke<->spoke traffic.
#   WG_ROLE=spoke  worker: default route via its NAT router, PROVE it cannot
#                  reach the control plane yet, dial the hub, wait for the
#                  handshake.
#
# Keys are exchanged through the shared bootstrap volume, the stand-in for an
# operator copying public keys between hosts. Private keys never leave the
# node that generated them.
set -euo pipefail

ROLE_SCRIPT="${1:?usage: wg-node.sh <role-script>}"
: "${WG_ROLE:?}" "${WG_NODE_NAME:?}" "${WG_ADDRESS:?}"
KEY_DIR=/var/lib/temps/wireguard
SHARED_DIR="${DEV_CLUSTER_STATE_DIR:-/run/temps-bootstrap}/wireguard"
PEERS_DIR="$SHARED_DIR/peers"

log() { printf '\033[1;35m[wg %s]\033[0m %s\n' "$WG_NODE_NAME" "$*"; }
die() { log "ERROR: $*"; exit 1; }
iface_for() { ip -o -4 addr show | awk -v ip="$1" '{split($4, a, "/"); if (a[1] == ip) print $2}'; }

ensure_key() {
  install -d -m 0700 "$KEY_DIR"
  if [[ ! -s "$KEY_DIR/private.key" ]]; then
    (umask 077 && wg genkey > "$KEY_DIR/private.key")
  fi
  wg pubkey < "$KEY_DIR/private.key"
}

create_interface() {
  # A restarted container keeps its network namespace: start clean.
  ip link del wg0 2>/dev/null || true
  ip link add wg0 type wireguard
  wg set wg0 private-key "$KEY_DIR/private.key" ${1:+listen-port "$1"}
  ip address add "$WG_ADDRESS" dev wg0
  ip link set wg0 up
}

run_hub() {
  : "${WG_LISTEN_PORT:?}" "${WG_PUBLIC_INTERFACE_IP:?}" "${WG_HOST_GATEWAY_IP:?}"
  local wan_if
  wan_if="$(iface_for "$WG_PUBLIC_INTERFACE_IP")"
  [[ -n "$wan_if" ]] || die "no interface holds $WG_PUBLIC_INTERFACE_IP"

  # The only things the wan may send this node: WireGuard, replies to
  # connections it opened (image pulls), and the API port the Docker host
  # publishes for the test driver.
  iptables -N WG-E2E-WAN 2>/dev/null || iptables -F WG-E2E-WAN
  iptables -A WG-E2E-WAN -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
  iptables -A WG-E2E-WAN -p udp --dport "$WG_LISTEN_PORT" -j ACCEPT
  iptables -A WG-E2E-WAN -s "$WG_HOST_GATEWAY_IP" -j ACCEPT
  iptables -A WG-E2E-WAN -j DROP
  iptables -D INPUT -i "$wan_if" -j WG-E2E-WAN 2>/dev/null || true
  iptables -I INPUT 1 -i "$wan_if" -j WG-E2E-WAN
  log "wan $wan_if ($WG_PUBLIC_INTERFACE_IP): only UDP $WG_LISTEN_PORT and the Docker host may connect"

  local public_key
  public_key="$(ensure_key)"
  create_interface "$WG_LISTEN_PORT"
  # dockerd (started by the entrypoint) sets FORWARD to DROP; let spokes reach
  # each other through the hub.
  sysctl -qw net.ipv4.ip_forward=1
  iptables -C DOCKER-USER -i wg0 -o wg0 -j ACCEPT 2>/dev/null \
    || iptables -I DOCKER-USER 1 -i wg0 -o wg0 -j ACCEPT

  install -d -m 0755 "$SHARED_DIR" "$PEERS_DIR"
  printf '%s\n' "$public_key" > "$SHARED_DIR/$WG_NODE_NAME.pub"
  log "listening on $WG_PUBLIC_INTERFACE_IP:$WG_LISTEN_PORT as $WG_ADDRESS"

  # Add spokes as they register. Idempotent, so it simply keeps running next
  # to `temps serve` for the lifetime of the container. A peer that fails to
  # apply (a malformed key file, a half-written one) is logged and retried on
  # the next pass; it must never stop the loop, or later spokes would never
  # be added while the control plane keeps running.
  (
    set +e
    declare -A added=() failed=()
    while true; do
      for peer_file in "$PEERS_DIR"/*; do
        [[ -f "$peer_file" ]] || continue
        peer_key="" peer_ip=""
        read -r peer_key peer_ip < "$peer_file"
        [[ -n "$peer_key" && -n "$peer_ip" ]] || continue
        [[ "${added[$peer_key]:-}" != "$peer_ip" ]] || continue
        if error="$(wg set wg0 peer "$peer_key" allowed-ips "$peer_ip/32" 2>&1)"; then
          added[$peer_key]="$peer_ip"
          log "added peer $(basename "$peer_file") ($peer_ip)"
        else
          if [[ "${failed[$peer_key]:-}" != "$peer_ip" ]]; then
            failed[$peer_key]="$peer_ip"
            log "could not add peer $(basename "$peer_file") ($peer_ip), will keep retrying: $error"
          fi
        fi
      done
      sleep 2
    done
  ) &
}

run_spoke() {
  : "${WG_DEFAULT_GATEWAY:?}" "${WG_HUB_ENDPOINT:?}" "${WG_HUB_TUNNEL_IP:?}" "${WG_NETWORK:?}"
  ip route replace default via "$WG_DEFAULT_GATEWAY"
  log "default route via NAT router $WG_DEFAULT_GATEWAY"

  # The point of this topology: before the tunnel exists, nothing on the
  # control plane is reachable. If any probe connects, the test would pass
  # over a plain network path and prove nothing about WireGuard.
  local probe
  for probe in ${WG_ISOLATION_PROBES:-}; do
    if timeout 3 bash -c "exec 3<>/dev/tcp/${probe%:*}/${probe##*:}" 2>/dev/null; then
      die "isolation broken: reached $probe without WireGuard"
    fi
    log "isolation ok: $probe unreachable without the tunnel"
  done

  local public_key hub_key=""
  public_key="$(ensure_key)"
  for _ in $(seq 1 300); do
    [[ -s "$SHARED_DIR/control-plane.pub" ]] && hub_key="$(cat "$SHARED_DIR/control-plane.pub")" && break
    sleep 1
  done
  [[ -n "$hub_key" ]] || die "hub public key never appeared in $SHARED_DIR"

  create_interface ""
  wg set wg0 peer "$hub_key" endpoint "$WG_HUB_ENDPOINT" \
    allowed-ips "$WG_NETWORK" persistent-keepalive 25
  install -d -m 0755 "$PEERS_DIR"
  printf '%s %s\n' "$public_key" "${WG_ADDRESS%/*}" > "$PEERS_DIR/$WG_NODE_NAME"
  log "dialing hub $WG_HUB_ENDPOINT as $WG_ADDRESS"

  for i in $(seq 1 90); do
    if ping -c1 -W1 "$WG_HUB_TUNNEL_IP" >/dev/null 2>&1; then
      log "tunnel up after ${i}s: $(wg show wg0 latest-handshakes | awk '{print "handshake at " $2}')"
      return
    fi
    sleep 1
  done
  die "no WireGuard handshake with $WG_HUB_ENDPOINT after 90s"
}

case "$WG_ROLE" in
  hub) run_hub ;;
  spoke) run_spoke ;;
  *) die "WG_ROLE must be hub or spoke, got '$WG_ROLE'" ;;
esac

exec bash "$ROLE_SCRIPT"
