// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isLoopbackHost, isTailscaleHost } from '@/lib/network-address'
import { mcpKeyName } from '@/lib/mcp-clients'
import { createZip } from '@/lib/zip'

/**
 * The Temps desktop extension for the Claude app (an MCP Bundle, `.mcpb`).
 *
 * It is built in the browser from this console's own address, so the server
 * never derives a download from request headers, and it carries no
 * credential: Claude asks for the API key when it installs the extension and
 * keeps it in the operating system's keychain (`sensitive`). The extension's
 * server is `bridge.cjs`, which forwards MCP over stdio to `<server>/mcp`,
 * and `deploy.cjs`, which deploys a project folder on this computer.
 */

export const CLAUDE_EXTENSION_FILENAME = 'temps.mcpb'
export const CLAUDE_EXTENSION_VERSION = '1.1.0'

/** Name of the API key the console creates for the extension. */
export function claudeExtensionKeyName(now: Date): string {
  return mcpKeyName('claude-app', now)
}

/**
 * Whether the extension will connect to this console address: over HTTPS,
 * or plain HTTP to this computer or a tailnet. A single-label name ("nas")
 * may resolve to a tailnet address, which the extension checks when it
 * connects, so it is not refused here.
 */
export function extensionCanConnect(origin: string): boolean {
  let url: URL
  try {
    url = new URL(origin)
  } catch {
    return false
  }
  if (url.protocol === 'https:') return true
  if (url.protocol !== 'http:') return false
  return (
    isLoopbackHost(url.hostname) ||
    isTailscaleHost(url.hostname) ||
    !url.hostname.includes('.')
  )
}

export function claudeExtensionManifest(serverOrigin: string) {
  const origin = new URL(serverOrigin).origin
  return {
    manifest_version: '0.3',
    name: 'temps',
    display_name: 'Temps',
    version: CLAUDE_EXTENSION_VERSION,
    description:
      'Work with your Temps projects, deployments, logs and errors from Claude.',
    long_description:
      "Connects Claude to your Temps server through its MCP endpoint. Claude runs the extension on this computer, so it reaches a Temps console on localhost or a tailnet. Changes are proposed first and run when Claude calls confirm_action, so keep Claude asking you before it runs confirm_action or temps_deploy_folder; the API key's role decides which changes are allowed. Claude can also deploy a project folder from this computer, but only from folders you allow, and never .env files, keys or what .gitignore lists. It sends the key only to the server address you set, over HTTPS, or plain HTTP to this computer or a Tailscale address.",
    author: { name: 'Temps', url: 'https://github.com/gotempsh/temps' },
    homepage: 'https://github.com/gotempsh/temps',
    license: 'MIT OR Apache-2.0',
    server: {
      type: 'node',
      entry_point: 'server/index.js',
      mcp_config: {
        command: 'node',
        // A setting with several values becomes several arguments.
        args: ['${__dirname}/server/index.js', '${user_config.deploy_folders}'],
        env: {
          TEMPS_URL: '${user_config.server_url}',
          TEMPS_API_KEY: '${user_config.api_key}',
        },
      },
    },
    user_config: {
      server_url: {
        type: 'string',
        title: 'Temps server address',
        description: 'The address you open the Temps console at.',
        default: origin,
        required: true,
      },
      api_key: {
        type: 'string',
        title: 'API key',
        description:
          'A Temps API key. Create one in the Temps console; its role decides whether Claude may make changes.',
        sensitive: true,
        required: true,
      },
      deploy_folders: {
        type: 'directory',
        title: 'Folders Claude may deploy',
        description:
          'Claude deploys a project only from inside these folders. Leave empty to turn deploying from this computer off.',
        multiple: true,
        required: false,
      },
    },
    tools: [
      {
        name: 'temps',
        description:
          'Read Temps: projects, deployments, logs, errors and more.',
      },
      {
        name: 'temps_write',
        description:
          'Propose a change, such as creating a project or deploying. Nothing runs until it is confirmed.',
      },
      {
        name: 'confirm_action',
        description:
          'Run a proposed change after you approve it, if the API key allows it.',
      },
      {
        name: 'temps_deploy_folder',
        description:
          'Deploy a project folder from this computer, from the folders you allow.',
      },
    ],
    tools_generated: true,
    compatibility: { runtimes: { node: '>=18.0.0' } },
  }
}

/**
 * The extension's entry file. It starts the bridge unconditionally: the
 * Claude app may load it through a wrapper of its own, so a
 * `require.main === module` check would never fire and every request would
 * time out without a word.
 */
export const CLAUDE_EXTENSION_ENTRY = `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use strict'

require('./bridge.cjs').main()
`

/** The extension server's sources, as `bridge.cjs` and `deploy.cjs` read. */
export interface ClaudeExtensionSources {
  bridge: string
  deploy: string
}

/** The `.mcpb` file: the manifest, the entry file and the server's two files. */
export function buildClaudeExtension(
  serverOrigin: string,
  sources: ClaudeExtensionSources
): Uint8Array {
  return createZip([
    {
      name: 'manifest.json',
      data: `${JSON.stringify(claudeExtensionManifest(serverOrigin), null, 2)}\n`,
    },
    { name: 'server/index.js', data: CLAUDE_EXTENSION_ENTRY },
    // `.cjs`, as in this repository, so `require('./deploy.cjs')` resolves.
    { name: 'server/bridge.cjs', data: sources.bridge },
    { name: 'server/deploy.cjs', data: sources.deploy },
  ])
}
