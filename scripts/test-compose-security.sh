#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail
trap 'echo "test-compose-security.sh failed at line ${LINENO}: ${BASH_COMMAND}" >&2' ERR

if ! command -v docker >/dev/null 2>&1; then
  echo "SKIP: Docker is not installed; Compose security tests require Docker"
  exit 0
fi
if ! docker info >/dev/null 2>&1; then
  echo "SKIP: Docker daemon is unavailable; Compose security tests require Docker"
  exit 0
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "SKIP: Docker Compose is unavailable; Compose security tests require Docker Compose"
  exit 0
fi

project="temps-compose-security-${GITHUB_RUN_ID:-local}-$$"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
config_compose=(docker compose --project-name "$project" --file docker-compose.yml
  --file "$script_dir/compose-security.harness.yml")
compose=("${config_compose[@]}")
if [[ -n "${COMPOSE_SECURITY_OVERRIDE:-}" ]]; then
  compose+=(--file "$COMPOSE_SECURITY_OVERRIDE")
fi
# CI pre-builds the temps image with layer caching and points the compose
# `temps` service at it via COMPOSE_SECURITY_OVERRIDE. In that case skip the
# uncached in-compose rebuild; locally we still build from source.
build_flag=(--build)
if [[ -n "${COMPOSE_SECURITY_PREBUILT:-}" ]]; then
  build_flag=()
fi
safe_postgres="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
safe_clickhouse="23456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef01"
old_clickhouse="3456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012"
workload_probe_name="${project}-workload-probe"
saturation_probe_name="${project}-saturation-probe"
admin_saturation_probe_name="${project}-admin-saturation-probe"
export TEMPS_NETWORK_NAME="${TEMPS_NETWORK_NAME:-temps-docker-workloads}"
export CLICKHOUSE_PASSWORD="$safe_clickhouse"
export TEMPS_ADMIN_EMAIL="Admin@Example.TEST"
admin_secret_dir="$(mktemp -d)"
chmod 700 "$admin_secret_dir"
admin_password_file="$admin_secret_dir/admin_password"
admin_ingress_password_file="$admin_secret_dir/admin_ingress_password"
sse_response_file="$admin_secret_dir/sse_response"
sse_curl_pid=""
admin_ingress_password='iI4!0123456789abcdef0123456789abcdef'
printf 'tT3!0123456789abcdef0123456789abcdef\n' >"$admin_password_file"
printf '%s\n' "$admin_ingress_password" >"$admin_ingress_password_file"
chmod 444 "$admin_password_file" "$admin_ingress_password_file"
export TEMPS_ADMIN_PASSWORD_FILE="$admin_password_file"
export TEMPS_ADMIN_INGRESS_PASSWORD_FILE="$admin_ingress_password_file"
if [[ -z "${DOCKER_GID:-}" ]]; then
  # Inspect from a container because Docker Desktop can present a different
  # socket owner than the host-side symlink reports.
  DOCKER_GID="$(docker run --rm \
    --volume /var/run/docker.sock:/var/run/docker.sock \
    alpine:3.22 stat -c '%g' /var/run/docker.sock)"
fi
export DOCKER_GID

cleanup() {
  if [[ -n "$sse_curl_pid" ]]; then
    kill "$sse_curl_pid" >/dev/null 2>&1 || true
    wait "$sse_curl_pid" >/dev/null 2>&1 || true
  fi
  docker rm --force "$admin_saturation_probe_name" >/dev/null 2>&1 || true
  docker rm --force "$saturation_probe_name" >/dev/null 2>&1 || true
  docker rm --force "$workload_probe_name" >/dev/null 2>&1 || true
  POSTGRES_PASSWORD="$safe_postgres" \
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -f "$admin_password_file" "$admin_ingress_password_file" "$sse_response_file"
  rmdir "$admin_secret_dir" 2>/dev/null || true
}
trap cleanup EXIT

wait_for_completed_service() {
  local service="$1"
  local description="$2"
  local container_id state exit_code

  container_id="$("${compose[@]}" ps --all --quiet "$service")"
  if [[ -z "$container_id" ]]; then
    echo "$description container was not created" >&2
    return 1
  fi
  for _ in {1..150}; do
    state="$(docker inspect --format '{{.State.Status}}' "$container_id")"
    if [[ "$state" == "exited" ]]; then
      exit_code="$(docker inspect --format '{{.State.ExitCode}}' "$container_id")"
      if [[ "$exit_code" == "0" ]]; then
        return 0
      fi
      break
    fi
    sleep 1
  done

  "${compose[@]}" logs postgres "$service" >&2 || true
  echo "$description failed or timed out" >&2
  return 1
}

assert_runtime_loopback_binding() {
  local container="$1"
  local container_port="$2"
  local -a bindings=()

  # `docker port` asks the daemon for the realized mapping. Some Docker Engine
  # versions leave NetworkSettings.Ports empty even when HostConfig mappings
  # are active, so inspecting that field is not portable runtime evidence.
  mapfile -t bindings < <(docker port "$container" "${container_port}/tcp" 2>/dev/null || true)
  if [[ "${#bindings[@]}" -ne 1 || \
    ! "${bindings[0]:-}" =~ ^127\.0\.0\.1:[0-9]+$ ]]; then
    echo "$container port $container_port is not published exclusively on 127.0.0.1" >&2
    docker port "$container" >&2 || true
    return 1
  fi
}

