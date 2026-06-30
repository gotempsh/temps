#!/usr/bin/env bash
# Prove the filter-dropdown fix: available_sources lists ALL containers/nodes for
# the scope regardless of the active container/node filter (so you can switch
# between them without first resetting to "All").
set -uo pipefail
cd "$(dirname "$0")"
J=/tmp/e2e_sources_cookies.txt
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
IMG="nginxinc/nginx-unprivileged:alpine"
ts() { python3 -c "import datetime,sys;print((datetime.datetime.now(datetime.UTC)+datetime.timedelta(minutes=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%SZ'))" "$1"; }
step() { echo; echo "######## $* ########"; }

PW=$(sed -n '2p' .state/admin.txt)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "login -> %{http_code}\n"

PID=$($PSQL -tAc "select id from projects where name='e2e-app' order by id desc limit 1;" | tr -d '[:space:]')
FILTER_CID=$($PSQL -tAc "select container_id from deployment_containers where deleted_at is null and node_id is not null order by node_id limit 1;" | tr -d '[:space:]')
START=$(ts -120); END=$(ts 5)

# Generate a little fresh traffic so the resumed collector has lines to ship.
for w in 1 2; do
  port=$(docker exec temps-harden-worker-$w sh -c "docker ps --filter ancestor=$IMG --format '{{.Ports}}' | grep -oE '0.0.0.0:[0-9]+' | head -1 | cut -d: -f2" 2>/dev/null)
  for _ in $(seq 1 5); do docker exec temps-harden-worker-$w sh -c "docker run --rm --network host curlimages/curl:latest -s -o /dev/null -m5 http://127.0.0.1:$port/ 2>/dev/null" >/dev/null 2>&1; done
done
sleep 35

search() { # $1 = extra json filter fields
  curl -s -b $J -X POST http://localhost/api/logs/search -H 'Content-Type: application/json' \
    -d "{\"project_id\":$PID,\"start_time\":\"$START\",\"end_time\":\"$END\",\"page_size\":500$1}"
}
sources() { python3 -c "
import sys,json
d=json.load(sys.stdin)
src=d.get('available_sources',[])
conts=sorted({(s.get('container_id') or '')[:12] for s in src})
nodes=sorted({(s.get('node_id'),s.get('node_name')) for s in src})
lines=d.get('lines',[])
linec=sorted({(l.get('container_id') or '')[:12] for l in lines})
print('  result line containers:', linec)
print('  available_sources containers:', conts)
print('  available_sources nodes:', nodes)
print('  RESULT:', 'PASS — dropdown lists all sources' if len(conts)>=2 else 'FAIL — dropdown collapsed')
"; }

step "A. NO filter — baseline (expect 2 containers, 2 nodes available)"
search "" | sources

step "B. FILTER by one container — available_sources must STILL list BOTH"
search ",\"container_ids\":[\"$FILTER_CID\"]" | sources

step "C. FILTER by one node — available_sources must STILL list BOTH"
search ",\"node_ids\":[1]" | sources

echo; echo "######## SOURCES E2E DONE ########"
