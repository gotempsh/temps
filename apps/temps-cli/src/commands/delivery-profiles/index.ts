// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getDeliveryCapabilities,
  listDeliveryProfiles,
  createDeliveryProfile,
  deleteDeliveryProfile,
} from '../../api/sdk.gen.js'
import type {
  CreateDeliveryProfileRequest,
  DeliveryCapabilityResponse,
  DeliveryProfileResponse,
  DeliveryProviderKind,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptPassword, promptConfirm } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  error,
} from '../../ui/output.js'
import { isTTY } from '../../utils/tty.js'

export const DELIVERY_PROVIDER_KINDS: readonly DeliveryProviderKind[] = [
  'cloudflare',
  'bunny',
  'direct',
]

// --- Option interfaces ---

interface ListOptions {
  json?: boolean
}

export interface CreateOptions {
  name?: string
  kind?: string
  pullZoneId?: string
  apiKey?: string
  apiKeyStdin?: boolean
  json?: boolean
  yes?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

// --- Pure helpers (unit tested) ---

export function parseDeliveryProviderKind(
  value: string
): DeliveryProviderKind | undefined {
  const normalized = value.trim().toLowerCase()
  return DELIVERY_PROVIDER_KINDS.find((kind) => kind === normalized)
}

/** Parse a strictly positive integer flag; rejects trailing junk like `12abc`. */
export function parsePositiveInt(value: string): number | undefined {
  if (!/^\d+$/.test(value.trim())) return undefined
  const parsed = Number(value.trim())
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined
}

/**
 * Validate the flags for `delivery-profiles create` before any network call.
 * Returns the parsed kind + pull zone, or an actionable error message. The API
 * key is resolved separately because it may come from stdin or a prompt.
 */
export function validateCreateOptions(
  options: CreateOptions
):
  | { kind: DeliveryProviderKind; name: string; pullZoneId?: number }
  | { error: string } {
  const name = options.name?.trim()
  if (!name) {
    return { error: '--name is required (1 to 100 characters)' }
  }
  if (name.length > 100) {
    return { error: '--name must be at most 100 characters' }
  }
  if (!options.kind) {
    return {
      error: `--kind is required. Use one of: ${DELIVERY_PROVIDER_KINDS.join(', ')}`,
    }
  }
  const kind = parseDeliveryProviderKind(options.kind)
  if (!kind) {
    return {
      error: `Invalid --kind "${options.kind}". Use one of: ${DELIVERY_PROVIDER_KINDS.join(', ')}`,
    }
  }
  if (kind !== 'bunny') {
    if (
      options.pullZoneId !== undefined ||
      options.apiKey !== undefined ||
      options.apiKeyStdin
    ) {
      return {
        error:
          '--pull-zone-id, --api-key and --api-key-stdin are only valid with --kind bunny',
      }
    }
    return { kind, name }
  }
  if (options.apiKey !== undefined && options.apiKeyStdin) {
    return { error: 'Use either --api-key or --api-key-stdin, not both' }
  }
  if (options.pullZoneId === undefined) {
    return {
      error:
        '--pull-zone-id is required for a Bunny delivery profile (find it in the Bunny dashboard under CDN > Pull Zones)',
    }
  }
  const pullZoneId = parsePositiveInt(options.pullZoneId)
  if (pullZoneId === undefined) {
    return {
      error: `Invalid --pull-zone-id "${options.pullZoneId}". It must be a positive integer`,
    }
  }
  return { kind, name, pullZoneId }
}

export function buildCreateDeliveryProfileRequest(
  name: string,
  kind: DeliveryProviderKind,
  pullZoneId?: number,
  apiKey?: string
): CreateDeliveryProfileRequest {
  if (kind === 'bunny') {
    return {
      name,
      provider_kind: kind,
      bunny_pull_zone_id: pullZoneId,
      bunny_api_key: apiKey,
    }
  }
  return { name, provider_kind: kind }
}

/** One-line status for a capability: what is missing and where to fix it. */
export function describeCapability(
  capability: DeliveryCapabilityResponse
): string {
  if (!capability.supported) {
    return 'not supported on this instance'
  }
  if (capability.configured) {
    return 'ready'
  }
  const missing =
    capability.requirements.length > 0
      ? `needs ${capability.requirements.join(', ')}`
      : 'not configured'
  return capability.setup_path
    ? `${missing} (set up at ${capability.setup_path})`
    : missing
}

/** Read a secret from piped stdin. Returns undefined for an interactive TTY or empty input. */
export async function readSecretFromStdin(): Promise<string | undefined> {
  if (process.stdin.isTTY) {
    return undefined
  }
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.from(chunk))
  }
  const text = Buffer.concat(chunks).toString('utf8').trim()
  return text.length > 0 ? text : undefined
}

// --- Command registration ---

