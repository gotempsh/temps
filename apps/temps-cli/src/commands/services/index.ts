import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listServices,
  createService,
  getService,
  deleteService,
  startService,
  stopService,
  getServiceTypes,
  getServiceTypeParameters,
  listServiceProjects,
  updateService,
  upgradeService,
  importExternalService,
  linkServiceToProject,
  unlinkServiceFromProject,
  getServiceEnvironmentVariables,
  getServiceEnvironmentVariable,
  getProjectBySlug,
} from '../../api/sdk.gen.js'
import type { ExternalServiceInfo, ServiceTypeRoute } from '../../api/types.gen.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

const SERVICE_TYPE_LABELS: Record<ServiceTypeRoute, string> = {
  postgres: 'PostgreSQL',
  mongodb: 'MongoDB',
  redis: 'Redis',
  s3: 'MinIO (S3)',
}

// Default parameters for each service type when using automation mode (-y)
// These match the backend's required fields + sensible defaults
const SERVICE_TYPE_DEFAULTS: Record<string, Record<string, unknown>> = {
  postgres: { database: 'myapp', username: 'postgres' },
  mongodb: { database: 'myapp', username: 'mongoadmin' },
  redis: {},
  s3: {},
}

// JSON Schema → interactive prompt parameters
interface SchemaProperty {
  type?: string
  description?: string
  default?: unknown
  example?: unknown
  enum?: string[]
}

interface JsonSchema {
  type?: string
  title?: string
  properties?: Record<string, SchemaProperty>
  required?: string[]
  readonly?: string[]
}

interface PromptParam {
  name: string
  label: string
  description?: string
  default_value?: unknown
  required: boolean
  readonly: boolean
  enum_values?: string[]
  param_type: string
}

function schemaToPromptParams(schema: JsonSchema): PromptParam[] {
  if (!schema?.properties) return []
  const required = new Set(schema.required ?? [])
  const readonly = new Set(schema.readonly ?? [])
  return Object.entries(schema.properties).map(([name, prop]) => ({
    name,
    label: name.replace(/_/g, ' ').replace(/\b\w/g, c => c.toUpperCase()),
    description: prop.description,
    default_value: prop.default ?? prop.example,
    required: required.has(name),
    readonly: readonly.has(name),
    enum_values: prop.enum,
    param_type: prop.type ?? 'string',
  }))
}

/**
 * Parse repeatable --set key=value pairs into a Record.
 * Supports type coercion: numbers → number, true/false → boolean, rest → string.
 */
function parseSetPairs(pairs: string[]): Record<string, unknown> {
  const result: Record<string, unknown> = {}
  for (const pair of pairs) {
    const eqIdx = pair.indexOf('=')
    if (eqIdx === -1) {
      throw new Error(`Invalid parameter "${pair}". Expected format: key=value`)
    }
    const key = pair.slice(0, eqIdx).trim()
    const raw = pair.slice(eqIdx + 1)
    if (!key) {
      throw new Error(`Invalid parameter "${pair}". Key cannot be empty`)
    }
    // Type coercion
    if (raw === 'true') result[key] = true
    else if (raw === 'false') result[key] = false
    else if (raw === '0') result[key] = 0
    else if (raw !== '' && !isNaN(Number(raw)) && !raw.startsWith('0')) result[key] = Number(raw)
    else result[key] = raw
  }
  return result
}

/** Collect repeatable --set values into an array */
function collectSet(value: string, previous: string[]): string[] {
  return previous.concat([value])
}

interface CreateOptions {
  type?: string
  name?: string
  set?: string[]
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface StartStopOptions {
  id: string
}

interface ProjectsOptions {
  id: string
  json?: boolean
}

interface UpdateOptions {
  id: string
  name?: string
  set?: string[]
}

interface UpgradeOptions {
  id: string
  version?: string
}

interface ImportOptions {
  type?: string
  name?: string
  containerId?: string
  set?: string[]
  version?: string
  yes?: boolean
}

interface LinkOptions {
  id: string
  project?: string
}

interface UnlinkOptions {
  id: string
  project?: string
  force?: boolean
  yes?: boolean
}

interface EnvOptions {
  id: string
  project?: string
  json?: boolean
}

interface EnvVarOptions {
  id: string
  project?: string
  var: string
  json?: boolean
}

/** Resolve project slug (from flag, .temps/config.json, env, global) → project ID */
async function resolveProjectId(flagValue?: string): Promise<{ id: number; slug: string }> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { id: data.id, slug: resolved.slug }
}