assert_admin_ingress_secret_rejected() {
  local description="$1"
  local secret_value="$2"
  local probe_id=""
  local probe_state=""
  local probe_exit_code=""
  local probe_logs=""

  chmod 600 "$admin_ingress_password_file"
  printf '%s' "$secret_value" >"$admin_ingress_password_file"
  chmod 444 "$admin_ingress_password_file"

  probe_id="$(POSTGRES_PASSWORD="$safe_postgres" \
    "${compose[@]}" run --detach --no-deps temps-admin-ingress)"
  for _ in {1..20}; do
    probe_state="$(docker inspect --format '{{.State.Status}}' "$probe_id")"
    if [[ "$probe_state" == "exited" ]]; then
      break
    fi
    sleep 0.25
  done
  probe_logs="$(docker logs "$probe_id" 2>&1 || true)"
  if [[ "$probe_state" == "exited" ]]; then
    probe_exit_code="$(docker inspect --format '{{.State.ExitCode}}' "$probe_id")"
  fi
  docker rm --force "$probe_id" >/dev/null 2>&1 || true

  chmod 600 "$admin_ingress_password_file"
  printf '%s\n' "$admin_ingress_password" >"$admin_ingress_password_file"
  chmod 444 "$admin_ingress_password_file"

  if [[ "$probe_state" != "exited" || "$probe_exit_code" == "0" ]]; then
    printf '%s\n' "$probe_logs" >&2
    echo "admin ingress unexpectedly accepted a $description secret" >&2
    return 1
  fi
  if ! grep -Eq 'must be a single line|must contain only printable ASCII characters|must contain between 16 and 72 bytes|must not be blank or whitespace-only' \
    <<<"$probe_logs"; then
    printf '%s\n' "$probe_logs" >&2
    echo "admin ingress rejected a $description secret without a contextual validation error" >&2
    return 1
  fi
}

if env -u POSTGRES_PASSWORD "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing PostgreSQL credential" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" \
  env -u CLICKHOUSE_PASSWORD "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing ClickHouse credential" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" \
  env -u TEMPS_ADMIN_EMAIL "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing initial admin email" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" \
  env -u TEMPS_ADMIN_PASSWORD_FILE "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing initial admin password file" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" \
  env -u TEMPS_ADMIN_INGRESS_PASSWORD_FILE "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing admin ingress password file" >&2
  exit 1
fi

config="$({ POSTGRES_PASSWORD="$safe_postgres" \
  "${config_compose[@]}" config --format json; })"
jq -e '
  [.services.postgres.ports[], .services.clickhouse.ports[],
   .services["temps-ingress"].ports[],
   .services["temps-admin-ingress"].ports[]]
  | all(.host_ip == "127.0.0.1")
' <<<"$config" >/dev/null
jq -e --arg workload_network "$TEMPS_NETWORK_NAME" '
  .services.temps.environment.TEMPS_CLICKHOUSE_URL == "http://198.18.255.12:8123"
  and .services.temps.environment.TEMPS_CLICKHOUSE_DATABASE == "temps"
  and .services.temps.environment.TEMPS_CLICKHOUSE_USER == "temps"
  and .services.temps.environment.TEMPS_EXECUTION_ENV == "docker"
  and .services.temps.environment.TEMPS_NETWORK_NAME == $workload_network
  and .services.temps.environment.TEMPS_ADDRESS == "198.18.255.10:3000"
  and .services.temps.environment.TEMPS_TLS_ADDRESS == "198.18.255.10:3443"
  and .services.temps.environment.TEMPS_CONSOLE_ADDRESS == "198.18.255.10:9000"
  and .services.temps.environment.TEMPS_CONSOLE_ADMIN_ADDRESS == "198.18.255.10:9001"
  and .services.temps.environment.TEMPS_DATABASE_URL
    == "postgresql://temps:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef@198.18.255.11:5432/temps"
  and (.services.temps.environment | has("REDIS_URL") | not)
  and (.services | has("redis") | not)
