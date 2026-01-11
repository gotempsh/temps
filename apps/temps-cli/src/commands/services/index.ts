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
} from '../../api/sdk.gen.js'
import type { ExternalServiceInfo, ServiceTypeRoute } from '../../api/types.gen.js'
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

interface CreateOptions {
  type?: string
  name?: string
  parameters?: string
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
    .option('--parameters <json>', 'Service parameters as JSON string')
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

  services
    .command('types')
    .description('List available service types')
    .option('--json', 'Output in JSON format')
    .action(listServiceTypes)

  services
    .command('projects')
    .description('List projects linked to a service')
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(listLinkedProjects)
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

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.type && options.name

  if (isAutomation) {
    // Validate service type
    if (!types.includes(options.type as ServiceTypeRoute)) {
      warning(`Invalid service type: ${options.type}. Available: ${types.join(', ')}`)
      return
    }
    serviceType = options.type as ServiceTypeRoute
    name = options.name!

    // Parse parameters if provided
    if (options.parameters) {
      try {
        parameters = JSON.parse(options.parameters)
      } catch {
        warning('Invalid JSON in --parameters')
        return
      }
    }
  } else {
    // Interactive mode
    serviceType = await promptSelect({
      message: 'Service type',
      choices: types.map(t => ({
        name: SERVICE_TYPE_LABELS[t] || t,
        value: t,
      })),
    }) as ServiceTypeRoute

    name = await promptText({
      message: 'Service name',
      default: `my-${serviceType}`,
      required: true,
    })

    // Get parameters schema for the service type
    const { data: typeInfo } = await getServiceTypeParameters({
      client,
      path: { service_type: serviceType },
    })

    // Type guard for parameters response
    interface ServiceTypeParameter {
      name: string
      label?: string
      default_value?: unknown
      required?: boolean
      enum_values?: string[]
      param_type?: string
    }
    interface ServiceTypeParametersResponse {
      parameters?: ServiceTypeParameter[]
    }
    const paramResponse = typeInfo as ServiceTypeParametersResponse | undefined

    if (paramResponse?.parameters && paramResponse.parameters.length > 0) {
      info(`\nConfigure ${SERVICE_TYPE_LABELS[serviceType] || serviceType} parameters:`)
      newline()

      for (const param of paramResponse.parameters) {
        // Skip parameters that have defaults and aren't required
        if (param.default_value !== undefined && !param.required) {
          const useDefault = await promptConfirm({
            message: `${param.label || param.name}: Use default "${param.default_value}"?`,
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
            message: param.label || param.name,
            choices: param.enum_values.map((v: string) => ({ name: v, value: v })),
          })
        } else {
          value = await promptText({
            message: param.label || param.name,
            default: param.default_value?.toString() || '',
            required: param.required || false,
          })
        }

        if (value) {
          // Try to parse as number if the param type suggests it
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
