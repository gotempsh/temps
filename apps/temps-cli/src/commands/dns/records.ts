// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  setManagedRecord,
  removeManagedRecord,
  getRecordOwnership,
  importManagedRecord,
} from '../../api/sdk.gen.js'
import type {
  DnsRecord,
  DnsRecordContent,
  DnsRecordType,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { statusBadge } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
  error,
  keyValue,
} from '../../ui/output.js'
import { resolveProjectRef } from '../delivery/index.js'
import { parsePositiveInt } from '../delivery-profiles/index.js'

/**
 * Record types the managed-record endpoints accept. Ownership-guarded
 * management covers only the records that route traffic; the server rejects
 * every other type (`ManagedDnsRecordService::validate_record_type`), so the
 * CLI refuses them before auth instead of failing on the request. Deliberately
 * not the shared `DNS_RECORD_TYPES`, which lists every type the API describes.
 */
export const MANAGED_DNS_RECORD_TYPES = [
  'A',
  'AAAA',
  'CNAME',
] as const satisfies readonly DnsRecordType[]

export type ManagedDnsRecordType = (typeof MANAGED_DNS_RECORD_TYPES)[number]

/** "A, AAAA and CNAME", for help text and errors. */
const MANAGED_TYPES_TEXT = `${MANAGED_DNS_RECORD_TYPES.slice(0, -1).join(', ')} and ${MANAGED_DNS_RECORD_TYPES.at(-1)}`

// --- Option interfaces ---

interface RecordKeyOptions {
  domain: string
  name: string
  type: string
  json?: boolean
}

export interface RecordContentOptions {
  value?: string
}

interface SetOptions extends RecordKeyOptions, RecordContentOptions {
  ttl?: string
  proxied?: boolean
  project?: string
  environmentId?: string
}

interface ImportOptions extends RecordKeyOptions {
  project?: string
  environmentId?: string
}

interface RemoveOptions extends RecordKeyOptions {
  force?: boolean
  yes?: boolean
}

type ParseResult<T> = { value: T } | { error: string }

// --- Pure helpers (unit tested) ---

/**
 * Map `--type` + `--value` to the tagged content shape the API expects. A
 * wrong inner key here (e.g. `target` vs `address`) would be rejected
 * server-side only after auth, so it is validated up front.
 */
export function buildDnsRecordContent(
  type: ManagedDnsRecordType,
  options: RecordContentOptions
): ParseResult<DnsRecordContent> {
  const value = options.value?.trim()
  if (!value) {
    return { error: `--value is required for ${type} records` }
  }
  switch (type) {
    case 'A':
    case 'AAAA':
      return { value: { type, value: { address: value } } }
    case 'CNAME':
      return { value: { type, value: { target: value } } }
  }
}

/** Validate the shared --domain/--name/--type triple that identifies a record. */
export function parseRecordKey(
  options: Pick<RecordKeyOptions, 'domain' | 'name' | 'type'>
): ParseResult<{
  domain: string
  name: string
  record_type: ManagedDnsRecordType
}> {
  const normalizedType = options.type.trim().toUpperCase()
  const recordType = MANAGED_DNS_RECORD_TYPES.find(
    (type) => type === normalizedType
  )
  if (!recordType) {
    return {
      error: `Unsupported --type "${options.type}". Managed DNS records support ${MANAGED_TYPES_TEXT}; manage other record types directly at your DNS provider`,
    }
  }
  const domain = options.domain.trim()
  const name = options.name.trim()
  if (!domain || !name) {
    return {
      error:
        '--domain and --name must not be empty (use "@" for the zone apex)',
    }
  }
  return { value: { domain, name, record_type: recordType } }
}

function parseOptionalId(
  value: string | undefined,
  flag: string
): ParseResult<number | undefined> {
  if (value === undefined) return { value: undefined }
  const id = parsePositiveInt(value)
  return id === undefined
    ? { error: `Invalid ${flag} "${value}". It must be a positive integer` }
    : { value: id }
}

/**
 * Mirror of the server's TTL rule (`ManagedDnsRecordService::
 * validate_record_request`): 1 asks for the provider default, anything else
 * must be 60 to 86400 seconds. Checked here so a bad value fails before auth.
 */
export function parseTtl(
  value: string | undefined
): ParseResult<number | undefined> {
  if (value === undefined) return { value: undefined }
  const ttl = parsePositiveInt(value)
  if (ttl === undefined || (ttl !== 1 && (ttl < 60 || ttl > 86_400))) {
    return {
      error: `Invalid --ttl "${value}". Use 60 to 86400 seconds, or 1 for the provider default`,
    }
  }
  return { value: ttl }
}

