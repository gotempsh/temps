// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps start` runs Temps on this computer: it downloads the server binary,
 * starts a database in Docker, sets the platform up on first run, starts the
 * server in the background, mints an API key for the CLI (which every
 * coding agent using the Temps skill goes through), and copies that skill
 * in. Running it again only starts what is stopped. `temps stop` stops it.
 */

import type { Command } from 'commander'
import type { Ora } from 'ora'
import { readdir } from 'node:fs/promises'
import { homedir, hostname } from 'node:os'
import {
  clearContextDefaultProject,
  getContext,
  getSavedDefaultContext,
  upsertContext,
  type CliContext,
} from '../../config/contexts.js'
import { config, credentials } from '../../config/store.js'
import { colors, icons } from '../../ui/output.js'
import { promptConfirm } from '../../ui/prompts.js'
import { isQuietMode, startSpinner } from '../../ui/spinner.js'
import {
  BUNDLED_SKILL,
  DB_CONTAINER,
  DB_VOLUME,
  DOCKER_NETWORK,
  LOCAL_ADMIN_EMAIL,
  LOCAL_CONTEXT,
  StartError,
  consoleApiUrl,
  consolePortCandidates,
  dbPortCandidates,
  formatSeconds,
  generateAdminPassword,
  generateDbPassword,
  localPaths,
  localUrls,
  parseChannel,
  parsePort,
  proxyPortCandidates,
  releaseTarget,
  resolveSkillsScope,
  type LocalPaths,
  type LocalState,
  type SkillsScope,
} from './runtime.js'
import {
  containerState,
  deleteLocalData,
  destroySandboxes,
  dockerVersion,
  ensureBinary,
  ensureDatabase,
  findBundledSkill,
  installSkill,
  isLocalServer,
  isPortFree,
  mintApiKey,
  pickPort,
  readState,
  removeWorkloads,
  resolveVersion,
  runSetup,
  sandboxContainers,
  serverHealthy,
  startServer,
  stopDatabase,
  stopServers,
  waitForServer,
  withLock,
  writeState,
  type RemovedWorkloads,
} from './steps.js'

interface StartOptions {
  channel?: string
  runtimeVersion?: string
  skills?: string
  port?: string
  makeDefault?: boolean
  json?: boolean
}

interface StopOptions {
  deleteData?: boolean
  yes?: boolean
}

/** Prints one line per finished step; a spinner shows what is still running. */
class Steps {
  private spinner: Ora | null = null
  private pending = new Set<string>()

  constructor(private readonly quiet: boolean) {}

  begin(label: string): void {
    this.pending.add(label)
    this.refresh()
  }

  done(label: string, text: string, ms?: number): void {
    this.pending.delete(label)
    if (this.quiet) return
    this.spinner?.stop()
    const time = ms === undefined ? '' : colors.muted(` ${formatSeconds(ms)}`)
    console.log(`${colors.success('✔')} ${text}${time}`)
    this.spinner = null
    this.refresh()
  }

  stop(): void {
    this.spinner?.stop()
    this.spinner = null
  }

  async time<T>(label: string, work: () => Promise<T>, text: (result: T) => string): Promise<T> {
    const started = Date.now()
    this.begin(label)
    try {
      const result = await work()
      this.done(label, text(result), Date.now() - started)
      return result
    } catch (error) {
      this.pending.delete(label)
      this.stop()
      throw error
    }
  }

  private refresh(): void {
    if (this.quiet || this.pending.size === 0) return
    const text = `${[...this.pending].join(', ')}…`
    if (this.spinner) this.spinner.text = text
    else this.spinner = startSpinner(text)
  }
}

async function validateKey(apiUrl: string, apiKey: string): Promise<{ id: number; email: string } | null> {
  try {
    const response = await fetch(`${apiUrl}/user/me`, {
      headers: { Authorization: `Bearer ${apiKey}` },
      signal: AbortSignal.timeout(5_000),
    })
    if (!response.ok) return null
    return (await response.json()) as { id: number; email: string }
  } catch {
    return null
  }
}

/**
 * What a `temps` command run with no `TEMPS_CONTEXT` talks to today, or null
 * when that is nothing or already this local server. Reads the default
 * saved on disk, never the context `TEMPS_CONTEXT` selects for this one
 * process: with production saved as the default, `TEMPS_CONTEXT=local temps
 * start` must still see production as the default it would be replacing.
 * A login from before contexts existed counts too, named by its URL.
 */
