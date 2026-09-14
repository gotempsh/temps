// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { spawn } from 'node:child_process'

export type SetupStep = 'preflight' | 'install' | 'verify' | 'context'
export class SetupError extends Error {
  constructor(public readonly step: SetupStep, public readonly code: string, message: string) {
    super(message)
    this.name = 'SetupError'
  }
}

export interface SetupOptions {
  ssh: string
  email: string
  context: string
  port: string
  identity?: string
  channel: string
  runtimeVersion?: string
}

export function validateOptions(options: SetupOptions): void {
  // Restrict the destination grammar: OpenSSH also accepts options and URIs.
  if (!/^(?:[a-zA-Z_][a-zA-Z0-9_-]*@)?[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(options.ssh)) {
    throw new SetupError('preflight', 'invalid_target', 'Use an SSH alias or user@hostname (IPv4 supported).')
  }
  if (!/^\d+$/.test(options.port) || Number(options.port) < 1 || Number(options.port) > 65535) {
    throw new SetupError('preflight', 'invalid_port', 'SSH port must be between 1 and 65535.')
  }
  if (!/^[^\s@'"\\]+@[^\s@'"\\]+\.[^\s@'"\\]+$/.test(options.email) || options.email.length > 254) {
    throw new SetupError('preflight', 'invalid_email', 'Provide a valid admin and certificate contact email.')
  }
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/.test(options.context)) {
    throw new SetupError('preflight', 'invalid_context', 'Context must contain 1–64 letters, digits, dots, underscores or hyphens.')
  }
  if (!['stable', 'beta', 'nightly'].includes(options.channel)) {
    throw new SetupError('preflight', 'invalid_channel', 'Choose stable, beta or nightly.')
  }
  if (options.runtimeVersion && !/^v\d+\.\d+\.\d+(?:[.-][A-Za-z0-9.]+)?$/.test(options.runtimeVersion)) {
    throw new SetupError('preflight', 'invalid_version', 'Runtime version must be a release tag such as v0.1.0.')
  }
}

function quote(value: string): string {
  return "'" + value.replaceAll("'", "'\\''") + "'"
}

export function sshArgs(options: SetupOptions): string[] {
  return [
    '-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
    '-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
    '-o', 'ClearAllForwardings=yes', '-o', 'ForwardAgent=no', '-o', 'ForwardX11=no',
    '-p', options.port,
    ...(options.identity ? ['-i', options.identity] : []),
    options.ssh,
    'sudo -n bash -s',
  ]
}

// stdin carries the script; no local shell interprets customer input.
// Root users do not necessarily have sudo installed.
export function remoteCommand(options: SetupOptions): string[] {
  const args = sshArgs(options)
  args[args.length - 1] = 'if [ "$(id -u)" = 0 ]; then exec bash -s; else exec sudo -n bash -s; fi'
  return args
}

export const PREFLIGHT_SCRIPT = `set -eu
[ "$(uname -s)" = Linux ] || { echo unsupported_os; exit 10; }
case "$(uname -m)" in x86_64|aarch64|arm64) ;; *) echo unsupported_arch; exit 11;; esac
for tool in curl flock; do command -v "$tool" >/dev/null || { echo missing_prerequisite; exit 12; }; done
if [ ! -f /root/.temps/setup-result.json ] && [ ! -d /root/.temps/.wizard-state ]; then
  if command -v temps >/dev/null || [ -e /root/.temps/data ] || systemctl cat temps >/dev/null 2>&1; then
    echo existing_installation; exit 13
  fi
  if command -v ss >/dev/null && ss -H -ltn | awk '{print $4}' | grep -Eq ':(80|443|5432|8080)$'; then
    echo occupied_ports; exit 14
  fi
fi
echo ready
`

export function installScript(options: SetupOptions): string {
  validateOptions(options)
  const flags = ['--mode', 'quick', '--email', options.email, '--yes', '--no-telemetry', '--channel', options.channel]
  if (options.runtimeVersion) flags.push('--version', options.runtimeVersion)
  return `set -eu
umask 077
mkdir -p /root/.temps
exec 9>/root/.temps/.cli-setup.lock
flock -n 9 || { echo 'Setup is already running on this server.' >&2; exit 20; }
# A completed installation is never reconfigured on retry.
if [ -f /root/.temps/setup-result.json ]; then
  cat /root/.temps/setup-result.json
  exit 0
fi
installer=$(mktemp /root/.temps/cli-installer.XXXXXX)
trap 'rm -f "$installer"' EXIT
curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 15 --max-time 60 https://temps.sh/deploy.sh -o "$installer"
# Installer output includes credentials. Keep it on the server with mode 0600.
touch /root/.temps/cli-setup.log
chmod 600 /root/.temps/cli-setup.log
bash "$installer" ${flags.map(quote).join(' ')} > /root/.temps/cli-setup.log 2>&1
test -s /root/.temps/setup-result.json
cat /root/.temps/setup-result.json
`
}

