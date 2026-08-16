import type { Command } from 'commander'
import chalk from 'chalk'
import { cloudFetch, cloudFetchPublic } from '../../lib/cloud-client.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import {
  newline,
  header,
  keyValue,
  icons,
  json as jsonOutput,
  colors,
  box,
  info,
} from '../../ui/output.js'
import { promptSelect, promptConfirm } from '../../ui/prompts.js'

// --- Types ---

interface VpsInstance {
  id: string
  serverType: string
  status: string
  ipv4: string
  hostname: string
  isFreeVps: boolean
  monthlyPriceCents: number
  webPanelUrl: string
}

interface VpsLog {
  phase: string
  step: string
  status: string
  message: string
}

interface VpsServerSpecs {
  cores: number
  memory: number
  disk: number
}

interface VpsImage {
  id: string
  name: string
  description: string
  deprecated: boolean
}

interface VpsLocation {
  id: string
  name: string
  city: string
  country: string
  description: string
}

interface VpsServerType {
  id: string
  name: string
  cores: number
  memory: number
  disk: number
  monthlyPriceCents: number
  available: boolean
}

interface VpsCredentials {
  webPanelUrl: string
  credentials: {
    username: string
    password: string
  }
  apiKey?: string
}


// --- Helpers ---

export function vpsStatusBadge(status: string): string {
  const statusMap: Record<string, (s: string) => string> = {
    provisioning: chalk.yellow,
    installing: chalk.yellow,
    stalling: chalk.yellow,
    destroying: chalk.yellow,
    active: chalk.green,
    error: chalk.red,
    destroyed: chalk.gray,
  }
  const colorFn = statusMap[status.toLowerCase()] ?? chalk.white
  return colorFn(`● ${status}`)
}

export function formatPrice(cents: number): string {
  if (cents === 0) return colors.success('free')
  return `€${(cents / 100).toFixed(2)}/mo`
}

// --- Commands ---

async function vpsList(options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching VPS instances...', () =>
    cloudFetch<{ instances: VpsInstance[] }>('/api/vps')
  )

  const instances = data.instances ?? []

  if (options.json) {
    jsonOutput(instances)
    return
  }

  newline()
  header(`${icons.rocket} VPS Instances (${instances.length})`)

  const columns: TableColumn<VpsInstance>[] = [
    { header: 'ID', key: 'id' },
    { header: 'Hostname', key: 'hostname', color: (v) => colors.bold(v) },
    { header: 'Status', accessor: (i) => i.status, color: (v) => vpsStatusBadge(v) },
    { header: 'IPv4', key: 'ipv4' },
    { header: 'Type', key: 'serverType' },
    { header: 'Price', accessor: (i) => formatPrice(i.monthlyPriceCents) },
  ]

  printTable(instances, columns, { style: 'minimal' })
  newline()
}

async function vpsCreate(options: {
  image?: string
  location?: string
  type?: string
  json?: boolean
}): Promise<void> {
  let imageId = options.image
  let locationId = options.location
  let serverTypeId = options.type

  // Interactive wizard if flags not provided
  if (!imageId) {
    const data = await withSpinner('Fetching available images...', () =>
      cloudFetchPublic<{ images: VpsImage[] }>('/api/vps/images')
    )
    const images = (data.images ?? []).filter((i) => !i.deprecated)

    imageId = await promptSelect<string>({
      message: 'Select an OS image',
      choices: images.map((img) => ({
        name: `${img.name}${img.description ? ` — ${img.description}` : ''}`,
        value: img.id,
      })),
    })
  }

  if (!locationId) {
    const data = await withSpinner('Fetching locations...', () =>
      cloudFetchPublic<{ locations: VpsLocation[] }>('/api/vps/locations')
    )
    const locations = data.locations ?? []

    locationId = await promptSelect<string>({
      message: 'Select a datacenter location',
      choices: locations.map((loc) => ({
        name: `${loc.name} — ${loc.city}, ${loc.country}`,
        value: loc.id,
        description: loc.description,
      })),
    })
  }

  if (!serverTypeId) {
    const data = await withSpinner('Fetching server types...', () =>
      cloudFetchPublic<{ serverTypes: VpsServerType[] }>(
        `/api/vps/server-types?location=${locationId}`
      )
    )
    const types = (data.serverTypes ?? []).filter((t) => t.available)

    serverTypeId = await promptSelect<string>({
      message: 'Select a server type',
      choices: types.map((t) => ({
        name: `${t.name} — ${t.cores} vCPU, ${t.memory}GB RAM, ${t.disk}GB disk — ${formatPrice(t.monthlyPriceCents)}`,
        value: t.id,
      })),
    })
  }

  const result = await withSpinner('Provisioning VPS...', () =>
    cloudFetch<{ instanceId: string; hostname: string; message: string }>('/api/vps', {
      method: 'POST',
      body: JSON.stringify({
        serverType: serverTypeId,
        image: imageId,
        location: locationId,
      }),
    })
  )

  if (options.json) {
    jsonOutput(result)
    return
  }

  newline()
  box(
    `Instance: ${colors.bold(result.instanceId)}\n` +
      `Hostname: ${colors.bold(result.hostname)}\n\n` +
      `${result.message}`,
    `${icons.rocket} VPS Provisioning Started`
  )
  newline()
  info(`Track progress: ${colors.muted(`temps cloud vps show ${result.instanceId}`)}`)
  newline()
}

