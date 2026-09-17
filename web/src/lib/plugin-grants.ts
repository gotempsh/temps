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

export const pluginGrantsSchema = z.object({
  permissions: z
    .array(z.enum(pluginPermissionValues))
    .max(pluginPermissionValues.length),
  ai_daily_call_limit: z.number().int().min(0).max(10000),
  ai_max_output_tokens: z.number().int().min(1).max(4096),
})

export type PluginGrantsValues = z.infer<typeof pluginGrantsSchema>

export function emptyPluginGrants(): PluginGrantsValues {
  return {
    permissions: [],
    ai_daily_call_limit: 100,
    ai_max_output_tokens: 1024,
  }
}

export const pluginPermissionLabels = {
  ai_generate: {
    label: 'Use AI',
    description:
      'Send plugin content to the host’s configured AI provider. Subject to the limits below.',
  },
  projects_read: {
    label: 'Read projects',
    description: 'Read project metadata across this instance.',
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
      'Receive subscribed project, deployment, and domain events across this instance.',
  },
  api_write: {
    label: 'Change supported API resources',
    description:
      'Use supported API writes on behalf of an authenticated user, within that user’s permissions. Does not grant AI or credential access.',
  },
} satisfies Record<
  PluginGrantsValues['permissions'][number],
  { label: string; description: string }
>