/**
 * One-line rendering of a record's content. The managed-record endpoints only
 * return A, AAAA and CNAME records; any other content the response type allows
 * is shown as raw JSON rather than dropped.
 */
export function describeRecordContent(content: DnsRecordContent): string {
  switch (content.type) {
    case 'A':
    case 'AAAA':
      return content.value.address
    case 'CNAME':
      return content.value.target
    default:
      return JSON.stringify(content.value)
  }
}

function printRecord(record: DnsRecord): void {
  keyValue('FQDN', record.fqdn)
  keyValue('Type', record.content.type)
  keyValue('Content', describeRecordContent(record.content))
  keyValue('TTL', record.ttl)
  if (record.proxied !== undefined) {
    keyValue('Proxied', record.proxied ? 'yes' : 'no')
  }
}

function fail(message: string): void {
  error(message)
  process.exitCode = 1
}

// --- Command registration ---

export function registerDnsRecordsCommands(dns: Command): void {
  const records = dns
    .command('records')
    .alias('record')
    .description(
      `Manage ${MANAGED_TYPES_TEXT} records on managed domains (ownership-guarded: Temps only changes records it owns)`
    )

  const recordTypes = MANAGED_DNS_RECORD_TYPES.join(', ')

  records
    .command('ownership')
    .alias('owner')
    .description('Show whether Temps owns a DNS record and may change it')
    .requiredOption(
      '-d, --domain <domain>',
      'Domain under a managed zone, e.g. example.com'
    )
    .requiredOption(
      '--name <name>',
      'Record name relative to the zone ("@" for apex)'
    )
    .requiredOption('-t, --type <type>', `Record type (${recordTypes})`)
    .option('--json', 'Output in JSON format')
    .action(ownershipAction)

  records
    .command('set')
    .alias('create')
    .description('Create or update a Temps-owned DNS record')
    .requiredOption(
      '-d, --domain <domain>',
      'Domain under a managed zone, e.g. example.com'
    )
    .requiredOption(
      '--name <name>',
      'Record name relative to the zone ("@" for apex)'
    )
    .requiredOption('-t, --type <type>', `Record type (${recordTypes})`)
    .option(
      '--value <value>',
      'Record value: IPv4 address (A), IPv6 address (AAAA) or target hostname (CNAME)'
    )
    .option(
      '--ttl <seconds>',
      'TTL in seconds, 60-86400; omit (or use 1) for the provider default'
    )
    .option(
      '--proxied',
      'Proxy through the provider CDN (Cloudflare orange cloud)'
    )
    .option('--no-proxied', 'Do not proxy (DNS only)')
    .option(
      '-p, --project <project>',
      'Project slug or ID to stamp as the record owner'
    )
    .option(
      '--environment-id <id>',
      'Environment ID to stamp as the record owner'
    )
    .option('--json', 'Output in JSON format')
    .action(setAction)

  records
    .command('import')
    .alias('adopt')
    .description('Adopt an existing DNS record into Temps management')
    .requiredOption(
      '-d, --domain <domain>',
      'Domain under a managed zone, e.g. example.com'
    )
    .requiredOption(
      '--name <name>',
      'Record name relative to the zone ("@" for apex)'
    )
    .requiredOption('-t, --type <type>', `Record type (${recordTypes})`)
    .option(
      '-p, --project <project>',
      'Project slug or ID to stamp as the record owner'
    )
    .option(
      '--environment-id <id>',
      'Environment ID to stamp as the record owner'
    )
    .option('--json', 'Output in JSON format')
    .action(importAction)

  records
    .command('remove')
    .alias('rm')
    .alias('delete')
    .description('Delete a Temps-owned DNS record')
    .requiredOption(
      '-d, --domain <domain>',
      'Domain under a managed zone, e.g. example.com'
    )
    .requiredOption(
      '--name <name>',
      'Record name relative to the zone ("@" for apex)'
    )
    .requiredOption('-t, --type <type>', `Record type (${recordTypes})`)
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(removeAction)
}

// --- Action implementations ---