' <<<"$config" >/dev/null
jq -e --arg workload_network "$TEMPS_NETWORK_NAME" '
  (.services.temps.networks | has("temps-network"))
  and (.services.temps.networks | has("temps-app-network"))
  and .services.temps.networks["temps-network"].ipv4_address == "198.18.255.10"
  and .services.postgres.networks["temps-network"].ipv4_address == "198.18.255.11"
  and (.services.postgres.networks | has("temps-host-network"))
  and .services.clickhouse.networks["temps-network"].ipv4_address == "198.18.255.12"
  and (.services.clickhouse.networks | has("temps-host-network"))
  and .services["temps-ingress"].networks["temps-app-network"].ipv4_address == "198.20.255.10"
  and (.services["temps-ingress"].networks | has("temps-ingress-network") | not)
  and (.services["temps-ingress"].cap_drop | index("ALL") != null)
  and .services["temps-ingress"].read_only == true
  and (.services["temps-ingress"].security_opt | index("no-new-privileges:true") != null)
  and (.services["temps-ingress"].environment | has("TEMPS_ADMIN_INGRESS_PASSWORD") | not)
  and .services["temps-admin-ingress"].networks["temps-ingress-network"].ipv4_address == "198.19.255.10"
  and (.services["temps-admin-ingress"].networks | has("temps-app-network") | not)
  and (.services["temps-admin-ingress"].cap_drop | index("ALL") != null)
  and .services["temps-admin-ingress"].read_only == true
  and (.services["temps-admin-ingress"].security_opt | index("no-new-privileges:true") != null)
  and (.services["temps-admin-ingress"].secrets | map(.source) | index("temps_admin_ingress_password") != null)
  and (.services["temps-admin-ingress"].environment | has("TEMPS_ADMIN_INGRESS_PASSWORD") | not)
  and .networks["temps-ingress-network"].driver_opts["com.docker.network.bridge.enable_icc"] == "false"
  and .networks["temps-ingress-network"].driver_opts["com.docker.network.bridge.trusted_host_interfaces"] == "lo"
  and .networks["temps-host-network"].driver_opts["com.docker.network.bridge.enable_icc"] == "false"
  and .networks["temps-host-network"].driver_opts["com.docker.network.bridge.enable_ip_masquerade"] == "false"
  and .networks["temps-host-network"].driver_opts["com.docker.network.bridge.trusted_host_interfaces"] == "lo"
  and (.networks["temps-host-network"].internal != true)
  and .networks["temps-network"].internal == true
  and .networks["temps-network"].ipam.config[0].subnet == "198.18.255.0/24"
  and .networks["temps-app-network"].name == $workload_network
  and .networks["temps-app-network"].ipam.config[0].subnet == "198.20.255.0/24"
' <<<"$config" >/dev/null

POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" run --rm --no-TTY credential-check >/dev/null
if POSTGRES_PASSWORD='unsafe@password' \
  "${compose[@]}" run --rm --no-TTY credential-check >/dev/null 2>&1; then
  echo "credential validator unexpectedly accepted URL delimiters" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" \
  CLICKHOUSE_PASSWORD='unsafe@password' \
  "${compose[@]}" run --rm --no-TTY credential-check >/dev/null 2>&1; then
  echo "credential validator unexpectedly accepted ClickHouse URL delimiters" >&2
  exit 1
fi

if grep -En 'temps_password_change_me' \
  docker-compose.yml .env.example; then
  echo "compose files contain a known or argv-exposed credential" >&2
  exit 1
fi

if grep -Fxq 'scripts/' .dockerignore || \
  ! grep -Fxq '!scripts/source_attribution.py' .dockerignore || \
  ! grep -Fxq '!scripts/docker-ingress-entrypoint.sh' .dockerignore; then
  echo "Docker build context excludes a required build or ingress script" >&2
  exit 1
fi

for credential_doc in .env.example docs/installation/page.mdx docs/upgrade/page.mdx; do
  if ! grep -Fq 'install -m 600 .env.example .env' "$credential_doc"; then
    echo "$credential_doc does not require creating .env with mode 0600" >&2
    exit 1
  fi
done

if ! git check-ignore --quiet --no-index secrets/admin_password; then
  echo "repo-local admin secrets are not excluded by Git" >&2
  exit 1
fi
for ignore_file in .gitignore .dockerignore; do
  if ! grep -Fxq '/secrets/' "$ignore_file"; then
    echo "$ignore_file does not exclude repo-local admin secrets" >&2
    exit 1
  fi
done

# The ingress must fail closed before Nginx starts when its independent Basic
# credential is empty or contains only whitespace.
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" build temps-admin-ingress >/dev/null
assert_admin_ingress_secret_rejected "empty" ""
assert_admin_ingress_secret_rejected "whitespace-only" "                    "
assert_admin_ingress_secret_rejected "short" "too-short"
assert_admin_ingress_secret_rejected "overlong" \
  "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef012345678"
assert_admin_ingress_secret_rejected "embedded-newline" \
  $'0123456789abcdef\n0123456789abcdef'
assert_admin_ingress_secret_rejected "multiple-trailing-newlines" \
  $'0123456789abcdef0123456789abcdef\n\n'

old_postgres="temps_password_change_me"
POSTGRES_PASSWORD="$old_postgres" \
  "${compose[@]}" up --detach postgres-credential-sync >/dev/null
POSTGRES_PASSWORD="$old_postgres" \
  wait_for_completed_service postgres-credential-sync \
  "fresh-volume credential synchronization"

for _ in {1..90}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-postgres)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-postgres)" != "healthy" ]]; then
  echo "PostgreSQL did not become healthy with the legacy-volume credential" >&2
  exit 1
fi

POSTGRES_PASSWORD="$old_postgres" \
  "${compose[@]}" stop postgres >/dev/null
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" up --detach postgres >/dev/null
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" run --rm --no-deps --no-TTY postgres-credential-sync >/dev/null

for _ in {1..30}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-postgres)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-postgres)" != "healthy" ]]; then
  echo "PostgreSQL did not become healthy after credential rotation" >&2
  exit 1
fi
docker exec --env PGPASSWORD="$safe_postgres" temps-postgres \
  psql -h 127.0.0.1 -U temps -d temps -tAc \
  "SELECT rolpassword LIKE 'SCRAM-SHA-256$%' FROM pg_authid WHERE rolname = 'temps'" \
  | grep -qx t
