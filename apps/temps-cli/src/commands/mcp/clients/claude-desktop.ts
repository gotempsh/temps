// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

// Claude Desktop only speaks stdio MCP, so the entry bridges through the
// `mcp-remote` npm package (spawned via npx) rather than connecting to the
// HTTP endpoint directly. Requires Node.js on the user's machine.
export class ClaudeDesktopAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'claude-desktop'
  readonly label = 'Claude Desktop'

  protected getConfigPath(): string {
    const home = os.homedir()
    if (process.platform === 'darwin') {
      return path.join(home, 'Library', 'Application Support', 'Claude', 'claude_desktop_config.json')
    }
    if (process.platform === 'win32') {
      return path.join(process.env.APPDATA || '', 'Claude', 'claude_desktop_config.json')
    }
    throw new Error('Claude Desktop is only available on macOS and Windows')
  }

  protected getServerPropertyName(): string {
    return 'mcpServers'
  }

  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    // mcp-remote substitutes ${AUTH_HEADER} from the spawned process's env at
    // runtime, so the key itself never appears in args (and therefore never
    // shows up in `ps`/`/proc/<pid>/cmdline` on Linux) -- only the config
    // file on disk holds it, and that file is now written with mode 0o600.
    return {
      command: 'npx',
      args: ['-y', 'mcp-remote@latest', entry.url, '--header', 'Authorization:${AUTH_HEADER}'],
      env: { AUTH_HEADER: `Bearer ${entry.apiKey}` },
    }
  }

  // Only recognizes an entry this adapter itself wrote (the URL is a bare
  // arg to mcp-remote). An entry from the old @temps-sdk/mcp package encodes
  // the connection via env vars and --tools instead -- returns null there,
  // same as any other config this adapter doesn't understand.
  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    const args = serverConfig.args
    if (!Array.isArray(args)) return null
    const url = args.find((arg) => typeof arg === 'string' && /^https?:\/\//.test(arg))
    return typeof url === 'string' ? url : null
  }

  override async isClientSupported(): Promise<boolean> {
    return process.platform === 'darwin' || process.platform === 'win32'
  }
}
