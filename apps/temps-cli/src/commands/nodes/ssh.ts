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
import type {
  NodeSshCredentials,
  NodeSshEnrollmentResponse,
  NodeSshEnrollmentSummary,
  NodeSshHostKeyResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm, promptPassword } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, keyValue, info, success, warning } from '../../ui/output.js'
import { parseId, parsePortOption, validPort } from './options.js'

const POLL_INTERVAL_MS = 2000
/** Waits before retrying a failed progress poll; one entry per retry. */
const POLL_RETRY_DELAYS_MS = [1000, 2000, 4000]

// ============================================================================
// Presentation (unit tested)
// ============================================================================

/** One line on where an enrollment is: a list summary or a full enrollment. */
export function describeEnrollment(enrollment: NodeSshEnrollmentSummary): string {
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

/**
 * The file on the server holding its public host key for `algorithm` (as
 * the SSH handshake names it: `ssh-ed25519`, `ecdsa-sha2-nistp256`,
 * `rsa-sha2-512`...), or null for other key types.
 */
export function hostKeyFileForAlgorithm(algorithm: string): string | null {
  const name = algorithm.trim().toLowerCase()
  if (name === 'ssh-ed25519') return '/etc/ssh/ssh_host_ed25519_key.pub'
  if (name.startsWith('ecdsa-')) return '/etc/ssh/ssh_host_ecdsa_key.pub'
  if (name === 'ssh-rsa' || name.startsWith('rsa-')) return '/etc/ssh/ssh_host_rsa_key.pub'
  return null
}

/** Every host key's fingerprint, for key types without a known file. */
export const ALL_HOST_KEYS_COMMAND =
  'for f in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf "$f"; done'

/** What to run on the server itself to get the fingerprint to compare. */
export function hostKeyCompareCommand(algorithm: string): string {
  const file = hostKeyFileForAlgorithm(algorithm)
  return file ? `ssh-keygen -lf ${file}` : ALL_HOST_KEYS_COMMAND
}

/** The credential flags of `nodes ssh add`. */
export interface CredentialOptions {
  identityFile?: string
  askPassphrase?: boolean
  passphraseStdin?: boolean
  agent?: boolean
  passwordStdin?: boolean
}

/** Where `nodes ssh add` gets the credentials from. */
export type CredentialSource =
  | { method: 'private_key'; path: string; passphrase: 'none' | 'prompt' | 'stdin' }
  | { method: 'agent' }
  | { method: 'password'; from: 'prompt' | 'stdin' }

/**
 * Check the credential flags and pick where the credentials come from.
 * `interactive` is whether stdin is a terminal: prompts need one, and the
 * `*-stdin` flags need it not to be. Throws with what to pass instead.
 */
export function credentialSource(
  options: CredentialOptions,
  interactive: boolean
): CredentialSource {
  const chosen = [options.identityFile, options.agent, options.passwordStdin].filter(Boolean)
  if (chosen.length > 1) {
    throw new Error('use one of --identity-file, --agent or --password-stdin')
  }
  if (options.passwordStdin && options.passphraseStdin) {
    throw new Error('--password-stdin and --passphrase-stdin both read stdin: use one')
  }
  if (options.askPassphrase && options.passphraseStdin) {
    throw new Error('use either --ask-passphrase or --passphrase-stdin, not both')
  }
  if ((options.askPassphrase || options.passphraseStdin) && !options.identityFile) {
    const flag = options.askPassphrase ? '--ask-passphrase' : '--passphrase-stdin'
    throw new Error(`${flag} is for an encrypted --identity-file; pass --identity-file too`)
  }
  const readsStdin = options.passwordStdin || options.passphraseStdin
  if (readsStdin && interactive) {
    const flag = options.passwordStdin ? '--password-stdin' : '--passphrase-stdin'
    throw new Error(`${flag} reads from stdin, but stdin is a terminal: pipe the secret in`)
  }

  if (options.identityFile) {
    if (options.askPassphrase && !interactive) {
      throw new Error(
        '--ask-passphrase needs a terminal to prompt on; pipe the passphrase with --passphrase-stdin'
      )
    }
    return {
      method: 'private_key',
      path: options.identityFile,
      passphrase: options.askPassphrase ? 'prompt' : options.passphraseStdin ? 'stdin' : 'none',
    }
  }
  if (options.agent) return { method: 'agent' }
  if (options.passwordStdin) return { method: 'password', from: 'stdin' }
  if (!interactive) {
    throw new Error(
      'stdin is not a terminal, so the password cannot be prompted for: pass --password-stdin ' +
        '(and pipe it in), --identity-file <path> or --agent'
    )
  }
  return { method: 'password', from: 'prompt' }
}

/**
 * Read a resource, retrying a failed read after each of `delaysMs`. Throws
 * the last error once every retry failed.
 */
export async function withRetries<T>(
  read: () => Promise<T>,
  delaysMs: number[],
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await read()
    } catch (error) {
      const delay = delaysMs[attempt]
      if (delay === undefined) throw error
      await sleep(delay)
    }
  }
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
    .option('--port <port>', 'SSH port', parsePortOption, 22)
    .option('--user <user>', 'User to log in as: root, or a user with sudo', 'root')
    .option('--identity-file <path>', 'Log in with this private key')
    .option('--ask-passphrase', 'Prompt for the private key passphrase')
    .option(
      '--passphrase-stdin',
      'Read the private key passphrase from stdin (not with --password-stdin)'
    )
    .option('--agent', "Log in with the SSH agent of the control plane's temps serve process")
    .option('--password-stdin', 'Read the password from stdin (default: prompt for it)')
    .option(
      '--host-key <fingerprint>',
      'The SHA256:… host key fingerprint you verified (see `nodes ssh host-key`)'
    )
    .option('--name <name>', 'Name the node registers under (default: worker-<random>)')
    .option(
      '--node-address <ip[:port]>',
      "The server's public address for WireGuard, if not the one SSH connects to"
    )
    .option('--no-wait', 'Return once started instead of following the progress')
    .option('--json', 'Output in JSON format')
    .action(sshAddAction)

  ssh
    .command('host-key')
    .description(
      'Read the SSH host key of the server at --host, to verify it before `nodes ssh add ' +
        '--host-key`. Nothing is logged in to or changed'
    )
    .requiredOption('--host <host>', 'Hostname or IP address of the server')
    .option('--port <port>', 'SSH port', parsePortOption, 22)
    .option('--json', 'Output in JSON format')
    .action(sshHostKeyAction)

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
  passphraseStdin?: boolean
  agent?: boolean
  passwordStdin?: boolean
  hostKey?: string
  name?: string
  nodeAddress?: string
  wait: boolean
  json?: boolean
}

