// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from 'node:child_process'
import * as os from 'node:os'
import * as path from 'node:path'
import {
  bridgeKeyFilePath,
  bridgeUrlFromText,
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
  private getServerEntry(binary: string): string | null {
    try {
      return execFileSync(binary, ['mcp', 'get', MCP_SERVER_NAME], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: this.env,
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
      })
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      // Codex kept whatever it had, so the key file goes back to what that
      // entry reads: a refused install never changes the key it sends.
      if (keyFile) await restoreBridgeKeyFile(keyFile, previousKey).catch(() => undefined)
      if (/already (installed|exists|added|registered)/i.test(reason)) {
        const installed = await this.getServerUrl()
        if (sameOrigin(installed, entry.url)) return { success: true, alreadyInstalled: true }
        const where = installed ? ` for ${new URL(installed).origin}` : ''
        return {
          success: false,
          reason: `Codex already has a ${MCP_SERVER_NAME} server${where}. Run temps mcp remove codex, then add it again.`,
        }
      }
      return { success: false, reason }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The codex CLI was not found on PATH.' }
    try {
      execFileSync(binary, ['mcp', 'remove', MCP_SERVER_NAME], { stdio: ['ignore', 'pipe', 'pipe'], env: this.env })
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
