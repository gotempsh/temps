// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for creating a template with several services through
 * `POST /templates/{slug}/instantiate`: default service names, variable
 * rows, the client-side check of required values and the request body.
 * Kept out of the component so they can be tested on their own.
 */

import type {
  DatabaseChoice,
  EnvVarTemplateResponse,
  InstantiateTemplateRequest,
  InstantiationResponse,
  InstantiationStep,
  TemplateResponse,
  TemplateServiceResponse,
  TemplateVariableInput,
} from '@/api/client'
import { templateEnvironmentVariableDefaultsToSecret } from '@/lib/project-environment-variables'
import { normalizeTemplateServiceType } from '@/lib/template-service-requirements'
import { runGenerator, type DeploymentUrlBase } from './envVarGenerators'

/** Server-side limit of a project (group) slug. */
const MAX_PROJECT_SLUG_LEN = 100

/** Generators the server can resolve for a shared (project) variable. */
const SERVER_RESOLVED_SHARED_GENERATORS = new Set([
  'random_secret',
  'random_hex_32',
])

export type TargetMode = 'new_project' | 'existing_project'

export interface ServiceNameField {
  /** Name of the service in the template. */
  templateService: string
  /** Name of the service to create. */
  name: string
}

export interface VariableField {
  /** Template service the variable belongs to; `null` for a shared one. */
  service: string | null
  name: string
  value: string
  isSecret: boolean
}

export interface DatabaseField {
  /** Database type as the template names it, such as `postgres`. */
  type: string
  mode: 'create' | 'existing'
  serviceId: number | null
}

export interface MultiServiceFormState {
  target: TargetMode
  projectName: string
  groupId: number | null
  serviceNames: ServiceNameField[]
  variables: VariableField[]
  databases: DatabaseField[]
}

export interface FormIssue {
  path: (string | number)[]
  message: string
}

/** Whether the template creates several services (and so needs `instantiate`). */
export function isMultiServiceTemplate(
  template: Pick<TemplateResponse, 'service_definitions'>
): boolean {
  return (template.service_definitions?.length ?? 0) > 1
}

/** Same slug the server derives from a new project's name. */
export function slugifyProjectName(name: string): string {
  let slug = ''
  let pendingDash = false
  for (const ch of name) {
    if (/^[A-Za-z0-9]$/.test(ch)) {
      if (pendingDash && slug.length > 0) slug += '-'
      pendingDash = false
      slug += ch.toLowerCase()
    } else {
      pendingDash = true
    }
  }
  return slug.slice(0, MAX_PROJECT_SLUG_LEN).replace(/-+$/, '')
}

/** The name the server gives a service when none is chosen. */
export function defaultServiceName(
  projectSlug: string,
  templateService: string
): string {
  return projectSlug ? `${projectSlug}-${templateService}` : templateService
}

export function defaultServiceNames(
  template: Pick<TemplateResponse, 'service_definitions'>,
  projectSlug: string
): ServiceNameField[] {
  return template.service_definitions.map((service) => ({
    templateService: service.name,
    name: defaultServiceName(projectSlug, service.name),
  }))
}

export interface VariableDeclaration {
  service: string | null
  spec: EnvVarTemplateResponse
}

/** Every variable the form asks for: each service's, then the shared ones. */
export function variableDeclarations(
  template: Pick<TemplateResponse, 'service_definitions' | 'shared_env_vars'>
): VariableDeclaration[] {
  const declarations: VariableDeclaration[] = []
  for (const service of template.service_definitions) {
    for (const spec of service.env_vars) {
      declarations.push({ service: service.name, spec })
    }
  }
  for (const spec of template.shared_env_vars ?? []) {
    declarations.push({ service: null, spec })
  }
  return declarations
}

/** Stable key of a variable row. */
export function variableKey(service: string | null, name: string): string {
  return `${service ?? ''}::${name}`
}

/**
 * Service whose deployment URL an `app_url` generator should produce. A
 * service variable uses its own service; a shared one uses the service that
 * receives the project's hostnames (the source of the first route), else
 * the first service.
 */
