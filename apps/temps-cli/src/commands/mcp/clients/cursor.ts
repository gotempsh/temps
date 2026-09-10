// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

export class CursorAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'cursor'
  readonly label = 'Cursor'

  protected getConfigPath(): string {
    return path.join(os.homedir(), '.cursor', 'mcp.json')
  }

  protected getServerPropertyName(): string {
    return 'mcpServers'
  }

  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    return { url: entry.url, headers: { Authorization: `Bearer ${entry.apiKey}` } }
  }

  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    return typeof serverConfig.url === 'string' ? serverConfig.url : null
  }

  override async isClientSupported(): Promise<boolean> {
    return process.platform === 'darwin' || process.platform === 'win32' || process.platform === 'linux'
  }
}