export function registerServicesCommands(program: Command): void {
  const services = program
    .command('services')
    .alias('svc')
    .description('Manage external services (databases, caches, storage)')

  services
    .command('list')
    .alias('ls')
    .description('List all external services')
    .option('--json', 'Output in JSON format')
    .action(listServicesAction)

  services
    .command('create')
    .alias('add')
    .description('Create a new external service')
    .option('-t, --type <type>', 'Service type (postgres, mongodb, redis, s3)')
    .option('-n, --name <name>', 'Service name')
    .option('-s, --set <key=value>', 'Set a parameter (repeatable)', collectSet, [])
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createServiceAction)

  services
    .command('show')
    .description('Show service details')
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(showService)

  services
    .command('remove')
    .alias('rm')
    .description('Remove a service')
    .requiredOption('--id <id>', 'Service ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeService)

  services
    .command('start')
    .description('Start a stopped service')
    .requiredOption('--id <id>', 'Service ID')
    .action(startServiceAction)

  services
    .command('stop')
    .description('Stop a running service')
    .requiredOption('--id <id>', 'Service ID')
    .action(stopServiceAction)

  const typesCmd = services
    .command('types')
    .description('List available service types')
    .option('--json', 'Output in JSON format')
    .action(listServiceTypes)

  typesCmd
    .command('info <type>')
    .description('Show parameters schema for a service type (useful for automation)')
    .option('--json', 'Output as raw JSON schema (default)')
    .action(showServiceTypeInfo)

  services
    .command('projects')
    .description('List projects linked to a service')
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(listLinkedProjects)

  services
    .command('update')
    .description('Update a service')
    .requiredOption('--id <id>', 'Service ID')
    .option('-n, --name <name>', 'Docker image name (e.g., postgres:18-alpine)')
    .option('-s, --set <key=value>', 'Set a parameter (repeatable)', collectSet, [])
    .action(updateServiceAction)

  services
    .command('upgrade')
    .description('Upgrade a service to a newer version')
    .requiredOption('--id <id>', 'Service ID')
    .option('-v, --version <version>', 'Docker image to upgrade to (e.g., postgres:18-alpine)')
    .action(upgradeServiceAction)

  services
    .command('import')
    .description('Import an existing external service')
    .option('-t, --type <type>', 'Service type (postgres, mongodb, redis, s3)')
    .option('-n, --name <name>', 'Service name')
    .option('--container-id <id>', 'Container ID or name to import')
    .option('-s, --set <key=value>', 'Set a parameter (repeatable)', collectSet, [])
    .option('--version <version>', 'Optional version override')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(importServiceAction)

  services
    .command('link')
    .description('Link a service to a project')
    .requiredOption('--id <id>', 'Service ID')
    .option('-p, --project <slug>', 'Project slug (auto-detected from .temps/config.json)')
    .action(linkServiceAction)

  services
    .command('unlink')
    .description('Unlink a service from a project')
    .requiredOption('--id <id>', 'Service ID')
    .option('-p, --project <slug>', 'Project slug (auto-detected from .temps/config.json)')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(unlinkServiceAction)

  services
    .command('env')
    .description('Show environment variables for a linked service')
    .requiredOption('--id <id>', 'Service ID')
    .option('-p, --project <slug>', 'Project slug (auto-detected from .temps/config.json)')
    .option('--json', 'Output in JSON format')
    .action(envAction)

  services
    .command('env-var')
    .description('Get a specific environment variable')
    .requiredOption('--id <id>', 'Service ID')
    .option('-p, --project <slug>', 'Project slug (auto-detected from .temps/config.json)')
    .requiredOption('--var <name>', 'Environment variable name')
    .option('--json', 'Output in JSON format')
    .action(envVarAction)
}

