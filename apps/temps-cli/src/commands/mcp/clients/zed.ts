// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
import { JsonConfigMcpClientAdapter, type McpServerEntry } from './base.js'

export class ZedAdapter extends JsonConfigMcpClientAdapter {
  readonly id = 'zed'
  readonly label = 'Zed'

  protected getConfigPath(): string {
    const home = os.homedir()
    const xdgConfigHome = process.env.XDG_CONFIG_HOME
    if (process.platform === 'linux' && xdgConfigHome) {
      return path.join(xdgConfigHome, 'zed', 'settings.json')
    }
    return path.join(home, '.config', 'zed', 'settings.json')
  }

  protected getServerPropertyName(): string {
    return 'context_servers'
  }

  protected buildServerConfig(entry: McpServerEntry): Record<string, unknown> {
    return { enabled: true, url: entry.url, headers: { Authorization: `Bearer ${entry.apiKey}` } }
  }

  protected extractUrl(serverConfig: Record<string, unknown>): string | null {
    return typeof serverConfig.url === 'string' ? serverConfig.url : null
  }

  // Zed does not ship an official Windows build.
  override async isClientSupported(): Promise<boolean> {
    return process.platform === 'darwin' || process.platform === 'linux'
  }
}
