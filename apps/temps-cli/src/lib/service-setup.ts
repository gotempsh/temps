// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared service setup utilities for project creation and setup wizard.
 * Extracted from commands/projects/create.ts to avoid duplication.
 */
import { client, getErrorMessage } from './api-client.js'
import { listServices, createService, getServiceTypeParameters } from '../api/sdk.gen.js'
import type { CreatableServiceTypeRoute, ServiceTypeRoute } from '../api/types.gen.js'
import { promptSelect, promptText, promptConfirm, promptCheckbox, type SelectOption } from '../ui/prompts.js'
import { withSpinner, startSpinner, succeedSpinner } from '../ui/spinner.js'
import { success, error, info, newline, colors } from '../ui/output.js'

/** Service type configuration */
export const SERVICE_TYPES: {
  id: CreatableServiceTypeRoute
  name: string
  description: string
}[] = [
  { id: 'postgres', name: 'PostgreSQL', description: 'Reliable Relational Database' },
  { id: 'redis', name: 'Redis', description: 'In-Memory Data Store' },
  { id: 's3', name: 'S3', description: 'Object Storage (MinIO)' },
  { id: 'mongodb', name: 'MongoDB', description: 'Document Database' },
]

/**
 * Interactive service selection flow.
 * Asks user if they want services, shows existing ones, allows creating new ones.
 * Returns array of service IDs to link to the project.
 */
export async function selectStorageServices(): Promise<number[]> {
  newline()

  const addServices = await promptConfirm({
    message: 'Add storage services (PostgreSQL, Redis, etc.)?',
    default: false,
  })

  if (!addServices) {
    return []
  }

  // Load existing services
  const spinner = startSpinner('Loading services...')
  const { data: servicesData } = await listServices({ client })
  succeedSpinner('Services loaded')

  const existingServices = servicesData || []
  const selectedServiceIds: number[] = []

  newline()

  // Show existing services if any
  if (existingServices.length > 0) {
    const serviceChoices: SelectOption<number | string>[] = existingServices.map((s) => ({
      name: `${s.name} (${s.service_type})`,
      value: s.id,
      description: `Created ${new Date(s.created_at).toLocaleDateString()}`,
    }))

    serviceChoices.push({
      name: colors.success('+ Create new service'),
      value: 'create_new',
      description: 'Create a new storage service',
    })

    const selected = await promptSelect({
      message: 'Select existing service or create new',
      choices: serviceChoices,
    })

    if (selected !== 'create_new') {
      selectedServiceIds.push(selected as number)

      // Ask if they want to add more
      let addMore = true
      while (addMore) {
        addMore = await promptConfirm({
          message: 'Add another service?',
          default: false,
        })

        if (addMore) {
          const remainingServices = existingServices.filter(
            (s) => !selectedServiceIds.includes(s.id)
          )

          if (remainingServices.length === 0) {
            info('No more services available')
            break
          }

          const moreChoices: SelectOption<number | string>[] = remainingServices.map((s) => ({
            name: `${s.name} (${s.service_type})`,
            value: s.id,
          }))

          moreChoices.push({
            name: colors.success('+ Create new service'),
            value: 'create_new',
            description: 'Create a new storage service',
          })

          const moreSelected = await promptSelect({
            message: 'Select service',
            choices: moreChoices,
          })

          if (moreSelected === 'create_new') {
            const newServiceId = await createNewService()
            if (newServiceId) {
              selectedServiceIds.push(newServiceId)
            }
          } else {
            selectedServiceIds.push(moreSelected as number)
          }
        }
      }

      return selectedServiceIds
    }
  }

  // Create new service
  const newServiceId = await createNewService()
  if (newServiceId) {
    selectedServiceIds.push(newServiceId)
  }

  return selectedServiceIds
}

/**
 * Interactive service selection with pre-suggested types.
 * Similar to selectStorageServices but shows a checkbox of suggested types first.
 */
export async function selectServicesWithSuggestions(
  suggestedTypes: ServiceTypeRoute[]
): Promise<number[]> {
  newline()

  // Load existing services
  const spinner = startSpinner('Loading services...')
  const { data: servicesData } = await listServices({ client })
  succeedSpinner('Services loaded')

  const existingServices = servicesData || []
  const selectedServiceIds: number[] = []

  // Build choices - show suggested types with existing service matches
  const serviceChoices: SelectOption<string>[] = []

  for (const serviceType of SERVICE_TYPES) {
    const existing = existingServices.filter((s) => s.service_type === serviceType.id)
    const isSuggested = suggestedTypes.includes(serviceType.id)

    if (existing.length > 0) {
      for (const svc of existing) {
        serviceChoices.push({
          name: `${serviceType.name}: ${svc.name}`,
          value: `existing:${svc.id}`,
          description: isSuggested ? 'Recommended - already exists' : 'Existing service',
        })
      }
    }

    serviceChoices.push({
      name: `${serviceType.name}: Create new`,
      value: `new:${serviceType.id}`,
      description: isSuggested
        ? `Recommended for your project`
        : serviceType.description,
    })
  }

  newline()

  const selected = await promptCheckbox<string>({
    message: 'Select services to add (space to toggle, enter to confirm)',
    choices: serviceChoices,
  })

  for (const selection of selected) {
    if (selection.startsWith('existing:')) {
      const id = parseInt(selection.split(':')[1]!, 10)
      selectedServiceIds.push(id)
    } else if (selection.startsWith('new:')) {
      const type = selection.split(':')[1]! as CreatableServiceTypeRoute
      const newId = await createNewServiceOfType(type)
      if (newId) {
        selectedServiceIds.push(newId)
      }
    }
  }

  return selectedServiceIds
}