async function vpsShow(id: string, options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching VPS details...', () =>
    cloudFetch<{ instance: VpsInstance; logs: VpsLog[]; serverSpecs: VpsServerSpecs }>(
      `/api/vps/${id}`
    )
  )

  // Fetch credentials if instance is active
  let creds: VpsCredentials | null = null
  if (data.instance.status === 'active') {
    try {
      creds = await withSpinner('Fetching credentials...', () =>
        cloudFetch<VpsCredentials>(`/api/vps/${id}/credentials`)
      )
    } catch {
      // Credentials not available
    }
  }

  if (options.json) {
    jsonOutput({ ...data, credentials: creds })
    return
  }

  const inst = data.instance

  newline()
  header(`${icons.rocket} VPS: ${inst.hostname}`)
  keyValue('ID', inst.id)
  keyValue('Hostname', inst.hostname)
  keyValue('Status', vpsStatusBadge(inst.status))
  keyValue('IPv4', inst.ipv4 || 'pending')
  keyValue('Type', inst.serverType)
  keyValue('Price', formatPrice(inst.monthlyPriceCents))
  if (inst.webPanelUrl) {
    keyValue('Panel', inst.webPanelUrl)
  }

  if (creds) {
    newline()
    header(`${icons.key} Credentials`)
    keyValue('Panel URL', creds.webPanelUrl)
    keyValue('Username', creds.credentials.username)
    keyValue('Password', creds.credentials.password)
    if (creds.apiKey) {
      keyValue('API Key', creds.apiKey)
    }
  }

  if (data.serverSpecs) {
    newline()
    header('Server Specs')
    keyValue('vCPU', data.serverSpecs.cores)
    keyValue('Memory', `${data.serverSpecs.memory} GB`)
    keyValue('Disk', `${data.serverSpecs.disk} GB`)
  }

  if (data.logs && data.logs.length > 0) {
    newline()
    header('Provisioning Logs')

    const logColumns: TableColumn<VpsLog>[] = [
      { header: 'Phase', key: 'phase' },
      { header: 'Step', key: 'step' },
      {
        header: 'Status',
        key: 'status',
        color: (v) => statusBadge(v),
      },
      { header: 'Message', key: 'message' },
    ]

    printTable(data.logs, logColumns, { style: 'minimal' })
  }

  newline()
}

async function vpsDestroy(id: string): Promise<void> {
  const confirmed = await promptConfirm({
    message: `Are you sure you want to destroy VPS ${colors.bold(id)}? This cannot be undone.`,
    default: false,
  })

  if (!confirmed) {
    info('Cancelled.')
    return
  }

  await withSpinner('Destroying VPS...', () =>
    cloudFetch<unknown>(`/api/vps/${id}`, { method: 'DELETE' })
  )

  newline()
  info(`VPS ${colors.bold(id)} destruction initiated.`)
  newline()
}

async function vpsRetry(id: string): Promise<void> {
  const result = await withSpinner('Retrying provisioning...', () =>
    cloudFetch<{ message: string }>(`/api/vps/${id}/retry`, { method: 'POST' })
  )

  newline()
  info(result.message || 'Provisioning retry initiated.')
  newline()
}

async function vpsCredentials(id: string, options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching credentials...', () =>
    cloudFetch<VpsCredentials>(`/api/vps/${id}/credentials`)
  )

  if (options.json) {
    jsonOutput(data)
    return
  }

  newline()
  header(`${icons.key} VPS Credentials`)
  keyValue('Panel URL', data.webPanelUrl)
  keyValue('Username', data.credentials.username)
  keyValue('Password', data.credentials.password)
  if (data.apiKey) {
    keyValue('API Key', data.apiKey)
  }
  newline()
  info(colors.warning('Store these credentials securely. They will not be shown again.'))
  newline()
}