export async function savedDefaultElsewhere(apiUrl: string): Promise<string | null> {
  const saved = await getSavedDefaultContext()
  if (saved) return saved.name === LOCAL_CONTEXT ? null : saved.name
  const legacyUrl = config.getAll().apiUrl
  const legacyKey = (await credentials.getAll()).apiKey
  return legacyKey && legacyUrl && legacyUrl !== apiUrl ? legacyUrl : null
}

/**
 * Decide whether `local` becomes the CLI's saved default. It does when no
 * other default is saved. Otherwise it asks (or needs --make-default),
 * because switching would silently send `temps deploy` somewhere else.
 */
async function shouldActivate(
  options: StartOptions,
  apiUrl: string,
): Promise<{ activate: boolean; previous: string | null }> {
  const previous = await savedDefaultElsewhere(apiUrl)
  if (!previous) return { activate: true, previous: null }
  if (options.makeDefault) return { activate: true, previous }
  if (options.json || !process.stdin.isTTY) return { activate: false, previous }
  const activate = await promptConfirm({
    message: `Make "${LOCAL_CONTEXT}" your default CLI context? Your current default is "${previous}".`,
    default: true,
  })
  return { activate, previous }
}

/**
 * The default project to carry over when the `local` context is rewritten.
 * A project only exists in the install it was created in, and the one proof
 * that the saved context is for this install is that this server still
 * accepts its key: a reset or a fresh install has a new database, which
 * rejects every old key. So the default survives a restart and nothing else.
 */
export function carriedDefaultProject(existing: CliContext | null, keyStillValid: boolean): string | undefined {
  return keyStillValid ? existing?.defaultProject : undefined
}

/** Reuse the saved key while it still works; otherwise mint a new one. */
async function signInCli(
  bin: string,
  state: LocalState,
  paths: LocalPaths,
  options: StartOptions,
): Promise<{ minted: boolean; active: boolean; previous: string | null }> {
  const apiUrl = consoleApiUrl(state.consolePort)
  const existing = await getContext(LOCAL_CONTEXT)
  let apiKey = existing?.url === apiUrl ? existing.apiKey : undefined
  let keyPrefix = existing?.keyPrefix
  let user = apiKey ? await validateKey(apiUrl, apiKey) : null
  let minted = false
  if (!user) {
    const key = await mintApiKey(bin, state, paths, `temps start (${hostname()})`)
    apiKey = key.apiKey
    keyPrefix = key.keyPrefix
    user = await validateKey(apiUrl, apiKey)
    if (!user) {
      throw new StartError(
        'api_key',
        'rejected',
        `The API key minted for ${state.adminEmail} was rejected by ${apiUrl}. Check ${paths.log}.`,
      )
    }
    minted = true
  }
  const { activate, previous } = await shouldActivate(options, apiUrl)
  await upsertContext(
    {
      name: LOCAL_CONTEXT,
      url: apiUrl,
      apiKey: apiKey as string,
      email: user.email,
      keyPrefix,
      defaultProject: carriedDefaultProject(existing, !minted),
    },
    { makeActive: activate },
  )
  if (activate) {
    config.set('apiUrl', apiUrl)
    await credentials.setAll({ apiKey, userId: user.id, email: user.email })
  }
  return { minted, active: activate, previous }
}

async function freshState(options: StartOptions, target: string): Promise<LocalState> {
  const preferred = options.port === undefined ? undefined : parsePort(options.port)
  const isRoot = typeof process.getuid === 'function' && process.getuid() === 0
  const proxyPort = await pickPort(proxyPortCandidates(preferred, process.platform, isRoot), [], 'apps and console')
  const consolePort = await pickPort(consolePortCandidates(), [proxyPort], 'console API')
  const dbPort = await pickPort(dbPortCandidates(), [proxyPort, consolePort], 'database')
  const version = options.runtimeVersion ?? (await resolveVersion(parseChannel(options.channel), target))
  return {
    version,
    dbPort,
    dbPassword: generateDbPassword(),
    proxyPort,
    consolePort,
    adminEmail: LOCAL_ADMIN_EMAIL,
    adminPassword: generateAdminPassword(),
    setupComplete: false,
  }
}

