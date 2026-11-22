#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# End-to-end check of the managed S3 backends (RustFS and Garage) against a
# running `temps serve`: per-environment credentials, bucket isolation,
# stop/start, migration in both directions, backup/restore and unlink revoke.
#
# Requires: docker, curl, python3, shasum. A running Temps server whose Docker
# daemon is the local one, and an admin API key.
#
# Usage:
#   TEMPS_E2E_API=http://127.0.0.1:3001/api TEMPS_E2E_API_KEY=tk_... \
#     ./scripts/test-managed-s3-e2e.sh
#
# Everything it creates carries a per-run suffix and is deleted on exit
# (services, projects). The one exception is the backup source named
# `e2e-source-<run>`: Temps keeps a source while a backup record references
# it, so it stays behind (run this against a scratch instance). Docker is only used for the
# `aws` CLI helper container that talks to the services over the Temps
# network, exactly as an app container would. Nothing is written outside a
# scratch directory under $TMPDIR.

set -euo pipefail

API="${TEMPS_E2E_API:?set TEMPS_E2E_API to the server API base URL, e.g. http://127.0.0.1:3001/api}"
KEY="${TEMPS_E2E_API_KEY:?set TEMPS_E2E_API_KEY to an admin API key}"
NETWORK="${TEMPS_E2E_DOCKER_NETWORK:-temps-app-network}"
AWS_IMAGE="${TEMPS_E2E_AWS_IMAGE:-amazon/aws-cli:2.17.0}"
RUN="$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/temps-s3-e2e.XXXXXX")"
FAILURES=0
# Created resources are recorded in files: the helpers that create them run
# inside $(...) subshells, where array updates would be lost.
SERVICES="$WORK/services"
PROJECTS="$WORK/projects"
SOURCES="$WORK/sources"
touch "$SERVICES" "$PROJECTS" "$SOURCES"

