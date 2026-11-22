#!/bin/sh
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Behavioural test of the WireGuard mesh lockdown against a real kernel.
#
# Three containers: A (the host under test), B (a mesh peer, reached over a
# real WireGuard tunnel named temps-wg0) and C (a machine on A's private
# LAN that B has no business reaching). The lockdown is installed on A by the
# real code (tests/it_mesh_lockdown.rs, run as an unprivileged uid holding
# only CAP_NET_ADMIN), and B then probes what gets through.
#
# Usage: run.sh <path-to-it_mesh_lockdown-test-binary>
#   Needs docker, network access to pull images, and a kernel with
#   WireGuard and nf_tables.
set -eu

BIN=${1:?path to the it_mesh_lockdown test binary}
IMAGE=${IMAGE:-debian:stable-slim}
# Unique per run: cleanup only ever touches resources this run created, never
# anything that happens to share a name on a shared Docker host.
PFX=tml-$$-$(date +%s)
cleanup() {
  docker rm -f ${PFX}-a ${PFX}-b ${PFX}-c >/dev/null 2>&1 || true
  docker network rm ${PFX}-under ${PFX}-lan >/dev/null 2>&1 || true
}
trap cleanup EXIT

pass=0; fail=0
check() { # description, expected (yes|no), command...
  desc=$1; want=$2; shift 2
  if "$@" >/dev/null 2>&1; then got=yes; else got=no; fi
  if [ "$got" = "$want" ]; then pass=$((pass+1)); printf 'ok    %s\n' "$desc"
  else fail=$((fail+1)); printf 'FAIL  %s (reachable=%s, wanted %s)\n' "$desc" "$got" "$want"; fi
}

# Let Docker pick unused subnets (a fixed range would collide with another run
# or an existing network) and derive every address from what it chose.
docker network create ${PFX}-under >/dev/null
docker network create ${PFX}-lan >/dev/null
subnet() { docker network inspect -f '{{(index .IPAM.Config 0).Subnet}}' "$1"; }
under_cidr=$(subnet ${PFX}-under)
LAN_CIDR=$(subnet ${PFX}-lan)
host_in() { # CIDR, offset: the address `offset` above the network address
  # Plain integer arithmetic on the whole address, so a /25 or a /29 (whose
  # network address is not .0) stays inside its subnet. Offset 1 is Docker's
  # gateway, so callers use 2 and up.
  printf '%s' "${1%/*}" | awk -F. -v off="$2" '{
    n = (($1 * 256 + $2) * 256 + $3) * 256 + $4 + off
    printf "%d.%d.%d.%d", int(n / 16777216) % 256, int(n / 65536) % 256, int(n / 256) % 256, n % 256
  }'
}
A_UNDER=$(host_in "$under_cidr" 2); B_UNDER=$(host_in "$under_cidr" 3)
A_LAN=$(host_in "$LAN_CIDR" 2);     C_LAN=$(host_in "$LAN_CIDR" 3)

for n in a b c; do
  docker run -d --name ${PFX}-$n --cap-add NET_ADMIN --sysctl net.ipv4.ip_forward=1 \
    -v "$BIN":/it/test:ro "$IMAGE" sleep 3600 >/dev/null
done
docker network connect --ip $A_UNDER ${PFX}-under ${PFX}-a
docker network connect --ip $B_UNDER ${PFX}-under ${PFX}-b
docker network connect --ip $A_LAN ${PFX}-lan ${PFX}-a
docker network connect --ip $C_LAN ${PFX}-lan ${PFX}-c

for n in a b c; do
  docker exec ${PFX}-$n sh -c 'apt-get update -qq >/dev/null && apt-get install -y -qq iproute2 nftables wireguard-tools netcat-openbsd iputils-ping libcap2-bin >/dev/null 2>&1'
done

pub() { docker exec "$1" sh -c 'wg genkey | tee /k | wg pubkey'; }
PUBA=$(pub ${PFX}-a); PUBB=$(pub ${PFX}-b)

# Tunnel: A 10.201.0.1, B 10.201.0.2. B also routes C's LAN through A, so
# anything B can push to C must be forwarded by A.
docker exec ${PFX}-a sh -c "
  ip link add temps-wg0 type wireguard &&
  wg set temps-wg0 private-key /k listen-port 51820 peer $PUBB allowed-ips 10.201.0.2/32 &&
  ip addr add 10.201.0.1/16 dev temps-wg0 && ip link set temps-wg0 up"
docker exec ${PFX}-b sh -c "
  ip link add temps-wg0 type wireguard &&
  wg set temps-wg0 private-key /k peer $PUBA endpoint $A_UNDER:51820 allowed-ips 10.201.0.1/32,$LAN_CIDR persistent-keepalive 1 &&
  ip addr add 10.201.0.2/16 dev temps-wg0 && ip link set temps-wg0 up &&
  ip route add $LAN_CIDR dev temps-wg0"

