#!/usr/bin/env bash
# One-shot mTLS verification harness (ADR-020 WS-2.1). Brings up CP, enables
# require_mtls BEFORE the worker joins, joins the worker (which then gets a
# signed cert + serves mTLS), and verifies the agent's mutual TLS positively
# (CA-signed client accepted) and negatively (no cert rejected).
set -uo pipefail
cd "$(dirname "$0")"
DC="docker compose -p temps-harden-test -f docker-compose.yml -f docker-compose.harden.yml"

echo "### 1. bring up postgres + control-plane"
$DC up -d postgres control-plane >/dev/null 2>&1

echo "### wait for CP healthy (+ build)"
for i in $(seq 1 90); do
  h=$(docker inspect temps-harden-control-plane --format '{{.State.Health.Status}}' 2>/dev/null || echo gone)
  echo "  [$i] cp=$h"
  [[ "$h" == "healthy" ]] && break
  sleep 15
done

echo "### 2. enable require_mtls in settings (before the worker joins)"
docker exec temps-harden-postgres psql -U temps -d temps -v ON_ERROR_STOP=1 -c \
  "UPDATE settings SET data = jsonb_set(COALESCE(data::jsonb,'{}'::jsonb), '{multi_node,require_mtls}', 'true'::jsonb)::json WHERE id = 1;" 2>&1 | tail -1
docker exec temps-harden-postgres psql -U temps -d temps -tAc \
  "select data::jsonb->'multi_node'->'require_mtls' from settings where id=1;" 2>/dev/null | sed 's/^/  require_mtls = /'

echo "### 3. bring up worker-1"
$DC up -d worker-1 >/dev/null 2>&1
echo "### wait for worker join (nudge dockerd if it stalls)"
for i in $(seq 1 60); do
  joined=$(docker exec temps-harden-worker-1 test -f /var/lib/temps/.dev-cluster-join-done 2>/dev/null && echo 1 || echo 0)
  last=$(docker logs --tail 1 temps-harden-worker-1 2>&1 | tr -d '\r')
  echo "  [$i] joined=$joined :: ${last:0:70}"
  [[ "$joined" == "1" ]] && break
  # nudge a stuck inner dockerd (busy-host containerd timeout)
  if echo "$last" | grep -q "containerd"; then $DC restart worker-1 >/dev/null 2>&1; fi
  sleep 15
done

echo
echo "############ mTLS VERIFICATION ############"
echo "### worker serving mTLS? (agent log)"
docker logs --tail 200 temps-harden-worker-1 2>&1 | grep -iE "mutual TLS|agent server started" | tail -2
echo "### node address (https?)"
docker exec temps-harden-postgres psql -U temps -d temps -tAc "select id,name,address from nodes;" 2>/dev/null

# node token (for the Authorization header)
TOK=$(docker exec temps-harden-worker-1 sh -c 'cat /root/.temps/agent.json' 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])' 2>/dev/null)

echo "### POSITIVE: CP calls agent /agent/health WITH a CA-signed client cert (using the worker's own cert) — expect HTTP 200"
# Copy the worker's CA-signed cert/key/ca into the CP container and curl the agent.
docker exec temps-harden-worker-1 sh -c 'cat /root/.temps/node.cert.pem' > /tmp/w1.cert.pem 2>/dev/null
docker exec temps-harden-worker-1 sh -c 'cat /root/.temps/node.key.pem'  > /tmp/w1.key.pem  2>/dev/null
docker exec temps-harden-worker-1 sh -c 'cat /root/.temps/cluster-ca.pem' > /tmp/w1.ca.pem  2>/dev/null
docker cp /tmp/w1.cert.pem temps-harden-control-plane:/tmp/c.pem 2>/dev/null
docker cp /tmp/w1.key.pem  temps-harden-control-plane:/tmp/k.pem 2>/dev/null
docker cp /tmp/w1.ca.pem   temps-harden-control-plane:/tmp/ca.pem 2>/dev/null
docker exec temps-harden-control-plane sh -c \
  "curl -s -o /dev/null -w 'WITH client cert -> http_code=%{http_code}\n' -m6 --cert /tmp/c.pem --key /tmp/k.pem --cacert /tmp/ca.pem -H 'Authorization: Bearer $TOK' https://10.42.0.21:3100/agent/health" 2>&1

echo "### NEGATIVE: same call WITHOUT a client cert — expect TLS rejection (no http_code)"
docker exec temps-harden-control-plane sh -c \
  "curl -s -o /dev/null -w 'NO client cert -> http_code=%{http_code} exit=%{exitcode}\n' -m6 --cacert /tmp/ca.pem -H 'Authorization: Bearer $TOK' https://10.42.0.21:3100/agent/health" 2>&1 || echo "  (rejected, as expected)"

echo "### agent log: recent handshake outcomes"
docker logs --since 60s temps-harden-worker-1 2>&1 | grep -iE "TLS handshake|health" | tail -4
echo "############ DONE ############"