async function startAction(options: StartOptions): Promise<void> {
  const paths = localPaths()
  await withLock(paths, 'start', () => startLocked(options, paths))
}

async function startLocked(options: StartOptions, paths: LocalPaths): Promise<void> {
  const started = Date.now()
  const quiet = Boolean(options.json) || isQuietMode()
  const steps = new Steps(quiet)
  const target = releaseTarget(process.platform, process.arch)
  parseChannel(options.channel)
  if (options.port !== undefined) parsePort(options.port)
  const cwd = process.cwd()
  const scope: SkillsScope = resolveSkillsScope(options.skills, await readdir(cwd))

  try {
    await steps.time('Checking Docker', dockerVersion, (version) => `Docker found (${version})`)

    let state = await readState(paths)
    const running = Boolean(state && (await isLocalServer(state.pid, paths)) && (await serverHealthy(state.consolePort)))

    if (!state) {
      if ((await containerState(DB_CONTAINER)) !== 'missing') {
        throw new StartError(
          'state',
          'orphan_database',
          `A ${DB_CONTAINER} container exists but ${paths.state} is missing, so its password is unknown. ` +
            'Run `temps stop --delete-data` to remove it, then `temps start` again.',
        )
      }
      state = await steps.time('Resolving version', () => freshState(options, target), (s) => `Using Temps ${s.version}`)
      await writeState(paths, state)
    } else if (options.runtimeVersion && options.runtimeVersion !== state.version) {
      if (running) {
        throw new StartError(
          'preflight',
          'running',
          `Temps ${state.version} is running. Run \`temps stop\` before switching to ${options.runtimeVersion}.`,
        )
      }
      state = { ...state, version: options.runtimeVersion }
      await writeState(paths, state)
    }

    const current = state
    const bundled = scope === 'none' ? null : findBundledSkill()
    const skillsBase = scope === 'global' ? homedir() : cwd
    const [binary, , skills] = await Promise.all([
      steps.time(
        'Downloading Temps',
        () => ensureBinary(paths, current.version, target),
        (b) => (b.downloaded ? `Downloaded Temps ${current.version} (${Math.round(b.bytes / 1048576)} MB)` : `Temps ${current.version} ready`),
      ),
      running
        ? Promise.resolve(null)
        : steps.time(
            'Starting the database',
            () => ensureDatabase(current),
            (db) => (db.created ? `Database created${db.pulled ? ' (image downloaded)' : ''}` : 'Database started'),
          ),
      bundled
        ? steps.time(
            'Adding the Temps skill',
            () => installSkill(bundled, skillsBase),
            () => 'Added the Temps skill for Claude Code, Codex and OpenCode',
          )
        : Promise.resolve(null),
    ])

    const setupRan = !current.setupComplete
    if (setupRan) {
      await steps.time('Setting up Temps', () => runSetup(binary.path, current, paths), () => 'Temps set up')
      current.setupComplete = true
      await writeState(paths, current)
    }

    if (running) {
      steps.done('server', `Temps is already running (pid ${current.pid})`)
    } else {
      for (const [port, label] of [[current.proxyPort, 'apps and console'], [current.consolePort, 'console API']] as const) {
        if (!(await isPortFree(port))) {
          throw new StartError(
            'server',
            'port_taken',
            `Port ${port} (${label}) is in use by another program. Free it, or run \`temps stop --delete-data\` and start again to pick new ports.`,
          )
        }
      }
      await steps.time(
        'Starting Temps',
        async () => {
          const pid = await startServer(binary.path, current, paths)
          current.pid = pid
          await writeState(paths, current)
          await waitForServer(current, paths, pid)
          return pid
        },
        () => 'Temps is running',
      )
    }

    const cli = await steps.time(
      'Signing in the CLI',
      () => signInCli(binary.path, current, paths, options),
      (c) => (c.minted ? 'Created an API key and signed in the CLI' : 'CLI signed in'),
    )
    steps.stop()

    const urls = localUrls(current.proxyPort)
    const elapsed = Date.now() - started
    if (options.json) {
      console.log(
        JSON.stringify(
          {
            status: 'running',
            version: current.version,
            console_url: urls.console,
            api_url: consoleApiUrl(current.consolePort),
            app_url_pattern: urls.app('<project>'),
            context: LOCAL_CONTEXT,
            context_active: cli.active,
            api_key_created: cli.minted,
            admin_email: current.adminEmail,
            credentials_file: paths.state,
            skill: skills ? { scope, name: BUNDLED_SKILL, directories: skills } : { scope, name: null, directories: [] },
            elapsed_ms: elapsed,
          },
          null,
          2,
        ),
      )
      return
    }
    printSummary({ state: current, paths, urls, cli, skills, scope, bundled: Boolean(bundled), elapsed, firstRun: setupRan })
  } finally {
    steps.stop()
  }
}

