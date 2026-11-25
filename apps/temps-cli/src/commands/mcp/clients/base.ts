// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as fs from 'node:fs'
import * as path from 'node:path'
// jsonc-parser's package.json `main` points at a UMD build whose dynamic
// require() of sibling files (e.g. `./impl/format`) doesn't survive Bun's
// single-file bundle -- those files never get inlined, so Node throws
// MODULE_NOT_FOUND at runtime. The ESM build uses static imports Bun can
// inline correctly, so import it directly rather than via the package root.
import * as jsonc from 'jsonc-parser/lib/esm/main.js'

export interface McpServerEntry {
  url: string
  apiKey: string
  /**
   * Run the Temps bridge on this computer (`temps mcp bridge`) instead of
   * connecting to `url`: the app starts it over stdio, and it adds the
   * `temps_deploy_folder` tool for these folders.
   */
  bridge?: LocalBridge
}

export interface LocalBridge {
  /** Full paths of the folders the agent may deploy from. */
  folders: string[]
  /** The npm package that provides `temps mcp bridge`, pinned to a version. */
  packageSpec: string
}

/** How an app starts the bridge. The key is in `env`, never in `args`, so it stays out of the process list. */
export interface LocalBridgeCommand {
  command: string
  args: string[]
  env: Record<string, string>
}

export function localBridgeCommand(entry: McpServerEntry, bridge: LocalBridge): LocalBridgeCommand {
  return {
    command: 'npx',
    args: [
      '-y',
      bridge.packageSpec,
      'mcp',
      'bridge',
      '--url',
      new URL(entry.url).origin,
      ...bridge.folders.flatMap((folder) => ['--folder', folder]),
    ],
    env: { TEMPS_API_KEY: entry.apiKey },
  }
}

/**
 * The MCP URL a bridge entry connects to, from its command line: the bridge
 * always asks for the write tools. Null when the arguments are not a bridge.
 */
export function bridgeUrlFromArgs(args: unknown): string | null {
  if (!Array.isArray(args)) return null
  const bridgeAt = args.findIndex((arg, i) => arg === 'bridge' && args[i - 1] === 'mcp')
  const urlAt = args.indexOf('--url')
  const url = urlAt === -1 ? undefined : args[urlAt + 1]
  if (bridgeAt === -1 || typeof url !== 'string') return null
  try {
    const mcp = new URL('/mcp', new URL(url).origin)
    mcp.searchParams.set('write', '1')
    return mcp.toString()
  } catch {
    return null
  }
}

/** The same, from the text an app's `mcp get` prints for a bridge entry. */
export function bridgeUrlFromText(text: string): string | null {
  const match = text.match(/\bmcp\s+bridge\b[\s\S]*?--url\s+(\S+)/)
  return match?.[1] ? bridgeUrlFromArgs(['mcp', 'bridge', '--url', match[1]]) : null
}

export interface InstallResult {
  success: boolean
  alreadyInstalled?: boolean
  reason?: string
}

/** The server name written into every client's config file/registry. */
export const MCP_SERVER_NAME = 'temps'

/** Strips an Authorization header value before it can reach a log, screen, or thrown error. */
export function redactSecrets(text: string): string {
  return text.replace(/Bearer\s+\S+/gi, 'Bearer ***').replace(/Authorization:\s*\S+(\s+\S+)?/gi, 'Authorization: ***')
}

export interface McpClientAdapter {
  readonly id: string
  readonly label: string
  /** Best-effort detection of whether this client is installed on the machine. Advisory only -- add proceeds either way. */
  isClientSupported(): Promise<boolean>
  isServerInstalled(): Promise<boolean>
  /**
   * Best-effort: the connection URL this client currently has configured for
   * the Temps server, or null if not installed / not determinable. `mcp
   * status` parses this to show which tool groups and write mode are active
   * -- see parseMcpUrl in ../groups.ts.
   */
  getServerUrl(): Promise<string | null>
  addServer(entry: McpServerEntry): Promise<InstallResult>
  removeServer(): Promise<InstallResult>
  /** Human-readable description of what `addServer`/`removeServer` will change, for the pre-write confirmation prompt. */
  describeTarget(): Promise<string>
}

/**
 * Default adapter for clients that store MCP servers as a JSON(C) config
 * file with a `<propertyName>.temps` entry. Uses jsonc-parser so an existing
 * file's comments and formatting survive the edit, and diffs the entry
 * before writing so a re-run reports "already installed" instead of
 * rewriting an identical file.
 */
export abstract class JsonConfigMcpClientAdapter implements McpClientAdapter {
  abstract readonly id: string
  abstract readonly label: string

