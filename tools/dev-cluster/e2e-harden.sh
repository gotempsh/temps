#!/usr/bin/env bash
# From-scratch e2e for the multi-node hardening branch: fresh cluster (rebuilt
# from this worktree), require_mtls on, 2 workers join over mTLS, deploy a
# 2-replica app spread across them, verify it serves. Plus the new security
# surfaces (enrollment-token mint returns ca_fingerprint; masked settings DTO).
set -uo pipefail
cd "$(dirname "$0")"
DC="docker compose -p temps-harden-test -f docker-compose.yml -f docker-compose.harden.yml"
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
IMG="nginxinc/nginx-unprivileged:alpine"

step() { echo; echo "######## $* ########"; }

wait_healthy() { # $1=container  $2=max iters ; bounces on containerd stall
  local c=$1 max=$2 i
  for ((i=1;i<=max;i++)); do
    local h; h=$(docker inspect "$c" --format '{{.State.Health.Status}}' 2>/dev/null || echo gone)
    echo "  [$i] $c=$h"
    [[ "$h" == "healthy" ]] && return 0
    if docker logs --tail 1 "$c" 2>&1 | grep -q containerd && (( i % 4 == 0 )); then
      echo "  (containerd stall -> bounce $c)"; docker restart "$c" >/dev/null 2>&1
    fi
    sleep 12
  done; return 1
}

step "1. TEAR DOWN + remove state volumes (fresh)"
$DC down >/dev/null 2>&1
docker volume rm temps-harden-postgres-data temps-harden-cp-data temps-harden-cp-docker \
  temps-harden-w1-data temps-harden-w1-docker temps-harden-w1-home \
  temps-harden-w2-data temps-harden-w2-docker temps-harden-w2-home \
  temps-harden-w3-data temps-harden-w3-docker temps-harden-w3-home >/dev/null 2>&1
rm -rf .state
echo "  done"

step "2. BRING UP postgres + control-plane (rebuilds from worktree)"
$DC up -d postgres control-plane >/dev/null 2>&1
wait_healthy temps-harden-control-plane 80 || { echo "CP did not become healthy"; exit 1; }
echo "  CP healthy"

step "3. ENABLE require_mtls before workers join"
$PSQL -v ON_ERROR_STOP=1 -c \
 "update settings set data = jsonb_set(COALESCE(data::jsonb,'{}'),'{multi_node,require_mtls}','true'::jsonb)::json where id=1;" 2>&1 | tail -1
$PSQL -tAc "select data::jsonb->'multi_node'->'require_mtls' from settings where id=1;" 2>/dev/null | sed 's/^/  require_mtls = /'

step "4. BRING UP worker-1 + worker-2 (join over mTLS)"
$DC up -d worker-1 worker-2 >/dev/null 2>&1
for ((i=1;i<=70;i++)); do
  j1=$(docker exec temps-harden-worker-1 test -f /var/lib/temps/.dev-cluster-join-done 2>/dev/null && echo 1 || echo 0)
  j2=$(docker exec temps-harden-worker-2 test -f /var/lib/temps/.dev-cluster-join-done 2>/dev/null && echo 1 || echo 0)
  echo "  [$i] joined: w1=$j1 w2=$j2"
  [[ "$j1" == "1" && "$j2" == "1" ]] && break
  for w in worker-1 worker-2; do
    docker logs --tail 1 temps-harden-$w 2>&1 | grep -q containerd && $DC restart $w >/dev/null 2>&1
  done
  sleep 12
done

step "5. VERIFY nodes registered + mTLS (https addresses)"
$PSQL -c "select id,name,status,address from nodes order by id;" 2>&1 | sed 's/^/  /'
docker logs --tail 200 temps-harden-worker-1 2>&1 | grep -i "mutual TLS" | tail -1 | sed 's/^/  w1: /'

step "6. PRE-PULL image on workers + CP"
for c in temps-harden-worker-1 temps-harden-worker-2 temps-harden-control-plane; do
  docker exec "$c" sh -c "docker pull $IMG >/dev/null 2>&1 && echo present || echo FAIL" 2>&1 | sed "s/^/  $c: /"
done

