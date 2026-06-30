#!/usr/bin/env bash
# Deploy the OpenTelemetry demo (examples/otel-demo) across the dev cluster as
# two services:
#   - obs-backend  (ROLE=backend)  — plain instrumented echo
#   - obs-gateway  (ROLE=gateway)  — calls obs-backend over cluster DNS, and
#                                    self-drives traffic incl. a /boom error path
# Produces distributed traces (OTLP -> Temps /api/otel) and synthetic errors
# (Sentry -> Temps error tracking). Image built once on the host, docker save'd,
# and pushed via the image-upload deploy endpoint (no registry). Idempotent.
set -uo pipefail
cd "$(dirname "$0")"
REPO_ROOT="$(cd ../.. && pwd)"
J=/tmp/otel_deploy.txt
TAG="temps-otel-demo:local"
TAR="/tmp/temps-otel-demo-image.tar"
CP_INTERNAL="10.42.0.10"   # control-plane reachable cluster IP (app.localho.st is loopback in containers)
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
step() { echo; echo "######## $* ########"; }

PW=$(sed -n '2p' .state/admin.txt)
curl -s -c "$J" -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null -w "login -> %{http_code}\n"

step "BUILD $TAG + docker save -> tarball"
docker build -q -t "$TAG" "$REPO_ROOT/examples/otel-demo" >/dev/null 2>&1 && echo "  built $TAG"
docker save "$TAG" -o "$TAR" && echo "  saved $(du -h "$TAR" | cut -f1)"

# deploy_one <project-name> <env-json-object>
deploy_one() {
  local name="$1"
  local envjson="$2"
  echo; echo "==== deploy $name ===="
  local pid
  pid=$(curl -s -b "$J" http://localhost/api/projects | python3 -c "import sys,json;d=json.load(sys.stdin);items=d if isinstance(d,list) else d.get('projects',d.get('data',[]));print(next((p['id'] for p in items if p.get('name')=='$name'),''))")
  if [[ -z "$pid" ]]; then
    pid=$(curl -s -b "$J" -X POST http://localhost/api/projects -H 'Content-Type: application/json' \
      -d "{\"name\":\"$name\",\"directory\":\".\",\"main_branch\":\"main\",\"preset\":\"dockerfile\",\"storage_service_ids\":[],\"source_type\":\"docker_image\"}" \
      | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))")
    echo "  created project=$pid"
  else
    echo "  reusing project=$pid"
  fi
  local eid
  eid=$(curl -s -b "$J" http://localhost/api/projects/"$pid"/environments | python3 -c "import sys,json;print(json.load(sys.stdin)[0]['id'])")
  echo "  env=$eid"

  echo "$envjson" | python3 -c "import sys,json;[print(k+'\t'+v) for k,v in json.load(sys.stdin).items()]" | while IFS=$'\t' read -r k v; do
    code=$(curl -s -b "$J" -X POST http://localhost/api/projects/"$pid"/env-vars -H 'Content-Type: application/json' \
      -d "{\"key\":\"$k\",\"value\":\"$v\",\"environment_ids\":[$eid],\"include_in_preview\":true}" -o /dev/null -w "%{http_code}")
    echo "    env $k -> $code"
  done

  curl -s -b "$J" -X PUT http://localhost/api/projects/"$pid"/environments/"$eid"/settings -H 'Content-Type: application/json' \
    -d '{"replicas":2,"target_nodes":[0,1,2],"anti_affinity":true,"exposed_port":8080}' -o /dev/null -w "    settings -> %{http_code}\n"

  local did
  did=$(curl -s -b "$J" -X POST "http://localhost/api/projects/$pid/environments/$eid/deploy/image-upload?tag=$TAG&health_check_path=/health" \
    -F "file=@$TAR" | python3 -c "import sys,json;print(json.load(sys.stdin).get('id',''))" 2>/dev/null)
  echo "    deployment=$did"
  local i rows st
  for i in $(seq 1 40); do
    rows=$($PSQL -tAc "select count(*) from deployment_containers where deployment_id=$did and deleted_at is null;" 2>/dev/null | tr -d '[:space:]')
    st=$($PSQL -tAc "select state from deployments where id=$did;" 2>/dev/null | tr -d '[:space:]')
    echo "    [$i] running ${rows:-0}/2  state=$st"
    { [[ "${rows:-0}" -ge 2 ]] || [[ "$st" == "failed" ]]; } && break
    sleep 6
  done
  $PSQL -c "select coalesce(n.name,'control-plane') node, left(dc.container_id,12) cid, dc.status from deployment_containers dc left join nodes n on n.id=dc.node_id where dc.deployment_id=$did and dc.deleted_at is null order by dc.node_id nulls last;" 2>&1 | sed 's/^/    /'
}

step "DEPLOY obs-backend (ROLE=backend)"
deploy_one obs-backend "{\"ROLE\":\"backend\",\"CP_INTERNAL_HOST\":\"$CP_INTERNAL\"}"

step "DEPLOY obs-gateway (ROLE=gateway -> production.obs-backend.temps.local)"
deploy_one obs-gateway "{\"ROLE\":\"gateway\",\"TARGET\":\"production.obs-backend.temps.local\",\"CP_INTERNAL_HOST\":\"$CP_INTERNAL\"}"

rm -f "$TAR"
echo; echo "######## OTEL DEMO DEPLOYED ########"
echo "  gateway self-drives traffic -> backend over cluster DNS"
echo "  traces  -> http://$CP_INTERNAL/api/otel  (service.name = obs-gateway / obs-backend)"
echo "  errors  -> Sentry DSN -> Temps error tracking (the /boom path)"