  protected abstract getConfigPath(): string
  protected abstract getServerPropertyName(): string
  protected abstract buildServerConfig(entry: McpServerEntry): Record<string, unknown>
  /** The entry that starts the bridge over stdio. Most apps take a command, its arguments and environment. */
  protected buildBridgeConfig(command: LocalBridgeCommand): Record<string, unknown> {
    return { command: command.command, args: command.args, env: command.env }
  }
  /** Pulls the connection URL back out of a config entry previously built by buildServerConfig. */
  protected abstract extractUrl(serverConfig: Record<string, unknown>): string | null

  async isClientSupported(): Promise<boolean> {
    return true
  }

  private async readServerConfig(): Promise<Record<string, unknown> | null> {
    const configPath = this.getConfigPath()
    if (!fs.existsSync(configPath)) return null
    try {
      const content = await fs.promises.readFile(configPath, 'utf8')
      const config = jsonc.parse(content) as Record<string, any> | undefined
      const prop = this.getServerPropertyName()
      return config?.[prop]?.[MCP_SERVER_NAME] ?? null
    } catch {
      return null
    }
  }

  async isServerInstalled(): Promise<boolean> {
    return (await this.readServerConfig()) !== null
  }

  async getServerUrl(): Promise<string | null> {
    const serverConfig = await this.readServerConfig()
    if (!serverConfig) return null
    return bridgeUrlFromArgs(serverConfig.args) ?? this.extractUrl(serverConfig)
  }

  async addServer(entry: McpServerEntry): Promise<InstallResult> {
    const configPath = this.getConfigPath()
    try {
      await fs.promises.mkdir(path.dirname(configPath), { recursive: true })

      let content = ''
      if (fs.existsSync(configPath)) {
        // Tighten permissions BEFORE writing the new credential into this
        // file, not after: a config that predates this fix (or was created
        // by another tool) may still be at a looser mode like 0o644, and
        // `mode` on writeFile is ignored for an existing file -- it does not
        // chmod. Restricting first means the bearer key is never written to
        // a file that's world/group-readable, even for an instant.
        await fs.promises.chmod(configPath, 0o600)
        content = await fs.promises.readFile(configPath, 'utf8')
      }

      const prop = this.getServerPropertyName()
      const existing = (jsonc.parse(content || '{}') ?? {}) as Record<string, any>
      const newEntry = entry.bridge
        ? this.buildBridgeConfig(localBridgeCommand(entry, entry.bridge))
        : this.buildServerConfig(entry)
      const currentEntry = existing?.[prop]?.[MCP_SERVER_NAME]

      if (currentEntry !== undefined && JSON.stringify(currentEntry) === JSON.stringify(newEntry)) {
        return { success: true, alreadyInstalled: true }
      }

      const edits = jsonc.modify(content, [prop, MCP_SERVER_NAME], newEntry, {
        formattingOptions: { tabSize: 2, insertSpaces: true },
      })
      const updated = jsonc.applyEdits(content, edits)
      // Brand-new file: pass `mode` so open()'s O_CREAT path creates it at
      // 0o600 atomically -- there is no window where the credential sits on
      // disk at a looser mode. The chmod afterward is a cheap no-op in that
      // case, and a safety net in general (e.g. a platform where `mode` on
      // writeFile isn't fully honored).
      await fs.promises.writeFile(configPath, updated, { encoding: 'utf8', mode: 0o600 })
      await fs.promises.chmod(configPath, 0o600)
      return { success: true }
    } catch (error) {
      return { success: false, reason: redactSecrets(error instanceof Error ? error.message : String(error)) }
    }
  }

  async removeServer(): Promise<InstallResult> {
    const configPath = this.getConfigPath()
    try {
      if (!fs.existsSync(configPath)) return { success: true, alreadyInstalled: true }

      // Same rationale as addServer: tighten before reading/writing, not
      // after -- other MCP servers' credentials may already be in this file,
      // and rewriting it should never happen at a looser mode than 0o600.
      await fs.promises.chmod(configPath, 0o600)

      const content = await fs.promises.readFile(configPath, 'utf8')
      const prop = this.getServerPropertyName()
      const existing = jsonc.parse(content) as Record<string, any> | undefined
      if (!existing?.[prop]?.[MCP_SERVER_NAME]) {
        return { success: true, alreadyInstalled: true }
      }

      const edits = jsonc.modify(content, [prop, MCP_SERVER_NAME], undefined, {
        formattingOptions: { tabSize: 2, insertSpaces: true },
      })
      const updated = jsonc.applyEdits(content, edits)
      await fs.promises.writeFile(configPath, updated, { encoding: 'utf8', mode: 0o600 })
      await fs.promises.chmod(configPath, 0o600)
      return { success: true }
    } catch (error) {
      return { success: false, reason: redactSecrets(error instanceof Error ? error.message : String(error)) }
    }
  }

  async describeTarget(): Promise<string> {
    return `${this.getConfigPath()} (${this.getServerPropertyName()}.${MCP_SERVER_NAME})`
  }
}
