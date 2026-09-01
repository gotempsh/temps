// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { execFileSync } from 'node:child_process'
import { MCP_SERVER_NAME, redactSecrets, type InstallResult, type McpClientAdapter, type McpServerEntry } from './base.js'
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
  return match?.[1] ?? null
}

// Same rationale as Claude Code: shell out to the official `codex` CLI so
// Codex's own config format (config.toml) is never hand-written here.
export class CodexAdapter implements McpClientAdapter {
  readonly id = 'codex'
  readonly label = 'Codex'
  private binary: string | null | undefined

  private findBinary(): string | null {
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
    return parseCodexMcpGetOutput(entry)
  }

  async addServer(entry: McpServerEntry): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The codex CLI was not found on PATH.' }
    try {
      execFileSync(
        binary,
        ['mcp', 'add', MCP_SERVER_NAME, '--url', entry.url, '--bearer-token-env-var', TOKEN_ENV_VAR],
        {
          stdio: ['ignore', 'pipe', 'pipe'],
          env: { ...process.env, [TOKEN_ENV_VAR]: `Bearer ${entry.apiKey}` },
        },
      )
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/already (installed|exists|added|registered)/i.test(reason)) {
        return { success: true, alreadyInstalled: true }
      }
      return { success: false, reason }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const binary = this.findBinary()
    if (!binary) return { success: false, reason: 'The codex CLI was not found on PATH.' }
    try {
      execFileSync(binary, ['mcp', 'remove', MCP_SERVER_NAME], { stdio: ['ignore', 'pipe', 'pipe'] })
      return { success: true }
    } catch (error) {
      const reason = redactSecrets(execErrorMessage(error))
      if (/not found|no such/i.test(reason)) return { success: true, alreadyInstalled: true }
      return { success: false, reason }
    }
  }

  async describeTarget(): Promise<string> {
    return `codex mcp add ${MCP_SERVER_NAME} --url <url> --bearer-token-env-var ${TOKEN_ENV_VAR}`
  }
}
