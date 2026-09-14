#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Runs only disposable, uniquely named Docker resources. No provider credentials.
set -euo pipefail
image=${1:?Provide the development image tag}
if ! docker info >/dev/null 2>&1; then
  echo 'SKIP: Docker is unavailable; no lifecycle checks ran'
  exit 0
fi
suffix="$(date +%s)-$$"
container="temps-runtime-smoke-$suffix"
volume="temps-runtime-smoke-$suffix"
created_container=0
created_volume=0
cleanup() {
  if [ "$created_container" = 1 ]; then docker rm -f "$container" >/dev/null; fi
  if [ "$created_volume" = 1 ]; then docker volume rm "$volume" >/dev/null; fi
}
trap cleanup EXIT
command -v jq >/dev/null
docker volume create "$volume" >/dev/null
created_volume=1
start_container() {
  docker run -d --name "$container" --network none --read-only \
    --cap-drop ALL --security-opt no-new-privileges --pids-limit 128 --memory 512m \
    --tmpfs /tmp:rw,nosuid,nodev,size=32m \
    --tmpfs /home/temps:rw,nosuid,nodev,size=64m,uid=1000,gid=1000,mode=0700 \
    --tmpfs /run/temps-runtime:rw,nosuid,nodev,size=8m,uid=1000,gid=1000,mode=0700 \
    -v "$volume:/home/temps/workspace" "$image" >/dev/null
  created_container=1
  for attempt in $(seq 1 30); do
    if docker exec "$container" temps-sandbox-runtime request >/dev/null 2>&1; then return; fi
    sleep 1
  done
  docker logs "$container"
  return 1
}
rpc() { docker exec "$container" temps-sandbox-runtime request /run/temps-runtime/control.sock "$1"; }
start_container
docker exec "$container" temps-sandbox-runtime check
rpc '{"version":1,"operation":{"type":"health"}}' | jq -e '.version == 1'
rpc '{"version":1,"operation":{"type":"health"}}' | jq -e '.capabilities | index("atomic_process_start") != null'
rpc '{"version":1,"operation":{"type":"health"}}' | jq -e '.capabilities | index("retained_runtime") != null'
docker exec "$container" id -u | jq -e '. == 1000'
docker exec "$container" codex --version
docker exec "$container" claude --version
docker exec "$container" opencode --version
docker exec "$container" bun --version
docker exec "$container" sh -c 'command -v pgrep; command -v pkill; command -v ps; command -v ss'
docker exec "$container" temps-sandbox-runtime exec timeout 5 node -e 'require("fs").readFileSync(0);console.log("stdin-eof-ok")'
docker exec "$container" temps-sandbox-runtime exec node -e 'const fs=require("fs");if(!fs.readFileSync("/proc/"+process.ppid+"/cmdline","utf8").includes("temps-sandbox-runtime"))process.exit(1);console.log("daemon-exec-ok")'
start_request='{"version":1,"operation":{"type":"start_unique","idempotency_key":"smoke-start-1","name":"test-server","program":"node","args":["-e","require(\"http\").createServer((request,response)=>response.end(\"runtime-ok\")).listen(3017,\"0.0.0.0\",()=>console.log(\"ready\"))"],"directory":"."}}'
id=$(rpc "$start_request" | jq -er '.process.id')
# A retry from a new client reuses the same daemon-owned process, not a host cache.
rpc "$start_request" | jq -e --arg id "$id" '.process.id == $id'
conflicting_request=$(jq -c '.operation.idempotency_key = "smoke-start-2"' <<< "$start_request")
rpc "$conflicting_request" | jq -e --arg id "$id" '.type == "process_conflict" and .process.id == $id'
# The start client has exited. A new connection sees the same running service.
rpc '{"version":1,"operation":{"type":"list"}}' | jq -e --arg id "$id" '.processes[] | select(.id == $id) | .status == "running"'
rpc '{"version":1,"operation":{"type":"list"}}' | jq -e '.processes | length == 1'
# Exercise the SDK's typed client through a real `connect` subprocess. The
# retained runtime survives the first frontend connection and remains wholly
# independent from the legacy managed process above.
package_dir=$(cd "$(dirname "$0")" && pwd)
cargo run --quiet --manifest-path "$package_dir/Cargo.toml" --example retained_smoke -- "$container" | grep -Fx 'retained-acquire-reattach-dispose-ok'
rpc '{"version":1,"operation":{"type":"list"}}' | jq -e --arg id "$id" '.processes[] | select(.id == $id) | .status == "running"'
for attempt in $(seq 1 20); do
  if rpc "{\"version\":1,\"operation\":{\"type\":\"logs\",\"id\":\"$id\"}}" | jq -e '.lines[] | select(.text == "ready")' >/dev/null; then break; fi
  sleep 1
done
rpc "{\"version\":1,\"operation\":{\"type\":\"logs\",\"id\":\"$id\"}}" | jq -e '.lines[] | select(.text == "ready")'
test "$(docker exec "$container" curl --fail --silent --show-error --max-time 5 http://127.0.0.1:3017/)" = runtime-ok
rpc "{\"version\":1,\"operation\":{\"type\":\"restart\",\"id\":\"$id\"}}" | jq -e '.process.restart_count == 1'
rpc "{\"version\":1,\"operation\":{\"type\":\"stop\",\"id\":\"$id\"}}" | jq -e '.process.status == "cancelled"'
if rpc '{"version":99,"operation":{"type":"health"}}'; then echo 'ERROR: accepted incompatible version'; exit 1; fi
docker exec "$container" node -e 'require("fs").writeFileSync("persisted.txt","survives replacement")'
docker stop -t 15 "$container" >/dev/null
docker inspect "$container" | jq -e '.[0].State.ExitCode == 0'
docker rm "$container" >/dev/null
created_container=0
start_container
docker exec "$container" node -e 'if(require("fs").readFileSync("persisted.txt","utf8")!=="survives replacement")process.exit(1)'
echo 'PASS: retained SDK acquire/reconnect/attach/dispose/not-found, legacy process independence, atomic start replay/conflict, HTTP after client disconnect, logs, restart, stop, version rejection, graceful shutdown, volume-preserving replacement'