export function registerDeliveryProfilesCommands(program: Command): void {
  const profiles = program
    .command('delivery-profiles')
    .alias('delivery-profile')
    .description(
      'Manage traffic delivery profiles (Cloudflare proxy, Bunny CDN) used by project domains'
    )

  profiles
    .command('capabilities')
    .alias('caps')
    .description(
      'Show which delivery providers are available and what each one still needs'
    )
    .option('--json', 'Output in JSON format')
    .action(capabilitiesAction)

  profiles
    .command('list')
    .alias('ls')
    .description('List delivery profiles')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  profiles
    .command('create')
    .alias('add')
    .description('Create a delivery profile')
    .option('-n, --name <name>', 'Profile name (1-100 characters)')
    .option(
      '-k, --kind <kind>',
      `Delivery provider (${DELIVERY_PROVIDER_KINDS.join(', ')})`
    )
    .option(
      '--pull-zone-id <id>',
      'Bunny Pull Zone ID (required for --kind bunny)'
    )
    .option(
      '--api-key <key>',
      'Bunny account API key (prefer --api-key-stdin to keep it out of shell history)'
    )
    .option('--api-key-stdin', 'Read the Bunny API key from stdin')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Never prompt (for automation)')
    .action(createAction)

  profiles
    .command('remove')
    .alias('rm')
    .alias('delete')
    .description('Delete a delivery profile')
    .requiredOption('--id <id>', 'Profile ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(removeAction)
}

// --- Action implementations ---

async function capabilitiesAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const capabilities = await withSpinner(
    'Fetching delivery capabilities...',
    async () => {
      const { data, error: apiError } = await getDeliveryCapabilities({
        client,
      })
      if (apiError) {
        throw new Error(getErrorMessage(apiError))
      }
      return data ?? []
    }
  )

  if (options.json) {
    json(capabilities)
    return
  }

  newline()
  header(`${icons.info} Delivery Providers (${capabilities.length})`)

  const columns: TableColumn<DeliveryCapabilityResponse>[] = [
    { header: 'Provider', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Kind', key: 'provider_kind' },
    {
      header: 'Ready',
      accessor: (c) => (c.supported && c.configured ? 'yes' : 'no'),
      color: (v) => statusBadge(v === 'yes' ? 'active' : 'inactive'),
    },
    {
      header: 'Status',
      accessor: (c) => describeCapability(c),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(capabilities, columns, { style: 'minimal' })

  if (capabilities.some((c) => c.supported && !c.configured)) {
    newline()
    info(
      'Configure the missing pieces, then run: temps delivery-profiles create --kind <kind> --name <name>'
    )
  }
  newline()
}

async function listAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const profiles = await withSpinner(
    'Fetching delivery profiles...',
    async () => {
      const { data, error: apiError } = await listDeliveryProfiles({ client })
      if (apiError) {
        throw new Error(getErrorMessage(apiError))
      }
      return data ?? []
    }
  )

  if (options.json) {
    json(profiles)
    return
  }

  newline()
  header(`${icons.info} Delivery Profiles (${profiles.length})`)

  if (profiles.length === 0) {
    info('No delivery profiles configured')
    info(
      'Run: temps delivery-profiles capabilities   (to see what each provider needs)'
    )
    info(
      'Run: temps delivery-profiles create --kind cloudflare --name cloudflare'
    )
    newline()
    return
  }

  const columns: TableColumn<DeliveryProfileResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Kind', key: 'provider_kind' },
    {
      header: 'Bunny Pull Zone',
      accessor: (p) =>
        p.bunny_pull_zone_id ? String(p.bunny_pull_zone_id) : '-',
      color: (v) => colors.muted(v),
    },
    {
      header: 'Bunny Hostname',
      accessor: (p) => p.bunny_hostname || '-',
      color: (v) => colors.muted(v),
    },
    {
      header: 'Created',
      accessor: (p) => new Date(p.created_at).toLocaleDateString(),
    },
  ]

  printTable(profiles, columns, { style: 'minimal' })
  newline()
}

async function createAction(options: CreateOptions): Promise<void> {
  // Validate every flag before auth or any request so automation gets an
  // immediate, specific error instead of a half-made profile.
  const validated = validateCreateOptions(options)
  if ('error' in validated) {
    error(validated.error)
    process.exitCode = 1
    return
  }

  let apiKey: string | undefined
  if (validated.kind === 'bunny') {
    if (options.apiKeyStdin) {
      apiKey = await readSecretFromStdin()
      if (!apiKey) {
        error('--api-key-stdin was given but no API key was piped on stdin')
        process.exitCode = 1
        return
      }
    } else if (options.apiKey !== undefined) {
      apiKey = options.apiKey.trim()
    } else if (options.yes || !isTTY()) {
      error(
        'A Bunny API key is required: pass --api-key-stdin (recommended) or --api-key'
      )
      process.exitCode = 1
      return
    }
  }

  await requireAuth()
  await setupClient()

  if (validated.kind === 'bunny' && apiKey === undefined) {
    info(
      'Bunny delivery needs your account API key (Bunny dashboard > Account settings > API).'
    )
    apiKey = await promptPassword({ message: 'Bunny API Key' })
  }

  const body = buildCreateDeliveryProfileRequest(
    validated.name,
    validated.kind,
    validated.pullZoneId,
    apiKey
  )

  const profile = await withSpinner(
    `Creating ${validated.kind} delivery profile...`,
    async () => {
      const { data, error: apiError } = await createDeliveryProfile({
        client,
        body,
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) || 'Failed to create delivery profile'
        )
      }
      return data
    }
  )

  if (options.json) {
    json(profile)
    return
  }

  success(
    `Delivery profile "${profile.name}" (#${profile.id}, ${profile.provider_kind}) created`
  )
  if (profile.bunny_hostname) {
    info(`Bunny pull zone hostname: ${profile.bunny_hostname}`)
  }
  info(
    `Use it as a project default: temps delivery settings set -p <project> --default-profile ${profile.id}`
  )
}

async function removeAction(options: RemoveOptions): Promise<void> {
  const id = parsePositiveInt(options.id)
  if (id === undefined) {
    error(`Invalid profile ID "${options.id}"`)
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  if (!(options.force || options.yes)) {
    const confirmed = await promptConfirm({
      message: `Delete delivery profile #${id}? Projects and bindings using it must be moved first.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting delivery profile...', async () => {
    const { error: apiError } = await deleteDeliveryProfile({
      client,
      path: { profile_id: id },
    })
    if (apiError) {
      throw new Error(getErrorMessage(apiError))
    }
  })

  success(`Delivery profile #${id} deleted`)
}
