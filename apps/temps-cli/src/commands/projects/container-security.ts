// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getContainerSecurity, getProjectBySlug, updateContainerSecurity } from '../../api/sdk.gen.js'
import type { ContainerCapability, ContainerSecurityResponse } from '../../api/types.gen.js'
import { promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { colors, info, json, keyValue, newline, success, warning } from '../../ui/output.js'

const CLI = 'bunx @temps-sdk/cli'

interface ContainerSecurityOptions {
  project?: string
  allow?: string[]
  remove?: string[]
  json?: boolean
  yes?: boolean
}

/**
 * A capability name as Docker and `setcap` spell it: `NET_BIND_SERVICE`,
 * `CAP_NET_BIND_SERVICE` or `cap_net_bind_service`. The server decides what
 * may be allowed; this only normalises the spelling.
 */
export function normalizeCapability(value: string): ContainerCapability {
  const upper = value.trim().toUpperCase()
  const bare = upper.startsWith('CAP_') ? upper.slice(4) : upper
  if (!/^[A-Z_]{1,64}$/.test(bare)) {
    throw new Error(`"${value}" is not a Linux capability name (e.g. NET_BIND_SERVICE)`)
  }
  return bare as ContainerCapability
}

/** The saved value after adding `allow` and removing `remove`, sorted. */
export function nextCapabilities(
  current: readonly ContainerCapability[],
  allow: readonly ContainerCapability[],
  remove: readonly ContainerCapability[]
): ContainerCapability[] {
  const overlap = allow.filter((capability) => remove.includes(capability))
  if (overlap.length > 0) {
    throw new Error(`Cannot both allow and remove ${overlap.join(', ')}`)
  }
  return [...new Set([...current, ...allow])].filter((capability) => !remove.includes(capability)).sort()
}

function describe(data: ContainerSecurityResponse, slug: string): void {
  newline()
  keyValue('Project', slug)
  keyValue(
    'Allowed capabilities',
    data.allowed_capabilities.length > 0
      ? data.allowed_capabilities.join(', ')
      : 'none (fully hardened, the default)'
  )
  newline()
  for (const rule of data.always_enforced) info(`Always: ${rule}`)
  if (!data.can_edit) {
    newline()
    info(data.edit_requirement)
  }
}

/**
 * Show or change which Linux capabilities a project's containers get back
 * after Temps drops all of them. Allowing one asks for confirmation unless
 * `--yes`; the server still requires an administrator and may ask for MFA.
 */
export async function projectContainerSecurityAction(options: ContainerSecurityOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const allow = (options.allow ?? []).map(normalizeCapability)
  const remove = (options.remove ?? []).map(normalizeCapability)
  const resolved = await requireProjectSlug(options.project)
  const project = await withSpinner('Fetching project...', async () => {
    const { data, error } = await getProjectBySlug({ client, path: { slug: resolved.slug } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? `Project "${resolved.slug}" not found`)
    return data
  })
  const current = await withSpinner('Reading container security...', async () => {
    const { data, error } = await getContainerSecurity({ client, path: { id: project.id } })
    if (error || !data) throw new Error(getErrorMessage(error) ?? 'Failed to read container security')
    return data
  })

  if (allow.length === 0 && remove.length === 0) {
    if (options.json) {
      json({ project: project.slug, ...current })
      return
    }
    describe(current, project.slug)
    newline()
    info(`Allow one: ${CLI} projects container-security --allow NET_BIND_SERVICE --project ${project.slug}`)
    return
  }

  const next = nextCapabilities(current.allowed_capabilities, allow, remove)
  const added = next.filter((capability) => !current.allowed_capabilities.includes(capability))
  if (added.length > 0 && !options.yes) {
    for (const capability of added) {
      const option = current.options.find((candidate) => candidate.id === capability)
      warning(`Allowing ${capability} for every container of ${project.slug}.`)
      if (option) {
        info(option.description)
        info(`Trade-off: ${option.trade_off}`)
      }
    }
    info('To keep full hardening, remove the file capability from the image (`setcap -r <binary>`) instead.')
    newline()
    const confirmed = await promptConfirm({
      message: `Allow ${added.join(', ')} for ${project.slug}?`,
      default: false,
    })
    if (!confirmed) {
      console.log('Cancelled')
      return
    }
  }

  const updated = await withSpinner('Saving container security...', async () => {
    const { data, error } = await updateContainerSecurity({
      client,
      path: { id: project.id },
      body: {
        allowed_capabilities: next,
        expected_allowed_capabilities: current.allowed_capabilities,
        acknowledge_risks: added.length > 0,
      },
    })
    if (error || !data) throw new Error(getErrorMessage(error) ?? 'Failed to save container security')
    return data
  })
  if (options.json) {
    json({ project: project.slug, ...updated })
    return
  }
  success(
    updated.allowed_capabilities.length > 0
      ? `${colors.bold(project.slug)} containers may use ${updated.allowed_capabilities.join(', ')}`
      : `${colors.bold(project.slug)} containers are fully hardened`
  )
  info(`Applies from the next deployment: ${CLI} deploy --project ${project.slug}`)
}