if docker exec --env PGPASSWORD="$old_postgres" temps-postgres \
  psql -h 127.0.0.1 -U temps -d temps -tAc 'SELECT 1' >/dev/null 2>&1; then
  echo "legacy PostgreSQL password still authenticates after rotation" >&2
  exit 1
fi

POSTGRES_PASSWORD="$safe_postgres" \
  CLICKHOUSE_PASSWORD="$old_clickhouse" \
  "${compose[@]}" up --detach clickhouse >/dev/null
for _ in {1..60}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-clickhouse)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-clickhouse)" != "healthy" ]]; then
  "${compose[@]}" logs clickhouse >&2 || true
  echo "ClickHouse did not become healthy with authentication enabled" >&2
  exit 1
fi
docker exec temps-clickhouse awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
if docker exec --env CLICKHOUSE_PASSWORD= temps-clickhouse \
  clickhouse-client --user temps --query 'SELECT 1' \
  >/dev/null 2>&1; then
  echo "ClickHouse unexpectedly accepted an unauthenticated query" >&2
  exit 1
fi
docker exec --env CLICKHOUSE_PASSWORD="$old_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps --query 'SELECT 1' | grep -qx 1
if docker exec --env CLICKHOUSE_PASSWORD="$old_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --query 'CREATE USER temps_compose_security_probe' \
  >/dev/null 2>&1; then
  echo "ClickHouse application user unexpectedly has access-management privileges" >&2
  exit 1
fi
docker exec --env CLICKHOUSE_PASSWORD="$old_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps \
  --query 'CREATE TABLE compose_security_persistence_probe (value UInt8) ENGINE = TinyLog'
docker exec --env CLICKHOUSE_PASSWORD="$old_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps \
  --query 'INSERT INTO compose_security_persistence_probe VALUES (1)'

POSTGRES_PASSWORD="$safe_postgres" \
  CLICKHOUSE_PASSWORD="$old_clickhouse" \
  "${compose[@]}" stop clickhouse >/dev/null
POSTGRES_PASSWORD="$safe_postgres" \
  CLICKHOUSE_PASSWORD="$safe_clickhouse" \
  "${compose[@]}" up --detach clickhouse >/dev/null
for _ in {1..60}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-clickhouse)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-clickhouse)" != "healthy" ]]; then
  "${compose[@]}" logs clickhouse >&2 || true
  echo "ClickHouse did not become healthy after credential rotation" >&2
  exit 1
fi
if docker exec --env CLICKHOUSE_PASSWORD="$old_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps --query 'SELECT 1' \
  >/dev/null 2>&1; then
  echo "Old ClickHouse password still authenticates after rotation" >&2
  exit 1
fi
docker exec --env CLICKHOUSE_PASSWORD="$safe_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps \
  --query 'SELECT count() FROM compose_security_persistence_probe' | grep -qx 1

# Build the production image and seed its persistent data volume before the
# first application start. This models an upgrade where an existing volume
# masks /app/data and verifies immutable runtime assets remain available.
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" run --rm --no-deps ${build_flag[@]+"${build_flag[@]}"} --entrypoint /bin/sh temps \
  -ec 'touch /app/data/.preexisting-volume' >/dev/null

# Start an adversarially named workload before Temps. Docker DNS exposes these
# aliases to the dual-network control plane, so private dependencies and bind
# addresses must remain pinned to non-confusable control-network IPs.
docker run --detach --rm \
  --name "$workload_probe_name" \
  --network "$TEMPS_NETWORK_NAME" \
  --network-alias temps-postgres \
  --network-alias temps-clickhouse \
  alpine:3.22 \
  /bin/sh -ec \
  'while true; do printf "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nready\n" | nc -l -p 8080; done' \
  >/dev/null

POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" up --detach temps-ingress temps-admin-ingress >/dev/null
for _ in {1..180}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-app)" == "healthy" ]]; then
    break
  fi
  if [[ "$(docker inspect --format '{{.State.Status}}' temps-app)" == "exited" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-app)" != "healthy" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" "${compose[@]}" logs temps >&2 || true
  echo "Temps did not become ready on the console /readyz endpoint" >&2
  exit 1
fi
for _ in {1..30}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-ingress)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-ingress)" != "healthy" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" "${compose[@]}" logs temps-ingress >&2 || true
  echo "Temps loopback ingress did not become healthy" >&2
  exit 1
fi
for _ in {1..30}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-admin-ingress)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-admin-ingress)" != "healthy" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" "${compose[@]}" logs temps-admin-ingress >&2 || true
  echo "Temps admin ingress did not become healthy" >&2
  exit 1
fi

# Rendered Compose configuration is necessary but not sufficient: verify the
# effective bindings installed in Docker after all overrides are applied.
assert_runtime_loopback_binding temps-postgres 5432
assert_runtime_loopback_binding temps-clickhouse 8123
assert_runtime_loopback_binding temps-ingress 3000
assert_runtime_loopback_binding temps-ingress 3443
assert_runtime_loopback_binding temps-ingress 9000
assert_runtime_loopback_binding temps-admin-ingress 9001

# Docker-mode routing must use the explicitly named workload network and
# internal ports even while malicious private-service aliases are present.
for _ in {1..30}; do
  if docker exec temps-app wget --quiet --output-document=- \
    "http://${workload_probe_name}:8080/ready" | grep -qx ready; then
    break
  fi
  sleep 1
