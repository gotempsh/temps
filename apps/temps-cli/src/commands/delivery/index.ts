// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProjectBySlug,
  getDeliveryProfile,
  listDeliveryProfiles,
  getProjectDeliverySettings,
  updateProjectDeliverySettings,
  listDomainDeliveryBindings,
  previewDomainDeliveryBinding,
  applyDomainDeliveryBinding,
  deleteDomainDeliveryBinding,
} from '../../api/sdk.gen.js'
import type {
  AdoptDeliveryRecord,
  DeliveryProfileResponse,
  DnsRecordType,
  DomainDeliveryBindingResponse,
  EnvironmentDeliveryOverride,
  PreviewDomainDeliveryBindingRequest,
  ProjectDeliverySettingsResponse,
  UpdateProjectDeliverySettingsRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
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
import {
  DEFAULT_PAGE_SIZE,
  MAX_PAGE_SIZE,
  pageFooter,
  parseListPaging,
  parsePositiveInt,
  pastLastPageMessage,
  type ListPagingOptions,
} from '../delivery-profiles/index.js'

/** `delivery bindings list --sort-by` values, as the API accepts them. */
export const BINDING_SORT_FIELDS = [
  'created_at',
  'hostname',
  'updated_at',
] as const

export const DNS_RECORD_TYPES: readonly DnsRecordType[] = [
  'A',
  'AAAA',
  'CNAME',
  'TXT',
  'MX',
  'NS',
  'SRV',
  'CAA',
  'PTR',
]

// --- Option interfaces ---

interface ProjectOptions {
  project?: string
  json?: boolean
}

interface SettingsSetOptions extends ProjectOptions {
  defaultProfile?: string
  env: string[]
}

interface BindingsListOptions extends ProjectOptions, ListPagingOptions {}

export interface PreviewOptions extends ProjectOptions {
  environmentId?: string
  hostname?: string
  zone?: string
  dnsProvider?: string
  originTarget?: string
  profile?: string
}

interface ApplyOptions extends ProjectOptions {
  previewId: string
  adopt: string[]
}

interface RemoveOptions extends ProjectOptions {
  id: string
  force?: boolean
  yes?: boolean
}

type ParseResult<T> = { value: T } | { error: string }

// --- Pure helpers (unit tested) ---

function collect(value: string, previous: string[]): string[] {
  return [...previous, value]
}

export function parseDnsRecordType(value: string): DnsRecordType | undefined {
  const normalized = value.trim().toUpperCase()
  return DNS_RECORD_TYPES.find((type) => type === normalized)
}

/** `--default-profile 3` selects profile 3; `none` clears the project default. */
export function parseDefaultProfile(value: string): ParseResult<number | null> {
  if (value.trim().toLowerCase() === 'none') {
    return { value: null }
  }
  const id = parsePositiveInt(value)
  return id === undefined
    ? {
        error: `Invalid --default-profile "${value}". Use a profile ID or "none"`,
      }
    : { value: id }
}

/** `--env 12=3` pins environment 12 to profile 3; `--env 12=inherit` removes the override. */
export function parseEnvironmentOverride(
  value: string
): ParseResult<EnvironmentDeliveryOverride> {
  const [envPart, profilePart, ...rest] = value.split('=')
  const environmentId =
    envPart === undefined ? undefined : parsePositiveInt(envPart)
  if (
    rest.length > 0 ||
    profilePart === undefined ||
    environmentId === undefined
  ) {
    return {
      error: `Invalid --env "${value}". Use <environment-id>=<profile-id> or <environment-id>=inherit`,
    }
  }
  if (profilePart.trim().toLowerCase() === 'inherit') {
    return { value: { environment_id: environmentId, profile_id: null } }
  }
  const profileId = parsePositiveInt(profilePart)
  if (profileId === undefined) {
    return {
      error: `Invalid profile in --env "${value}". Use a profile ID or "inherit"`,
    }
  }
  return { value: { environment_id: environmentId, profile_id: profileId } }
}

/**
 * The PUT replaces the project default with whatever it is sent (omitting it
 * clears it), while listed environment overrides are upserted one by one. So a
 * change that only touches overrides must resend the current default, or it
 * would silently wipe it.
 */
export function buildDeliverySettingsUpdate(
  current: Pick<ProjectDeliverySettingsResponse, 'default_profile_id'>,
  defaultProfile: number | null | undefined,
  overrides: EnvironmentDeliveryOverride[]
): UpdateProjectDeliverySettingsRequest {
  return {
    default_profile_id:
      defaultProfile === undefined
        ? (current.default_profile_id ?? null)
        : defaultProfile,
    environment_overrides: overrides,
  }
}

/** `--adopt CNAME:www` explicitly adopts an existing record the preview flagged. */
export function parseAdoptRecord(
  value: string
): ParseResult<AdoptDeliveryRecord> {
  const separator = value.indexOf(':')
  const recordType =
    separator > 0 ? parseDnsRecordType(value.slice(0, separator)) : undefined
  const name = separator > 0 ? value.slice(separator + 1).trim() : ''
  if (!recordType || !name) {
    return {
      error: `Invalid --adopt "${value}". Use <TYPE>:<name>, e.g. CNAME:www or A:@`,
    }
  }
  return { value: { record_type: recordType, name } }
}

export function validatePreviewOptions(
  options: PreviewOptions
): ParseResult<PreviewDomainDeliveryBindingRequest> {
  const missing = [
    ['--environment-id', options.environmentId],
    ['--hostname', options.hostname],
    ['--zone', options.zone],
    ['--dns-provider', options.dnsProvider],
    ['--origin-target', options.originTarget],
  ]
    .filter(([, v]) => v === undefined || String(v).trim() === '')
    .map(([flag]) => flag)
  if (missing.length > 0) {
    return { error: `Missing required option(s): ${missing.join(', ')}` }
  }
  const environmentId = parsePositiveInt(options.environmentId as string)
  if (environmentId === undefined) {
    return {
      error: `Invalid --environment-id "${options.environmentId}". It must be a positive integer`,
    }
  }
  const dnsProviderId = parsePositiveInt(options.dnsProvider as string)
  if (dnsProviderId === undefined) {
    return {
      error: `Invalid --dns-provider "${options.dnsProvider}". It must be a DNS provider ID`,
    }
  }
  let profileId: number | undefined
  if (options.profile !== undefined) {
    profileId = parsePositiveInt(options.profile)
    if (profileId === undefined) {
      return {
        error: `Invalid --profile "${options.profile}". It must be a delivery profile ID`,
      }
    }
  }
  return {
    value: {
      environment_id: environmentId,
      dns_provider_id: dnsProviderId,
      hostname: (options.hostname as string).trim(),
      zone: (options.zone as string).trim(),
      origin_target: (options.originTarget as string).trim(),
      ...(profileId !== undefined && { delivery_profile_id: profileId }),
    },
  }
}

/**
 * Resolve a project slug or numeric ID to the numeric ID the delivery
 * endpoints take. Numeric input is used as-is.
 */
export async function resolveProjectRef(ref: string): Promise<number> {
  const id = parsePositiveInt(ref)
  if (id !== undefined) {
    return id
  }
  const { data, error: apiError } = await getProjectBySlug({
    client,
    path: { slug: ref },
  })
  if (apiError || !data) {
    throw new Error(getErrorMessage(apiError) || `Project "${ref}" not found`)
  }
  return data.id
}

async function resolveProjectId(
  flagValue?: string
): Promise<{ id: number; slug: string }> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(
      `Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`
    )
  }
  return { id: await resolveProjectRef(resolved.slug), slug: resolved.slug }
}