/**
 * Create a new service with interactive type selection and name prompt.
 */
export async function createNewService(): Promise<number | null> {
  newline()

  const typeChoices: SelectOption<CreatableServiceTypeRoute>[] = SERVICE_TYPES.map((t) => ({
    name: t.name,
    value: t.id,
    description: t.description,
  }))

  const serviceType = await promptSelect({
    message: 'Select service type',
    choices: typeChoices,
  })

  return createNewServiceOfType(serviceType)
}

/**
 * Sanitize a service name into a valid identifier for database/username fields.
 * Replaces hyphens and non-alphanumeric chars with underscores, ensures it starts with a letter.
 */
function sanitizeToIdentifier(name: string): string {
  let sanitized = name.replace(/[^a-zA-Z0-9_]/g, '_').toLowerCase()
  // Ensure it starts with a letter
  if (sanitized && !/^[a-z]/.test(sanitized)) {
    sanitized = `svc_${sanitized}`
  }
  return sanitized || 'default'
}

/**
 * Known required fields per service type.
 * Used as a fallback when the schema API call fails or returns no data.
 * Must be kept in sync with the backend parameter_strategies.rs.
 */
const KNOWN_REQUIRED_FIELDS: Partial<Record<ServiceTypeRoute, string[]>> = {
  postgres: ['database', 'username'],
  mongodb: ['database', 'username'],
  // redis and s3 have no required fields
}

/**
 * Auto-generate sensible defaults for required parameters based on service type and name.
 * This avoids prompting the user for values that can be derived automatically.
 */
function autoGenerateRequiredParams(
  serviceType: CreatableServiceTypeRoute,
  serviceName: string,
  requiredFields: string[]
): Record<string, unknown> {
  const params: Record<string, unknown> = {}
  const identifier = sanitizeToIdentifier(serviceName)

  for (const field of requiredFields) {
    switch (field) {
      case 'database':
        params.database = identifier
        break
      case 'username':
        params.username = identifier
        break
      // Other required fields can be added here as new service types are introduced.
      // Non-required fields (password, port, docker_image) are auto-generated by the backend.
    }
  }

  return params
}

/**
 * Create a new service of a specific type with a name prompt.
 * Fetches the parameter schema to auto-populate required fields.
 * Includes retry logic if creation fails.
 */
export async function createNewServiceOfType(
  serviceType: CreatableServiceTypeRoute
): Promise<number | null> {
  const typeLabel = SERVICE_TYPES.find((t) => t.id === serviceType)?.name || serviceType

  const serviceName = await promptText({
    message: `${typeLabel} service name`,
    default: `${serviceType}-${Date.now().toString(36)}`,
    required: true,
  })

  // Fetch parameter schema to discover required fields, with hardcoded fallback
  let requiredFields: string[] = []
  try {
    const { data: schemaData, error: schemaError } = await getServiceTypeParameters({
      client,
      path: { service_type: serviceType },
    })

    if (!schemaError && schemaData) {
      // The schema is a JSON Schema object with a "required" array
      const schema = schemaData as Record<string, unknown>
      if (schema.required && Array.isArray(schema.required)) {
        requiredFields = schema.required as string[]
      }
    }
  } catch {
    // Network-level failure — fall through to fallback
  }

  // Fallback to hardcoded known required fields if schema fetch returned nothing
  if (requiredFields.length === 0) {
    requiredFields = KNOWN_REQUIRED_FIELDS[serviceType] ?? []
  }

  // Auto-generate required parameters from the service name
  const parameters = autoGenerateRequiredParams(serviceType, serviceName, requiredFields)

  // Attempt creation with retry
  const MAX_RETRIES = 2
  for (let attempt = 1; attempt <= MAX_RETRIES; attempt++) {
    const { data, error: apiError } = await withSpinner(
      `Creating ${typeLabel} service...`,
      async () => {
        return await createService({
          client,
          body: {
            name: serviceName,
            service_type: serviceType,
            parameters,
          },
        })
      }
    )

    if (data?.id) {
      success(`Service "${serviceName}" created`)
      return data.id
    }

    const errorMsg = getErrorMessage(apiError)
    error(`Failed to create service: ${errorMsg}`)

    if (attempt < MAX_RETRIES) {
      const retry = await promptConfirm({
        message: 'Retry creating this service?',
        default: true,
      })

      if (!retry) {
        return null
      }
    }
  }

  return null
}
