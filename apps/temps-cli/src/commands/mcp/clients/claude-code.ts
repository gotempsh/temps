// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as fs from 'node:fs'
import * as os from 'node:os'
import * as path from 'node:path'
import { execFileSync } from 'node:child_process'
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
 * Pulls the connection URL out of `claude mcp get <name>` stdout. Pure and
 * exported so the parsing logic is unit-testable without shelling out to a
 * real `claude` binary or mocking `execFileSync`.
 */
export function parseClaudeCodeMcpGetOutput(output: string): string | null {
  const match = output.match(/^\s*URL:\s*(\S+)/m)
  return match?.[1] ?? bridgeUrlFromText(output)
}

const CLAUDE_CODE_ID = 'claude-code'

/**
 * The `claude mcp add` arguments for an entry: the HTTP endpoint, or the
 * bridge over stdio when it deploys local folders. The name comes before
 * `-e`, which takes several values, and `--` ends the options. `-e` values
 * are arguments, visible in the process list while `claude mcp add` runs, so
 * the bridge entry names the private key file (`bridgeKeyFilePath`) that
 * `addServer` writes, never the key itself.
 */
export function claudeCodeAddArgs(
  entry: McpServerEntry,
  keyFile: string = bridgeKeyFilePath(CLAUDE_CODE_ID, entry.url),
): string[] {
  if (entry.bridge) {
    const command = localBridgeCommand(entry, entry.bridge, keyFile)
    return [
      'mcp',
      'add',
      '--scope',
      'user',
      MCP_SERVER_NAME,
      ...Object.entries(command.env).flatMap(([name, value]) => ['-e', `${name}=${value}`]),
      '--',
      command.command,
      ...command.args,
    ]
  }
  return [
    'mcp',
    'add',
    '--transport',
    'http',
    '--scope',
    'user',
    MCP_SERVER_NAME,
    entry.url,
    '--header',
    `Authorization: Bearer ${entry.apiKey}`,
  ]
}

// Claude Code owns its own config format/location, so this shells out to the
// `claude` CLI (same as PostHog's installer) instead of hand-writing a JSON
// file -- that survives Claude Code changing its config shape underneath us.
export class ClaudeCodeAdapter implements McpClientAdapter {
  readonly id = CLAUDE_CODE_ID
  readonly label = 'Claude Code'
  private binary: string | null | undefined

  constructor(private readonly options: CliAdapterOptions = {}) {}

  private get home(): string {
    return this.options.home ?? os.homedir()
  }

  private get env(): NodeJS.ProcessEnv {
    return this.options.env ?? process.env
  }

  /** Claude Code keeps one user configuration per config directory; key files are kept apart the same way. */
  private profile(): string {
    return this.env.CLAUDE_CONFIG_DIR ? path.resolve(this.env.CLAUDE_CONFIG_DIR) : ''
  }

  private keyFile(entry: McpServerEntry): string {
    return bridgeKeyFilePath(CLAUDE_CODE_ID, entry.url, this.profile(), this.home)
  }

  private findBinary(): string | null {
    if (this.options.binary) return this.options.binary
    if (this.binary !== undefined) return this.binary
    const candidates = [
      path.join(this.home, '.claude', 'local', 'claude'),
      '/usr/local/bin/claude',
      '/opt/homebrew/bin/claude',
    ]
    for (const candidate of candidates) {
      if (fs.existsSync(candidate)) {
        this.binary = candidate
        return candidate
      }
    }
    this.binary = resolveOnPath('claude')
    return this.binary
  }

  async isClientSupported(): Promise<boolean> {
    return this.findBinary() !== null
  }

  // `mcp get <name>` exits non-zero when the name isn't configured, unlike
  // grepping `mcp list` output for MCP_SERVER_NAME -- a substring match
  // there false-positives on any unrelated server whose URL merely contains
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
    return parseClaudeCodeMcpGetOutput(entry)
  }

  async addServer(entry: McpServerEntry): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The claude CLI was not found on PATH.' }
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
      // The claude CLI's HTTP transport only accepts headers as a literal
      // --header value (no env-var-substitution flag, unlike Codex's
      // --bearer-token-env-var below) -- confirmed against `claude mcp add
      // --help`, which offers -e/--env only for stdio servers. This means the
      // key is briefly visible in this process's argv (e.g. /proc/<pid>/cmdline
      // on Linux) for the duration of the exec call. Tracked as a known
      // upstream limitation rather than worked around with a fragile hack.
      execFileSync(binary, claudeCodeAddArgs(entry, keyFile), { stdio: ['ignore', 'pipe', 'pipe'], env: this.env })
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      // Claude kept whatever it had, so the key file goes back to what that
      // entry reads: a refused install never changes the key it sends.
      if (keyFile) await restoreBridgeKeyFile(keyFile, previousKey).catch(() => undefined)
      if (/already exists/i.test(reason)) {
        const installed = await this.getServerUrl()
        if (sameOrigin(installed, entry.url)) return { success: true, alreadyInstalled: true }
        const where = installed ? ` for ${new URL(installed).origin}` : ''
        return {
          success: false,
          reason: `Claude Code already has a ${MCP_SERVER_NAME} server${where}. Run temps mcp remove claude-code, then add it again.`,
        }
      }
      return { success: false, reason }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The claude CLI was not found on PATH.' }
    try {
      execFileSync(binary, ['mcp', 'remove', '--scope', 'user', MCP_SERVER_NAME], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env: this.env,
      })
      await removeBridgeKeyFiles(CLAUDE_CODE_ID, this.profile(), this.home)
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/no such|not found/i.test(reason)) {
        await removeBridgeKeyFiles(CLAUDE_CODE_ID, this.profile(), this.home).catch(() => undefined)
        return { success: true, alreadyInstalled: true }
      }
      return { success: false, reason }
    }
  }

  async describeTarget(): Promise<string> {
    return `claude mcp add --scope user ${MCP_SERVER_NAME} (Claude Code's user settings)`
  }
}