function profileLabel(
  id: number | null | undefined,
  profiles: DeliveryProfileResponse[]
): string {
  if (id === null || id === undefined) {
    return 'inherit'
  }
  const profile = profiles.find((p) => p.id === id)
  return profile
    ? `${profile.name} (#${id}, ${profile.provider_kind})`
    : `#${id}`
}

type SettingsProfileRefs = Pick<
  ProjectDeliverySettingsResponse,
  'default_profile_id' | 'effective_default_profile' | 'environment_overrides'
>

/** Every profile ID the settings refer to, once each, in ascending order. */
export function referencedProfileIds(settings: SettingsProfileRefs): number[] {
  const ids = new Set<number>()
  if (settings.default_profile_id != null) {
    ids.add(settings.default_profile_id)
  }
  for (const override of settings.environment_overrides) {
    if (override.profile_id != null) {
      ids.add(override.profile_id)
    }
  }
  return [...ids].sort((a, b) => a - b)
}

/**
 * The profiles `settings` refer to, for labelling them. The default arrives
 * with the settings; every other profile is fetched by ID, once, so the cost
 * follows the profiles in use rather than how many exist on the instance.
 * `fetchProfile` returns `undefined` for a profile that no longer exists,
 * which then prints as its bare `#id`.
 */
export async function resolveReferencedProfiles(
  settings: SettingsProfileRefs,
  fetchProfile: (id: number) => Promise<DeliveryProfileResponse | undefined>
): Promise<DeliveryProfileResponse[]> {
  const known = settings.effective_default_profile
    ? [settings.effective_default_profile]
    : []
  const missing = referencedProfileIds(settings).filter(
    (id) => !known.some((profile) => profile.id === id)
  )
  const fetched = await Promise.all(missing.map((id) => fetchProfile(id)))
  return [
    ...known,
    ...fetched.filter(
      (profile): profile is DeliveryProfileResponse => profile !== undefined
    ),
  ]
}

