#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

set -Eeuo pipefail

SERVICE_NAME="temps-agent.service"
UNIT_PATH="/etc/systemd/system/${SERVICE_NAME}"
MANAGED_MARKER="# Managed by Temps. Do not edit by hand."
ACTION="install"
DATA_DIR="${TEMPS_DATA_DIR:-}"
TEMPS_BINARY=""
DRY_RUN=false

usage() {
  cat <<'EOF'
Install and manage the Temps worker agent as a systemd service.

Usage:
  sudo ./scripts/install-agent-service.sh install [options]
  sudo ./scripts/install-agent-service.sh run [options]
  sudo ./scripts/install-agent-service.sh uninstall
  ./scripts/install-agent-service.sh status
  ./scripts/install-agent-service.sh logs

Options:
  --binary PATH    Temps binary to execute (default: command -v temps)
  --data-dir PATH  Directory containing agent.json and node certificates
  --dry-run        Print the generated unit without changing the machine
  -h, --help       Show this help

Run `temps join` before installing the service. The enrollment token and mTLS
private key remain in the owner-only agent data directory; they are never
copied into the systemd unit.
EOF
}

fail() {
  printf 'Error: %s\n' "$*" >&2
  exit 1
}

while (($# > 0)); do
  case "$1" in
    install|run|uninstall|status|logs)
      ACTION="$1"
      shift
      ;;
    --binary)
      (($# >= 2)) || fail "--binary requires a path"
      TEMPS_BINARY="$2"
      shift 2
      ;;
    --data-dir)
      (($# >= 2)) || fail "--data-dir requires a path"
      DATA_DIR="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument '$1' (use --help)"
      ;;
  esac
done

require_root() {
  [[ ${EUID} -eq 0 ]] || fail "${ACTION} requires root; rerun with sudo"
}

require_systemd() {
  [[ "$(uname -s)" == "Linux" ]] || fail "systemd service installation supports Linux only"
  command -v systemctl >/dev/null 2>&1 || fail "systemctl was not found"
  [[ -d /run/systemd/system ]] || fail "this machine is not running systemd"
}

resolve_data_dir() {
  if [[ -n "${DATA_DIR}" ]]; then
    return
  fi

  # Root-operated workers are the common case because the agent manages
  # Docker, VXLAN, routes, and firewall state. Prefer its joined config.
  if [[ -s /root/.temps/agent.json ]]; then
    DATA_DIR=/root/.temps
    return
  fi

  # Also support `temps join` as an ordinary user followed by this script via
  # sudo. Do not assume sudo preserved HOME.
  if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
    local sudo_home
    sudo_home="$(getent passwd "${SUDO_USER}" 2>/dev/null | cut -d: -f6 || true)"
    if [[ -n "${sudo_home}" && -s "${sudo_home}/.temps/agent.json" ]]; then
      DATA_DIR="${sudo_home}/.temps"
      return
    fi
  fi

  if [[ -s "${HOME}/.temps/agent.json" ]]; then
    DATA_DIR="${HOME}/.temps"
    return
  fi

  fail "agent.json was not found; run 'temps join' first or pass --data-dir"
}

normalize_data_dir() {
  [[ -d "${DATA_DIR}" ]] || fail "agent data directory '${DATA_DIR}' does not exist"
  if command -v realpath >/dev/null 2>&1; then
    DATA_DIR="$(realpath "${DATA_DIR}")"
  fi
  [[ "${DATA_DIR}" == /* ]] || fail "could not resolve an absolute agent data directory"
}

resolve_binary() {
  if [[ -z "${TEMPS_BINARY}" ]]; then
    TEMPS_BINARY="$(command -v temps || true)"
  fi
  [[ -n "${TEMPS_BINARY}" ]] || fail "temps was not found in PATH; pass --binary"
  [[ -x "${TEMPS_BINARY}" ]] || fail "Temps binary '${TEMPS_BINARY}' is not executable"

  # readlink -f is present on the Linux hosts supported by this installer.
  if command -v readlink >/dev/null 2>&1; then
    TEMPS_BINARY="$(readlink -f "${TEMPS_BINARY}")"
  fi
  [[ "${TEMPS_BINARY}" == /* ]] || fail "could not resolve an absolute Temps binary path"
}

escape_systemd_value() {
  local value="$1"
  [[ "${value}" != *$'\n'* && "${value}" != *$'\r'* ]] || fail "paths cannot contain newlines"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\%/%%}"
  printf '%s' "${value}"
}

render_unit() {
  local escaped_binary escaped_data_dir
  escaped_binary="$(escape_systemd_value "${TEMPS_BINARY}")"
  escaped_data_dir="$(escape_systemd_value "${DATA_DIR}")"

  cat <<EOF
${MANAGED_MARKER}
[Unit]
Description=Temps worker agent
Documentation=https://temps.sh/docs/multi-node
Wants=network-online.target
After=network-online.target docker.service
Requires=docker.service

[Service]
Type=simple
Environment="TEMPS_DATA_DIR=${escaped_data_dir}"
ExecStart="${escaped_binary}" agent
Restart=on-failure
RestartSec=5s
TimeoutStopSec=30s
KillSignal=SIGTERM
UMask=0077

[Install]
WantedBy=multi-user.target
EOF
}

refuse_foreign_unit() {
  if [[ -e "${UNIT_PATH}" ]] && ! grep -Fqx "${MANAGED_MARKER}" "${UNIT_PATH}"; then
    fail "refusing to replace ${UNIT_PATH}: it is not managed by Temps"
  fi
}

install_service() {
  resolve_data_dir
  normalize_data_dir
  [[ -s "${DATA_DIR}/agent.json" ]] || fail "${DATA_DIR}/agent.json is missing or empty"
  resolve_binary

  if [[ "${DRY_RUN}" == true ]]; then
    render_unit
    return
  fi

  require_root
  require_systemd
  refuse_foreign_unit

  local temporary
  temporary="$(mktemp /etc/systemd/system/.temps-agent.service.XXXXXX)"
  trap 'rm -f "${temporary:-}"' EXIT
  render_unit >"${temporary}"
  chmod 0644 "${temporary}"
  mv -f "${temporary}" "${UNIT_PATH}"
  trap - EXIT

  systemctl daemon-reload
  systemctl enable "${SERVICE_NAME}"
  systemctl restart "${SERVICE_NAME}"

  printf 'Installed and started %s.\n' "${SERVICE_NAME}"
  printf '  Status: %s status\n' "$0"
  printf '  Logs:  %s logs\n' "$0"
}

run_foreground() {
  require_root
  resolve_data_dir
  normalize_data_dir
  [[ -s "${DATA_DIR}/agent.json" ]] || fail "${DATA_DIR}/agent.json is missing or empty"
  resolve_binary
  exec env TEMPS_DATA_DIR="${DATA_DIR}" "${TEMPS_BINARY}" agent
}

uninstall_service() {
  require_root
  require_systemd
  if [[ ! -e "${UNIT_PATH}" ]]; then
    printf '%s is not installed.\n' "${SERVICE_NAME}"
    return
  fi
  refuse_foreign_unit
  systemctl disable --now "${SERVICE_NAME}" || true
  rm -f "${UNIT_PATH}"
  systemctl daemon-reload
  printf 'Removed %s. Agent data and certificates were preserved.\n' "${SERVICE_NAME}"
}

case "${ACTION}" in
  install)
    install_service
    ;;
  run)
    run_foreground
    ;;
  uninstall)
    uninstall_service
    ;;
  status)
    require_systemd
    systemctl status --no-pager --full "${SERVICE_NAME}"
    ;;
  logs)
    require_systemd
    journalctl -u "${SERVICE_NAME}" -f
    ;;
esac