export function appUrlService(
  template: Pick<TemplateResponse, 'service_definitions' | 'routes'>,
  service: string | null
): string | undefined {
  if (service) return service
  return template.routes?.[0]?.source ?? template.service_definitions[0]?.name
}

/** Slug a service will likely get, for generating its URL. */
export function serviceSlugFor(
  serviceNames: ServiceNameField[],
  templateService: string | undefined
): string {
  if (!templateService) return ''
  const chosen = serviceNames.find(
    (field) => field.templateService === templateService
  )
  return slugifyProjectName(chosen?.name ?? templateService)
}

/** Value a variable starts with: its default, else its generator's output. */
export function generatedValue(
  template: Pick<TemplateResponse, 'service_definitions' | 'routes'>,
  declaration: VariableDeclaration,
  serviceNames: ServiceNameField[],
  base: DeploymentUrlBase | undefined
): string {
  return (
    runGenerator(declaration.spec.default_generator, {
      repositoryName: serviceSlugFor(
        serviceNames,
        appUrlService(template, declaration.service)
      ),
      base,
    }) || ''
  )
}

export function initialVariables(
  template: Pick<
    TemplateResponse,
    'service_definitions' | 'shared_env_vars' | 'routes' | 'kind'
  >,
  serviceNames: ServiceNameField[],
  base: DeploymentUrlBase | undefined
): VariableField[] {
  return variableDeclarations(template).map((declaration) => ({
    service: declaration.service,
    name: declaration.spec.name,
    value:
      declaration.spec.default ||
      generatedValue(template, declaration, serviceNames, base),
    isSecret: templateEnvironmentVariableDefaultsToSecret({
      templateKind: template.kind,
      key: declaration.spec.name,
      defaultGenerator: declaration.spec.default_generator,
      explicitSecret: declaration.spec.secret,
    }),
  }))
}

/** Variable names a service receives from its linked databases. */
export function databaseProvidedNames(
  service: Pick<TemplateServiceResponse, 'database_bindings'>
): Set<string> {
  const names = new Set<string>()
  for (const bindings of Object.values(service.database_bindings ?? {})) {
    for (const name of Object.keys(bindings)) names.add(name)
  }
  return names
}

/**
 * Whether the server would reject the request for this variable being
 * empty. Mirrors `missing_required_template_configuration` (service
 * variables) and `resolve_shared_variables` (shared ones), where a shared
 * variable with a generator the server cannot run (`app_url`) needs a value
 * even when it is optional.
 */
export function variableNeedsValue(
  template: Pick<TemplateResponse, 'service_definitions'>,
  declaration: VariableDeclaration,
  value: string
): boolean {
  if (value.trim() !== '') return false
  const { spec } = declaration
  if (spec.default) return false
  if (declaration.service === null) {
    if (spec.default_generator) {
      return !SERVER_RESOLVED_SHARED_GENERATORS.has(spec.default_generator)
    }
    return spec.required
  }
  if (!spec.required || spec.default_generator) return false
  const service = template.service_definitions.find(
    (candidate) => candidate.name === declaration.service
  )
  return !(service && databaseProvidedNames(service).has(spec.name))
}

/** Indexes of `variables` that still need a value. */
export function missingVariableIndexes(
  template: Pick<TemplateResponse, 'service_definitions' | 'shared_env_vars'>,
  variables: VariableField[]
): number[] {
  const declarations = new Map(
    variableDeclarations(template).map((declaration) => [
      variableKey(declaration.service, declaration.spec.name),
      declaration,
    ])
  )
  const missing: number[] = []
  variables.forEach((variable, index) => {
    const declaration = declarations.get(
      variableKey(variable.service, variable.name)
    )
    if (
      declaration &&
      variableNeedsValue(template, declaration, variable.value)
    ) {
      missing.push(index)
    }
  })
  return missing
}