// --- Command registration ---

export function registerDeliveryCommands(program: Command): void {
  const delivery = program
    .command('delivery')
    .description(
      'Route project domains through a delivery provider (Cloudflare proxy, Bunny CDN)'
    )

  const settings = delivery
    .command('settings')
    .description(
      'Project default delivery profile and per-environment overrides'
    )

  settings
    .command('get')
    .alias('show')
    .description('Show the delivery profile a project and its environments use')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(settingsGetAction)

  settings
    .command('set')
    .description(
      'Change the project default profile and/or environment overrides'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--default-profile <id>',
      'Project default delivery profile ID, or "none" to clear it'
    )
    .option(
      '--env <environment-id=profile-id>',
      'Environment override, e.g. 12=3; use 12=inherit to fall back to the project default (repeatable)',
      collect,
      []
    )
    .option('--json', 'Output in JSON format')
    .action(settingsSetAction)

  const bindings = delivery
    .command('bindings')
    .alias('binding')
    .description(
      'Domain delivery bindings: the DNS record + provider routing for a hostname'
    )

  bindings
    .command('list')
    .alias('ls')
    .description(
      'List domain delivery bindings for a project, one page at a time (newest first)'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--page <n>', 'Page number (default: 1)')
    .option(
      '--page-size <n>',
      `Bindings per page, 1-${MAX_PAGE_SIZE} (default: ${DEFAULT_PAGE_SIZE})`
    )
    .option(
      '--sort-by <field>',
      `Sort field: ${BINDING_SORT_FIELDS.join(', ')} (default: created_at)`
    )
    .option('--sort-order <order>', 'asc or desc (default: desc)')
    .option('--json', 'Output the page as JSON (items, total, page, page_size)')
    .action(bindingsListAction)

  bindings
    .command('preview')
    .description(
      'Plan a delivery binding without changing DNS; prints a preview ID to apply'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Environment ID the hostname serves')
    .option('--hostname <hostname>', 'Hostname to route, e.g. app.example.com')
    .option(
      '--zone <zone>',
      'DNS zone that contains the hostname, e.g. example.com'
    )
    .option('--dns-provider <id>', 'DNS provider ID that manages the zone')
    .option(
      '--origin-target <target>',
      'Origin the record points at (IP for A/AAAA, hostname for CNAME)'
    )
    .option(
      '--profile <id>',
      'Delivery profile ID (defaults to the environment or project profile)'
    )
    .option('--json', 'Output in JSON format')
    .action(bindingsPreviewAction)

  bindings
    .command('apply')
    .description('Apply a previewed delivery binding (writes DNS)')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption(
      '--preview-id <id>',
      'Preview ID returned by `delivery bindings preview`'
    )
    .option(
      '--adopt <TYPE:name>',
      'Adopt an existing unmanaged record the preview flagged, e.g. CNAME:www (repeatable)',
      collect,
      []
    )
    .option('--json', 'Output in JSON format')
    .action(bindingsApplyAction)

  bindings
    .command('remove')
    .alias('rm')
    .alias('delete')
    .description('Remove a delivery binding and the DNS record it manages')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--id <id>', 'Binding ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(bindingsRemoveAction)
}

// --- Action implementations ---

/** One delivery profile by ID; `undefined` when it no longer exists. */
async function fetchProfileById(
  id: number
): Promise<DeliveryProfileResponse | undefined> {
  const {
    data,
    error: apiError,
    response,
  } = await getDeliveryProfile({ client, path: { profile_id: id } })
  if (response?.status === 404) {
    return undefined
  }
  if (apiError || !data) {
    throw new Error(
      getErrorMessage(apiError) || `Failed to fetch delivery profile ${id}`
    )
  }
  return data
}

/** What `printSettings` needs besides the settings themselves. */
interface SettingsProfiles {
  /** The profiles the settings refer to, for labels. */
  profiles: DeliveryProfileResponse[]
  /** True when the instance has no delivery profile at all. */
  noneExist: boolean
}

async function fetchSettingsProfiles(
  settings: ProjectDeliverySettingsResponse
): Promise<SettingsProfiles> {
  const profiles = await resolveReferencedProfiles(settings, fetchProfileById)
  if (referencedProfileIds(settings).length > 0) {
    return { profiles, noneExist: false }
  }
  // Nothing is referenced, so the smallest page is enough: its total says
  // whether any profile exists at all.
  const { data, error: apiError } = await listDeliveryProfiles({
    client,
    query: { page: 1, page_size: 1 },
  })
  if (apiError || !data) {
    throw new Error(
      getErrorMessage(apiError) || 'Failed to list delivery profiles'
    )
  }
  return { profiles, noneExist: data.total === 0 }
}

function printSettings(
  settings: ProjectDeliverySettingsResponse,
  { profiles, noneExist }: SettingsProfiles,
  slug: string
): void {
  newline()
  header(`${icons.info} Delivery settings for ${slug}`)
  keyValue(
    'Project default',
    settings.default_profile_id
      ? profileLabel(settings.default_profile_id, profiles)
      : colors.muted('none')
  )
  if (
    settings.effective_default_profile &&
    settings.effective_default_profile.id !== settings.default_profile_id
  ) {
    keyValue(
      'Effective default',
      profileLabel(settings.effective_default_profile.id, profiles)
    )
  }
  newline()

  if (settings.environment_overrides.length === 0) {
    info('No environment overrides; every environment uses the project default')
    info(
      'Run: temps delivery settings set -p <project> --env <environment-id>=<profile-id>'
    )
  } else {
    const columns: TableColumn<EnvironmentDeliveryOverride>[] = [
      { header: 'Environment ID', key: 'environment_id' },
      {
        header: 'Profile',
        accessor: (o) => profileLabel(o.profile_id, profiles),
      },
    ]
    printTable(settings.environment_overrides, columns, { style: 'minimal' })
  }

  if (noneExist) {
    newline()
    info(
      'No delivery profiles exist yet. Run: temps delivery-profiles capabilities'
    )
  }
  newline()
}

async function settingsGetAction(options: ProjectOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  const [settings, profiles] = await withSpinner(
    'Fetching delivery settings...',
    async () => {
      const { data, error: apiError } = await getProjectDeliverySettings({
        client,
        path: { project_id: project.id },
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Delivery settings for project ${project.slug} not found`
        )
      }
      return [data, await fetchSettingsProfiles(data)] as const
    }
  )

  if (options.json) {
    json(settings)
    return
  }

  printSettings(settings, profiles, project.slug)
}

async function settingsSetAction(options: SettingsSetOptions): Promise<void> {
  let defaultProfile: number | null | undefined
  if (options.defaultProfile !== undefined) {
    const parsed = parseDefaultProfile(options.defaultProfile)
    if ('error' in parsed) {
      error(parsed.error)
      process.exitCode = 1
      return
    }
    defaultProfile = parsed.value
  }

  const overrides: EnvironmentDeliveryOverride[] = []
  for (const raw of options.env) {
    const parsed = parseEnvironmentOverride(raw)
    if ('error' in parsed) {
      error(parsed.error)
      process.exitCode = 1
      return
    }
    overrides.push(parsed.value)
  }

  if (defaultProfile === undefined && overrides.length === 0) {
    error(
      'Nothing to update. Pass --default-profile <id|none> and/or --env <environment-id>=<profile-id|inherit>'
    )
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  const [updated, profiles] = await withSpinner(
    'Updating delivery settings...',
    async () => {
      const { data: current, error: getError } =
        await getProjectDeliverySettings({
          client,
          path: { project_id: project.id },
        })
      if (getError || !current) {
        throw new Error(
          getErrorMessage(getError) ||
            `Delivery settings for project ${project.slug} not found`
        )
      }
      const body = buildDeliverySettingsUpdate(
        current,
        defaultProfile,
        overrides
      )
      const { data, error: apiError } = await updateProjectDeliverySettings({
        client,
        path: { project_id: project.id },
        body,
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to update delivery settings for project ${project.slug}`
        )
      }
      return [data, await fetchSettingsProfiles(data)] as const
    }
  )

  if (options.json) {
    json(updated)
    return
  }

  success(`Delivery settings updated for ${project.slug}`)
  printSettings(updated, profiles, project.slug)
}

async function bindingsListAction(options: BindingsListOptions): Promise<void> {
  const paging = parseListPaging(options, BINDING_SORT_FIELDS)
  if ('error' in paging) {
    error(paging.error)
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  const result = await withSpinner(
    'Fetching delivery bindings...',
    async () => {
      const { data, error: apiError } = await listDomainDeliveryBindings({
        client,
        path: { project_id: project.id },
        query: paging.value,
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to list delivery bindings for project ${project.slug}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(
    `${icons.info} Delivery Bindings for ${project.slug} (${result.total})`
  )

  if (result.total === 0) {
    info('No domains are routed through a delivery provider')
    info(
      'Run: temps delivery bindings preview -p <project> --environment-id <id> --hostname <host> --zone <zone> --dns-provider <id> --origin-target <target>'
    )
    newline()
    return
  }

  if (result.items.length === 0) {
    info(pastLastPageMessage(result, 'binding'))
    newline()
    return
  }

  const columns: TableColumn<DomainDeliveryBindingResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Hostname', key: 'hostname', color: (v) => colors.bold(v) },
    { header: 'Env', key: 'environment_id' },
    {
      header: 'Profile',
      accessor: (b) => `${b.delivery_profile_name} (${b.provider_kind})`,
    },
    {
      header: 'Record',
      accessor: (b) =>
        `${b.record_type} -> ${b.origin_target}${b.proxied ? ' (proxied)' : ''}`,
    },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v) },
    {
      header: 'Last Error',
      accessor: (b) => b.last_error || '-',
      color: (v) => (v === '-' ? colors.muted(v) : colors.error(v)),
    },
  ]

  printTable(result.items, columns, { style: 'minimal' })
  newline()
  info(pageFooter(result, 'binding'))
  newline()
}

async function bindingsPreviewAction(options: PreviewOptions): Promise<void> {
  const validated = validatePreviewOptions(options)
  if ('error' in validated) {
    error(validated.error)
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  const preview = await withSpinner(
    `Planning delivery for ${validated.value.hostname}...`,
    async () => {
      const { data, error: apiError } = await previewDomainDeliveryBinding({
        client,
        path: { project_id: project.id },
        body: validated.value,
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to preview delivery for ${validated.value.hostname}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(preview)
    return
  }

  newline()
  header(`${icons.info} Delivery preview for ${validated.value.hostname}`)
  keyValue('Preview ID', preview.preview_id)
  keyValue('Expires', new Date(preview.expires_at).toLocaleString())
  keyValue('Provider', preview.provider_kind)
  keyValue('Profile', `#${preview.profile_id} (from ${preview.profile_source})`)
  keyValue(
    'DNS record',
    `${preview.record.record_type} ${preview.record.name} -> ${preview.record.value}${preview.record.proxied ? ' (proxied)' : ''}`
  )
  keyValue('Record ownership', preview.record.ownership_status)
  keyValue(
    'Custom domain',
    preview.routing.will_create_custom_domain
      ? 'will be created'
      : `existing #${preview.routing.custom_domain_id ?? '-'}`
  )
  keyValue('Origin TLS', preview.origin_tls)
  for (const message of preview.warnings) {
    warning(message)
  }
  newline()

  const adopt = preview.record.requires_adoption
    ? ` --adopt ${preview.record.record_type}:${preview.record.name}`
    : ''
  if (preview.record.requires_adoption) {
    warning(
      `An existing ${preview.record.record_type} record "${preview.record.name}" is not managed by Temps; applying replaces it only if you adopt it explicitly.`
    )
  }
  info(
    `Apply with: temps delivery bindings apply -p ${project.slug} --preview-id ${preview.preview_id}${adopt}`
  )
  newline()
}

async function bindingsApplyAction(options: ApplyOptions): Promise<void> {
  const adoptRecords: AdoptDeliveryRecord[] = []
  for (const raw of options.adopt) {
    const parsed = parseAdoptRecord(raw)
    if ('error' in parsed) {
      error(parsed.error)
      process.exitCode = 1
      return
    }
    adoptRecords.push(parsed.value)
  }

  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  const binding = await withSpinner(
    'Applying delivery binding...',
    async () => {
      const { data, error: apiError } = await applyDomainDeliveryBinding({
        client,
        path: { project_id: project.id },
        body: {
          preview_id: options.previewId,
          ...(adoptRecords.length > 0 && { adopt_records: adoptRecords }),
        },
      })
      if (apiError || !data) {
        throw new Error(
          getErrorMessage(apiError) ||
            `Failed to apply delivery preview ${options.previewId}`
        )
      }
      return data
    }
  )

  if (options.json) {
    json(binding)
    return
  }

  success(
    `${binding.hostname} now routes through ${binding.delivery_profile_name} (${binding.provider_kind})`
  )
  keyValue('Binding ID', binding.id)
  keyValue(
    'Record',
    `${binding.record_type} -> ${binding.origin_target}${binding.proxied ? ' (proxied)' : ''}`
  )
  keyValue('Status', statusBadge(binding.status))
  if (binding.last_error) {
    keyValue('Last Error', colors.error(binding.last_error))
  }
}

async function bindingsRemoveAction(options: RemoveOptions): Promise<void> {
  const id = parsePositiveInt(options.id)
  if (id === undefined) {
    error(`Invalid binding ID "${options.id}"`)
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  const project = await resolveProjectId(options.project)

  if (!(options.force || options.yes)) {
    const confirmed = await promptConfirm({
      message: `Remove delivery binding #${id} from ${project.slug}? The DNS record it manages will be deleted.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing delivery binding...', async () => {
    const { error: apiError } = await deleteDomainDeliveryBinding({
      client,
      path: { project_id: project.id, binding_id: id },
    })
    if (apiError) {
      throw new Error(getErrorMessage(apiError))
    }
  })

  success(`Delivery binding #${id} removed from ${project.slug}`)
}
