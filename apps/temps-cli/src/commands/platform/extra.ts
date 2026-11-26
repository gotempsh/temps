// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Platform-wide reads: the operations feed (deployments, rollbacks,
 * restores, backups in flight), feature maturity, an IP geolocation lookup,
 * and a file from the server's static directory.
 */

import type { Command } from 'commander'
import { client } from '../../lib/api-client.js'
import { getFeatureMaturity, getIpGeolocation, listOperations } from '../../api/sdk.gen.js'
import type { OperationKind, OperationStatusFilter } from '../../api/types.gen.js'
import { call, connect, fail, output, parseIntegerFlag, type JsonOption } from '../../lib/command-kit.js'
import { json, newline, header, icons, keyValue } from '../../ui/output.js'
import { ValidationError } from '../../utils/errors.js'

const STATUSES: readonly OperationStatusFilter[] = ['running', 'waiting', 'finished', 'all']
const KINDS: readonly OperationKind[] = ['deployment', 'rollback', 'promotion', 'restore', 'backup', 'autofix']

export interface OperationsFlags {
  status?: string
  kind?: string
  project?: string
  page?: string
  pageSize?: string
}

export function operationsQuery(flags: OperationsFlags) {
  if (flags.status !== undefined && !STATUSES.includes(flags.status as OperationStatusFilter)) {
    throw new ValidationError(`Unknown --status "${flags.status}": use one of ${STATUSES.join(', ')}`)
  }
  if (flags.kind !== undefined && !KINDS.includes(flags.kind as OperationKind)) {
    throw new ValidationError(`Unknown --kind "${flags.kind}": use one of ${KINDS.join(', ')}`)
  }
  return {
    ...(flags.status !== undefined && { status: flags.status as OperationStatusFilter }),
    ...(flags.kind !== undefined && { kind: flags.kind as OperationKind }),
    ...(flags.project !== undefined && { project_id: parseIntegerFlag(flags.project, '--project', 1) }),
    ...(flags.page !== undefined && { page: parseIntegerFlag(flags.page, '--page', 1) }),
    ...(flags.pageSize !== undefined && { page_size: parseIntegerFlag(flags.pageSize, '--page-size', 1, 100) }),
  }
}

/** A path under the static directory, without leading slashes or `..`. */
export function staticFilePath(raw: string): string {
  const path = raw.trim().replace(/^\/+/, '')
  if (!path || path.split('/').includes('..')) throw new ValidationError(`"${raw}" is not a path inside the static directory`)
  return path
}

export function registerPlatformExtraCommands(platform: Command): void {
  platform
    .command('operations')
    .alias('ops')
    .description('List recent and in-flight operations: deployments, rollbacks, promotions, restores, backups, autofixes')
    .option('--status <status>', `Which ones: ${STATUSES.join(', ')} (default: the server's)`)
    .option('--kind <kind>', `Only one kind: ${KINDS.join(', ')}`)
    .option('--project <id>', 'Only this project ID')
    .option('--page <n>', 'Page (default 1)')
    .option('--page-size <n>', 'Operations per page (max 100)')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption & OperationsFlags) => {
      const query = operationsQuery(options)
      await connect()
      const result = await call('Could not list operations', () => listOperations({ client, query }))
      if (options.json) {
        json(result)
        return
      }
      newline()
      header(`${icons.info} Operations (${result.operations.length} of ${result.total}, ${result.running_count} running)`)
      for (const op of result.operations) console.log(JSON.stringify(op))
      newline()
    })

  platform
    .command('maturity')
    .description('Show the compatibility promise (stable, beta, experimental) for every feature')
    .option('--json', 'Output in JSON format')
    .action(async (options: JsonOption) => {
      await connect()
      const features = await call('Could not read feature maturity', () => getFeatureMaturity({ client }))
      if (options.json) {
        json(features)
        return
      }
      newline()
      header(`${icons.info} Feature maturity`)
      for (const f of features) keyValue(f.key, `${f.maturity}: ${f.reason} (${f.docs_path})`)
      newline()
    })

  platform
    .command('geo <ip>')
    .description('Look up the location of an IP address in the server\'s geolocation database')
    .option('--json', 'Output in JSON format')
    .action(async (ip: string, options: JsonOption) => {
      await connect()
      output(await call(`Could not geolocate ${ip}`, () => getIpGeolocation({ client, path: { ip } })), options, ip)
    })

  platform
    .command('file <path>')
    .description("Print a file from the server's static directory (or save it with -o)")
    .option('-o, --output <file>', 'Write it to a file instead of stdout')
    .action(async (raw: string, options: { output?: string }) => {
      const path = staticFilePath(raw)
      await connect()
      // The generated getFile would encode the slashes of a nested path.
      // api-coverage: GET /files/{file_path}
      const { data, error, response } = await client.get({
        url: `/files/${path.split('/').map(encodeURIComponent).join('/')}`,
        parseAs: 'stream',
      })
      if (error || !response?.ok) fail(`Could not read ${path}`, error)
      const body = new Response(data as ReadableStream | null)
      if (options.output) await Bun.write(options.output, body)
      else await Bun.write(Bun.stdout, body)
    })
}