# C answers over the LAN: it needs a way back to the mesh, through A.
docker exec ${PFX}-c ip route add 10.201.0.0/16 via $A_LAN

# Services on A (never meant to be reachable over the mesh, except where the
# lockdown says so) and on C.
docker exec -d ${PFX}-a sh -c 'nc -lk -p 8080 -s 0.0.0.0'
docker exec -d ${PFX}-a sh -c 'nc -lk -p 9999 -s 0.0.0.0'
docker exec -d ${PFX}-a sh -c 'nc -lu -p 4789 -s 0.0.0.0 > /udp.out'
docker exec -d ${PFX}-c sh -c 'nc -lk -p 22 -s 0.0.0.0'
sleep 2

in_b() { docker exec ${PFX}-b "$@"; }
tcp() { in_b sh -c "nc -z -w 3 $1 $2"; }
udp_delivered() { # B sends a datagram to A:4789 over the mesh; did A see it?
  docker exec ${PFX}-a sh -c ': > /udp.out'
  in_b sh -c "echo probe | nc -u -w 1 10.201.0.1 4789" || true
  sleep 1
  docker exec ${PFX}-a sh -c 'test -s /udp.out'
}
it() { # run one integration test on A as uid 1000 holding only NET_ADMIN
  docker exec ${PFX}-a sh -c "
    cp /it/test /tmp/it && chmod 755 /tmp/it && setcap cap_net_admin+ep /tmp/it &&
    setcap cap_net_admin+ep \$(readlink -f \$(command -v nft)) &&
    setpriv --reuid=1000 --regid=1000 --clear-groups --bounding-set=-all,+net_admin \
      env TEMPS_RUN_DIND_TESTS=1 /tmp/it --exact $1 --test-threads=1 --nocapture"
}

echo "== before any lockdown: the tunnel is an open door"
check "mesh peer reaches an arbitrary service on A"          yes tcp 10.201.0.1 8080
check "mesh peer reaches a service on A's private LAN via A" yes tcp $C_LAN 22

echo "== install the lockdown with the real code, no node API"
it lockdown_installs_once_and_is_then_current
check "mesh peer reaches an arbitrary service on A"          no  tcp 10.201.0.1 8080
check "mesh peer reaches the (unconfigured) node API port"   no  tcp 10.201.0.1 9999
check "mesh peer's ping to A is answered"                    yes in_b ping -c 1 -W 3 10.201.0.1
check "mesh peer's VXLAN datagrams reach A"                  yes udp_delivered
check "mesh peer cannot reach A's private LAN through A"     no  tcp $C_LAN 22
check "underlay path to A is untouched by the lockdown"      yes in_b sh -c "nc -z -w 3 $A_UNDER 8080"

echo "== replies to connections A opens over the mesh are accepted"
docker exec -d ${PFX}-b sh -c 'nc -lk -p 7000 -s 0.0.0.0'
sleep 1
check "A reaches B over the mesh"                            yes docker exec ${PFX}-a sh -c 'nc -z -w 3 10.201.0.2 7000'

echo "== configure the node API port: only that port opens"
it a_changed_setting_replaces_the_table_atomically
check "node API port reachable from the mesh"                yes tcp 10.201.0.1 9999
check "other ports stay closed"                              no  tcp 10.201.0.1 8080

echo "== a published container port (DNAT) stays reachable, its target does not"
docker exec ${PFX}-a sh -c '
  nft add table ip pub &&
  nft "add chain ip pub pre { type nat hook prerouting priority -100; }" &&
  nft add rule ip pub pre iifname temps-wg0 tcp dport 2222 dnat to '"$C_LAN"':22'
check "DNAT'd port is reachable over the mesh"               yes tcp 10.201.0.1 2222
check "its target is still not reachable directly"           no  tcp $C_LAN 22
docker exec ${PFX}-a nft delete table ip pub

echo "== repair after the table is flushed behind its back"
docker exec ${PFX}-a sh -c 'nft flush table inet temps_mesh'
check "flushed table fails open (what the repair loop fixes)" yes tcp 10.201.0.1 8080
it a_flushed_table_is_repaired
check "after repair other ports are closed again"            no  tcp 10.201.0.1 8080
check "after repair the node API port is open"               yes tcp 10.201.0.1 9999

echo "== repair after the table is deleted"
it a_deleted_table_is_reinstalled
check "after reinstall other ports are closed"               no  tcp 10.201.0.1 8080
check "private LAN is still not reachable through A"         no  tcp $C_LAN 22

echo
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