function printSummary(input: {
  state: LocalState
  paths: LocalPaths
  urls: ReturnType<typeof localUrls>
  cli: { active: boolean; previous: string | null }
  /** Directories the Temps skill was installed into, or null if it was not. */
  skills: string[] | null
  scope: SkillsScope
  bundled: boolean
  elapsed: number
  firstRun: boolean
}): void {
  const { state, paths, urls, cli, skills, scope } = input
  const row = (label: string, value: string) => console.log(`  ${colors.muted(label.padEnd(9))} ${value}`)
  console.log()
  console.log(colors.success(`${icons.success} Temps is ready in ${formatSeconds(input.elapsed)}`))
  console.log()
  row('Console', urls.console)
  row('Sign in', input.firstRun ? `${state.adminEmail} / ${state.adminPassword}` : `${state.adminEmail} (password in ${paths.state})`)
  row(
    'CLI',
    cli.active
      ? `context "${LOCAL_CONTEXT}" is your default: try \`temps projects list\``
      : `context "${LOCAL_CONTEXT}" saved; "${cli.previous}" is still your default. Switch with \`temps context use ${LOCAL_CONTEXT}\`, or prefix one command (or your agent) with TEMPS_CONTEXT=${LOCAL_CONTEXT}`,
  )
  if (skills) {
    row('Agents', `the Temps skill, in ${skills.map((t) => t.replace(homedir(), '~')).join(' and ')}`)
  } else if (scope !== 'none' && !input.bundled) {
    row('Agents', `The Temps skill is not bundled in this build. Add it with \`bunx skills add gotempsh/temps --skill ${BUNDLED_SKILL}\``)
  }
  row('Apps', urls.app('<project>'))
  console.log()
  console.log(`  ${colors.bold('Next:')} cd into your app and run ${colors.bold('temps up')},`)
  console.log(`        or ask your agent: "Deploy this app to Temps"`)
  console.log(`  ${colors.muted('Stop it any time with')} temps stop`)
  if (input.firstRun) {
    console.log(colors.muted(`\n  The password is shown once. It is kept, readable only by you, in ${paths.state}.`))
  }
}

async function stopAction(options: StopOptions): Promise<void> {
  const paths = localPaths()
  await withLock(paths, 'stop', () => stopLocked(options, paths))
}