done
docker exec temps-app wget --quiet --output-document=- \
  "http://${workload_probe_name}:8080/ready" | grep -qx ready
# Runners that inject a DNS search domain (e.g. GitHub Actions' Azure host
# search suffix) break plain `nslookup`: Alpine's musl-libc resolver queries
# the search-suffixed name first and, unlike glibc, does not fall back to the
# bare name on NXDOMAIN. `getent hosts` resolves the alias correctly in both
# environments, so use it instead of `nslookup` for this liveness check.
for control_plane_alias in temps-postgres temps-clickhouse; do
  alias_resolved=false
  for _ in {1..10}; do
    if docker exec "$workload_probe_name" getent hosts "$control_plane_alias" >/dev/null 2>&1; then
      alias_resolved=true
      break
    fi
    sleep 1
  done
  if [[ "$alias_resolved" != "true" ]]; then
    echo "workload network alias $control_plane_alias did not become resolvable" >&2
    exit 1
  fi
done
for control_plane_port in 3000 3443 9000; do
  if docker exec "$workload_probe_name" nc -z -w 1 temps-app "$control_plane_port" \
    >/dev/null 2>&1; then
    echo "workload network unexpectedly reaches Temps on port $control_plane_port" >&2
    exit 1
  fi
done
for private_endpoint in 198.18.255.10:9000 198.18.255.10:9001 198.18.255.11:5432 198.18.255.12:8123; do
  private_host="${private_endpoint%:*}"
  private_port="${private_endpoint##*:}"
  if docker exec "$workload_probe_name" nc -z -w 1 "$private_host" "$private_port" \
    >/dev/null 2>&1; then
    echo "workload network unexpectedly reaches private endpoint $private_endpoint" >&2
    exit 1
  fi
done

# Managed workloads must reach the public ingress over their shared network,
# while the physically separate admin proxy remains undiscoverable and
# unreachable from that network.
docker exec "$workload_probe_name" wget --quiet --output-document=- \
  "http://temps-ingress:9000/readyz" | grep -qx ready
for public_port in 3000 9000; do
  workload_admin_response="$(docker exec "$workload_probe_name" \
    wget --server-response --spider \
    "http://temps-ingress:${public_port}/api/auth/login" 2>&1 || true)"
  workload_admin_status="$(sed -n 's/^[[:space:]]*HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p' \
    <<<"$workload_admin_response" | tail -1)"
  if [[ "$workload_admin_status" != "404" ]]; then
    printf '%s\n' "$workload_admin_response" >&2
    echo "workload-facing public port $public_port returned $workload_admin_status for an admin route; expected 404" >&2
    exit 1
  fi
done
docker exec "$workload_probe_name" nc -z -w 5 temps-ingress 3443
if docker exec "$workload_probe_name" getent hosts temps-admin-ingress >/dev/null 2>&1; then
  echo "workload network unexpectedly resolves the private admin ingress" >&2
  exit 1
fi
# Native Linux bridge isolation rejects this route. Docker Desktop may route a
# numerically addressed packet across otherwise-unconnected bridges; if it
# does, the independent authentication boundary must still fail closed.
if docker exec "$workload_probe_name" nc -z -w 1 198.19.255.10 9001 >/dev/null 2>&1; then
  workload_admin_response="$(docker exec "$workload_probe_name" \
    wget --server-response --spider http://198.19.255.10:9001/ 2>&1 || true)"
  workload_admin_status="$(sed -n 's/^[[:space:]]*HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p' \
    <<<"$workload_admin_response" | tail -1)"
  if [[ "$workload_admin_status" != "401" ]]; then
    printf '%s\n' "$workload_admin_response" >&2
    echo "routed workload request bypassed the admin ingress authentication barrier" >&2
    exit 1
  fi
fi

