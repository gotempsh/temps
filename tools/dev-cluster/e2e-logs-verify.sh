#!/usr/bin/env bash
# Verify remote worker-node container logs reach searchable history with the
# new container/node filters. Run AFTER e2e-harden.sh (cluster up, 2-replica
# "e2e-app" deployed across worker-1 + worker-2).
#
# Proves, end to end:
#   1. log_chunks rows for the project carry node_id/node_name (remote collector
#      streamed remote container logs into the same chunk pipeline as local).
#   2. /api/logs/search returns those lines tagged with container_id + node_name.
#   3. container_ids filter narrows to a single container ("filter by container").
#   4. node_ids filter narrows to a single worker node.
set -uo pipefail
cd "$(dirname "$0")"
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
IMG="nginxinc/nginx-unprivileged:alpine"
J=/tmp/e2e_logs_cookies.txt
step() { echo; echo "######## $* ########"; }
ts()   { python3 -c "import datetime,sys;print((datetime.datetime.utcnow()+datetime.timedelta(minutes=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%SZ'))" "$1"; }

step "0. LOGIN"
PW=$(sed -n '2p' .state/admin.txt 2>/dev/null)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "  login -> %{http_code}\n"

step "1. RESOLVE project + remote containers (ground truth from DB)"
PID=$($PSQL -tAc "select id from projects where name='e2e-app' order by id desc limit 1;" 2>/dev/null | tr -d '[:space:]')
echo "  project_id=$PID"
$PSQL -c "select dc.node_id, n.name node, left(dc.container_id,12) cid, dc.status from deployment_containers dc left join nodes n on n.id=dc.node_id where dc.deleted_at is null and dc.node_id is not null order by dc.node_id;" 2>&1 | sed 's/^/  /'
# A specific remote container_id + node_id to test the filters with.
FILTER_CID=$($PSQL -tAc "select container_id from deployment_containers where deleted_at is null and node_id is not null order by node_id limit 1;" 2>/dev/null | tr -d '[:space:]')
FILTER_NID=$($PSQL -tAc "select node_id from deployment_containers where deleted_at is null and node_id is not null order by node_id limit 1;" 2>/dev/null | tr -d '[:space:]')
echo "  will test filters with container=${FILTER_CID:0:12} node=$FILTER_NID"

step "2. GENERATE log traffic on each replica"
for w in 1 2; do
  port=$(docker exec temps-harden-worker-$w sh -c "docker ps --filter ancestor=$IMG --format '{{.Ports}}' | grep -oE '0.0.0.0:[0-9]+' | head -1 | cut -d: -f2" 2>/dev/null)
  for _ in $(seq 1 10); do
    docker exec temps-harden-worker-$w sh -c "docker run --rm --network host curlimages/curl:latest -s -o /dev/null -m5 http://127.0.0.1:$port/ 2>/dev/null" >/dev/null 2>&1
  done
  echo "  worker-$w replica (:$port) hit 10x"
done

step "3. WAIT for remote collector (reconcile 30s + flush 10s)"
for i in $(seq 1 15); do
  cnt=$($PSQL -tAc "select count(*) from log_chunks where project_id=$PID and node_id is not null;" 2>/dev/null | tr -d '[:space:]')
  echo "  [$i] log_chunks with node_id for project=$PID: ${cnt:-0}"
  [[ "${cnt:-0}" -ge 1 ]] && break
  sleep 10
done

step "4. log_chunks node attribution (direct proof remote logs landed)"
$PSQL -c "select node_id, node_name, left(container_id,12) cid, sum(line_count) lines, count(*) chunks from log_chunks where project_id=$PID and node_id is not null group by 1,2,3 order by 1;" 2>&1 | sed 's/^/  /'

step "5. API search — ALL containers (expect remote node-tagged lines)"
START=$(ts -120); END=$(ts 5)
curl -s -b $J -X POST http://localhost/api/logs/search -H 'Content-Type: application/json' \
  -d "{\"project_id\":$PID,\"start_time\":\"$START\",\"end_time\":\"$END\",\"page_size\":500}" \
  | python3 -c "
import sys,json
d=json.load(sys.stdin); lines=d.get('lines',[])
remote=[l for l in lines if l.get('node_id') is not None]
print('  total=%d  remote-tagged=%d'%(len(lines),len(remote)))
print('  nodes seen:', sorted({(l.get('node_id'),l.get('node_name')) for l in remote}))
print('  containers seen:', sorted({(l.get('container_id') or '')[:12] for l in remote}))
print('  RESULT:', 'PASS — remote logs are in history' if remote else 'FAIL — no remote-tagged lines')
"

step "6. API search — filter by ONE container (\"filter by container\")"
curl -s -b $J -X POST http://localhost/api/logs/search -H 'Content-Type: application/json' \
  -d "{\"project_id\":$PID,\"start_time\":\"$START\",\"end_time\":\"$END\",\"page_size\":500,\"container_ids\":[\"$FILTER_CID\"]}" \
  | python3 -c "
import sys,json
d=json.load(sys.stdin); lines=d.get('lines',[])
want='$FILTER_CID'
bad=[l for l in lines if l.get('container_id')!=want]
print('  lines=%d  all-match-container=%s'%(len(lines), not bad))
print('  RESULT:', 'PASS' if lines and not bad else 'FAIL')
"

step "7. API search — filter by ONE node (node_ids)"
curl -s -b $J -X POST http://localhost/api/logs/search -H 'Content-Type: application/json' \
  -d "{\"project_id\":$PID,\"start_time\":\"$START\",\"end_time\":\"$END\",\"page_size\":500,\"node_ids\":[$FILTER_NID]}" \
  | python3 -c "
import sys,json
d=json.load(sys.stdin); lines=d.get('lines',[])
want=$FILTER_NID
bad=[l for l in lines if l.get('node_id')!=want]
print('  lines=%d  all-match-node=%s'%(len(lines), not bad))
print('  RESULT:', 'PASS' if lines and not bad else 'FAIL')
"
echo
echo "######## LOGS E2E DONE ########"