/** The first line of stdin; `flag` and `what` name it in errors. */
async function readSecretFromStdin(flag: string, what: string): Promise<string> {
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.from(chunk))
  }
  const secret = Buffer.concat(chunks).toString('utf8').split(/\r?\n/)[0] ?? ''
  if (!secret) {
    throw new Error(`${flag} read an empty ${what}`)
  }
  return secret
}

async function credentials(options: AddOptions, source: CredentialSource): Promise<NodeSshCredentials> {
  switch (source.method) {
    case 'private_key': {
      // The published CLI is bundled for Node (see docs.ts), so this must use
      // Node's fs promises rather than Bun.file, which is undefined there.
      const privateKey = await readFile(source.path, 'utf8').catch((error: Error) => {
        throw new Error(`could not read ${source.path}: ${error.message}`)
      })
      const passphrase =
        source.passphrase === 'prompt'
          ? await promptPassword({ message: `Passphrase for ${source.path}:` })
          : source.passphrase === 'stdin'
            ? await readSecretFromStdin('--passphrase-stdin', 'passphrase')
            : null
      return { method: 'private_key', private_key: privateKey, passphrase }
    }
    case 'agent':
      return { method: 'agent' }
    case 'password': {
      const password =
        source.from === 'stdin'
          ? await readSecretFromStdin('--password-stdin', 'password')
          : await promptPassword({ message: `Password for ${options.user}@${options.host}:` })
      return { method: 'password', password }
    }
  }
}

async function readHostKey(host: string, port: number): Promise<NodeSshHostKeyResponse> {
  return withSpinner(`Reading the host key of ${host}...`, async () => {
    const { data, error } = await nodeSshHostKey({ body: { host, port } })
    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })
}

function printHostKey(key: NodeSshHostKeyResponse): void {
  newline()
  keyValue('Server', key.address)
  keyValue('Host key', `${key.algorithm} ${key.fingerprint}`)
  console.log(
    `  ${colors.muted("Compare it with the server's own, from its console or a session you trust:")}`
  )
  console.log(`    ${hostKeyCompareCommand(key.algorithm)}`)
  console.log(`  ${colors.muted('If they differ, do not continue: something else answered.')}`)
  newline()
}

async function confirmedHostKey(options: AddOptions): Promise<string> {
  if (options.hostKey) return options.hostKey.trim()
  if (!process.stdin.isTTY) {
    throw new Error(
      'pass --host-key SHA256:… (the fingerprint you verified) when not running interactively'
    )
  }
  const key = await readHostKey(options.host, options.port)
  printHostKey(key)
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
  if (!validPort(options.port)) {
    throw new Error('--port must be a number between 1 and 65535')
  }
  const interactive = Boolean(process.stdin.isTTY)
  // Check every flag before asking for anything or touching the server.
  const source = credentialSource(options, interactive)
  if (!options.hostKey && !interactive) {
    throw new Error(
      'pass --host-key SHA256:… when not running interactively (read it with ' +
        '`bunx @temps-sdk/cli nodes ssh host-key --host <server>` and verify it)'
    )
  }

  await requireAuth()
  await setupClient()

  // Credentials first: the *-stdin flags consume stdin before any prompt.
  const creds = await credentials(options, source)
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
    const id = current.id
    try {
      current = await withRetries(async () => {
        const { data, error } = await nodeSshEnrollmentGet({ path: { enrollment_id: id } })
        if (error || !data) {
          throw new Error(getErrorMessage(error))
        }
        return data
      }, POLL_RETRY_DELAYS_MS)
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error)
      throw new Error(
        `lost track of the progress (${reason}). Adding the server goes on without this ` +
          `command; continue following it with: bunx @temps-sdk/cli nodes ssh show ${id}`
      )
    }
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

  const enrollmentId = parseId(id)
  if (enrollmentId === null) {
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

async function sshHostKeyAction(options: {
  host: string
  port: number
  json?: boolean
}): Promise<void> {
  if (!validPort(options.port)) {
    throw new Error('--port must be a number between 1 and 65535')
  }
  await requireAuth()
  await setupClient()

  const key = await readHostKey(options.host, options.port)
  if (options.json) {
    json({ ...key, compare_command: hostKeyCompareCommand(key.algorithm) })
    return
  }
  printHostKey(key)
  console.log(`  ${colors.muted('Once it matches, add the server with it:')}`)
  const port = options.port === 22 ? '' : ` --port ${options.port}`
  console.log(
    `    bunx @temps-sdk/cli nodes ssh add --host ${options.host}${port} --host-key ${key.fingerprint}`
  )
  newline()
}
