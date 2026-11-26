// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Apps that connect to the Temps MCP endpoint themselves: they run on the
 * user's computer and speak MCP over HTTP, so they need only the endpoint
 * address and a key, no extension. The settings shapes mirror what
 * `temps mcp add <app>` writes (`apps/temps-cli/src/commands/mcp/clients`).
 */

/** What a key the console creates for an MCP app may do. */
export type McpKeyAccess = 'changes' | 'read'

/**
 * The role behind each access level. `user` is the role `temps mcp add
 * --write` gives its keys; a key never exceeds its owner's own role.
 */
export const MCP_KEY_ROLES: Record<McpKeyAccess, string> = {
  changes: 'user',
  read: 'reader',
}

/**
 * Name of a key created for an MCP app. The `mcp-` prefix is what the server
 * recognises as an MCP client key when it decides an agent has connected
 * (`MCP_CLIENT_KEY_PREFIX` in temps-auth); the time keeps names unique.
 */
export function mcpKeyName(client: string, now: Date): string {
  const stamp = now.toISOString().slice(0, 19).replace(/:/g, '-')
  return `mcp-${client}-${stamp}Z`
}

/**
 * The MCP endpoint with the write tools listed. Every write is proposed
 * first and runs through `confirm_action`, which checks the key's
 * permissions again, so a read-only key is refused there.
 */
export function mcpEndpointUrl(origin: string): string {
  const url = new URL('/mcp', new URL(origin).origin)
  url.searchParams.set('write', '1')
  return url.toString()
}

export type McpAppId = 'cursor' | 'vscode' | 'windsurf' | 'zed'

export interface McpApp {
  id: McpAppId
  name: string
  /** Where the snippet goes, as the app's documentation names it. */
  settingsFile: string
  /** The key the server entry sits under in that file. */
  root: string
  entry(url: string, apiKey: string): Record<string, unknown>
}

const bearer = (apiKey: string) => ({ Authorization: `Bearer ${apiKey}` })

export const MCP_APPS: McpApp[] = [
  {
    id: 'cursor',
    name: 'Cursor',
    settingsFile: '~/.cursor/mcp.json',
    root: 'mcpServers',
    entry: (url, apiKey) => ({ url, headers: bearer(apiKey) }),
  },
  {
    id: 'vscode',
    name: 'VS Code',
    settingsFile:
      'mcp.json in your VS Code user folder (MCP: Open User Configuration)',
    root: 'servers',
    entry: (url, apiKey) => ({ type: 'http', url, headers: bearer(apiKey) }),
  },
  {
    id: 'windsurf',
    name: 'Windsurf',
    settingsFile: '~/.codeium/windsurf/mcp_config.json',
    root: 'mcpServers',
    // Windsurf names the address `serverUrl`, unlike the others.
    entry: (url, apiKey) => ({ serverUrl: url, headers: bearer(apiKey) }),
  },
  {
    id: 'zed',
    name: 'Zed',
    settingsFile: '~/.config/zed/settings.json',
    root: 'context_servers',
    entry: (url, apiKey) => ({ enabled: true, url, headers: bearer(apiKey) }),
  },
]

export function mcpApp(id: McpAppId): McpApp {
  return MCP_APPS.find((app) => app.id === id) ?? MCP_APPS[0]
}

/** The settings to merge into the app's file, with the key in place. */
export function mcpAppSnippet(
  id: McpAppId,
  origin: string,
  apiKey: string
): string {
  const app = mcpApp(id)
  return JSON.stringify(
    { [app.root]: { temps: app.entry(mcpEndpointUrl(origin), apiKey) } },
    null,
    2
  )
}

/**
 * The same through the Temps CLI, which writes the file itself instead of
 * using the key shown here: read-only creates a reader key (the CLI's `-y`
 * never installs its login for an app without `--write`), and changes use
 * the CLI's own login. With `deployFolder`, the app
 * runs the Temps bridge on this computer instead, which can also deploy
 * project folders from inside that folder (and needs write access).
 */
export function mcpAddCommand(
  id: McpAppId,
  origin: string,
  access: McpKeyAccess,
  deployFolder?: string
): string {
  const write = access === 'changes' || deployFolder ? ' --write' : ''
  const deploy = deployFolder ? ` --deploy-folder ${deployFolder}` : ''
  return `bunx @temps-sdk/cli mcp add ${id} --url ${new URL(origin).origin}${write}${deploy} -y`
}