async function listServicesAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const services = await withSpinner('Fetching services...', async () => {
    const { data, error } = await listServices({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(services)
    return
  }

  newline()
  header(`${icons.info} External Services (${services.length})`)

  if (services.length === 0) {
    info('No external services configured')
    info('Run: temps services create --type postgres --name my-db')
    newline()
    return
  }

  const columns: TableColumn<ExternalServiceInfo>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', accessor: (s) => SERVICE_TYPE_LABELS[s.service_type] || s.service_type },
    { header: 'Version', accessor: (s) => s.version || '-' },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v === 'running' ? 'active' : v === 'stopped' ? 'inactive' : 'pending') },
  ]

  printTable(services, columns, { style: 'minimal' })
  newline()
}

async function createServiceAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Get available service types
  const types = await withSpinner('Fetching service types...', async () => {
    const { data, error } = await getServiceTypes({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (types.length === 0) {
    warning('No service types available')
    return
  }

  let serviceType: ServiceTypeRoute
  let name: string
  let parameters: Record<string, unknown> = {}

  const hasSetParams = options.set && options.set.length > 0

  // Automation mode: -y with type+name, OR type+name+set (explicit params = no need for -y)
  const isAutomation = (options.yes && options.type && options.name) ||
    (options.type && options.name && hasSetParams)

  if (isAutomation) {
    // Validate service type
    if (!types.includes(options.type as ServiceTypeRoute)) {
      warning(`Invalid service type: ${options.type}. Available: ${types.join(', ')}`)
      return
    }
    serviceType = options.type as ServiceTypeRoute
    name = options.name!

    // Parse --set key=value pairs if provided, otherwise use smart defaults
    if (hasSetParams) {
      try {
        parameters = parseSetPairs(options.set!)
      } catch (e) {
        warning((e as Error).message)
        return
      }
    } else {
      // Apply default parameters for this service type (e.g., database/username for postgres)
      parameters = { ...(SERVICE_TYPE_DEFAULTS[serviceType] ?? {}) }
    }
  } else {
    // Interactive mode — use --type and --name if provided, prompt for the rest
    if (options.type) {
      if (!types.includes(options.type as ServiceTypeRoute)) {
        warning(`Invalid service type: ${options.type}. Available: ${types.join(', ')}`)
        return
      }
      serviceType = options.type as ServiceTypeRoute
      info(`Service type: ${colors.bold(SERVICE_TYPE_LABELS[serviceType] || serviceType)}`)
    } else {
      serviceType = await promptSelect({
        message: 'Service type',
        choices: types.map(t => ({
          name: SERVICE_TYPE_LABELS[t] || t,
          value: t,
        })),
      }) as ServiceTypeRoute
    }

    if (options.name) {
      name = options.name
    } else {
      name = await promptText({
        message: 'Service name',
        default: `my-${serviceType}`,
        required: true,
      })
    }

    // Get parameters schema for the service type (returns JSON Schema)
    const { data: typeInfo } = await getServiceTypeParameters({
      client,
      path: { service_type: serviceType },
    })

    const schema = typeInfo as JsonSchema | undefined
    const promptParams = schemaToPromptParams(schema ?? {})
    // Only show user-configurable params (skip readonly ones the backend auto-generates)
    const configurableParams = promptParams.filter(p => !p.readonly || p.required)

    if (configurableParams.length > 0) {
      info(`\nConfigure ${SERVICE_TYPE_LABELS[serviceType] || serviceType} parameters:`)
      newline()

      for (const param of configurableParams) {
        // Skip non-required params that have defaults — use the default automatically
        if (param.default_value !== undefined && !param.required) {
          const useDefault = await promptConfirm({
            message: `${param.label}${param.description ? ` (${param.description})` : ''}: Use default "${param.default_value}"?`,
            default: true,
          })
          if (useDefault) {
            parameters[param.name] = param.default_value
            continue
          }
        }

        let value: string | undefined

        if (param.enum_values && param.enum_values.length > 0) {
          value = await promptSelect({
            message: param.label,
            choices: param.enum_values.map((v: string) => ({ name: v, value: v })),
          })
        } else {
          value = await promptText({
            message: `${param.label}${param.description ? ` (${param.description})` : ''}`,
            default: param.default_value?.toString() ?? '',
            required: param.required,
          })
        }

        if (value) {
          if (param.param_type === 'integer' || param.param_type === 'number') {
            parameters[param.name] = parseInt(value, 10)
          } else if (param.param_type === 'boolean') {
            parameters[param.name] = value.toLowerCase() === 'true'
          } else {
            parameters[param.name] = value
          }
        }
      }
    }
  }

  await withSpinner(`Creating ${SERVICE_TYPE_LABELS[serviceType] || serviceType} service...`, async () => {
    const { error } = await createService({
      client,
      body: {
        name,
        service_type: serviceType,
        parameters,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`${SERVICE_TYPE_LABELS[serviceType] || serviceType} service "${name}" created successfully`)
  info('The service is starting up...')
  info('Run: temps services list to check the status')
}

async function showService(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const details = await withSpinner('Fetching service details...', async () => {
    const { data, error } = await getService({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Service ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(details)
    return
  }

  const service = details.service
  newline()
  header(`${icons.info} ${service.name}`)
  keyValue('ID', service.id)
  keyValue('Type', SERVICE_TYPE_LABELS[service.service_type] || service.service_type)
  keyValue('Version', service.version || 'N/A')
  keyValue('Status', statusBadge(service.status === 'running' ? 'active' : service.status === 'stopped' ? 'inactive' : 'pending'))
  if (service.connection_info) {
    keyValue('Connection', colors.muted(service.connection_info))
  }
  keyValue('Created', new Date(service.created_at).toLocaleString())
  keyValue('Updated', new Date(service.updated_at).toLocaleString())

  if (details.current_parameters && Object.keys(details.current_parameters).length > 0) {
    newline()
    header('Parameters')
    for (const [key, value] of Object.entries(details.current_parameters)) {
      keyValue(key, value)
    }
  }
  newline()
}

async function removeService(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  // Get service details first
  const { data: details, error: getError } = await getService({
    client,
    path: { id },
  })

  if (getError || !details) {
    warning(`Service ${options.id} not found`)
    return
  }

  const service = details.service
  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning(`This will permanently delete the service and all its data!`)
    const confirmed = await promptConfirm({
      message: `Remove service "${service.name}" (${SERVICE_TYPE_LABELS[service.service_type] || service.service_type})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing service...', async () => {
    const { error } = await deleteService({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Service removed')
}

async function startServiceAction(options: StartStopOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  await withSpinner('Starting service...', async () => {
    const { error } = await startService({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Service started')
  info(`Run: temps services show --id ${options.id} to check the status`)
}

async function stopServiceAction(options: StartStopOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  await withSpinner('Stopping service...', async () => {
    const { error } = await stopService({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Service stopped')
}

async function listServiceTypes(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const types = await withSpinner('Fetching service types...', async () => {
    const { data, error } = await getServiceTypes({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(types)
    return
  }

  newline()
  header(`${icons.info} Available Service Types`)

  for (const t of types) {
    console.log(`  ${colors.bold(SERVICE_TYPE_LABELS[t] || t)} ${colors.muted(`(${t})`)}`)
  }
  newline()
  info(`Run ${colors.bold('services types info <type>')} to see parameters for a specific type`)
}

/** Build an example `services create` command using --set flags with all schema defaults */
function buildExampleCommand(type: string, schema?: JsonSchema): string {
  const setParts: string[] = []
  if (schema?.properties) {
    for (const [key, prop] of Object.entries(schema.properties)) {
      // Skip params with null defaults (auto-generated like password, port)
      if (prop.default === null || prop.default === undefined) continue
      setParts.push(`--set ${key}=${prop.default}`)
    }
  }
  const setsStr = setParts.length > 0 ? ` ${setParts.join(' ')}` : ''
  return `bunx @temps-sdk/cli services create -t ${type} -n my-${type}${setsStr}`
}

async function showServiceTypeInfo(type: string): Promise<void> {
  await requireAuth()
  await setupClient()

  const { data, error } = await getServiceTypeParameters({
    client,
    path: { service_type: type as ServiceTypeRoute },
  })

  if (error) {
    warning(`Failed to get parameters for "${type}": ${getErrorMessage(error)}`)
    return
  }

  const schema = data as JsonSchema | undefined
  if (!schema?.properties) {
    json({ type, parameters: {}, defaults: SERVICE_TYPE_DEFAULTS[type] ?? {} })
    return
  }

  // Build a clean output for agents: each parameter with its metadata
  const params: Record<string, {
    type: string
    description?: string
    required: boolean
    readonly: boolean
    default?: unknown
    example?: unknown
  }> = {}

  const requiredKeys = new Set(schema.required ?? [])
  const readonlyKeys = new Set(schema.readonly ?? [])

  for (const [name, prop] of Object.entries(schema.properties)) {
    params[name] = {
      type: prop.type ?? 'string',
      ...(prop.description ? { description: prop.description } : {}),
      required: requiredKeys.has(name),
      readonly: readonlyKeys.has(name),
      ...(prop.default !== undefined ? { default: prop.default } : {}),
      ...(prop.example !== undefined ? { example: prop.example } : {}),
    }
  }

  const output = {
    service_type: type,
    label: SERVICE_TYPE_LABELS[type as ServiceTypeRoute] || type,
    parameters: params,
    defaults: SERVICE_TYPE_DEFAULTS[type] ?? {},
    example_create: buildExampleCommand(type, schema),
  }

  json(output)
}

async function listLinkedProjects(options: ProjectsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const projects = await withSpinner('Fetching linked projects...', async () => {
    const { data, error } = await listServiceProjects({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(projects)
    return
  }

  newline()
  header(`${icons.info} Linked Projects (${projects.length})`)

  if (projects.length === 0) {
    info('No projects linked to this service')
    newline()
    return
  }

  for (const link of projects) {
    console.log(`  ${colors.bold(link.project.slug)} ${colors.muted(`(ID: ${link.project.id})`)}`)
  }
  newline()
}

async function updateServiceAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  let parameters: Record<string, unknown> = {}
  if (options.set && options.set.length > 0) {
    try {
      parameters = parseSetPairs(options.set)
    } catch (e) {
      warning((e as Error).message)
      return
    }
  }

  await withSpinner('Updating service...', async () => {
    const { error } = await updateService({
      client,
      path: { id },
      body: {
        docker_image: options.name ?? null,
        parameters,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Service updated')
  info(`Run: temps services show --id ${options.id} to check the details`)
}

async function upgradeServiceAction(options: UpgradeOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  let dockerImage: string

  if (options.version) {
    dockerImage = options.version
  } else {
    dockerImage = await promptText({
      message: 'Docker image to upgrade to (e.g., postgres:18-alpine)',
      required: true,
    })
  }

  await withSpinner('Upgrading service...', async () => {
    const { error } = await upgradeService({
      client,
      path: { id },
      body: {
        docker_image: dockerImage,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Service upgrade initiated')
  info(`Run: temps services show --id ${options.id} to check the status`)
}

async function importServiceAction(options: ImportOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let serviceType: ServiceTypeRoute
  let name: string
  let containerId: string
  let parameters: Record<string, unknown> = {}

  const isAutomation = options.yes && options.type && options.name && options.containerId

  if (isAutomation) {
    // Get available types for validation
    const types = await withSpinner('Fetching service types...', async () => {
      const { data, error } = await getServiceTypes({ client })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data ?? []
    })

    if (!types.includes(options.type as ServiceTypeRoute)) {
      warning(`Invalid service type: ${options.type}. Available: ${types.join(', ')}`)
      return
    }
    serviceType = options.type as ServiceTypeRoute
    name = options.name!
    containerId = options.containerId!

    if (options.set && options.set.length > 0) {
      try {
        parameters = parseSetPairs(options.set)
      } catch (e) {
        warning((e as Error).message)
        return
      }
    }
  } else {
    // Interactive mode
    const types = await withSpinner('Fetching service types...', async () => {
      const { data, error } = await getServiceTypes({ client })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data ?? []
    })

    if (types.length === 0) {
      warning('No service types available')
      return
    }

    serviceType = (options.type as ServiceTypeRoute) ?? await promptSelect({
      message: 'Service type',
      choices: types.map(t => ({
        name: SERVICE_TYPE_LABELS[t] || t,
        value: t,
      })),
    }) as ServiceTypeRoute

    name = options.name ?? await promptText({
      message: 'Service name',
      default: `imported-${serviceType}`,
      required: true,
    })

    containerId = options.containerId ?? await promptText({
      message: 'Container ID or name to import',
      required: true,
    })

    if (options.set && options.set.length > 0) {
      try {
        parameters = parseSetPairs(options.set)
      } catch (e) {
        warning((e as Error).message)
        return
      }
    }
  }

  await withSpinner('Importing service...', async () => {
    const { error } = await importExternalService({
      client,
      body: {
        service_type: serviceType,
        name,
        container_id: containerId,
        parameters,
        version: options.version ?? null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Service "${name}" imported successfully`)
  info('Run: temps services list to see all services')
}

async function linkServiceAction(options: LinkOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const project = await resolveProjectId(options.project)

  await withSpinner(`Linking service to project ${colors.bold(project.slug)}...`, async () => {
    const { error } = await linkServiceToProject({
      client,
      path: { id },
      body: {
        project_id: project.id,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Service ${options.id} linked to project ${project.slug}`)
}

async function unlinkServiceAction(options: UnlinkOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const project = await resolveProjectId(options.project)

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Unlink service ${options.id} from project ${project.slug}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner(`Unlinking service from project ${colors.bold(project.slug)}...`, async () => {
    const { error } = await unlinkServiceFromProject({
      client,
      path: { id, project_id: project.id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Service ${options.id} unlinked from project ${project.slug}`)
}

async function envAction(options: EnvOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const project = await resolveProjectId(options.project)

  const envVars = await withSpinner('Fetching environment variables...', async () => {
    const { data, error } = await getServiceEnvironmentVariables({
      client,
      path: { id, project_id: project.id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    // API returns HashMap<String, String> but OpenAPI spec says Vec<EnvironmentVariableInfo>
    // Handle both formats for compatibility
    if (data && !Array.isArray(data)) {
      return Object.entries(data as Record<string, string>).map(([name, value]) => ({
        name,
        value: String(value),
        sensitive: /password|secret|token|key/i.test(name),
      }))
    }
    return data ?? []
  })

  if (options.json) {
    json(envVars)
    return
  }

  newline()
  header(`${icons.info} Environment Variables (${envVars.length})`)

  if (envVars.length === 0) {
    info('No environment variables found')
    newline()
    return
  }

  for (const v of envVars) {
    const sensitiveTag = v.sensitive ? colors.muted(' [sensitive]') : ''
    keyValue(v.name, v.value + sensitiveTag)
  }
  newline()
}

async function envVarAction(options: EnvVarOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid service ID')
    return
  }

  const project = await resolveProjectId(options.project)

  const envVar = await withSpinner('Fetching environment variable...', async () => {
    const { data, error } = await getServiceEnvironmentVariable({
      client,
      path: { id, project_id: project.id, var_name: options.var },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(envVar)
    return
  }

  newline()
  if (envVar) {
    const sensitiveTag = envVar.sensitive ? colors.muted(' [sensitive]') : ''
    keyValue(envVar.name, envVar.value + sensitiveTag)
  } else {
    warning(`Environment variable "${options.var}" not found`)
  }
  newline()
}
