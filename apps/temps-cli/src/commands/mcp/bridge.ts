// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as os from 'node:os'
import * as path from 'node:path'
// The same bridge the Claude app extension runs (built by the console), so
// both behave alike: it forwards MCP to `<url>/mcp?write=1` and answers
// `temps_deploy_folder` itself, with the same rules on where the key may go
// and which folders may be read. Bundled into the CLI at build time.
import bridge from '../../../../../web/src/lib/claude-extension/bridge.cjs'

/** The npm package an AI app runs to start the bridge, pinned to this CLI's version. */
export function bridgePackageSpec(version: string): string {
  return `@temps-sdk/cli@${version}`
}

/** A folder from the command line as a full path: `~` is this user's home, a relative path is from here. */
export function resolveFolder(folder: string): string {
  const trimmed = folder.trim()
  const expanded = trimmed === '~' || trimmed.startsWith('~/') ? path.join(os.homedir(), trimmed.slice(1)) : trimmed
  return path.resolve(expanded)
}

/**
 * Check the address the bridge will connect to, so `mcp add` refuses one the
 * bridge would refuse (plain HTTP to a LAN address) before it creates a key
 * or writes anything. Throws the bridge's own explanation. The key is checked
 * separately, so a stand-in is enough here.
 */
export function checkBridgeAddress(url: string): void {
  bridge.readConfig({ TEMPS_URL: url, TEMPS_API_KEY: 'address-check', TEMPS_BRIDGE_HOST: 'cli' }, [])
}

export interface BridgeOptions {
  url?: string
  folder?: string[]
}

/**
 * `temps mcp bridge`: the Temps MCP server over stdio, for AI apps that run
 * it on this computer. The API key comes from TEMPS_API_KEY, never a flag, so
 * it does not show in the process list. Writes only MCP messages to stdout;
 * everything else goes to stderr.
 */
export function bridgeAction(options: BridgeOptions): void {
  const folders = (options.folder ?? []).map(resolveFolder)
  bridge.main(
    {
      TEMPS_URL: options.url ?? process.env.TEMPS_URL ?? '',
      TEMPS_API_KEY: process.env.TEMPS_API_KEY ?? '',
      // Messages point at `temps mcp add`, not the Claude extension's settings.
      TEMPS_BRIDGE_HOST: 'cli',
    },
    folders,
  )
}