async function stopLocked(options: StopOptions, paths: LocalPaths): Promise<void> {
  // A reset must work on an install whose state file is unreadable: that is
  // what `readState` tells the user to run. Nothing below needs the saved
  // pid, so a missing state only costs the sandbox cleanup, which says so.
  let state: LocalState | null = null
  try {
    state = await readState(paths)
  } catch (error) {
    if (!options.deleteData) throw error
    console.log(colors.muted(`${(error as Error).message.split('. Run ')[0]}. Deleting it with everything else.`))
  }

  if (options.deleteData) {
    console.log(`This deletes your local Temps: the apps and services it runs (every container on the`)
    console.log(`${DOCKER_NETWORK} Docker network, and the volumes created for them), its sandboxes, the`)
    console.log(`${DB_CONTAINER} container, the ${DB_VOLUME} volume and the data in ${paths.root},`)
    console.log('including the admin password.')
    if (!options.yes) {
      if (!process.stdin.isTTY) {
        throw new StartError('stop', 'confirmation_required', 'Pass --yes to delete local data without a prompt.')
      }
      if (!(await promptConfirm({ message: 'Delete all local Temps data?', default: false }))) {
        console.log(colors.muted('Nothing was deleted, and nothing was stopped.'))
        return
      }
    }
  }

  // Sandboxes can only be attributed to this install by asking it, so that
  // happens while the server is still up.
  const sandboxes = options.deleteData ? await removeSandboxes(state) : null

  const stoppedServer = await stopServers(paths, state?.pid)
  let workloads: RemovedWorkloads | null = null
  if (options.deleteData) workloads = await removeWorkloads()
  let stoppedDb = false
  try {
    stoppedDb = await stopDatabase()
  } catch (error) {
    if (!options.deleteData) throw error
  }
  if (!options.deleteData && state?.pid !== undefined) await writeState(paths, { ...state, pid: undefined })
  console.log(stoppedServer ? `${colors.success('✔')} Temps stopped` : colors.muted('Temps was not running'))
  console.log(stoppedDb ? `${colors.success('✔')} Database stopped` : colors.muted('The database was not running'))

  if (!options.deleteData) {
    console.log(colors.muted('Start it again with `temps start`. Your apps and data are kept.'))
    return
  }
  await deleteLocalData(paths)
  // The context stays so `temps start` can reuse its name, but the projects
  // its default pointed at are gone.
  await clearContextDefaultProject(LOCAL_CONTEXT)
  if (workloads && (workloads.containers.length > 0 || workloads.volumes.length > 0)) {
    console.log(
      `${colors.success('✔')} Removed ${workloads.containers.length} app and service container(s) and ${workloads.volumes.length} volume(s)`,
    )
  }
  if (sandboxes?.destroyed.length) console.log(`${colors.success('✔')} Destroyed ${sandboxes.destroyed.length} sandbox(es)`)
  console.log(`${colors.success('✔')} Local Temps data deleted. The "${LOCAL_CONTEXT}" CLI context remains; \`temps start\` replaces its key.`)
  for (const kept of [...(workloads?.kept ?? []), ...(sandboxes?.notes ?? [])]) {
    console.log(colors.warning(`  Left in place: ${kept}`))
  }
}

/**
 * Destroy this install's sandboxes before its database goes. Needs the
 * running server and the `local` context's key; when either is missing it
 * cannot tell this install's sandbox containers from another Temps', so it
 * removes none and reports any sandbox containers present on the engine.
 */
async function removeSandboxes(state: LocalState | null): Promise<{ destroyed: string[]; notes: string[] }> {
  const context = await getContext(LOCAL_CONTEXT)
  const apiUrl = state ? consoleApiUrl(state.consolePort) : null
  if (state && apiUrl && context?.url === apiUrl && (await serverHealthy(state.consolePort))) {
    const result = await destroySandboxes(apiUrl, context.apiKey)
    return { destroyed: result.destroyed, notes: result.failed.map((entry) => `sandbox ${entry}`) }
  }
  const present = await sandboxContainers()
  if (present.length === 0) return { destroyed: [], notes: [] }
  return {
    destroyed: [],
    notes: [
      `${present.length} sandbox container(s) on this Docker engine (${present.slice(0, 5).join(', ')}${present.length > 5 ? ', …' : ''}). ` +
        'Temps was not running, so they could not be matched to this install. Remove the ones you created here with `docker rm -f`.',
    ],
  }
}

export function registerStartCommands(program: Command): void {
  program
    .command('start')
    .description('Run Temps on this computer, sign in the CLI and add the Temps skill for your coding agent')
    .option('--channel <channel>', 'Release channel for a fresh install: stable, beta or nightly', 'beta')
    .option('--runtime-version <tag>', 'Run this exact Temps release')
    .option('--skills <scope>', 'Where to add the Temps agent skill: project, global or none (default: project inside a project, otherwise global)')
    .option('--port <port>', 'Port for your apps and the console on a fresh install (default: 8080 or the next free port)')
    .option('--make-default', 'Make "local" the default CLI context even if another one is active')
    .option('--json', 'Print the result as JSON, for scripts and agents')
    .addHelpText(
      'after',
      '\nNeeds Docker (Docker Desktop, OrbStack or Colima). Everything listens on 127.0.0.1 only.\n' +
        'State, logs and the admin password live in ~/.temps/local. The API key is stored in the "local" CLI context.\n',
    )
    .action(startAction)

  program
    .command('stop')
    .description('Stop the Temps server and database started by `temps start`')
    .option('--delete-data', 'Also delete everything in your local Temps: its apps, services, sandboxes, database and admin password (asks first)')
    .option('-y, --yes', 'Do not ask before deleting data')
    .action(stopAction)
}
