// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from 'node:child_process'
import * as os from 'node:os'
import * as path from 'node:path'
import {
  bridgeKeyFilePath,
  bridgeUrlFromArgs,
  bridgeUrlFromText,
  readKeyFile,
  localBridgeCommand,
  MCP_SERVER_NAME,
  redactSecrets,
  removeBridgeKeyFiles,
  restoreBridgeKeyFile,
  sameOrigin,
  writeBridgeKeyFile,
  type CliAdapterOptions,
  type InstallResult,
  type McpClientAdapter,
  type McpServerEntry,
} from './base.js'
import { execErrorMessage, resolveOnPath } from './exec-utils.js'

/**
 * Codex resolves this env var from its OWN process environment at MCP
 * connect time (confirmed via `codex mcp add --help`: "Optional environment
 * variable to read for a bearer token"), not from the environment of the
 * `codex mcp add` subprocess this adapter shells out to below -- config.toml
 * only ever stores the variable's *name* (`bearer_token_env_var = "..."`),
 * never its value. That means setting it just for this subprocess call does
 * nothing for the real `codex` sessions the user launches afterward; it must
 * be exported in the user's shell profile. See index.ts's addAction, which
 * prints that instruction for this adapter specifically after a successful
 * add.
 */
export const TOKEN_ENV_VAR = 'TEMPS_MCP_AUTH_HEADER'

/**
 * Pulls the connection URL out of `codex mcp get <name>` stdout. Pure and
 * exported so the parsing logic is unit-testable without shelling out to a
 * real `codex` binary or mocking `execFileSync`.
 */
export function parseCodexMcpGetOutput(output: string): string | null {
  const match = output.match(/^\s*url:\s*(\S+)/m)
  return match?.[1] ?? bridgeUrlFromText(output)
}

const CODEX_ID = 'codex'

/** What `codex mcp get --json` says of an entry, as far as Temps reads it. */
export interface CodexJsonEntry {
  transport?: {
    type?: string
    args?: unknown
    env?: Record<string, unknown> | null
    cwd?: unknown
  }
}

/** Longest a `codex` command may run before the CLI gives up on it. */
const CODEX_TIMEOUT_MS = 30_000

/** `codex mcp get --json`'s answer, or null where this Codex has none (an older one) or it is not JSON. */
export function parseCodexJsonEntry(text: string | null): CodexJsonEntry | null {
  if (!text) return null
  try {
    const value: unknown = JSON.parse(text)
    return value && typeof value === 'object' && !Array.isArray(value) ? (value as CodexJsonEntry) : null
  } catch {
    return null
  }
}

/** A bridge entry's arguments as Codex stores them, one by one; null for any other entry. */
function codexBridgeArgs(json: CodexJsonEntry): string[] | null {
  const args = json.transport?.args
  if (!Array.isArray(args) || !args.every((a): a is string => typeof a === 'string')) return null
  return bridgeUrlFromArgs(args) === null ? null : args
}

/**
 * The key a bridge entry sends, from what Codex stores for it: the file its
 * TEMPS_API_KEY_FILE names (as the bridge reads it, wherever the user
 * pointed it), else an inline TEMPS_API_KEY. A relative file is read from
 * the entry's working folder, where the bridge runs; without one it cannot
 * be told which file the bridge reads. `undefined` for anything else.
 */
export function codexBridgeKey(json: CodexJsonEntry): string | undefined {
  if (!codexBridgeArgs(json)) return undefined
  const env = json.transport?.env ?? {}
  const file = typeof env.TEMPS_API_KEY_FILE === 'string' ? env.TEMPS_API_KEY_FILE.trim() : ''
  if (file) {
    if (path.isAbsolute(file)) return readKeyFile(file)
    const cwd = json.transport?.cwd
    return typeof cwd === 'string' && path.isAbsolute(cwd) ? readKeyFile(path.join(cwd, file)) : undefined
  }
  const inline = env.TEMPS_API_KEY
  return typeof inline === 'string' && inline.trim() ? inline.trim() : undefined
}

/**
 * Whether the entry `codex mcp get` printed is exactly the one asked for:
 * the same transport and endpoint, and for a bridge the same command (CLI
 * version, URL, folders, write mode). Only then is a refused duplicate
 * already installed; a bridge allowing other folders is not.
 */
