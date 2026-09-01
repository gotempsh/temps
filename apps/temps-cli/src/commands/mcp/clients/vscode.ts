// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

// VS Code's native MCP support (1.102+) reads a dedicated `mcp.json` in the
// user profile folder, keyed by "servers" (not "mcpServers"), with remote
// entries requiring an explicit "type": "http". This is NOT the older
// `settings.json` / `github.copilot.chat.mcp.*` shape from earlier previews.
export class VsCodeAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'vscode'
  readonly label = 'VS Code'

  protected getConfigPath(): string {
    const home = os.homedir()
    if (process.platform === 'darwin') {
      return path.join(home, 'Library', 'Application Support', 'Code', 'User', 'mcp.json')
    }
    if (process.platform === 'win32') {
      return path.join(process.env.APPDATA || '', 'Code', 'User', 'mcp.json')
    }
    return path.join(home, '.config', 'Code', 'User', 'mcp.json')
  }

  protected getServerPropertyName(): string {
    return 'servers'
  }

  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    return { type: 'http', url: entry.url, headers: { Authorization: `Bearer ${entry.apiKey}` } }
  }

  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    return typeof serverConfig.url === 'string' ? serverConfig.url : null
  }

  override async isClientSupported(): Promise<boolean> {
    return process.platform === 'darwin' || process.platform === 'win32' || process.platform === 'linux'
  }
}
