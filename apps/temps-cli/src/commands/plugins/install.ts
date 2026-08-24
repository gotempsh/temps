import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, keyValue, success, error as printError } from '../../ui/output.js'

// See src/commands/plugins/list.ts for why these are hand-written rather
// than generated: `POST /x/plugins/install` is not yet in the regenerated
// openapi.json client.

export interface InstallPluginRequest {
  name: string
  version?: string
}

export interface InstallPluginResponse {
  name: string
  version: string
  path: string
  reloaded: boolean
  message: string
}

interface InstallOptions {
  version?: string
  json?: boolean
}

export async function installPluginAction(name: string, options: InstallOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const body: InstallPluginRequest = { name }
  if (options.version) {
    body.version = options.version
  }

  let result: InstallPluginResponse
  try {
    result = await withSpinner(`Installing plugin "${name}"...`, async () => {
      const { data, error, response } = await client.post<InstallPluginResponse, ProblemDetails>({
        url: '/x/plugins/install',
        body,
      })
      if (error || !data) {
        // Surface the server's actual detail (checksum mismatch, unsupported
        // platform, unknown plugin name, etc.) rather than a generic message —
        // these failures are meaningfully different and the operator needs to
        // know which one happened.
        const detail = getErrorMessage(error)
        const statusSuffix = response ? ` (HTTP ${response.status})` : ''
        throw new Error(detail ? `${detail}${statusSuffix}` : `Plugin install failed${statusSuffix}`)
      }
      return data
    })
  } catch (err) {
    printError(`Failed to install plugin "${name}"`)
    printError(getErrorMessage(err))
    process.exitCode = 1
    return
  }

  if (options.json) {
    json(result)
    return
  }

  newline()
  success(result.message || `Plugin "${result.name}" installed`)
  header(`${icons.info} ${result.name}`)
  keyValue('Version', result.version)
  keyValue('Path', result.path)
  keyValue('Process reloaded', result.reloaded ? 'yes' : 'no')
  newline()
}