async function vpsImages(options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching images...', () =>
    cloudFetchPublic<{ images: VpsImage[] }>('/api/vps/images')
  )

  const images = data.images ?? []

  if (options.json) {
    jsonOutput(images)
    return
  }

  newline()
  header(`${icons.package} Available OS Images (${images.length})`)

  const columns: TableColumn<VpsImage>[] = [
    { header: 'ID', key: 'id' },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Description', key: 'description' },
    {
      header: 'Status',
      accessor: (i) => (i.deprecated ? 'deprecated' : 'available'),
      color: (v, i) => (i.deprecated ? colors.muted(v) : colors.success(v)),
    },
  ]

  printTable(images, columns, { style: 'minimal' })
  newline()
}

async function vpsLocations(options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching locations...', () =>
    cloudFetchPublic<{ locations: VpsLocation[] }>('/api/vps/locations')
  )

  const locations = data.locations ?? []

  if (options.json) {
    jsonOutput(locations)
    return
  }

  newline()
  header(`${icons.globe} Available Locations (${locations.length})`)

  const columns: TableColumn<VpsLocation>[] = [
    { header: 'ID', key: 'id' },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'City', key: 'city' },
    { header: 'Country', key: 'country' },
    { header: 'Description', key: 'description' },
  ]

  printTable(locations, columns, { style: 'minimal' })
  newline()
}

async function vpsTypes(options: { location?: string; json?: boolean }): Promise<void> {
  const query = options.location ? `?location=${options.location}` : ''
  const data = await withSpinner('Fetching server types...', () =>
    cloudFetchPublic<{ serverTypes: VpsServerType[] }>(`/api/vps/server-types${query}`)
  )

  const types = data.serverTypes ?? []

  if (options.json) {
    jsonOutput(types)
    return
  }

  newline()
  const suffix = options.location ? ` for ${options.location}` : ''
  header(`Server Types${suffix} (${types.length})`)

  const columns: TableColumn<VpsServerType>[] = [
    { header: 'ID', key: 'id' },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'vCPU', key: 'cores', align: 'right' },
    { header: 'Memory (GB)', key: 'memory', align: 'right' },
    { header: 'Disk (GB)', key: 'disk', align: 'right' },
    { header: 'Price', accessor: (t) => formatPrice(t.monthlyPriceCents), align: 'right' },
    {
      header: 'Available',
      accessor: (t) => (t.available ? 'yes' : 'no'),
      color: (v, t) => (t.available ? colors.success(v) : colors.muted(v)),
    },
  ]

  printTable(types, columns, { style: 'minimal' })
  newline()
}

// --- Registration ---

export function registerCloudVpsCommands(cloud: Command): void {
  const vps = cloud
    .command('vps')
    .description('Manage cloud VPS instances')

  vps
    .command('list')
    .description('List VPS instances')
    .option('--json', 'Output as JSON')
    .action(vpsList)

  vps
    .command('create')
    .description('Provision a new VPS instance')
    .option('--image <image>', 'OS image ID')
    .option('--location <location>', 'Datacenter location ID')
    .option('--type <type>', 'Server type ID')
    .option('--json', 'Output as JSON')
    .action(vpsCreate)

  vps
    .command('show <id>')
    .description('Show VPS instance details and provisioning logs')
    .option('--json', 'Output as JSON')
    .action(vpsShow)

  vps
    .command('destroy <id>')
    .description('Destroy a VPS instance')
    .action(vpsDestroy)

  vps
    .command('retry <id>')
    .description('Retry failed VPS provisioning')
    .action(vpsRetry)

  vps
    .command('credentials <id>')
    .description('Show VPS panel credentials')
    .option('--json', 'Output as JSON')
    .action(vpsCredentials)

  vps
    .command('images')
    .description('List available OS images')
    .option('--json', 'Output as JSON')
    .action(vpsImages)

  vps
    .command('locations')
    .description('List available datacenter locations')
    .option('--json', 'Output as JSON')
    .action(vpsLocations)

  vps
    .command('types')
    .description('List available server types with pricing')
    .option('--location <location>', 'Filter by datacenter location')
    .option('--json', 'Output as JSON')
    .action(vpsTypes)
}
