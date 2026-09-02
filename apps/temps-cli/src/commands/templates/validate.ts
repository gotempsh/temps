// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface NativeTemplateValidationResult {
  valid: boolean
  errors: string[]
  templateCount: number
}

const SUPPORTED_KINDS = new Set(['starter', 'service'])
const SUPPORTED_MANAGED_SERVICES = new Set([
  'postgres',
  'redis',
  'mongodb',
  'mariadb',
  's3',
  'rustfs',
])

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * Fast, offline validation for contributors authoring Temps-native service
 * templates. The server remains authoritative and performs the same checks
 * when loading the catalog.
 */
export function validateNativeTemplateConfig(
  document: unknown
): NativeTemplateValidationResult {
  const errors: string[] = []
  if (!isRecord(document)) {
    return {
      valid: false,
      errors: ['Document must be a YAML object'],
      templateCount: 0,
    }
  }

  if (document.version !== '2') {
    errors.push('version must be "2"')
  }
  if (!Array.isArray(document.templates)) {
    errors.push('templates must be an array')
    return { valid: false, errors, templateCount: 0 }
  }

  const seenSlugs = new Set<string>()
  document.templates.forEach((value, index) => {
    const prefix = `templates[${index}]`
    if (!isRecord(value)) {
      errors.push(`${prefix} must be an object`)
      return
    }

    const slug = typeof value.slug === 'string' ? value.slug.trim() : ''
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug)) {
      errors.push(`${prefix}.slug must be lowercase kebab-case`)
    } else if (seenSlugs.has(slug)) {
      errors.push(`${prefix}.slug duplicates "${slug}"`)
    } else {
      seenSlugs.add(slug)
    }
    if (typeof value.name !== 'string' || value.name.trim() === '') {
      errors.push(`${prefix}.name is required`)
    }
    if (typeof value.kind !== 'string' || !SUPPORTED_KINDS.has(value.kind)) {
      errors.push(`${prefix}.kind must be starter or service`)
    }

    if (value.kind === 'service') {
      if (typeof value.image !== 'string' || !value.image.includes(':')) {
        errors.push(`${prefix}.image must be a version-pinned image reference`)
      } else if (value.image.endsWith(':latest')) {
        errors.push(`${prefix}.image must not use the floating latest tag`)
      }
      if (value.resources !== undefined) {
        if (!isRecord(value.resources)) {
          errors.push(`${prefix}.resources must be an object`)
        } else {
          for (const field of ['cpu_request', 'memory_request', 'memory_limit']) {
            const resource = value.resources[field]
            if (resource !== undefined && (!Number.isInteger(resource) || Number(resource) <= 0)) {
              errors.push(`${prefix}.resources.${field} must be a positive integer`)
            }
          }
          const request = value.resources.memory_request
          const limit = value.resources.memory_limit
          if (
            Number.isInteger(request) &&
            Number.isInteger(limit) &&
            Number(request) > Number(limit)
          ) {
            errors.push(
              `${prefix}.resources.memory_request must not exceed memory_limit`
            )
          }
        }
      }
      if (!Number.isInteger(value.exposed_port) || Number(value.exposed_port) <= 0) {
        errors.push(`${prefix}.exposed_port must be a positive integer`)
      }
    }

    const services = Array.isArray(value.services) ? value.services : []
    services.forEach((service) => {
      if (typeof service !== 'string' || !SUPPORTED_MANAGED_SERVICES.has(service)) {
        errors.push(
          `${prefix}.services contains unsupported service "${String(service)}"`
        )
      }
    })

    if (isRecord(value.managed_service_bindings)) {
      for (const [service, bindings] of Object.entries(
        value.managed_service_bindings
      )) {
        if (!services.includes(service)) {
          errors.push(
            `${prefix}.managed_service_bindings.${service} must also be listed in services`
          )
        }
        if (!isRecord(bindings) || Object.keys(bindings).length === 0) {
          errors.push(
            `${prefix}.managed_service_bindings.${service} must contain environment aliases`
          )
        }
      }
    }
  })

  return {
    valid: errors.length === 0,
    errors,
    templateCount: document.templates.length,
  }
}

export async function readAndValidateTemplateFile(
  path: string
): Promise<NativeTemplateValidationResult> {
  const file = Bun.file(path)
  if (!(await file.exists())) {
    return {
      valid: false,
      errors: [`File not found: ${path}`],
      templateCount: 0,
    }
  }
  try {
    return validateNativeTemplateConfig(Bun.YAML.parse(await file.text()))
  } catch (error) {
    return {
      valid: false,
      errors: [
        `Invalid YAML: ${error instanceof Error ? error.message : String(error)}`,
      ],
      templateCount: 0,
    }
  }
}
