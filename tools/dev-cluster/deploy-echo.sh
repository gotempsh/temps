#!/usr/bin/env bash
# Deploy the first-party echo example (examples/echo-server) with 3 replicas
# spread across the dev cluster (control-plane + worker-1 + worker-2), demoing
# multi-node scheduling + the node-identity env vars
# (TEMPS_NODE_NAME / TEMPS_NODE_ID / TEMPS_REPLICA).
#
# The image is built once on the host, `docker save`d, and pushed via Temps'
# image-upload deploy endpoint — Temps imports it and transfers it to each
# target node (no registry needed, no `docker pull`). Idempotent: reuses an
# existing "echo" project if present, else creates it.
set -uo pipefail
cd "$(dirname "$0")"
REPO_ROOT="$(cd ../.. && pwd)"
J=/tmp/echo_deploy.txt
TAG="temps-echo:local"
TAR="/tmp/temps-echo-image.tar"
step() { echo; echo "######## $* ########"; }
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
projects_json() { python3 -c "import sys,json;d=json.load(sys.stdin);print(' '.join(f\"{p['id']}:{p.get('name')}\" for p in (d if isinstance(d,list) else d.get('projects',d.get('data',[])))))"; }

PW=$(sed -n '2p' .state/admin.txt)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "login -> %{http_code}\n"

step "1. BUILD $TAG on host + docker save -> tarball"
docker build -q -t "$TAG" "$REPO_ROOT/examples/echo-server" >/dev/null 2>&1 && echo "  built $TAG"
docker save "$TAG" -o "$TAR" && echo "  saved $(du -h "$TAR" | cut -f1) -> $TAR"

step "2. FIND-OR-CREATE project 'echo'"
PID=$(curl -s -b $J http://localhost/api/projects | python3 -c "import sys,json;d=json.load(sys.stdin);items=d if isinstance(d,list) else d.get('projects',d.get('data',[]));print(next((p['id'] for p in items if p.get('name')=='echo'),''))")
if [[ -z "$PID" ]]; then
  PID=$(curl -s -b $J -X POST http://localhost/api/projects -H 'Content-Type: application/json' \
    -d '{"name":"echo","directory":".","main_branch":"main","preset":"dockerfile","storage_service_ids":[],"source_type":"docker_image"}' \
    | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))")
  echo "  created project=$PID"
else
  echo "  reusing project=$PID"
fi
EID=$(curl -s -b $J http://localhost/api/projects/$PID/environments | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")
echo "  env=$EID"

step "3. SET env vars (GREETING, APP_VERSION, OWNER) — tolerant of re-runs"
for kv in "GREETING=hello-from-temps" "APP_VERSION=1.0.0" "OWNER=david"; do
  k=${kv%%=*}; v=${kv#*=}
  code=$(curl -s -b $J -X POST http://localhost/api/projects/$PID/env-vars -H 'Content-Type: application/json' \
    -d "{\"key\":\"$k\",\"value\":\"$v\",\"environment_ids\":[$EID],\"include_in_preview\":true}" -o /dev/null -w "%{http_code}")
  echo "  set $k -> $code"
done

step "4. SETTINGS: 3 replicas, all nodes [0,1,2], anti-affinity, exposed_port 8080"
curl -s -b $J -X PUT http://localhost/api/projects/$PID/environments/$EID/settings -H 'Content-Type: application/json' \
  -d '{"replicas":3,"target_nodes":[0,1,2],"anti_affinity":true,"exposed_port":8080}' \
  -o /dev/null -w "  settings -> %{http_code}\n"

step "5. UPLOAD-DEPLOY $TAG (import on CP, transfer to each node, no pull)"
DID=$(curl -s -b $J -X POST "http://localhost/api/projects/$PID/environments/$EID/deploy/image-upload?tag=$TAG&health_check_path=/" \
  -F "file=@$TAR" | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
echo "  deployment=$DID"

step "6. WAIT for 3 replicas across nodes"
for i in $(seq 1 40); do
  rows=$($PSQL -tAc "select count(*) from deployment_containers where deployment_id=$DID and deleted_at is null;" 2>/dev/null | tr -d '[:space:]')
  st=$($PSQL -tAc "select state from deployments where id=$DID;" 2>/dev/null | tr -d '[:space:]')
  echo "  [$i] running: ${rows:-0}/3  (deployment state=$st)"
  { [[ "${rows:-0}" -ge 3 ]] || [[ "$st" == "failed" ]]; } && break
  sleep 8
done

step "7. PLACEMENT"
$PSQL -c "select dc.node_id, coalesce(n.name,'control-plane') node, left(dc.container_id,12) cid, dc.status from deployment_containers dc left join nodes n on n.id=dc.node_id where dc.deployment_id=$DID and dc.deleted_at is null order by dc.node_id;" 2>&1 | sed 's/^/  /'

rm -f "$TAR"
echo; echo "PID=$PID EID=$EID DID=$DID"
echo "######## ECHO DEPLOY DONE ########"
