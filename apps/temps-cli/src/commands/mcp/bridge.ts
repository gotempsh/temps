// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as fs from 'node:fs'
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

/**
 * A folder from the command line as a full path: `~` is this user's home, a
 * relative path is from here. A blank one is refused: it would otherwise be
 * the current folder, which the user never named.
 */
export function resolveFolder(folder: string): string {
  const trimmed = folder.trim()
  if (!trimmed) {
    throw new Error('A deploy folder was left blank. Name the folder that holds your projects.')
  }
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

/** A key file holds one key; anything larger is not one, and is not read into memory. */
const MAX_KEY_FILE_BYTES = 4096

/**
 * The API key the bridge sends: the contents of the file TEMPS_API_KEY_FILE
 * names, or else TEMPS_API_KEY. The file wins: `temps mcp add` sets it in the
 * app's entry, next to the `--url` it was minted for, while TEMPS_API_KEY may
 * be inherited from wherever the app was started and belong to another
 * instance. Empty when neither is set, which the bridge reports as a missing
 * key. Throws when the file cannot be read, saying which file.
 */
export function bridgeApiKey(env: NodeJS.ProcessEnv): string {
  const file = env.TEMPS_API_KEY_FILE?.trim()
  if (!file) return env.TEMPS_API_KEY ?? ''
  let handle: number | undefined
  try {
    handle = fs.openSync(file, 'r')
    const buffer = Buffer.alloc(MAX_KEY_FILE_BYTES + 1)
    const read = fs.readSync(handle, buffer, 0, buffer.length, 0)
    if (read > MAX_KEY_FILE_BYTES) {
      throw new Error(`it is larger than ${MAX_KEY_FILE_BYTES} bytes, so it is not an API key file`)
    }
    return buffer.subarray(0, read).toString('utf8').trim()
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error)
    throw new Error(`Could not read the Temps API key from ${file} (TEMPS_API_KEY_FILE): ${reason}. Run temps mcp add again.`)
  } finally {
    if (handle !== undefined) fs.closeSync(handle)
  }
}

/**
 * `temps mcp bridge`: the Temps MCP server over stdio, for AI apps that run
 * it on this computer. The API key comes from the file TEMPS_API_KEY_FILE
 * names or TEMPS_API_KEY, never a flag, so it does not show in the process
 * list. Writes only MCP messages to stdout; everything else goes to stderr.
 */
export function bridgeAction(options: BridgeOptions): void {
  // A blank --folder allows nothing, rather than the folder the app started in.
  const folders = (options.folder ?? []).flatMap((folder) => {
    try {
      return [resolveFolder(folder)]
    } catch (error) {
      process.stderr.write(`temps: ${error instanceof Error ? error.message : String(error)}\n`)
      return []
    }
  })
  let key = ''
  try {
    key = bridgeApiKey(process.env)
  } catch (error) {
    // The bridge answers every request with this, like any other setup error.
    process.stderr.write(`temps: ${error instanceof Error ? error.message : String(error)}\n`)
  }
  bridge.main(
    {
      TEMPS_URL: options.url ?? process.env.TEMPS_URL ?? '',
      TEMPS_API_KEY: key,
      // Messages point at `temps mcp add`, not the Claude extension's settings.
      TEMPS_BRIDGE_HOST: 'cli',
    },
    folders,
  )
}
