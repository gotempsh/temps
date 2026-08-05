#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/temps-zero-config-test.XXXXXX")
trap 'rm -rf -- "$TEST_DIR"' EXIT
mkdir -p "$TEST_DIR/bin"

export TEMPS_COMPAT_TEST_LOG="$TEST_DIR/calls.log"

cat >"$TEST_DIR/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
method=GET
output=
url=
while (($#)); do
  case "$1" in
    --request) method=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    --write-out|--header|--data|--retry|--retry-delay) shift 2 ;;
    --silent|--show-error|--insecure|--fail|--retry-all-errors) shift ;;
    *) url=$1; shift ;;
  esac
done
printf '%s %s\n' "$method" "$url" >>"$TEMPS_COMPAT_TEST_LOG"
case "$method $url" in
  "POST "*/projects)
    printf '%s\n' '{"id":42,"slug":"compat-nextjs-app-playground"}' >"$output"
    ;;
  "GET "*/projects/42/deployments*)
    printf '%s\n' '{"deployments":[{"status":"deployed","url":""}]}' >"$output"
    ;;
  "DELETE "*/projects/42)
    printf '%s\n' '{}' >"$output"
    ;;
esac
[[ -z $output ]] || printf '200'
EOF

cat >"$TEST_DIR/bin/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'docker %s\n' "$*" >>"$TEMPS_COMPAT_TEST_LOG"
if [[ $1 == image && $2 == ls && $* == *"reference=temps-"* ]]; then
  printf '%s\n' compat-image-id
fi
EOF

chmod +x "$TEST_DIR/bin/curl" "$TEST_DIR/bin/docker"

PATH="$TEST_DIR/bin:$PATH" \
  TEMPS_API_TOKEN=test-token \
  TEMPS_COMPAT_POLL_INTERVAL=0 \
  "$SCRIPT_DIR/zero-config-compat.sh" nextjs-app-playground >/dev/null

delete_line=$(grep -n 'DELETE .*/projects/42' "$TEMPS_COMPAT_TEST_LOG" | cut -d: -f1)
container_check_line=$(grep -n 'docker container ls.*sh.temps.project_id=42' "$TEMPS_COMPAT_TEST_LOG" | cut -d: -f1)
image_remove_line=$(grep -n 'docker image rm compat-image-id' "$TEMPS_COMPAT_TEST_LOG" | cut -d: -f1)

if [[ -z $delete_line || -z $container_check_line || -z $image_remove_line ]]; then
  echo "Expected delete, container verification, and image cleanup calls" >&2
  exit 1
fi

if (( delete_line >= container_check_line || container_check_line >= image_remove_line )); then
  echo "Cleanup calls happened in the wrong order" >&2
  exit 1
fi

echo "zero-config compatibility harness test passed"