export function codexEntryMatches(installed: string, entry: McpServerEntry, json?: CodexJsonEntry | null): boolean {
  // Its arguments one by one, where Codex gives them: exact, whatever a
  // folder's name holds.
  if (json && entry.bridge) {
    const args = codexBridgeArgs(json)
    if (!args) return false
    const command = localBridgeCommand(entry, entry.bridge).args
    const wanted = command.slice(command.indexOf(entry.bridge.packageSpec))
    const at = args.indexOf(entry.bridge.packageSpec)
    return at !== -1 && JSON.stringify(args.slice(at)) === JSON.stringify(wanted)
  }
  const bridgeUrl = bridgeUrlFromText(installed)
  if (!entry.bridge) return bridgeUrl === null && parseCodexMcpGetOutput(installed) === entry.url
  if (bridgeUrl === null) return false
  const command = localBridgeCommand(entry, entry.bridge).args
  // Codex prints the arguments on one line, joined by spaces: from the
  // package on, that line holds exactly these, so a folder with a space
  // in it compares whole, and another folder or option is no match.
  // A folder whose own name reads as another --folder would make two
  // setups print the same line: never taken for the one installed.
  if (entry.bridge.folders.some((folder) => /(^|\s)--folder(\s|$)/.test(folder))) return false
  const wanted = command.slice(command.indexOf(entry.bridge.packageSpec)).join(' ')
  return installed.split('\n').some((line) => {
    const at = line.indexOf(entry.bridge?.packageSpec ?? '')
    return at !== -1 && line.slice(at).trimEnd() === wanted
  })
}

/**
 * The `codex mcp add` arguments for an entry. Over HTTP, config.toml names
 * the environment variable that holds the token (see TOKEN_ENV_VAR). The
 * bridge runs over stdio, and Codex starts it with only the variables its
 * entry sets; `--env` takes them as arguments, visible in the process list
 * while `codex mcp add` runs, so the entry names the private key file
 * (`bridgeKeyFilePath`) that `addServer` writes, never the key itself.
 */
export function codexAddArgs(
  entry: McpServerEntry,
  keyFile: string = bridgeKeyFilePath(CODEX_ID, entry.url, codexHome()),
): string[] {
  if (entry.bridge) {
    const command = localBridgeCommand(entry, entry.bridge, keyFile)
    return [
      'mcp',
      'add',
      MCP_SERVER_NAME,
      ...Object.entries(command.env).flatMap(([name, value]) => ['--env', `${name}=${value}`]),
      '--',
      command.command,
      ...command.args,
    ]
  }
  return ['mcp', 'add', MCP_SERVER_NAME, '--url', entry.url, '--bearer-token-env-var', TOKEN_ENV_VAR]
}

/** Codex's config directory: CODEX_HOME, one per profile, or ~/.codex. */
export function codexHome(env: NodeJS.ProcessEnv = process.env, home: string = os.homedir()): string {
  return path.resolve(env.CODEX_HOME || path.join(home, '.codex'))
}

/** Codex's settings file. */
export function codexConfigPath(env: NodeJS.ProcessEnv = process.env, home: string = os.homedir()): string {
  return path.join(codexHome(env, home), 'config.toml')
}

// Same rationale as Claude Code: shell out to the official `codex` CLI so
// Codex's own config format (config.toml) is never hand-written here.
export class CodexAdapter implements McpClientAdapter {
  readonly id = CODEX_ID
  readonly label = 'Codex'
  private binary: string | null | undefined

  constructor(private readonly options: CliAdapterOptions = {}) {}

  private get home(): string {
    return this.options.home ?? os.homedir()
  }

  private get env(): NodeJS.ProcessEnv {
    return this.options.env ?? process.env
  }

  /** Each CODEX_HOME is its own profile, so each keeps its own key files. */
  private profile(): string {
    return codexHome(this.env, this.home)
  }

  private keyFile(entry: McpServerEntry): string {
    return bridgeKeyFilePath(CODEX_ID, entry.url, this.profile(), this.home)
  }

  private findBinary(): string | null {
    if (this.options.binary) return this.options.binary
    if (this.binary !== undefined) return this.binary
    this.binary = resolveOnPath('codex')
    return this.binary
  }

  async isClientSupported(): Promise<boolean> {
    return this.findBinary() !== null
  }

