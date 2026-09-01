// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as fs from 'node:fs'
import * as os from 'node:os'
import * as path from 'node:path'
import { execFileSync } from 'node:child_process'
import { MCP_SERVER_NAME, redactSecrets, type InstallResult, type McpClientAdapter, type McpServerEntry } from './base.js'
import { execErrorMessage, resolveOnPath } from './exec-utils.js'

/**
 * Pulls the connection URL out of `claude mcp get <name>` stdout. Pure and
 * exported so the parsing logic is unit-testable without shelling out to a
 * real `claude` binary or mocking `execFileSync`.
 */
export function parseClaudeCodeMcpGetOutput(output: string): string | null {
  const match = output.match(/^\s*URL:\s*(\S+)/m)
  return match?.[1] ?? null
}

// Claude Code owns its own config format/location, so this shells out to the
// `claude` CLI (same as PostHog's installer) instead of hand-writing a JSON
// file -- that survives Claude Code changing its config shape underneath us.
export class ClaudeCodeAdapter implements McpClientAdapter {
  readonly id = 'claude-code'
  readonly label = 'Claude Code'
  private binary: string | null | undefined

  private findBinary(): string | null {
    if (this.binary !== undefined) return this.binary
    const candidates = [
      path.join(os.homedir(), '.claude', 'local', 'claude'),
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
      return execFileSync(binary, ['mcp', 'get', MCP_SERVER_NAME], { stdio: ['ignore', 'pipe', 'pipe'] }).toString()
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
    try {
      // The claude CLI's HTTP transport only accepts headers as a literal
      // --header value (no env-var-substitution flag, unlike Codex's
      // --bearer-token-env-var below) -- confirmed against `claude mcp add
      // --help`, which offers -e/--env only for stdio servers. This means the
      // key is briefly visible in this process's argv (e.g. /proc/<pid>/cmdline
      // on Linux) for the duration of the exec call. Tracked as a known
      // upstream limitation rather than worked around with a fragile hack.
      execFileSync(
        binary,
        [
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
        ],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      )
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/already exists/i.test(reason)) return { success: true, alreadyInstalled: true }
      return { success: false, reason }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The claude CLI was not found on PATH.' }
    try {
      execFileSync(binary, ['mcp', 'remove', '--scope', 'user', MCP_SERVER_NAME], {
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/no such|not found/i.test(reason)) return { success: true, alreadyInstalled: true }
      return { success: false, reason }
    }
  }

  async describeTarget(): Promise<string> {
    return `claude mcp add --transport http --scope user ${MCP_SERVER_NAME} <url>`
  }
}
