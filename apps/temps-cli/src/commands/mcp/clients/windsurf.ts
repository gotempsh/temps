// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

// Windsurf's remote-MCP shape uses `serverUrl` (not `url`, unlike every other
// native-HTTP client here) -- verified against Windsurf/Cascade docs directly
// since PostHog's wizard does not support this client and has no reference
// implementation to copy. Re-verify against docs.windsurf.com if this client
// stops connecting.
export class WindsurfAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'windsurf'
  readonly label = 'Windsurf'

  protected getConfigPath(): string {
    return path.join(os.homedir(), '.codeium', 'windsurf', 'mcp_config.json')
  }

  protected getServerPropertyName(): string {
    return 'mcpServers'
  }

  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    return { serverUrl: entry.url, headers: { Authorization: `Bearer ${entry.apiKey}` } }
  }

  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    return typeof serverConfig.serverUrl === 'string' ? serverConfig.serverUrl : null
  }

  override async isClientSupported(): Promise<boolean> {
    return process.platform === 'darwin' || process.platform === 'win32' || process.platform === 'linux'
  }
}