# The private admin listener is published only through a second authentication
# barrier. Temps authentication remains active behind this proxy.
admin_binding="$(docker port temps-admin-ingress 9001/tcp)"
if [[ "$(curl --silent --output /dev/null --write-out '%{http_code}' "http://${admin_binding}/")" != "401" ]]; then
  echo "admin ingress unexpectedly accepted an unauthenticated request" >&2
  exit 1
fi
if [[ "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  --user 'temps:wrong-password' "http://${admin_binding}/")" != "401" ]]; then
  echo "admin ingress unexpectedly accepted an incorrect password" >&2
  exit 1
fi
admin_authenticated=false
for _ in {1..10}; do
  if curl --fail --silent --user "temps:${admin_ingress_password}" \
    "http://${admin_binding}/" >/dev/null 2>&1; then
    admin_authenticated=true
    break
  fi
  sleep 1
done
if [[ "$admin_authenticated" != "true" ]]; then
  echo "admin ingress did not accept the correct credentials" >&2
  exit 1
fi

# Slow/incomplete requests must be bounded before HTTP parsing and Basic auth.
# Saturate the listener and prove the ceiling is a temporary, self-healing
# throttle rather than a permanent lockout: once the attacking connections
# close (their incomplete headers hit client_header_timeout, or the flood's
# own hold expires), a legitimate administrator must be able to authenticate
# again. This test cannot assert that the administrator stays reachable
# *during* the flood: admin-ingress is loopback-only and temps-ingress-network
# has inter-container communication disabled by design (nothing but loopback
# traffic may ever reach it), so both the flood and any legitimate client
# necessarily share the same source address nginx sees, `127.0.0.1`, and
# limit_conn -- keyed on `$binary_remote_addr` -- correctly cannot tell them
# apart while both are open. That's expected, not a bug in the ceiling.
#
# The probe must reach admin-ingress the same way any real client does: through
# its loopback-published host port. temps-ingress-network disables inter-
# container communication on its bridge (com.docker.network.bridge.enable_icc:
# "false", by design -- see the network's compose comment), so a probe
# attached directly to that bridge network has its connections silently
# dropped before nginx ever sees them, which would make this assertion pass
# for the wrong reason (no requests arrived) rather than because nginx
# enforced the limit. --network host bypasses that bridge entirely and hits
# the same published address the admin_authenticated checks above already use.
docker run --detach --rm --name "$admin_saturation_probe_name" \
  --network host alpine:3.22 sleep 300 >/dev/null
admin_binding_host="${admin_binding%:*}"
admin_binding_port="${admin_binding##*:}"
docker exec --detach \
  --env ADMIN_HOST="$admin_binding_host" --env ADMIN_PORT="$admin_binding_port" \
  "$admin_saturation_probe_name" sh -ec '
  index=0
  while [ "$index" -lt 64 ]; do
    { printf "GET /slow HTTP/1.1\r\nHost: temps"; sleep 30; } \
      | nc -w 35 "$ADMIN_HOST" "$ADMIN_PORT" >/dev/null 2>&1 &
    index=$((index + 1))
  done
  wait
'
admin_connection_limit_enforced=false
for _ in {1..40}; do
  admin_ingress_logs="$(docker logs temps-admin-ingress 2>&1)"
  if grep -Fq 'limiting connections by zone "admin_clients"' <<<"$admin_ingress_logs"; then
    admin_connection_limit_enforced=true
    break
  fi
  sleep 1
done
if [[ "$admin_connection_limit_enforced" != "true" ]]; then
  echo "admin ingress did not enforce its pre-authentication connection ceiling" >&2
  echo "--- established connections on temps-admin-ingress ---" >&2
  docker exec temps-admin-ingress sh -ec 'netstat -ant 2>/dev/null || cat /proc/net/tcp' >&2 || true
  echo "--- full nginx error log ---" >&2
  printf '%s\n' "$admin_ingress_logs" >&2
  exit 1
fi
admin_recovered_after_saturation=false
for _ in {1..60}; do
  if curl --fail --silent --user "temps:${admin_ingress_password}" \
    "http://${admin_binding}/" >/dev/null 2>&1; then
    admin_recovered_after_saturation=true
    break
  fi
  sleep 1
done
if [[ "$admin_recovered_after_saturation" != "true" ]]; then
  echo "admin ingress did not recover access for a loopback administrator after the saturating connections closed" >&2
  exit 1
fi
docker rm --force "$admin_saturation_probe_name" >/dev/null

# Exercise an actual HTTP/1.1 upgrade through the same admin ingress image.
# The mock upstream records the forwarded request so the test also proves the
# independent Basic credential is removed before application traffic.
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" up --detach websocket-ingress-probe >/dev/null
websocket_binding="$(POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" port websocket-ingress-probe 9001)"
websocket_upgraded=false
for _ in {1..10}; do
  websocket_status="$(curl --http1.1 --silent --output /dev/null --write-out '%{http_code}' \
    --max-time 5 --user "temps:${admin_ingress_password}" \
    --header 'Connection: Upgrade' --header 'Upgrade: websocket' \
    "http://${websocket_binding}/socket" || true)"
  if [[ "$websocket_status" == "101" ]]; then
    websocket_upgraded=true
    break
  fi
  sleep 1
done
if [[ "$websocket_upgraded" != "true" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" "${compose[@]}" logs websocket-ingress-probe websocket-upstream-probe >&2 || true
  echo "admin ingress did not preserve a WebSocket upgrade" >&2
  exit 1
fi
websocket_request="$(POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" exec --no-TTY websocket-upstream-probe cat /tmp/websocket-request)"
websocket_request_clean="$(tr -d '\r' <<<"$websocket_request")"
if ! grep -Eiq '^Upgrade:[[:space:]]*websocket$' <<<"$websocket_request_clean" || \
  ! grep -Eiq '^Connection:[[:space:]]*upgrade$' <<<"$websocket_request_clean"; then
  printf '%s\n' "$websocket_request_clean" >&2
  echo "admin ingress did not forward the WebSocket upgrade headers" >&2
  exit 1
fi
if grep -Eiq '^Authorization:' <<<"$websocket_request_clean"; then
  echo "admin ingress forwarded its Basic Authorization credential upstream" >&2
  exit 1
fi

# Verify real response-side SSE semantics through the authenticated HTTP proxy:
# the first event must not be buffered, the stream must survive more than ten
# quiet seconds, and a later event must reach the client on the same response.
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" up --detach sse-ingress-probe >/dev/null
sse_container_id="$(POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" ps --quiet sse-ingress-probe)"
assert_runtime_loopback_binding "$sse_container_id" 9001
sse_binding="$(POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" port sse-ingress-probe 9001)"
sse_listener_ready=false
for _ in {1..20}; do
  if nc -z -w 1 "${sse_binding%:*}" "${sse_binding##*:}" >/dev/null 2>&1; then
    sse_listener_ready=true
    break
  fi
  sleep 0.5
done
if [[ "$sse_listener_ready" != "true" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" \
    "${compose[@]}" logs sse-ingress-probe sse-upstream-probe >&2 || true
  echo "SSE ingress regression fixture did not become ready" >&2
  exit 1
fi
: >"$sse_response_file"
curl --http1.1 --no-buffer --fail --silent --show-error --max-time 25 \
  --user "temps:${admin_ingress_password}" "http://${sse_binding}/events" \
  >"$sse_response_file" &
sse_curl_pid=$!
sse_first_event_received=false
for _ in {1..20}; do
  if grep -Fqx 'data: first' "$sse_response_file"; then
    sse_first_event_received=true
    break
  fi
  if ! kill -0 "$sse_curl_pid" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
if [[ "$sse_first_event_received" != "true" ]]; then
  echo "admin ingress buffered or dropped the initial SSE event" >&2
  exit 1
fi
if ! wait "$sse_curl_pid"; then
  sse_curl_pid=""
  printf '%s\n' "$(cat "$sse_response_file")" >&2
  echo "admin ingress dropped the SSE stream during its quiet interval" >&2
  exit 1
fi
sse_curl_pid=""
if ! grep -Fqx 'data: second' "$sse_response_file"; then
  printf '%s\n' "$(cat "$sse_response_file")" >&2
  echo "admin ingress did not deliver the SSE event after the quiet interval" >&2
  exit 1
fi

# Loopback publication must still reach the listener bound to the private
# control-network address.
console_binding="$(docker port temps-ingress 9000/tcp)"
loopback_ready=false
for _ in {1..10}; do
  if curl --fail --silent "http://${console_binding}/readyz" 2>/dev/null | grep -qx ready; then
    loopback_ready=true
    break
  fi
  sleep 1
done
if [[ "$loopback_ready" != "true" ]]; then
  echo "loopback-published console listener did not become ready" >&2
  exit 1
fi

# No public listener may expose the split admin/auth surface. Check the public
# HTTP proxy, HTTPS proxy, and console/ingest listener independently.
for public_port in 3000 9000; do
  public_binding="$(docker port temps-ingress "${public_port}/tcp")"
  public_status="$(curl --silent --output /dev/null --write-out '%{http_code}' \
    "http://${public_binding}/api/auth/login")"
  if [[ "$public_status" != "404" ]]; then
    echo "public port $public_port returned $public_status for an admin route; expected 404" >&2
    exit 1
  fi
done
tls_binding="$(docker port temps-ingress 3443/tcp)"
tls_host="${tls_binding%:*}"
tls_port="${tls_binding##*:}"
if ! nc -z -w 5 "$tls_host" "$tls_port"; then
  echo "public TLS port 3443 is not reachable through its loopback publication" >&2
  exit 1
fi
tls_admin_status="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
  --connect-timeout 5 "https://${tls_binding}/api/auth/login" || true)"
if [[ "$tls_admin_status" != "404" && "$tls_admin_status" != "000" ]]; then
  echo "public TLS port 3443 returned $tls_admin_status for an admin route; expected 404 or a fail-closed TLS handshake" >&2
  exit 1
fi

# Regression for the former global 10-second stream timeout: a quiet client
# must remain connected long enough to send its request after that boundary.
# A dedicated HTTP upstream isolates the ingress timeout from Temps' own HTTP
# connection timeout.
POSTGRES_PASSWORD="$safe_postgres" \
  "${compose[@]}" up --detach idle-stream-ingress-probe >/dev/null
idle_stream_ready=false
for _ in {1..10}; do
  if docker exec "$workload_probe_name" nc -z -w 1 idle-stream-ingress-probe 9000 \
    >/dev/null 2>&1; then
    idle_stream_ready=true
    break
  fi
  sleep 1
done
if [[ "$idle_stream_ready" != "true" ]]; then
  POSTGRES_PASSWORD="$safe_postgres" \
    "${compose[@]}" logs idle-stream-ingress-probe idle-stream-upstream-probe >&2 || true
  echo "idle-stream ingress regression fixture did not become ready" >&2
  exit 1
fi
idle_stream_response="$(docker run --rm --network "$TEMPS_NETWORK_NAME" \
  alpine:3.22 timeout 30 sh -ec \
  '{ sleep 12; printf "GET /readyz HTTP/1.1\r\nHost: temps\r\nConnection: close\r\n\r\n"; sleep 5; } | nc idle-stream-ingress-probe 9000')"
if ! grep -Fq ready <<<"$idle_stream_response"; then
  echo "public ingress dropped a valid stream after more than ten idle seconds" >&2
  exit 1
fi

# Exercise more slow public connections than the per-source ceiling. Nginx must
# reject the excess without spawning one ingress process per connection. Use a
# disposable source container so the test explicitly terminates every client.
docker run --detach --rm --name "$saturation_probe_name" \
  --network "$TEMPS_NETWORK_NAME" alpine:3.22 sleep 300 >/dev/null
docker exec --detach "$saturation_probe_name" sh -ec '
  index=0
  while [ "$index" -lt 128 ]; do
    (sleep 60) | nc -w 65 temps-ingress 9000 >/dev/null 2>&1 &
    index=$((index + 1))
  done
  wait
'
connection_limit_enforced=false
for _ in {1..20}; do
  ingress_process_count="$(docker exec temps-ingress sh -ec \
    'set -- /proc/[0-9]*; printf "%s" "$#"')"
  if ((ingress_process_count > 8)); then
    echo "public connection saturation spawned $ingress_process_count ingress processes" >&2
    exit 1
  fi
  ingress_logs="$(docker logs temps-ingress 2>&1)"
  if grep -Fq 'limiting connections by zone "public_clients"' <<<"$ingress_logs"; then
    connection_limit_enforced=true
    break
  fi
  sleep 1
done
if [[ "$connection_limit_enforced" != "true" ]]; then
  echo "public ingress did not enforce its concurrent connection ceiling" >&2
  exit 1
fi
docker rm --force "$saturation_probe_name" >/dev/null
for _ in {1..10}; do
  if curl --fail --silent "http://${console_binding}/readyz" 2>/dev/null | grep -qx ready; then
    break
  fi
  sleep 1
done
curl --fail --silent --show-error "http://${console_binding}/readyz" | grep -qx ready

for ingress_container in temps-ingress temps-admin-ingress; do
  if [[ "$(docker inspect --format '{{.HostConfig.PidsLimit}}' "$ingress_container")" != "32" ]]; then
    echo "$ingress_container PID limit is not 32" >&2
    exit 1
  fi
  if [[ "$(docker inspect --format '{{.HostConfig.Memory}}' "$ingress_container")" != "134217728" ]]; then
    echo "$ingress_container memory limit is not 128 MiB" >&2
    exit 1
  fi
done

docker exec temps-app awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
for ingress_container in temps-ingress temps-admin-ingress; do
  docker exec "$ingress_container" awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
  if docker exec "$ingress_container" touch /etc/compose-security-write-test >/dev/null 2>&1; then
    echo "$ingress_container root filesystem is unexpectedly writable" >&2
    exit 1
  fi
done
docker exec temps-app sh -ec 'touch /app/data/.compose-security-write-test; rm /app/data/.compose-security-write-test'
docker exec temps-app sh -ec 'test -r /var/run/docker.sock && test -w /var/run/docker.sock'
docker exec temps-app sh -ec \
  'test -r /run/secrets/temps_admin_password && test ! -w /run/secrets/temps_admin_password'
docker exec temps-app sh -ec \
  'test -r /usr/share/temps/GeoLite2-City.mmdb && test ! -w /usr/share/temps/GeoLite2-City.mmdb'
docker exec temps-app test -f /app/data/.preexisting-volume
docker exec --env PGPASSWORD="$safe_postgres" temps-postgres \
  psql -h 127.0.0.1 -U temps -d temps -tAc \
  "SELECT count(*) FROM users u JOIN user_roles ur ON ur.user_id = u.id JOIN roles r ON r.id = ur.role_id WHERE u.email = 'admin@example.test' AND u.deleted_at IS NULL AND r.name = 'admin'" \
  | grep -qx 1
for _ in {1..60}; do
  if docker exec --env CLICKHOUSE_PASSWORD="$safe_clickhouse" temps-clickhouse \
    clickhouse-client --user temps --database temps \
    --query "EXISTS TABLE _temps_ch_migrations" | grep -qx 1; then
    break
  fi
  sleep 1
done
docker exec --env CLICKHOUSE_PASSWORD="$safe_clickhouse" temps-clickhouse \
  clickhouse-client --user temps --database temps \
  --query "EXISTS TABLE _temps_ch_migrations" | grep -qx 1
# The harness must never report product telemetry (see
# compose-security.harness.yml). Assert on the container env, not on logs:
# the ENABLED/DISABLED notice is emitted during plugin registration, before
# the tracing subscriber is initialized, so it never reaches `docker logs`.
if ! docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' temps-app \
  | grep -qx 'TEMPS_TELEMETRY=0'; then
  echo "product telemetry is not disabled inside the compose security harness" >&2
  exit 1
fi
if [[ "$(docker logs temps-app 2>&1 | grep -c 'Initial admin created from TEMPS_ADMIN_EMAIL and password secret file')" != "1" ]]; then
  echo "expected exactly one unattended initial-admin creation notice" >&2
  exit 1
fi
if docker inspect --format '{{json .Config.Env}}' temps-app | grep -Fq 'tT3!0123456789abcdef'; then
  echo "initial admin password leaked into the application environment" >&2
  exit 1
fi
if docker logs temps-app 2>&1 | grep -Fq 'tT3!0123456789abcdef'; then
  echo "initial admin password leaked into application logs" >&2
  exit 1
fi
if docker inspect --format '{{json .Config.Env}}' temps-admin-ingress \
  | grep -Fq "$admin_ingress_password"; then
  echo "admin ingress password leaked into the proxy environment" >&2
  exit 1
fi
if docker logs temps-admin-ingress 2>&1 | grep -Fq "$admin_ingress_password"; then
  echo "admin ingress password leaked into proxy logs" >&2
  exit 1
fi

echo "Compose security checks passed"
