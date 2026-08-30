#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -euo pipefail
trap 'echo "test-compose-security.sh failed at line ${LINENO}: ${BASH_COMMAND}" >&2' ERR

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
export TEMPS_NETWORK_NAME="${TEMPS_NETWORK_NAME:-temps-docker-workloads}"
export CLICKHOUSE_PASSWORD="$safe_clickhouse"
export TEMPS_ADMIN_EMAIL="Admin@Example.TEST"
admin_secret_dir="$(mktemp -d)"
chmod 700 "$admin_secret_dir"
admin_password_file="$admin_secret_dir/admin_password"
admin_ingress_password_file="$admin_secret_dir/admin_ingress_password"
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
  docker rm --force "$workload_probe_name" >/dev/null 2>&1 || true
  POSTGRES_PASSWORD="$safe_postgres" \
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -f "$admin_password_file" "$admin_ingress_password_file"
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
   .services["temps-ingress"].ports[]]
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
  and .services.clickhouse.networks["temps-network"].ipv4_address == "198.18.255.12"
  and .services["temps-ingress"].networks["temps-ingress-network"].ipv4_address == "198.19.255.10"
  and (.services["temps-ingress"].networks | has("temps-app-network") | not)
  and (.services["temps-ingress"].cap_drop | index("ALL") != null)
  and .services["temps-ingress"].read_only == true
  and (.services["temps-ingress"].security_opt | index("no-new-privileges:true") != null)
  and (.services["temps-ingress"].secrets | map(.source) | index("temps_admin_ingress_password") != null)
  and (.services["temps-ingress"].environment | has("TEMPS_ADMIN_INGRESS_PASSWORD") | not)
  and .networks["temps-ingress-network"].driver_opts["com.docker.network.bridge.enable_icc"] == "false"
  and .networks["temps-ingress-network"].driver_opts["com.docker.network.bridge.trusted_host_interfaces"] == "lo"
  and .networks["temps-network"].internal == true
  and .networks["temps-network"].ipam.config[0].subnet == "198.18.255.0/24"
  and .networks["temps-app-network"].name == $workload_network
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
  "${compose[@]}" up --detach temps-ingress >/dev/null
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

# NOTE: temps-app-network and temps-ingress-network are unconnected bridge
# networks, so a workload container cannot reach temps-ingress's
# 198.19.255.10 address at all on real Docker Engine (only Docker Desktop's
# more permissive cross-bridge routing made that look reachable here before).
# Verifying "the workload can reach the public ingress surface" from this
# vantage point is deferred until that connectivity is deliberately wired up.
# The admin/auth-hiding properties below are still verified from the host via
# the published loopback ports, which is a real, currently-reachable surface.

# The private admin listener is published only through a second authentication
# barrier. Temps authentication remains active behind this proxy.
admin_binding="$(docker port temps-ingress 9001/tcp)"
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
tls_admin_status="$(curl --insecure --silent --output /dev/null --write-out '%{http_code}' \
  --connect-timeout 5 "https://${tls_binding}/api/auth/login" || true)"
if [[ "$tls_admin_status" != "404" && "$tls_admin_status" != "000" ]]; then
  echo "public TLS port 3443 returned $tls_admin_status for an admin route; expected 404 or a fail-closed TLS handshake" >&2
  exit 1
fi

# Exercise more slow public connections than the per-source ceiling. Nginx must
# reject the excess without spawning one ingress process per connection, then
# recover after the configured idle timeout.
docker exec --detach "$workload_probe_name" sh -ec '
  index=0
  while [ "$index" -lt 128 ]; do
    (sleep 30) | nc -w 15 198.19.255.10 9000 >/dev/null 2>&1 &
    index=$((index + 1))
  done
  wait
'
connection_limit_enforced=false
for _ in {1..10}; do
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
if [[ "$(docker inspect --format '{{.HostConfig.PidsLimit}}' temps-ingress)" != "32" ]]; then
  echo "admin ingress PID limit is not 32" >&2
  exit 1
fi
if [[ "$(docker inspect --format '{{.HostConfig.Memory}}' temps-ingress)" != "134217728" ]]; then
  echo "admin ingress memory limit is not 128 MiB" >&2
  exit 1
fi
sleep 10
curl --fail --silent --show-error "http://${console_binding}/readyz" | grep -qx ready

docker exec temps-app awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
docker exec temps-ingress awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
if docker exec temps-ingress touch /etc/compose-security-write-test >/dev/null 2>&1; then
  echo "admin ingress root filesystem is unexpectedly writable" >&2
  exit 1
fi
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
if docker inspect --format '{{json .Config.Env}}' temps-ingress \
  | grep -Fq "$admin_ingress_password"; then
  echo "admin ingress password leaked into the proxy environment" >&2
  exit 1
fi
if docker logs temps-ingress 2>&1 | grep -Fq "$admin_ingress_password"; then
  echo "admin ingress password leaked into proxy logs" >&2
  exit 1
fi

echo "Compose security checks passed"
