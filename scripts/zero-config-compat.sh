#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
MANIFEST=${TEMPS_COMPAT_MANIFEST:-"$SCRIPT_DIR/zero-config-compat.json"}
API_URL=${TEMPS_API_URL:-http://localhost:8130/api}
DEPLOY_TIMEOUT=${TEMPS_COMPAT_DEPLOY_TIMEOUT:-1200}
POLL_INTERVAL=${TEMPS_COMPAT_POLL_INTERVAL:-5}
FILTER=${1:-}

for command_name in curl docker jq; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "Missing required command: $command_name" >&2
    exit 2
  fi
done

if [[ -z ${TEMPS_API_TOKEN:-} ]]; then
  echo "TEMPS_API_TOKEN is required" >&2
  exit 2
fi

if ! jq -e 'type == "array" and all(.[]; .name and .repo_owner and .repo_name and .git_url and .branch and .directory and .preset)' "$MANIFEST" >/dev/null; then
  echo "Invalid compatibility manifest: $MANIFEST" >&2
  exit 2
fi

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/temps-zero-config.XXXXXX")
CURRENT_PROJECT_ID=
CURRENT_PROJECT_SLUG=
CURRENT_CASE=
CURRENT_CONTAINER_BASELINE=
cleanup_errors=0

api_request() {
  local method=$1
  local path=$2
  local body=${3:-}
  local output=$4
  local status
  local args=(--silent --show-error --output "$output" --write-out '%{http_code}' --request "$method"
    --header "Authorization: Bearer $TEMPS_API_TOKEN" --header 'Accept: application/json')

  if [[ -n $body ]]; then
    args+=(--header 'Content-Type: application/json' --data "$body")
  fi

  status=$(curl "${args[@]}" "$API_URL$path") || return 1
  if [[ $status -lt 200 || $status -ge 300 ]]; then
    echo "Temps API $method $path returned HTTP $status" >&2
    jq . "$output" 2>/dev/null || sed -n '1,20p' "$output" >&2
    return 1
  fi
}

project_container_ids() {
  docker container ls --all --quiet --filter "label=sh.temps.project_id=$1"
}

new_project_container_ids() {
  local current="$WORK_DIR/current-containers"
  project_container_ids "$CURRENT_PROJECT_ID" | sort -u >"$current" || return 1
  comm -13 "$CURRENT_CONTAINER_BASELINE" "$current"
}

remove_project_images() {
  local slug=$1
  local image_id
  local image_list="$WORK_DIR/images"
  local reference
  [[ -n $slug ]] || return 0
  for reference in "temps-$slug:*" "$slug-*:*"; do
    docker image ls --quiet --filter "reference=$reference" >"$image_list" || return 1
    while IFS= read -r image_id; do
      [[ -z $image_id ]] || docker image rm "$image_id" >/dev/null || return 1
    done <"$image_list"
  done
}

print_deployment_failure() {
  local deployment_id=$1
  local jobs_response="$WORK_DIR/jobs.json"
  if api_request GET "/projects/$CURRENT_PROJECT_ID/deployments/$deployment_id/jobs" '' "$jobs_response"; then
    jq -r '.jobs[] | select(.status == "failure") | "[" + .name + "] " + (.error_message // "failed without an error message")' "$jobs_response" >&2
  fi
}

cleanup_current_project() {
  local response="$WORK_DIR/delete.json"
  local deadline
  local remaining
  local failed=0

  [[ -n $CURRENT_PROJECT_ID ]] || return 0
  echo "[$CURRENT_CASE] deleting project $CURRENT_PROJECT_ID"
  if ! api_request DELETE "/projects/$CURRENT_PROJECT_ID" '' "$response"; then
    failed=1
  else
    deadline=$((SECONDS + 60))
    while :; do
      remaining=$(new_project_container_ids) || {
        failed=1
        break
      }
      [[ -z $remaining ]] && break
      if (( SECONDS >= deadline )); then
        echo "[$CURRENT_CASE] containers remain after project deletion: $remaining" >&2
        failed=1
        break
      fi
      sleep 1
    done
  fi

  if ! remove_project_images "$CURRENT_PROJECT_SLUG"; then
    echo "[$CURRENT_CASE] failed to remove project images" >&2
    failed=1
  fi

  CURRENT_PROJECT_ID=
  CURRENT_PROJECT_SLUG=
  CURRENT_CONTAINER_BASELINE=
  (( failed == 0 )) || cleanup_errors=$((cleanup_errors + 1))
  return "$failed"
}

on_exit() {
  cleanup_current_project || true
  rm -rf -- "$WORK_DIR"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

run_case() {
  local case_json=$1
  local create_response="$WORK_DIR/create.json"
  local deployments_response="$WORK_DIR/deployments.json"
  local create_body
  local deadline
  local status
  local deployment_url

  CURRENT_CASE=$(jq -r .name <<<"$case_json")
  CURRENT_CONTAINER_BASELINE="$WORK_DIR/baseline-containers"
  docker container ls --all --quiet | sort -u >"$CURRENT_CONTAINER_BASELINE" || return 1
  create_body=$(jq -n --argjson case "$case_json" '{
    name: ("compat-" + $case.name),
    repo_name: $case.repo_name,
    repo_owner: $case.repo_owner,
    directory: $case.directory,
    main_branch: $case.branch,
    preset: $case.preset,
    preset_config: null,
    output_dir: null,
    build_command: null,
    install_command: null,
    environment_variables: null,
    automatic_deploy: false,
    project_type: "web",
    is_web_app: true,
    performance_metrics_enabled: false,
    storage_service_ids: [],
    use_default_wildcard: true,
    custom_domain: null,
    is_public_repo: true,
    git_url: $case.git_url,
    git_provider_connection_id: null,
    is_on_demand: false,
    exposed_port: null,
    source_type: "git"
  }')

  echo "[$CURRENT_CASE] creating project"
  api_request POST /projects "$create_body" "$create_response" || return 1
  CURRENT_PROJECT_ID=$(jq -er .id "$create_response") || return 1
  CURRENT_PROJECT_SLUG=$(jq -er .slug "$create_response") || return 1

  deadline=$((SECONDS + DEPLOY_TIMEOUT))
  while :; do
    if ! api_request GET "/projects/$CURRENT_PROJECT_ID/deployments?page=1&per_page=1" '' "$deployments_response"; then
      return 1
    fi
    status=$(jq -r '.deployments[0].status // "waiting"' "$deployments_response")
    case "$status" in
      deployed|completed)
        deployment_url=$(jq -r '.deployments[0].url // empty' "$deployments_response")
        if [[ -n $deployment_url ]]; then
          curl --insecure --fail --silent --show-error --retry 6 --retry-all-errors --retry-delay 2 "$deployment_url" >/dev/null || return 1
        fi
        echo "[$CURRENT_CASE] deployed successfully"
        return 0
        ;;
      failed|cancelled|canceled|stopped)
        echo "[$CURRENT_CASE] deployment ended with status: $status" >&2
        deployment_id=$(jq -r '.deployments[0].id' "$deployments_response")
        print_deployment_failure "$deployment_id" || true
        return 1
        ;;
    esac
    if (( SECONDS >= deadline )); then
      echo "[$CURRENT_CASE] deployment timed out in status: $status" >&2
      return 1
    fi
    sleep "$POLL_INTERVAL"
  done
}

selected_cases=$(jq -c --arg filter "$FILTER" '.[] | select($filter == "" or (.name | contains($filter)))' "$MANIFEST")
if [[ -z $selected_cases ]]; then
  echo "No compatibility cases matched: $FILTER" >&2
  exit 2
fi

passed=0
failed=0
while IFS= read -r case_json; do
  if run_case "$case_json"; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
  fi
  cleanup_current_project || true
done <<<"$selected_cases"

echo "Compatibility results: $passed passed, $failed deployment failures, $cleanup_errors cleanup failures"
(( failed == 0 && cleanup_errors == 0 ))