step "7. DEPLOY a 2-replica app over the (mTLS) channel"
J=/tmp/e2e_cookies.txt
PW=$(sed -n '2p' .state/admin.txt 2>/dev/null)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "  login -> %{http_code}\n"
# node ids
NIDS=$(curl -s -b $J http://localhost/api/internal/nodes | python3 -c "import sys,json;print(','.join(str(n['id']) for n in json.load(sys.stdin)['nodes']))" 2>/dev/null)
echo "  worker node ids: $NIDS"
PID=$(curl -s -b $J -X POST http://localhost/api/projects -H 'Content-Type: application/json' \
  -d '{"name":"e2e-app","directory":".","main_branch":"main","preset":"dockerfile","storage_service_ids":[],"source_type":"docker_image"}' \
  | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
EID=$(curl -s -b $J http://localhost/api/projects/$PID/environments | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])" 2>/dev/null)
echo "  project=$PID env=$EID"
curl -s -b $J -X PUT http://localhost/api/projects/$PID/environments/$EID/settings -H 'Content-Type: application/json' \
  -d "{\"replicas\":2,\"target_nodes\":[$NIDS],\"anti_affinity\":true,\"exposed_port\":8080}" -o /dev/null -w "  set replicas=2 -> %{http_code}\n"
DID=$(curl -s -b $J -X POST http://localhost/api/projects/$PID/environments/$EID/deploy/image -H 'Content-Type: application/json' \
  -d "{\"image_ref\":\"$IMG\",\"health_check_path\":\"/\"}" | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
echo "  deployment=$DID"
for ((i=1;i<=18;i++)); do
  c1=$(docker exec temps-harden-worker-1 sh -c "docker ps -q --filter ancestor=$IMG|wc -l" 2>/dev/null|tr -d ' ')
  c2=$(docker exec temps-harden-worker-2 sh -c "docker ps -q --filter ancestor=$IMG|wc -l" 2>/dev/null|tr -d ' ')
  echo "  [$i] replicas: w1=$c1 w2=$c2"
  [[ "$c1" -ge 1 && "$c2" -ge 1 ]] && break
  sleep 8
done

step "8. VERIFY both replicas serve"
$PSQL -c "select node_id,left(container_id,12) c,status from deployment_containers where deployment_id=$DID and deleted_at is null order by node_id;" 2>&1 | sed 's/^/  /'
for w in 1 2; do
  port=$(docker exec temps-harden-worker-$w sh -c "docker ps --filter ancestor=$IMG --format '{{.Ports}}' | grep -oE '0.0.0.0:[0-9]+' | head -1 | cut -d: -f2" 2>/dev/null)
  code=$(docker exec temps-harden-worker-$w sh -c "docker run --rm --network host curlimages/curl:latest -s -o /dev/null -w '%{http_code}' -m5 http://127.0.0.1:$port/ 2>/dev/null" 2>/dev/null)
  echo "    worker-$w replica (host :$port) -> HTTP $code"
done

step "9. NEW SECURITY SURFACES"
echo "  -- enrollment token mint (expect ca_fingerprint populated, mTLS is on) --"
curl -s -b $J -X POST http://localhost/api/settings/enrollment-tokens -H 'Content-Type: application/json' -d '{"ttl_secs":3600,"max_uses":1}' \
  | python3 -c "import sys,json;d=json.load(sys.stdin);print('    token_len=%d ca_fingerprint=%s'%(len(d.get('token','')), d.get('ca_fingerprint')))" 2>&1
echo "  -- cap rejection: ttl_secs too large (expect 400) --"
curl -s -b $J -X POST http://localhost/api/settings/enrollment-tokens -H 'Content-Type: application/json' -d '{"ttl_secs":999999999}' -o /dev/null -w "    ttl too large -> HTTP %{http_code} (expect 400)\n"
echo "  -- masked settings DTO exposes posture, no CA key --"
curl -s -b $J http://localhost/api/settings | python3 -c "import sys,json;m=json.load(sys.stdin).get('multi_node',{});print('    require_mtls=%s legacy_token=%s ca_fp=%s... key_leaked=%s'%(m.get('require_mtls'),m.get('legacy_shared_token_enabled'),str(m.get('cluster_ca_fingerprint'))[:16],'cluster_ca_key' in json.dumps(m)))" 2>&1
echo
echo "######## E2E DONE ########"
