// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `templates create`: create every service of a template in one server-side
 * operation (the databases it needs, its shared variables and path routes
 * included) and follow it until it finishes.
 */

import { getProjectTemplate, instantiateTemplate } from '../../api/sdk.gen.js'
import type {
  DatabaseChoice,
  InstantiateTemplateRequest,
  InstantiationTarget,
  TemplateResponse,
  TemplateVariableInput,
} from '../../api/types.gen.js'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { info } from '../../ui/output.js'
import { setQuietMode, withSpinner } from '../../ui/spinner.js'
import { report, waitForInstantiation, waitTimeoutMs } from '../instantiations/index.js'
import { resolveGroup } from '../project-groups/index.js'

const CLI = 'bunx @temps-sdk/cli'

export interface CreateTemplateOptions {
  project?: string
  into?: string
  standalone?: boolean
  name?: string[]
  env?: string[]
  secret?: string[]
  database?: string[]
  forkConnection?: string
  repoName?: string
  repoOwner?: string
  publicRepo?: boolean
  autoDeploy?: boolean
  wait?: boolean
  idempotencyKey?: string
  timeout?: string
  json?: boolean
}

function unwrap<T>(result: { data?: T; error?: unknown }): T {
  if (result.error !== undefined || result.data === undefined) {
    throw new Error(getErrorMessage(result.error))
  }
  return result.data
}

/** Where the services go: `--project`, `--into`, `--standalone`, or a default. */
export function resolveTarget(
  template: Pick<TemplateResponse, 'name' | 'service_definitions'>,
  options: Pick<CreateTemplateOptions, 'project' | 'into' | 'standalone'>,
  intoGroupId?: number
): InstantiationTarget {
  const chosen = [options.project, options.into, options.standalone].filter((value) => value !== undefined && value !== false)
  if (chosen.length > 1) {
    throw new Error('Choose one of --project, --into and --standalone')
  }
  const serviceCount = template.service_definitions.length
  if (options.standalone) {
    if (serviceCount > 1) {
      throw new Error(
        `${template.name} has ${serviceCount} services, which go into a project. Use --project <name> or --into <project>.`
      )
    }
    return { type: 'no_project' }
  }
  if (options.into !== undefined) {
    if (intoGroupId === undefined) throw new Error(`Project "${options.into}" was not resolved`)
    return { type: 'existing_project', group_id: intoGroupId }
  }
  if (options.project !== undefined) return { type: 'new_project', name: options.project }
  // A single service stands alone, as it always did; several go into a new
  // project named after the template.
  return serviceCount > 1 ? { type: 'new_project', name: template.name } : { type: 'no_project' }
}

/**
 * `KEY=VALUE` or `service:KEY=VALUE`. The service prefix is only read when it
 * names a service of the template, so values containing `:` stay intact.
 */
export function parseVariable(
  value: string,
  serviceNames: ReadonlySet<string>,
  isSecret: boolean
): TemplateVariableInput {
  const eq = value.indexOf('=')
  if (eq <= 0) throw new Error(`"${value}" is not KEY=VALUE`)
  let name = value.slice(0, eq)
  let service: string | undefined
  const colon = name.indexOf(':')
  if (colon > 0 && serviceNames.has(name.slice(0, colon))) {
    service = name.slice(0, colon)
    name = name.slice(colon + 1)
  }
  return { name, value: value.slice(eq + 1), is_secret: isSecret, service: service ?? null }
}

/** `postgres=new`, `postgres=new:shop-db` or `postgres=<database id>`. */
export function parseDatabase(value: string): [string, DatabaseChoice] {
  const eq = value.indexOf('=')
  if (eq <= 0) throw new Error(`"${value}" is not TYPE=new or TYPE=<database id>`)
  const type = value.slice(0, eq).trim().toLowerCase()
  const choice = value.slice(eq + 1).trim()
  if (choice === 'new') return [type, { type: 'create' }]
  if (choice.startsWith('new:')) return [type, { type: 'create', name: choice.slice(4) }]
  const id = Number.parseInt(choice, 10)
  if (!Number.isInteger(id) || String(id) !== choice) {
    throw new Error(`"${value}": use ${type}=new, ${type}=new:<name> or ${type}=<database id>`)
  }
  return [type, { type: 'existing', service_id: id }]
}