export function runSsh(options: SetupOptions, script: string, step: SetupStep): Promise<string> {
  validateOptions(options)
  return new Promise((resolve, reject) => {
    const child = spawn('ssh', remoteCommand(options), { stdio: ['pipe', 'pipe', 'pipe'] })
    const chunks: Buffer[] = []
    let size = 0
    let failure: SetupError | undefined
    const stop = (code: string, message: string) => {
      failure = new SetupError(step, code, message)
      child.kill('SIGTERM')
    }
    const timer = setTimeout(() => stop('ssh_timeout', 'SSH setup timed out. The remote installer may still be running; retry after checking /root/.temps/cli-setup.log.'), step === 'install' ? 20 * 60_000 : 30_000)
    child.stdout.on('data', (chunk: Buffer) => {
      size += chunk.length
      if (size > 64 * 1024) stop('output_limit', 'SSH returned an unexpectedly large setup result.')
      else chunks.push(chunk)
    })
    // Never forward arbitrary remote output: it can contain credentials.
    child.stderr.on('data', () => {})
    child.stdin.on('error', () => {}) // EPIPE is reported by the process exit.
    child.on('error', () => {
      clearTimeout(timer)
      reject(new SetupError(step, 'ssh_unavailable', 'Could not start SSH. Install OpenSSH and verify the destination with ssh first.'))
    })
    child.on('close', (code) => {
      clearTimeout(timer)
      if (failure) return reject(failure)
      if (code !== 0) {
        const preflightReasons: Record<number, string> = {
          10: 'This PoC requires Linux.',
          11: 'This PoC requires x86_64 or ARM64.',
          12: 'Install curl and flock on the server, then retry.',
          13: 'An existing installation was found without wizard state. Connect with temps login instead; no installation was changed.',
          14: 'A required port (80, 443, 5432 or 8080) is occupied. Choose a clean VPS or resolve the conflict; no service was stopped.',
        }
        if (step === 'preflight' && code !== null && preflightReasons[code]) {
          return reject(new SetupError(step, 'preflight_failed', preflightReasons[code]))
        }
        if (code === 20) return reject(new SetupError(step, 'setup_running', 'Another setup holds the server lock. Wait for it to finish, then retry.'))
        return reject(new SetupError(step, 'ssh_failed', `SSH ${step} failed (exit ${code}). Check trusted host keys, key authentication and passwordless sudo. For installation failures inspect /root/.temps/cli-setup.log on the server; rerun setup to resume.`))
      }
      resolve(Buffer.concat(chunks).toString('utf8'))
    })
    child.stdin.end(script)
  })
}

export interface SetupResult { url: string; apiKey: string; email: string }
export function parseResult(raw: string): SetupResult {
  try {
    const data = JSON.parse(raw)
    const url = new URL(data.console_url)
    if (data.status !== 'ok' || url.protocol !== 'https:' || url.username || url.password ||
        url.search || url.hash || url.pathname !== '/' ||
        typeof data.api_key !== 'string' || !/^[\x21-\x7e]{8,4096}$/.test(data.api_key) ||
        typeof data.admin_email !== 'string' || !data.admin_email.includes('@')) throw new Error()
    return { url: url.origin, apiKey: data.api_key, email: data.admin_email }
  } catch {
    throw new SetupError('verify', 'invalid_result', 'Installer did not return an HTTPS console URL and API key. Inspect /root/.temps/setup-result.json on the server. Resolve DNS/TLS or missing credentials before retrying; no local context was saved.')
  }
}

export interface SetupDependencies {
  remote(script: string, step: SetupStep): Promise<string>
  verify(result: SetupResult): Promise<void>
  save(result: SetupResult): Promise<void>
  event(step: SetupStep, status: 'started' | 'completed' | 'failed'): void
}

export async function provision(options: SetupOptions, deps: SetupDependencies): Promise<SetupResult> {
  validateOptions(options)
  let step: SetupStep = 'preflight'
  try {
    deps.event(step, 'started')
    await deps.remote(PREFLIGHT_SCRIPT, step)
    deps.event(step, 'completed')
    step = 'install'
    deps.event(step, 'started')
    const raw = await deps.remote(installScript(options), step)
    deps.event(step, 'completed')
    step = 'verify'
    deps.event(step, 'started')
    const result = parseResult(raw)
    await deps.verify(result)
    deps.event(step, 'completed')
    step = 'context'
    deps.event(step, 'started')
    await deps.save(result)
    deps.event(step, 'completed')
    return result
  } catch (error) {
    deps.event(step, 'failed')
    if (error instanceof SetupError) throw error
    throw new SetupError(step, 'step_failed', `Setup ${step} failed. Retry setup after resolving the problem; completed remote installation is preserved.`)
  }
}
