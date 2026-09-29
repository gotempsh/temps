// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `nodes ssh`: add a server over SSH (ADR 048 D2c). The control plane logs in
// with credentials used for this enrollment only, installs `temps` if needed,
// pairs the server and starts its agent.

import { readFile } from 'node:fs/promises'
import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import {
  nodeSshEnrollmentCreate,
  nodeSshEnrollmentGet,
  nodeSshEnrollmentList,
  nodeSshHostKey,
} from '../../api/sdk.gen.js'
import type { NodeSshEnrollmentResponse, SshCredentials } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm, promptPassword } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, keyValue, info, success, warning } from '../../ui/output.js'

const POLL_INTERVAL_MS = 2000

// ============================================================================
// Presentation (unit tested)
// ============================================================================

/** One line on where an enrollment is. */
export function describeEnrollment(enrollment: NodeSshEnrollmentResponse): string {
  if (enrollment.status === 'running') return `running: ${enrollment.step}`
  if (enrollment.status === 'failed') return `failed while ${enrollment.step}`
  if (enrollment.agent_mode === 'detached') {
    return 'added (agent started without a service manager: it stops at reboot)'
  }
  return 'added'
}

/** The log lines not printed yet, given how many characters were. */
export function newLogLines(log: string, printed: number): { lines: string[]; printed: number } {
  // The server keeps the last 64 KiB; if it trimmed past what was printed,
  // start again from what it has.
  const from = printed > log.length ? 0 : printed
  const end = log.lastIndexOf('\n') + 1
  if (end <= from) return { lines: [], printed: from }
  return { lines: log.slice(from, end - 1).split('\n'), printed: end }
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerNodesSshCommands(nodes: Command): void {
  const ssh = nodes
    .command('ssh')
    .description(
      'Add servers over SSH: the control plane logs in, installs temps if needed, pairs the ' +
        'server and starts its agent. Lists recent ones and their progress'
    )
    .option('--json', 'Output in JSON format')
    .action(sshListAction)

  ssh
    .command('add')
    .description(
      "Add the server at --host. Shows its SSH host key to confirm first (or pass --host-key). " +
        'Credentials are used for this enrollment only and never stored'
    )
    .requiredOption('--host <host>', 'Hostname or IP address of the server')
    .option('--port <port>', 'SSH port', (value) => Number.parseInt(value, 10), 22)
    .option('--user <user>', 'User to log in as: root, or a user with sudo', 'root')
    .option('--identity-file <path>', 'Log in with this private key')
    .option('--ask-passphrase', 'Prompt for the private key passphrase')
    .option('--agent', "Log in with the SSH agent of the control plane's temps serve process")
    .option('--password-stdin', 'Read the password from stdin (default: prompt for it)')
    .option('--host-key <fingerprint>', 'The SHA256:… host key fingerprint you verified')
    .option('--name <name>', 'Name the node registers under (default: worker-<random>)')
    .option(
      '--node-address <ip[:port]>',
      "The server's public address for WireGuard, if not the one SSH connects to"
    )
    .option('--no-wait', 'Return once started instead of following the progress')
    .option('--json', 'Output in JSON format')
    .action(sshAddAction)

  ssh
    .command('show <id>')
    .description('Show one enrollment: its progress and the log with the server output')
    .option('--json', 'Output in JSON format')
    .action(sshShowAction)
}

// ============================================================================
// Actions
// ============================================================================

async function sshListAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Reading SSH enrollments...', async () => {
    const { data, error } = await nodeSshEnrollmentList()
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data.enrollments
  })

  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.globe} Servers Added Over SSH`)
  if (result.length === 0) {
    console.log(
      `  ${colors.muted('None yet. Add one:')} bunx @temps-sdk/cli nodes ssh add --host <server> --user root`
    )
    newline()
    return
  }
  printTable(result, [
    { header: 'ID', key: 'id' },
    { header: 'Node', key: 'name' },
    { header: 'Server', accessor: (e) => `${e.ssh_user}@${e.ssh_address}` },
    { header: 'Progress', accessor: (e) => describeEnrollment(e) },
  ])
  console.log(`  ${colors.muted('Details and log:')} bunx @temps-sdk/cli nodes ssh show <id>`)
  newline()
}

interface AddOptions {
  host: string
  port: number
  user: string
  identityFile?: string
  askPassphrase?: boolean
  agent?: boolean
  passwordStdin?: boolean
  hostKey?: string
  name?: string
  nodeAddress?: string
  wait: boolean
  json?: boolean
}

async function readPasswordFromStdin(): Promise<string> {
  if (process.stdin.isTTY) {
    throw new Error('--password-stdin reads the password from stdin, but stdin is a terminal')
  }
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.from(chunk))
  }
  const password = Buffer.concat(chunks).toString('utf8').split(/\r?\n/)[0] ?? ''
  if (!password) {
    throw new Error('--password-stdin read an empty password')
  }
  return password
}

async function credentials(options: AddOptions): Promise<SshCredentials> {
  const chosen = [options.identityFile, options.agent, options.passwordStdin].filter(Boolean)
  if (chosen.length > 1) {
    throw new Error('use one of --identity-file, --agent or --password-stdin')
  }
  if (options.identityFile) {
    const privateKey = await readFile(options.identityFile, 'utf8').catch((error: Error) => {
      throw new Error(`could not read ${options.identityFile}: ${error.message}`)
    })
    const passphrase = options.askPassphrase
      ? await promptPassword({ message: `Passphrase for ${options.identityFile}:` })
      : null
    return { method: 'private_key', private_key: privateKey, passphrase }
  }
  if (options.agent) {
    return { method: 'agent' }
  }
  const password = options.passwordStdin
    ? await readPasswordFromStdin()
    : await promptPassword({ message: `Password for ${options.user}@${options.host}:` })
  return { method: 'password', password }
}

async function confirmedHostKey(options: AddOptions): Promise<string> {
  if (options.hostKey) return options.hostKey.trim()
  if (!process.stdin.isTTY) {
    throw new Error(
      'pass --host-key SHA256:… (the fingerprint you verified) when not running interactively'
    )
  }
  const key = await withSpinner(`Reading the host key of ${options.host}...`, async () => {
    const { data, error } = await nodeSshHostKey({
      body: { host: options.host, port: options.port },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })
  newline()
  keyValue('Server', key.address)
  keyValue('Host key', `${key.algorithm} ${key.fingerprint}`)
  console.log(
    `  ${colors.muted('Compare it with the server\'s: ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub')}`
  )
  newline()
  const confirmed = await promptConfirm({
    message: 'Is this the server\'s key?',
    default: false,
  })
  if (!confirmed) {
    throw new Error('host key not confirmed; nothing was done')
  }
  return key.fingerprint
}

async function sshAddAction(options: AddOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Credentials first: --password-stdin consumes stdin before any prompt.
  const creds = await credentials(options)
  const hostKey = await confirmedHostKey(options)

  const started = await withSpinner(`Starting to add ${options.host}...`, async () => {
    const { data, error } = await nodeSshEnrollmentCreate({
      body: {
        host: options.host,
        port: options.port,
        user: options.user,
        credentials: creds,
        host_key_fingerprint: hostKey,
        name: options.name ?? null,
        node_address: options.nodeAddress ?? null,
      },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!options.wait) {
    if (options.json) {
      json(started)
      return
    }
    success(`Adding ${started.name} (enrollment ${started.id})`)
    console.log(`  ${colors.muted('Progress:')} bunx @temps-sdk/cli nodes ssh show ${started.id}`)
    return
  }

  const finished = await follow(started, !options.json)
  if (options.json) {
    json(finished)
  } else {
    printOutcome(finished)
  }
  if (finished.status !== 'succeeded') {
    process.exitCode = 1
  }
}

/** Poll until the enrollment ends, printing its log as it grows. */
async function follow(
  enrollment: NodeSshEnrollmentResponse,
  print: boolean
): Promise<NodeSshEnrollmentResponse> {
  let current = enrollment
  let printed = 0
  let step = ''
  for (;;) {
    if (print) {
      if (current.step !== step && current.status === 'running') {
        step = current.step
        info(`${step}...`)
      }
      const next = newLogLines(current.log, printed)
      for (const line of next.lines) console.log(`  ${colors.muted(line)}`)
      printed = next.printed
    }
    if (current.status !== 'running') return current
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS))
    const { data, error } = await nodeSshEnrollmentGet({
      path: { enrollment_id: current.id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    current = data
  }
}

function printOutcome(enrollment: NodeSshEnrollmentResponse): void {
  newline()
  if (enrollment.status === 'succeeded') {
    success(`${enrollment.name} added (node ${enrollment.node_id ?? '?'})`)
    if (enrollment.agent_mode === 'detached') {
      warning(
        'The server has no systemd: its agent was started in the background and will not ' +
          'come back after a reboot. Run `temps agent` there under a supervisor.'
      )
    }
  } else {
    console.log(`  ${colors.error(`Adding ${enrollment.name} failed while ${enrollment.step}.`)}`)
    if (enrollment.error) {
      newline()
      console.log(`  ${enrollment.error.split('\n').join('\n  ')}`)
    }
    newline()
    console.log(`  ${colors.muted('Full log:')} bunx @temps-sdk/cli nodes ssh show ${enrollment.id}`)
  }
  newline()
}

async function sshShowAction(id: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const enrollmentId = Number.parseInt(id, 10)
  if (!Number.isInteger(enrollmentId)) {
    throw new Error('the enrollment id must be a number (see `bunx @temps-sdk/cli nodes ssh`)')
  }
  const enrollment = await withSpinner('Reading the enrollment...', async () => {
    const { data, error } = await nodeSshEnrollmentGet({ path: { enrollment_id: enrollmentId } })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })
  if (options.json) {
    json(enrollment)
    return
  }
  newline()
  header(`${icons.globe} ${enrollment.name}`)
  keyValue('Server', `${enrollment.ssh_user}@${enrollment.ssh_address} (${enrollment.host})`)
  keyValue('Logged in with', enrollment.auth_method.replace('_', ' '))
  keyValue('Host key', enrollment.host_key_fingerprint)
  keyValue('Progress', describeEnrollment(enrollment))
  if (enrollment.node_id) keyValue('Node', enrollment.node_id)
  if (enrollment.error) {
    newline()
    console.log(`  ${colors.error(enrollment.error.split('\n').join('\n  '))}`)
  }
  if (enrollment.log) {
    newline()
    for (const line of enrollment.log.trimEnd().split('\n')) {
      console.log(`  ${colors.muted(line)}`)
    }
  }
  newline()
}
