#!/usr/bin/env bash
#
# Bring a single node onto the multi-host overlay.
#
# Required env vars:
#   LOCAL_CIDR         e.g. 172.20.1.0/24      — this node's compute CIDR
#   LOCAL_BRIDGE_IP    e.g. 172.20.1.1         — bridge gateway address
#   PEER_UNDERLAY      e.g. 10.0.0.2           — other node's underlay IP
#   PEER_CIDR          e.g. 172.20.2.0/24      — other node's compute CIDR
#
# Optional env vars (defaults match crates/temps-network):
#   BRIDGE_NAME        default: br-temps0
#   VXLAN_NAME         default: vxlan-temps0
#   VXLAN_VNI          default: 42
#   VXLAN_PORT         default: 4789
#   UNDERLAY_DEV       default: auto-detected from default route
#   UNDERLAY_MTU       default: 1500
#   DOCKER_NETWORK     default: temps0
#
# Idempotent: re-running with the same env is a no-op.

set -euo pipefail

require() { [[ -n "${!1:-}" ]] || { echo "missing env: $1" >&2; exit 1; }; }
require LOCAL_CIDR
require LOCAL_BRIDGE_IP
require PEER_UNDERLAY
require PEER_CIDR

BRIDGE_NAME="${BRIDGE_NAME:-br-temps0}"
VXLAN_NAME="${VXLAN_NAME:-vxlan-temps0}"
VXLAN_VNI="${VXLAN_VNI:-42}"
VXLAN_PORT="${VXLAN_PORT:-4789}"
UNDERLAY_MTU="${UNDERLAY_MTU:-1500}"
DOCKER_NETWORK="${DOCKER_NETWORK:-temps0}"
NFT_TABLE="temps_network"
BRIDGE_MTU=$(( UNDERLAY_MTU - 50 ))   # VXLAN overhead

if [[ -z "${UNDERLAY_DEV:-}" ]]; then
  UNDERLAY_DEV="$(ip -4 route show default | awk '/default/ {print $5; exit}')"
  [[ -n "$UNDERLAY_DEV" ]] || { echo "could not auto-detect UNDERLAY_DEV" >&2; exit 1; }
fi

log() { printf '[node-up] %s\n' "$*"; }

# 1. ip_forward (running kernel only; persistence is the operator's job).
log "enabling ipv4 forwarding"
sysctl -wq net.ipv4.ip_forward=1

# 2. Bridge.
if ! ip link show "$BRIDGE_NAME" >/dev/null 2>&1; then
  log "creating bridge $BRIDGE_NAME"
  ip link add "$BRIDGE_NAME" type bridge
fi
ip link set "$BRIDGE_NAME" mtu "$BRIDGE_MTU"
if ! ip -4 addr show dev "$BRIDGE_NAME" | grep -qE "inet ${LOCAL_BRIDGE_IP}/"; then
  log "assigning ${LOCAL_BRIDGE_IP}/${LOCAL_CIDR##*/} to $BRIDGE_NAME"
  ip addr add "${LOCAL_BRIDGE_IP}/${LOCAL_CIDR##*/}" dev "$BRIDGE_NAME"
fi
ip link set "$BRIDGE_NAME" up

# 3. VXLAN device.
if ! ip link show "$VXLAN_NAME" >/dev/null 2>&1; then
  log "creating vxlan $VXLAN_NAME (vni=$VXLAN_VNI port=$VXLAN_PORT parent=$UNDERLAY_DEV)"
  ip link add "$VXLAN_NAME" type vxlan \
    id "$VXLAN_VNI" \
    dstport "$VXLAN_PORT" \
    dev "$UNDERLAY_DEV" \
    nolearning
fi
ip link set "$VXLAN_NAME" mtu "$BRIDGE_MTU"
ip link set "$VXLAN_NAME" master "$BRIDGE_NAME"
ip link set "$VXLAN_NAME" up

# 4. FDB entry for the peer (idempotent: append-or-noop).
if ! bridge fdb show dev "$VXLAN_NAME" | grep -q "dst $PEER_UNDERLAY "; then
  log "adding fdb entry for peer $PEER_UNDERLAY"
  bridge fdb append 00:00:00:00:00:00 dev "$VXLAN_NAME" dst "$PEER_UNDERLAY"
fi

# 5. Route to the peer's compute CIDR.
if ! ip -4 route show "$PEER_CIDR" | grep -q "dev $VXLAN_NAME"; then
  log "adding route $PEER_CIDR dev $VXLAN_NAME"
  ip route replace "$PEER_CIDR" dev "$VXLAN_NAME"
fi

# 6. nftables baseline.
log "installing nftables table $NFT_TABLE"
nft -f - <<NFT
add table inet $NFT_TABLE
delete table inet $NFT_TABLE
add table inet $NFT_TABLE
add chain inet $NFT_TABLE forward { type filter hook forward priority -100; policy accept; }
add rule inet $NFT_TABLE forward iifname "$BRIDGE_NAME" accept
add rule inet $NFT_TABLE forward oifname "$BRIDGE_NAME" accept
add chain inet $NFT_TABLE postrouting { type nat hook postrouting priority 100; policy accept; }
add rule inet $NFT_TABLE postrouting ip saddr $LOCAL_CIDR oifname != "$BRIDGE_NAME" masquerade
NFT

# 7. Docker network pinned to our bridge.
if ! docker network inspect "$DOCKER_NETWORK" >/dev/null 2>&1; then
  log "creating docker network $DOCKER_NETWORK"
  docker network create \
    --driver bridge \
    --subnet "$LOCAL_CIDR" \
    --gateway "$LOCAL_BRIDGE_IP" \
    --opt "com.docker.network.bridge.name=$BRIDGE_NAME" \
    --opt "com.docker.network.driver.mtu=$BRIDGE_MTU" \
    --opt "com.docker.network.bridge.enable_ip_masquerade=false" \
    "$DOCKER_NETWORK" >/dev/null
fi

log "node up: bridge=$BRIDGE_NAME cidr=$LOCAL_CIDR vxlan=$VXLAN_NAME peer=$PEER_UNDERLAY -> $PEER_CIDR"
log "test: docker run --rm --network $DOCKER_NETWORK alpine ping -c1 ${PEER_CIDR%.*}.10"
