#!/usr/bin/env bash
#
# Tear down the multi-host overlay on this node. Idempotent.

set -euo pipefail

BRIDGE_NAME="${BRIDGE_NAME:-br-temps0}"
VXLAN_NAME="${VXLAN_NAME:-vxlan-temps0}"
DOCKER_NETWORK="${DOCKER_NETWORK:-temps0}"
NFT_TABLE="temps_network"

log() { printf '[node-down] %s\n' "$*"; }

# Docker network first — removing the bridge underneath an attached network
# leaves Docker confused.
if docker network inspect "$DOCKER_NETWORK" >/dev/null 2>&1; then
  log "removing docker network $DOCKER_NETWORK"
  docker network rm "$DOCKER_NETWORK" >/dev/null || \
    log "warning: docker network rm failed (containers may still be attached)"
fi

# nftables.
if nft list table inet "$NFT_TABLE" >/dev/null 2>&1; then
  log "removing nftables table $NFT_TABLE"
  nft delete table inet "$NFT_TABLE"
fi

# VXLAN.
if ip link show "$VXLAN_NAME" >/dev/null 2>&1; then
  log "removing vxlan $VXLAN_NAME"
  ip link del "$VXLAN_NAME"
fi

# Bridge.
if ip link show "$BRIDGE_NAME" >/dev/null 2>&1; then
  log "removing bridge $BRIDGE_NAME"
  ip link del "$BRIDGE_NAME"
fi

log "node down"