step() { printf '\n== %s\n' "$*"; }
pass() { printf '   ok    %s\n' "$*"; }
fail() { printf '   FAIL  %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
check() { # check <description> <command...>
  local description="$1"; shift
  if "$@" >/dev/null 2>&1; then pass "$description"; else fail "$description"; fi
}

api() { # api <METHOD> <path> [json body]
  curl -sS -X "$1" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
    ${3:+-d "$3"} "$API$2"
}
json() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

cleanup() {
  step "cleanup"
  while read -r id; do api DELETE "/projects/$id" >/dev/null 2>&1 || true; done <"$PROJECTS"
  while read -r id; do api DELETE "/external-services/$id" >/dev/null 2>&1 || true; done <"$SERVICES"
  while read -r id; do
    api DELETE "/backups/s3-sources/$id" >/dev/null 2>&1 \
      || echo "   note  backup source $id is still referenced by a backup record; delete it manually"
  done <"$SOURCES"
  rm -rf "$WORK"
}
trap cleanup EXIT

wait_running() { # wait_running <service id>
  local status=""
  for _ in $(seq 1 60); do
    status="$(api GET "/external-services/$1" | json "d['service']['status']")"
    [ "$status" = running ] && return 0
    sleep 3
  done
  echo "service $1 is '$status', not running" >&2
  return 1
}

create_service() { # create_service <name> <parameters json> -> id
  local id
  id="$(api POST /external-services "{\"name\":\"$1\",\"service_type\":\"s3\",\"parameters\":$2}" | json "d['id']")"
  echo "$id" >>"$SERVICES"
  wait_running "$id"
  echo "$id"
}

create_project() { # create_project <name> -> "<project id> <production env id> <staging env id>"
  local pid prod staging
  pid="$(api POST /projects "{\"name\":\"$1\",\"preset\":\"custom\",\"directory\":\"/\",\"main_branch\":\"main\",\"source_type\":\"manual\"}" | json "d['id']")"
  echo "$pid" >>"$PROJECTS"
  api POST "/projects/$pid/environments" '{"name":"staging","branch":"staging","set_as_preview":false}' >/dev/null
  prod="$(api GET "/projects/$pid/environments" | json "[e['id'] for e in (d if isinstance(d,list) else d['environments']) if e['name']=='production'][0]")"
  staging="$(api GET "/projects/$pid/environments" | json "[e['id'] for e in (d if isinstance(d,list) else d['environments']) if e['name']=='staging'][0]")"
  echo "$pid $prod $staging"
}

# credentials <service> <project> <env> <file> : issue the environment's runtime variables
credentials() {
  api POST "/external-services/$1/projects/$2/environments/$3/runtime-credentials" >"$WORK/$4.json"
}
var() { python3 -c "import json,sys; print(json.load(open('$WORK/$1.json'))['variables']['$2'])"; }
has_var() { python3 -c "import json; assert '$2' in json.load(open('$WORK/$1.json'))['variables']"; }

# s3 <creds file> <aws args...> : run the aws CLI with an environment's own credentials
s3() {
  local file="$1"; shift
  docker run --rm -i --network "$NETWORK" \
    -e AWS_ACCESS_KEY_ID="$(var "$file" AWS_ACCESS_KEY_ID)" \
    -e AWS_SECRET_ACCESS_KEY="$(var "$file" AWS_SECRET_ACCESS_KEY)" \
    -e AWS_DEFAULT_REGION=us-east-1 \
    "$AWS_IMAGE" --endpoint-url "$(var "$file" S3_ENDPOINT)" "$@"
}
put() { printf '%s' "$3" | s3 "$1" s3 cp - "s3://$2/$4" >/dev/null 2>&1; }
get() { s3 "$1" s3 cp "s3://$2/$3" - 2>/dev/null; }
denied() { ! "$@" >/dev/null 2>&1; }

# admin_s3 <service id> <aws args...> : the service's own key, for verification only
admin_s3() {
  local id="$1"; shift
  local name access secret backend host port
  name="$(api GET "/external-services/$id" | json "d['service']['name']")"
  backend="$(api GET "/external-services/$id" | json "d['current_parameters'].get('backend') or 'rustfs'")"
  access="$(api GET "/external-services/$id/parameters/access_key" | json "d['value']")"
  secret="$(api GET "/external-services/$id/parameters/secret_key" | json "d['value']")"
  host="$backend-$name"
  if [ "$backend" = garage ]; then port=3900; else port=9000; fi
  docker run --rm -i --network "$NETWORK" -e AWS_ACCESS_KEY_ID="$access" \
    -e AWS_SECRET_ACCESS_KEY="$secret" -e AWS_DEFAULT_REGION=us-east-1 \
    "$AWS_IMAGE" --endpoint-url "http://$host:$port" "$@"
}

step "services"
GARAGE="$(create_service "e2e-garage-$RUN" '{"backend":"garage","buckets":["uploads","exports","assets"]}')"
RUSTFS="$(create_service "e2e-rustfs-$RUN" '{}')"
check "garage service reports the garage backend" \
  test "$(api GET "/external-services/$GARAGE" | json "d['current_parameters']['backend']")" = garage
check "garage secrets are masked" \
  test "$(api GET "/external-services/$GARAGE" | json "d['current_parameters']['secret_key']")" = '***'
check "a service created without a backend runs RustFS" \
  test "$(api GET "/external-services/$RUSTFS" | json "d['current_parameters'].get('backend')")" = rustfs
for health in "$GARAGE" "$RUSTFS"; do
  check "service $health health check is operational" \
    test "$(api POST "/external-services/$health/health-check" | json "d['status']")" = operational
done

step "validation"
reject() { # reject <description> <parameters json> <expected message fragment>
  local out
  out="$(api POST /external-services "{\"name\":\"e2e-bad-$RUN\",\"service_type\":\"s3\",\"parameters\":$2}")"
  if echo "$out" | grep -q "$3"; then pass "$1"; else fail "$1 ($out)"; fi
}
reject "new MinIO services are refused" '{"backend":"minio"}' "deprecated"
reject "unknown backends are refused" '{"backend":"ceph"}' "unsupported managed S3 backend"
reject "invalid bucket names are refused" '{"buckets":["Bad_Name"]}' "invalid bucket name"
reject "more than 16 buckets are refused" \
  "{\"buckets\":$(python3 -c "import json;print(json.dumps([f'bkt-{i:03d}' for i in range(40)]))")}" "at most 16"

step "per-environment credentials"
read -r GP GPROD GSTAG <<<"$(create_project "e2e-garage-app-$RUN")"
read -r RP RPROD RSTAG <<<"$(create_project "e2e-rustfs-app-$RUN")"
api POST "/external-services/$GARAGE/projects" "{\"project_id\":$GP}" >/dev/null
api POST "/external-services/$RUSTFS/projects" "{\"project_id\":$RP}" >/dev/null
credentials "$GARAGE" "$GP" "$GPROD" g_prod
credentials "$GARAGE" "$GP" "$GSTAG" g_stag
credentials "$RUSTFS" "$RP" "$RPROD" r_prod
credentials "$RUSTFS" "$RP" "$RSTAG" r_stag
for name in S3_BUCKET S3_ENDPOINT S3_ACCESS_KEY S3_SECRET_KEY AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_ENDPOINT_URL AWS_REGION; do
  check "garage env exposes $name" has_var g_prod "$name"
  check "rustfs env exposes $name" has_var r_prod "$name"
done
for name in S3_BUCKET_UPLOADS S3_BUCKET_EXPORTS S3_BUCKET_ASSETS; do
  check "garage env exposes $name" has_var g_prod "$name"
done
check "S3_BUCKET is the first declared bucket" \
  test "$(var g_prod S3_BUCKET)" = "$(var g_prod S3_BUCKET_UPLOADS)"
check "environments get different access keys" \
  test "$(var g_prod AWS_ACCESS_KEY_ID)" != "$(var g_stag AWS_ACCESS_KEY_ID)"
check "environments get different buckets" \
  test "$(var g_prod S3_BUCKET)" != "$(var g_stag S3_BUCKET)"
credentials "$GARAGE" "$GP" "$GPROD" g_prod2
check "issuing credentials twice returns the same key" \
  test "$(var g_prod AWS_ACCESS_KEY_ID)" = "$(var g_prod2 AWS_ACCESS_KEY_ID)"

step "isolation (garage)"
check "production key writes its own bucket" put g_prod "$(var g_prod S3_BUCKET)" garage-prod obj.txt
check "production key reads it back" test "$(get g_prod "$(var g_prod S3_BUCKET)" obj.txt)" = garage-prod
check "production key writes every declared bucket" put g_prod "$(var g_prod S3_BUCKET_ASSETS)" assets obj.txt
check "staging key cannot write the production bucket" denied put g_stag "$(var g_prod S3_BUCKET)" evil obj.txt
check "staging key cannot read the production bucket" denied get g_stag "$(var g_prod S3_BUCKET)" obj.txt
check "production key cannot write the staging bucket" denied put g_prod "$(var g_stag S3_BUCKET)" evil obj.txt
check "staging key writes its own bucket" put g_stag "$(var g_stag S3_BUCKET)" garage-stag obj.txt

step "isolation (rustfs)"
check "production key writes its own bucket" put r_prod "$(var r_prod S3_BUCKET)" rustfs-prod obj.txt
check "production key reads it back" test "$(get r_prod "$(var r_prod S3_BUCKET)" obj.txt)" = rustfs-prod
check "staging key cannot write the production bucket" denied put r_stag "$(var r_prod S3_BUCKET)" evil obj.txt
check "staging key cannot read the production bucket" denied get r_stag "$(var r_prod S3_BUCKET)" obj.txt
check "staging key writes its own bucket" put r_stag "$(var r_stag S3_BUCKET)" rustfs-stag obj.txt

step "stop / start keeps data and keys"
for id in "$GARAGE" "$RUSTFS"; do api POST "/external-services/$id/stop" >/dev/null; done
for id in "$GARAGE" "$RUSTFS"; do api POST "/external-services/$id/start" >/dev/null; done
for id in "$GARAGE" "$RUSTFS"; do wait_running "$id"; done
check "garage data survives a restart" test "$(get g_prod "$(var g_prod S3_BUCKET)" obj.txt)" = garage-prod
check "rustfs data survives a restart" test "$(get r_prod "$(var r_prod S3_BUCKET)" obj.txt)" = rustfs-prod

step "migration (garage -> rustfs -> garage) with checksums"
head -c 3145728 /dev/urandom >"$WORK/big.bin"
SRC_SUM="$(shasum -a 256 <"$WORK/big.bin" | cut -d' ' -f1)"
s3 g_prod s3 cp - "s3://$(var g_prod S3_BUCKET)/dir/big.bin" <"$WORK/big.bin" >/dev/null 2>&1
put g_prod "$(var g_prod S3_BUCKET)" "" dir/empty || true
REPORT="$(api POST "/external-services/$RUSTFS/migrate-s3" "{\"source_service_id\":$GARAGE}")"
check "garage -> rustfs reports both backends" \
  test "$(echo "$REPORT" | json "d['source_backend']+'>'+d['target_backend']")" = 'garage>rustfs'
check "garage -> rustfs preserves the checksum" \
  test "$(admin_s3 "$RUSTFS" s3 cp "s3://$(var g_prod S3_BUCKET)/dir/big.bin" - 2>/dev/null | shasum -a 256 | cut -d' ' -f1)" = "$SRC_SUM"
check "garage -> rustfs preserves the zero-byte object" \
  admin_s3 "$RUSTFS" s3 ls "s3://$(var g_prod S3_BUCKET)/dir/empty"
GARAGE2="$(create_service "e2e-garage2-$RUN" '{"backend":"garage"}')"
REPORT="$(api POST "/external-services/$GARAGE2/migrate-s3" "{\"source_service_id\":$RUSTFS}")"
check "rustfs -> garage reports both backends" \
  test "$(echo "$REPORT" | json "d['source_backend']+'>'+d['target_backend']")" = 'rustfs>garage'
check "rustfs -> garage preserves the checksum" \
  test "$(admin_s3 "$GARAGE2" s3 cp "s3://$(var g_prod S3_BUCKET)/dir/big.bin" - 2>/dev/null | shasum -a 256 | cut -d' ' -f1)" = "$SRC_SUM"
check "rustfs -> garage preserves the zero-byte object" \
  admin_s3 "$GARAGE2" s3 ls "s3://$(var g_prod S3_BUCKET)/dir/empty"

step "backup and restore (garage -> new garage service)"
STORE="$(create_service "e2e-store-$RUN" '{}')"
STORE_BUCKET="e2e-backups-$RUN"
admin_s3 "$STORE" s3 mb "s3://$STORE_BUCKET" >/dev/null 2>&1
STORE_NAME="e2e-store-$RUN"
STORE_PORT="$(docker port "rustfs-$STORE_NAME" 9000 | head -1 | sed 's/.*://')"
SOURCE_ID="$(api POST /backups/s3-sources "$(python3 - <<EOF
import json,subprocess
def param(name):
    return subprocess.run(["curl","-sS","-H","Authorization: Bearer $KEY","$API/external-services/$STORE/parameters/"+name],
                          capture_output=True,text=True).stdout
print(json.dumps({"name":"e2e-source-$RUN","bucket_name":"$STORE_BUCKET","bucket_path":"/e2e",
  "access_key_id":json.loads(param("access_key"))["value"],"secret_key":json.loads(param("secret_key"))["value"],
  "region":"us-east-1","endpoint":"http://127.0.0.1:$STORE_PORT","force_path_style":True,"backing_service_id":$STORE}))
EOF
)" | json "d['id']")"
echo "$SOURCE_ID" >>"$SOURCES"
api POST "/backups/external-services/$GARAGE/run" "{\"s3_source_id\":$SOURCE_ID}" >/dev/null
BACKUP_STATE=running
for _ in $(seq 1 40); do
  BACKUP_STATE="$(api GET "/backups/external-services/$GARAGE/backups" | json "d['backups'][0]['state']")"
  [ "$BACKUP_STATE" != running ] && [ "$BACKUP_STATE" != pending ] && break
  sleep 3
done
check "garage backup completes" test "$BACKUP_STATE" = completed
BACKUP_ID="$(api GET "/backups/external-services/$GARAGE/backups" | json "d['backups'][0]['external_service_backup_id']")"
api POST "/external-services/$GARAGE/restore" \
  "{\"mode\":\"new_service\",\"name\":\"e2e-restored-$RUN\",\"backup_id\":$BACKUP_ID}" >/dev/null
RESTORE_STATE=running
for _ in $(seq 1 60); do
  RESTORE_STATE="$(api GET "/external-services/$GARAGE/restore-runs" | json "d[0]['status']")"
  [ "$RESTORE_STATE" != running ] && [ "$RESTORE_STATE" != pending ] && break
  sleep 3
done
check "restore into a new service completes" test "$RESTORE_STATE" = completed
RESTORED="$(api GET /external-services | json "[s['id'] for s in d if s['name']=='e2e-restored-$RUN'][0]")"
echo "$RESTORED" >>"$SERVICES"
check "the restored service is a garage service" \
  test "$(api GET "/external-services/$RESTORED" | json "d['current_parameters']['backend']")" = garage
check "the restored data keeps its checksum" \
  test "$(admin_s3 "$RESTORED" s3 cp "s3://$(var g_prod S3_BUCKET)/dir/big.bin" - 2>/dev/null | shasum -a 256 | cut -d' ' -f1)" = "$SRC_SUM"

step "unlink revokes the environment's access and keeps the data"
api DELETE "/external-services/$GARAGE/projects/$GP" >/dev/null
check "unlinked production key is denied" denied get g_prod "$(var g_prod S3_BUCKET)" obj.txt
check "unlinked staging key is denied" denied get g_stag "$(var g_stag S3_BUCKET)" obj.txt
api POST "/external-services/$GARAGE/projects" "{\"project_id\":$GP}" >/dev/null
credentials "$GARAGE" "$GP" "$GPROD" g_prod3
check "relinking issues the same key" test "$(var g_prod3 AWS_ACCESS_KEY_ID)" = "$(var g_prod AWS_ACCESS_KEY_ID)"
check "relinking restores access to the kept data" test "$(get g_prod3 "$(var g_prod3 S3_BUCKET)" obj.txt)" = garage-prod
api DELETE "/external-services/$RUSTFS/projects/$RP" >/dev/null
check "unlinked rustfs key is denied" denied get r_prod "$(var r_prod S3_BUCKET)" obj.txt

step "result"
if [ "$FAILURES" -eq 0 ]; then
  echo "all checks passed"
else
  echo "$FAILURES check(s) failed"
  exit 1
fi
