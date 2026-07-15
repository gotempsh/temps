#!/usr/bin/env bash
set -euo pipefail

project="temps-compose-security-${GITHUB_RUN_ID:-local}-$$"
config_compose=(docker compose --project-name "$project" --file docker-compose.yml)
compose=("${config_compose[@]}")
if [[ -n "${COMPOSE_SECURITY_OVERRIDE:-}" ]]; then
  compose+=(--file "$COMPOSE_SECURITY_OVERRIDE")
fi
safe_postgres="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
safe_redis="abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
export TEMPS_ADMIN_EMAIL="Admin@Example.TEST"
admin_secret_dir="$(mktemp -d)"
chmod 700 "$admin_secret_dir"
admin_password_file="$admin_secret_dir/admin_password"
printf 'tT3!0123456789abcdef0123456789abcdef\n' >"$admin_password_file"
chmod 444 "$admin_password_file"
export TEMPS_ADMIN_PASSWORD_FILE="$admin_password_file"
if [[ -z "${DOCKER_GID:-}" ]]; then
  # Inspect from a container because Docker Desktop can present a different
  # socket owner than the host-side symlink reports.
  DOCKER_GID="$(docker run --rm \
    --volume /var/run/docker.sock:/var/run/docker.sock \
    alpine:3.22 stat -c '%g' /var/run/docker.sock)"
fi
export DOCKER_GID

cleanup() {
  POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -f "$admin_password_file"
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

if env -u POSTGRES_PASSWORD -u REDIS_PASSWORD "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted missing credentials" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" env -u REDIS_PASSWORD \
  "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing Redis credential" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  env -u TEMPS_ADMIN_EMAIL "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing initial admin email" >&2
  exit 1
fi
if POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  env -u TEMPS_ADMIN_PASSWORD_FILE "${config_compose[@]}" config --quiet 2>/dev/null; then
  echo "compose config unexpectedly accepted a missing initial admin password file" >&2
  exit 1
fi

config="$({ POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${config_compose[@]}" config --format json; })"
jq -e '
  [.services.postgres.ports[], .services.redis.ports[],
   (.services.temps.ports[] | select(.target == 9000))]
  | all(.host_ip == "127.0.0.1")
' <<<"$config" >/dev/null
jq -e '.services.redis.user != null and .services.redis.user != "0" and .services.redis.user != "root"' \
  <<<"$config" >/dev/null
jq -e '
  [.services.temps.ports[] | select(.target == 3000 or .target == 3443)]
  | all(has("host_ip") | not)
' <<<"$config" >/dev/null

POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" run --rm --no-TTY credential-check >/dev/null
if POSTGRES_PASSWORD='unsafe@password' REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" run --rm --no-TTY credential-check >/dev/null 2>&1; then
  echo "credential validator unexpectedly accepted URL delimiters" >&2
  exit 1
fi

if grep -En 'temps_password_change_me|redis-server .*--requirepass|redis-cli -a' \
  docker-compose.yml .env.example; then
  echo "compose files contain a known or argv-exposed credential" >&2
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
POSTGRES_PASSWORD="$old_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" up --detach postgres-credential-sync >/dev/null
POSTGRES_PASSWORD="$old_postgres" REDIS_PASSWORD="$safe_redis" \
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

POSTGRES_PASSWORD="$old_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" stop postgres >/dev/null
POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" up --detach postgres >/dev/null
POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
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

POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" up --detach redis >/dev/null
for _ in {1..30}; do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-redis)" == "healthy" ]]; then
    break
  fi
  sleep 1
done
if [[ "$(docker inspect --format '{{.State.Health.Status}}' temps-redis)" != "healthy" ]]; then
  echo "Redis did not become healthy with authentication enabled" >&2
  exit 1
fi
docker exec temps-redis redis-cli ping | grep -q 'NOAUTH'
docker exec --env REDISCLI_AUTH="$safe_redis" temps-redis redis-cli ping | grep -qx PONG
docker exec temps-redis awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
if docker inspect --format '{{json .Config.Cmd}}' temps-redis | grep -Fq "$safe_redis"; then
  echo "Redis password leaked into the long-lived process argv" >&2
  exit 1
fi

# Build the production image and seed its persistent data volume before the
# first application start. This models an upgrade where an existing volume
# masks /app/data and verifies immutable runtime assets remain available.
POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" run --rm --no-deps --build --entrypoint /bin/sh temps \
  -ec 'touch /app/data/.preexisting-volume' >/dev/null
POSTGRES_PASSWORD="$safe_postgres" REDIS_PASSWORD="$safe_redis" \
  "${compose[@]}" up --detach temps >/dev/null
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
  "${compose[@]}" logs temps >&2 || true
  echo "Temps did not become ready on the console /readyz endpoint" >&2
  exit 1
fi
docker exec temps-app awk '/^Uid:/ { exit !($2 != 0) }' /proc/1/status
docker exec temps-app sh -ec 'touch /app/data/.compose-security-write-test; rm /app/data/.compose-security-write-test'
docker exec temps-app sh -ec 'test -r /var/run/docker.sock && test -w /var/run/docker.sock'
docker exec temps-app sh -ec \
  'test -r /run/secrets/temps_admin_password && test ! -w /run/secrets/temps_admin_password'
docker exec temps-app sh -ec \
  'test -r /usr/share/temps/GeoLite2-City.mmdb && test ! -w /usr/share/temps/GeoLite2-City.mmdb'
docker exec temps-app test -f /app/data/.preexisting-volume
docker exec temps-app wget --quiet --output-document=- http://127.0.0.1:9000/readyz | grep -qx ready
docker exec --env PGPASSWORD="$safe_postgres" temps-postgres \
  psql -h 127.0.0.1 -U temps -d temps -tAc \
  "SELECT count(*) FROM users u JOIN user_roles ur ON ur.user_id = u.id JOIN roles r ON r.id = ur.role_id WHERE u.email = 'admin@example.test' AND u.deleted_at IS NULL AND r.name = 'admin'" \
  | grep -qx 1
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

echo "Compose security checks passed"