async function ownershipAction(options: RecordKeyOptions): Promise<void> {
  const key = parseRecordKey(options)
  if ('error' in key) return fail(key.error)

  await requireAuth()
  await setupClient()

  const ownership = await withSpinner(
    'Checking record ownership...',
    async () => {
      const { data, error: apiError } = await getRecordOwnership({
        client,
        query: key.value,
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to check ownership of ${key.value.record_type} ${key.value.name}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(ownership)
    return
  }

  newline()
  header(
    `${icons.info} ${key.value.record_type} ${key.value.name} (${key.value.domain})`
  )
  keyValue('Status', statusBadge(ownership.status))
  keyValue(
    'Writable by Temps',
    ownership.writable ? colors.success('yes') : colors.warning('no')
  )
  if (ownership.owner_instance) {
    keyValue('Owner instance', ownership.owner_instance)
  }
  if (ownership.project_id) {
    keyValue('Project ID', ownership.project_id)
  }
  if (ownership.environment_id) {
    keyValue('Environment ID', ownership.environment_id)
  }
  if (ownership.record) {
    printRecord(ownership.record)
  }
  if (
    !ownership.writable &&
    ownership.status !== 'not_found' &&
    ownership.status !== 'unmanaged'
  ) {
    newline()
    warning(`Temps will not modify this record (status: ${ownership.status})`)
  }
  if (ownership.status === 'unmanaged') {
    newline()
    info(
      `Adopt it with: temps dns records import --domain ${key.value.domain} --name ${key.value.name} --type ${key.value.record_type}`
    )
  }
  newline()
}

async function setAction(options: SetOptions): Promise<void> {
  const key = parseRecordKey(options)
  if ('error' in key) return fail(key.error)
  const content = buildDnsRecordContent(key.value.record_type, options)
  if ('error' in content) return fail(content.error)
  const ttl = parseTtl(options.ttl)
  if ('error' in ttl) return fail(ttl.error)
  const environmentId = parseOptionalId(
    options.environmentId,
    '--environment-id'
  )
  if ('error' in environmentId) return fail(environmentId.error)

  await requireAuth()
  await setupClient()

  const projectId = options.project
    ? await resolveProjectRef(options.project)
    : undefined

  const record = await withSpinner(
    `Setting ${key.value.record_type} ${key.value.name}...`,
    async () => {
      const { data, error: apiError } = await setManagedRecord({
        client,
        body: {
          domain: key.value.domain,
          name: key.value.name,
          content: content.value,
          ...(ttl.value !== undefined && { ttl: ttl.value }),
          ...(options.proxied !== undefined && { proxied: options.proxied }),
          ...(projectId !== undefined && { project_id: projectId }),
          ...(environmentId.value !== undefined && {
            environment_id: environmentId.value,
          }),
        },
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to set ${key.value.record_type} ${key.value.name}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(record)
    return
  }

  success(`${key.value.record_type} record ${record.fqdn} set`)
  printRecord(record)
}

async function importAction(options: ImportOptions): Promise<void> {
  const key = parseRecordKey(options)
  if ('error' in key) return fail(key.error)
  const environmentId = parseOptionalId(
    options.environmentId,
    '--environment-id'
  )
  if ('error' in environmentId) return fail(environmentId.error)

  await requireAuth()
  await setupClient()

  const projectId = options.project
    ? await resolveProjectRef(options.project)
    : undefined

  const imported = await withSpinner(
    `Importing ${key.value.record_type} ${key.value.name}...`,
    async () => {
      const { data, error: apiError } = await importManagedRecord({
        client,
        body: {
          ...key.value,
          ...(projectId !== undefined && { project_id: projectId }),
          ...(environmentId.value !== undefined && {
            environment_id: environmentId.value,
          }),
        },
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to import ${key.value.record_type} ${key.value.name}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(imported)
    return
  }

  success(
    `${imported.record_type} ${imported.name} on ${key.value.domain} is now managed by Temps`
  )
}

async function removeAction(options: RemoveOptions): Promise<void> {
  const key = parseRecordKey(options)
  if ('error' in key) return fail(key.error)

  await requireAuth()
  await setupClient()

  if (!(options.force || options.yes)) {
    const confirmed = await promptConfirm({
      message: `Delete ${key.value.record_type} record "${key.value.name}" on ${key.value.domain}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner(
    `Deleting ${key.value.record_type} ${key.value.name}...`,
    async () => {
      const { error: apiError } = await removeManagedRecord({
        client,
        query: key.value,
      })
      if (apiError) {
        throw new Error(getErrorMessage(apiError))
      }
    }
  )

  success(
    `${key.value.record_type} record "${key.value.name}" on ${key.value.domain} deleted`
  )
}