/** Everything the server would reject, as form issues. */
export function validateMultiServiceForm(
  template: Pick<TemplateResponse, 'service_definitions' | 'shared_env_vars'>,
  state: MultiServiceFormState
): FormIssue[] {
  const issues: FormIssue[] = []
  if (state.target === 'new_project') {
    const name = state.projectName.trim()
    if (!name) {
      issues.push({
        path: ['projectName'],
        message: 'Project name is required',
      })
    } else if (!slugifyProjectName(name)) {
      issues.push({
        path: ['projectName'],
        message: 'Use at least one letter or number',
      })
    } else if (name.length > 100) {
      issues.push({
        path: ['projectName'],
        message: 'Use at most 100 characters',
      })
    }
  } else if (state.groupId === null) {
    issues.push({ path: ['groupId'], message: 'Choose a project' })
  }

  const seen = new Map<string, number>()
  state.serviceNames.forEach((field, index) => {
    const name = field.name.trim()
    if (!name) {
      issues.push({
        path: ['serviceNames', index, 'name'],
        message: 'Service name is required',
      })
      return
    }
    const key = name.toLowerCase()
    if (seen.has(key)) {
      issues.push({
        path: ['serviceNames', index, 'name'],
        message: 'Each service needs its own name',
      })
    }
    seen.set(key, index)
  })

  for (const index of missingVariableIndexes(template, state.variables)) {
    issues.push({
      path: ['variables', index, 'value'],
      message: 'A value is required',
    })
  }

  state.databases.forEach((database, index) => {
    if (database.mode === 'existing' && database.serviceId === null) {
      issues.push({
        path: ['databases', index, 'serviceId'],
        message: 'Choose a database to link',
      })
    }
  })
  return issues
}

export function buildInstantiateRequest(
  state: MultiServiceFormState,
  idempotencyKey: string
): InstantiateTemplateRequest {
  const target: InstantiateTemplateRequest['target'] =
    state.target === 'new_project'
      ? { type: 'new_project', name: state.projectName.trim() }
      : { type: 'existing_project', group_id: state.groupId ?? 0 }

  const serviceNames: Record<string, string> = {}
  for (const field of state.serviceNames) {
    const name = field.name.trim()
    if (name) serviceNames[field.templateService] = name
  }

  // Service variables name their service, so a name declared by several
  // services (or by one service and the project) is never ambiguous.
  const environmentVariables: TemplateVariableInput[] = state.variables
    .filter((variable) => variable.value !== '')
    .map((variable) => ({
      name: variable.name,
      value: variable.value,
      is_secret: variable.isSecret,
      ...(variable.service ? { service: variable.service } : {}),
    }))

  const databases: Record<string, DatabaseChoice> = {}
  for (const database of state.databases) {
    databases[database.type] =
      database.mode === 'existing' && database.serviceId !== null
        ? { type: 'existing', service_id: database.serviceId }
        : { type: 'create' }
  }

  return {
    target,
    service_names: serviceNames,
    environment_variables: environmentVariables,
    databases,
    idempotency_key: idempotencyKey,
  }
}

/** Existing databases that can serve a template database type. */
export function compatibleDatabases<T extends { service_type: string }>(
  databaseType: string,
  services: T[]
): T[] {
  const wanted = normalizeTemplateServiceType(databaseType)
  return services.filter(
    (service) => normalizeTemplateServiceType(service.service_type) === wanted
  )
}

/** How a service runs: a prebuilt image, or a build of the template's source. */
export function serviceSourceLabel(
  service: Pick<TemplateServiceResponse, 'image' | 'directory' | 'git'>
): string {
  if (service.image) return `Image ${service.image}`
  const directory = service.directory?.trim()
  const where =
    directory && directory !== '.' && directory !== './'
      ? ` in ${directory.replace(/^\.\//, '')}`
      : ''
  if (service.git?.url) return `Builds from ${service.git.url}${where}`
  return `Builds from source${where}`
}

/** Index of the step running now: the first one not done yet. */
export function currentStepIndex(
  instantiation: Pick<InstantiationResponse, 'status' | 'steps'>
): number {
  if (
    instantiation.status !== 'pending' &&
    instantiation.status !== 'running'
  ) {
    return -1
  }
  return instantiation.steps.findIndex((step) => step.status === 'pending')
}

/** The step that failed, if one did. */
export function failedStep(
  instantiation: Pick<InstantiationResponse, 'steps'>
): InstantiationStep | undefined {
  return instantiation.steps.find((step) => step.status === 'failed')
}