/** `service=name`; a one-service template also takes a bare name. */
export function parseServiceNames(values: string[], template: Pick<TemplateResponse, 'service_definitions'>): Record<string, string> {
  const names: Record<string, string> = {}
  const services = template.service_definitions.map((service) => service.name)
  for (const value of values) {
    const eq = value.indexOf('=')
    if (eq > 0) {
      const service = value.slice(0, eq)
      if (!services.includes(service)) {
        throw new Error(`--name ${value}: the template has no service "${service}" (it has ${services.join(', ')})`)
      }
      names[service] = value.slice(eq + 1)
    } else if (services.length === 1 && services[0] !== undefined) {
      names[services[0]] = value
    } else {
      throw new Error(`--name ${value}: say which service, as <service>=<name> (${services.join(', ')})`)
    }
  }
  return names
}

/** Build the request; every database the template needs defaults to a new one. */
export function buildInstantiateRequest(
  template: Pick<TemplateResponse, 'name' | 'kind' | 'databases' | 'service_definitions'>,
  options: CreateTemplateOptions,
  target: InstantiationTarget,
  idempotencyKey: string
): InstantiateTemplateRequest {
  const serviceNames = new Set(template.service_definitions.map((service) => service.name))
  const databases: Record<string, DatabaseChoice> = {}
  for (const value of options.database ?? []) {
    const [type, choice] = parseDatabase(value)
    databases[type] = choice
  }
  for (const type of template.databases) {
    if (!(type in databases)) databases[type] = { type: 'create' }
  }
  let fork: InstantiateTemplateRequest['fork'] = null
  if (options.forkConnection !== undefined) {
    const connection = Number.parseInt(options.forkConnection, 10)
    if (!Number.isInteger(connection)) throw new Error('--fork-connection takes a git connection id')
    if (!options.repoName) throw new Error('--fork-connection needs --repo-name <name> for the new repository')
    fork = {
      git_provider_connection_id: connection,
      repository_name: options.repoName,
      repository_owner: options.repoOwner ?? null,
      private: !options.publicRepo,
      automatic_deploy: options.autoDeploy !== false,
    }
  }
  return {
    target,
    service_names: parseServiceNames(options.name ?? [], template),
    environment_variables: [
      ...(options.env ?? []).map((value) => parseVariable(value, serviceNames, false)),
      ...(options.secret ?? []).map((value) => parseVariable(value, serviceNames, true)),
    ],
    databases,
    fork,
    idempotency_key: idempotencyKey,
  }
}

export async function createTemplateAction(slug: string, options: CreateTemplateOptions): Promise<void> {
  if (options.json) setQuietMode(true)
  const timeoutMs = waitTimeoutMs(options.timeout)
  await requireAuth()
  await setupClient()
  const template = await withSpinner('Loading template...', async () =>
    unwrap(await getProjectTemplate({ client, path: { slug } }))
  )
  const intoGroupId = options.into !== undefined ? (await resolveGroup(options.into)).id : undefined
  const target = resolveTarget(template, options, intoGroupId)
  const request = buildInstantiateRequest(template, options, target, options.idempotencyKey ?? crypto.randomUUID())
  const created = Object.entries(request.databases ?? {}).filter(([, choice]) => choice.type === 'create')
  if (!options.json && created.length > 0) {
    info(`Creating ${created.map(([type]) => `a ${type} database`).join(' and ')} (link an existing one with --database <type>=<id>)`)
  }
  const started = unwrap(await instantiateTemplate({ client, path: { slug }, body: request }))
  if (options.wait === false) {
    report(started, options)
    if (!options.json) info(`Follow it with: ${CLI} instantiations show ${started.id} --wait`)
    process.exitCode = 0
    return
  }
  report(await waitForInstantiation(started.id, { timeoutMs, announce: true }), options)
}