  // Same rationale as Claude Code: `mcp get <name>` exits non-zero when
  // absent, instead of grepping `mcp list` output for MCP_SERVER_NAME, which
  // false-positives on any unrelated server whose URL merely contains
  // "temps" (e.g. a server hosted at some-app.temps.example.com).
  /** The entry as `codex mcp get --json` gives it; null from a Codex without it. */
  private getServerJson(binary: string): CodexJsonEntry | null {
    try {
      return parseCodexJsonEntry(
        execFileSync(binary, ['mcp', 'get', MCP_SERVER_NAME, '--json'], {
          stdio: ['ignore', 'pipe', 'pipe'],
          env: this.env,
          timeout: CODEX_TIMEOUT_MS,
        }).toString(),
      )
    } catch {
      return null
    }
  }

  private getServerEntry(binary: string): string | null {
    try {
      return execFileSync(binary, ['mcp', 'get', MCP_SERVER_NAME], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: this.env,
        timeout: CODEX_TIMEOUT_MS,
      }).toString()
    } catch {
      return null
    }
  }

  async isServerInstalled(): Promise<boolean> {
    const binary = this.findBinary()
    if (!binary) return false
    return this.getServerEntry(binary) !== null
  }

  /**
   * The key a bridge entry reads from its key file. An HTTP entry's token
   * is in the user's environment (TEMPS_MCP_AUTH_HEADER), not here, so
   * none is known: an old key file left beside it is not what it sends.
   */
  async installedApiKey(): Promise<string | undefined> {
    const binary = this.findBinary()
    // The file the entry names, which the user may have pointed elsewhere:
    // the key in the file Temps wrote is then no longer the one sent. An
    // older Codex without --json masks the entry's settings, so which file
    // it reads, and the key it sends, is not known.
    const json = binary ? this.getServerJson(binary) : null
    return json ? codexBridgeKey(json) : undefined
  }

  async getServerUrl(): Promise<string | null> {
    const binary = this.findBinary()
    if (!binary) return null
    const entry = this.getServerEntry(binary)
    if (!entry) return null
    return parseCodexMcpGetOutput(entry)
  }

  async addServer(entry: McpServerEntry): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The codex CLI was not found on PATH.' }
    const keyFile = entry.bridge ? this.keyFile(entry) : undefined
    let previousKey: string | null = null
    if (keyFile) {
      try {
        previousKey = await writeBridgeKeyFile(keyFile, entry.apiKey)
      } catch (error) {
        return {
          success: false,
          reason: `Could not save the API key to ${keyFile} (${redactSecrets(execErrorMessage(error))}).`,
        }
      }
    }
    try {
      execFileSync(binary, codexAddArgs(entry, keyFile), {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...this.env, [TOKEN_ENV_VAR]: `Bearer ${entry.apiKey}` },
        timeout: CODEX_TIMEOUT_MS,
      })
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      // Codex kept whatever it had, so the key file goes back to what that
      // entry reads: a refused install never changes the key it sends.
      if (keyFile) await restoreBridgeKeyFile(keyFile, previousKey, entry.apiKey).catch(() => undefined)
      if (/already (installed|exists|added|registered)/i.test(reason)) {
        const text = this.getServerEntry(binary)
        if (text && codexEntryMatches(text, entry, this.getServerJson(binary))) {
          return { success: true, alreadyInstalled: true }
        }
        const installed = text ? parseCodexMcpGetOutput(text) : null
        const where = installed && URL.canParse(installed) ? ` for ${new URL(installed).origin}` : ''
        const other = sameOrigin(installed, entry.url) ? ', set up differently (other folders or transport)' : ''
        return {
          success: false,
          reason: `Codex already has a ${MCP_SERVER_NAME} server${where}${other}. Run temps mcp remove codex, then add it again.`,
        }
      }
      return { success: false, reason }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The codex CLI was not found on PATH.' }
    try {
      execFileSync(binary, ['mcp', 'remove', MCP_SERVER_NAME], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: this.env,
        timeout: CODEX_TIMEOUT_MS,
      })
      await removeBridgeKeyFiles(CODEX_ID, this.profile(), this.home)
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/not found|no such/i.test(reason)) {
        await removeBridgeKeyFiles(CODEX_ID, this.profile(), this.home).catch(() => undefined)
        return { success: true, alreadyInstalled: true }
      }
      return { success: false, reason }
    }
  }

  async describeTarget(): Promise<string> {
    return `codex mcp add ${MCP_SERVER_NAME} (${codexConfigPath(this.env, this.home)})`
  }
}
