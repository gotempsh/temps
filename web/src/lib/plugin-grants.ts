// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { z } from 'zod'

export const pluginPermissionValues = [
  'ai_generate',
  'projects_read',
  'environments_read',
  'deployments_read',
  'events_read',
  'api_read',
  'api_write',
] as const

/** The areas of Temps a permission to change can be narrowed to. */
export const pluginWriteAreaValues = [
  'projects',
  'environments',
  'deployments',
] as const

export type PluginWriteAreaValue = (typeof pluginWriteAreaValues)[number]

/** How many services a permission to change can be limited to. */
export const MAX_WRITE_SERVICES = 100

const AI_CALLS_MESSAGE = 'Choose 0 to 10,000 AI calls per day.'
const AI_TOKENS_MESSAGE = 'Choose 1 to 4,096 output tokens per call.'

export const pluginGrantsSchema = z
  .object({
    permissions: z
      .array(z.enum(pluginPermissionValues))
      .max(pluginPermissionValues.length),
    ai_daily_call_limit: z
      .number(AI_CALLS_MESSAGE)
      .int(AI_CALLS_MESSAGE)
      .min(0, AI_CALLS_MESSAGE)
      .max(10000, AI_CALLS_MESSAGE),
    ai_max_output_tokens: z
      .number(AI_TOKENS_MESSAGE)
      .int(AI_TOKENS_MESSAGE)
      .min(1, AI_TOKENS_MESSAGE)
      .max(4096, AI_TOKENS_MESSAGE),
    /** What a plugin allowed to change things may change. Empty: nothing. */
    write_areas: z
      .array(z.enum(pluginWriteAreaValues))
      .max(pluginWriteAreaValues.length),
    /** The services its changes are limited to; `null` is every service. */
    write_project_ids: z
      .array(z.number().int().positive())
      .max(
        MAX_WRITE_SERVICES,
        `Changes can be limited to at most ${MAX_WRITE_SERVICES} services.`
      )
      .nullable(),
  })
  .superRefine((value, context) => {
    if (!value.permissions.includes('api_write')) return
    if (value.write_project_ids?.length === 0)
      context.addIssue({
        code: 'custom',
        path: ['write_project_ids'],
        message:
          'Choose at least one service, or let the plugin change every service.',
      })
  })

export type PluginGrantsValues = z.infer<typeof pluginGrantsSchema>

export function emptyPluginGrants(): PluginGrantsValues {
  return {
    permissions: [],
    ai_daily_call_limit: 100,
    ai_max_output_tokens: 1024,
    write_areas: [],
    write_project_ids: null,
  }
}

/**
 * What is sent to the server: where a plugin may change things only means
 * something together with the permission to change, so it is dropped
 * without it.
 */
export function pluginGrantsBody(
  values: PluginGrantsValues
): PluginGrantsValues {
  if (values.permissions.includes('api_write')) return values
  return { ...values, write_areas: [], write_project_ids: null }
}

/** Every message of a failed validation, in the order the form shows them. */
export function pluginGrantErrors(errors: unknown): string[] {
  if (typeof errors !== 'object' || errors === null) return []
  const messages = new Set<string>()
  const visit = (node: unknown) => {
    if (typeof node !== 'object' || node === null) return
    const message = (node as { message?: unknown }).message
    if (typeof message === 'string' && message !== '') messages.add(message)
    for (const [key, child] of Object.entries(node))
      if (key !== 'ref' && key !== 'message') visit(child)
  }
  visit(errors)
  return [...messages]
}

export const pluginWriteAreaLabels = {
  projects: {
    label: 'Services',
    description: 'Create, change and delete services.',
  },
  environments: {
    label: 'Environments',
    description: 'Create, change and delete environments.',
  },
  deployments: {
    label: 'Deployments',
    description: 'Start, cancel and roll back deployments.',
  },
} satisfies Record<PluginWriteAreaValue, { label: string; description: string }>

/** What the outbound profile of a plugin's service means, in a word. */
export function pluginOutboundLabel(profile: string): {
  label: string
  tone: 'open' | 'limited' | 'closed'
} {
  if (profile === 'none') return { label: 'No outbound access', tone: 'closed' }
  if (profile === 'restricted')
    return { label: 'Allowed destinations only', tone: 'limited' }
  return { label: 'Unrestricted', tone: 'open' }
}

export const pluginPermissionLabels = {
  ai_generate: {
    label: 'Use AI',
    description:
      'Send plugin content to the host’s configured AI provider. Subject to the limits below.',
  },
  projects_read: {
    label: 'Read services',
    description: 'Read service metadata across this instance.',
  },
  environments_read: {
    label: 'Read environments',
    description: 'Read environment metadata across this instance.',
  },
  deployments_read: {
    label: 'Read deployments',
    description: 'Read deployment metadata across this instance.',
  },
  api_read: {
    label: 'Read supported API resources',
    description:
      'Use supported API reads on behalf of an authenticated user, within that user’s permissions.',
  },
  events_read: {
    label: 'Receive platform events',
    description:
      'Receive subscribed service, deployment, and domain events across this instance.',
  },
  api_write: {
    label: 'Change things in Temps',
    description:
      'Make changes on behalf of a signed-in user, within that user’s permissions and only in the areas and services chosen below. Does not grant AI or credential access.',
  },
} satisfies Record<
  PluginGrantsValues['permissions'][number],
  { label: string; description: string }
>

/** Author-declared catalog requirements; runtime authorization remains host-owned. */
export type PluginPermissionRequirement = {
  permission: string
  required: boolean
  reason: string
}
