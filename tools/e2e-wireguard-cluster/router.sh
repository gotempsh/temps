#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# A home/office NAT router for one isolated LAN: hosts behind it can open
# connections out to the wan, and nothing on the wan can open a connection
# in. There are deliberately no port forwards, so a node behind this router
# is reachable only through a tunnel it dials itself.
set -eu

: "${ROUTER_WAN_IP:?}"
: "${ROUTER_WAN_GATEWAY:?}"
: "${ROUTER_LAN_SUBNET:?}"

# Docker may pick the LAN bridge as the default route; this router's way to
# the internet is its wan side.
ip route replace default via "$ROUTER_WAN_GATEWAY"
apk add --no-cache -q iptables iproute2 >/dev/null

iface_for() { ip -o -4 addr show | awk -v ip="$1" '{split($4, a, "/"); if (a[1] == ip) print $2}'; }
WAN_IF="$(iface_for "$ROUTER_WAN_IP")"
if [ -z "$WAN_IF" ]; then
  echo "[router] no interface holds $ROUTER_WAN_IP" >&2
  exit 1
fi

sysctl -qw net.ipv4.ip_forward=1
iptables -P FORWARD DROP
iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -A FORWARD -s "$ROUTER_LAN_SUBNET" ! -o "$WAN_IF" -j DROP
iptables -A FORWARD -s "$ROUTER_LAN_SUBNET" -o "$WAN_IF" -j ACCEPT
iptables -t nat -A POSTROUTING -s "$ROUTER_LAN_SUBNET" -o "$WAN_IF" -j MASQUERADE

echo "[router] NAT $ROUTER_LAN_SUBNET -> $WAN_IF ($ROUTER_WAN_IP); inbound: established only"
touch /run/router-ready
exec sleep infinity
