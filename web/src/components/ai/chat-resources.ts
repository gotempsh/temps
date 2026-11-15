// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listChatResourcesOptions,
  listChatResourcesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { ChatResource, ChatResourceKind } from '@/api/client/types.gen'
import { useQuery, type QueryClient } from '@tanstack/react-query'
import type { ProjectRef } from './project-refs'

/** The order the canvas lists kinds in: what the user asked for first. */
const KIND_ORDER: readonly ChatResourceKind[] = [
  'project',
  'service',
  'repository',
  'sandbox',
]

const KIND_LABELS: Record<ChatResourceKind, string> = {
  project: 'Projects',
  service: 'Databases and services',
  repository: 'Repositories',
  sandbox: 'Sandbox',
}

/** Service and sandbox states that settle on their own, so the list keeps checking. */
const SETTLING_STATUSES = new Set([
  'creating',
  'pending',
  'provisioning',
  'starting',
])

const SETTLING_POLL_MS = 4000

export function resourceGroupLabel(kind: ChatResourceKind): string {
  return KIND_LABELS[kind]
}

export interface ResourceGroup {
  kind: ChatResourceKind
  label: string
  resources: ChatResource[]
}

/** The pieces grouped by kind in a fixed order; kinds with nothing are left out. */
export function groupResources(
  resources: readonly ChatResource[]
): ResourceGroup[] {
  return KIND_ORDER.map((kind) => ({
    kind,
    label: KIND_LABELS[kind],
    resources: resources.filter((resource) => resource.kind === kind),
  })).filter((group) => group.resources.length > 0)
}

/** The console page that shows a piece, or null when it lives outside Temps. */
export function resourceHref(resource: ChatResource): string | null {
  switch (resource.kind) {
    case 'project':
      return resource.slug ? `/projects/${resource.slug}` : null
    case 'service':
      return `/storage/${resource.id}`
    case 'sandbox':
      return `/sandboxes/${resource.id}`
    case 'repository':
      return null
  }
}

/** A project piece as the working tabs know projects, or null for other kinds. */
export function projectRefOf(resource: ChatResource): ProjectRef | null {
  if (resource.kind !== 'project' || !resource.slug) return null
  const id = Number(resource.id)
  return Number.isInteger(id)
    ? { id, slug: resource.slug, name: resource.name }
    : null
}

/** How long to wait before asking again: only while something is still coming up. */
export function resourcesRefreshInterval(
  resources: readonly ChatResource[]
): number | false {
  return resources.some(
    (resource) => resource.status && SETTLING_STATUSES.has(resource.status)
  )
    ? SETTLING_POLL_MS
    : false
}

/** `kind:id` of every piece, for telling a new piece from one already seen. */
export function resourceKeys(resources: readonly ChatResource[]): string[] {
  return resources.map((resource) => `${resource.kind}:${resource.id}`)
}

export function useChatResources(conversationPublicId: string) {
  return useQuery({
    ...listChatResourcesOptions({ path: { public_id: conversationPublicId } }),
    refetchInterval: (query) =>
      resourcesRefreshInterval(query.state.data?.resources ?? []),
  })
}

/**
 * Ask every open chat for its pieces again. A write that just ran may have
 * created one, and the card that ran it does not know which chat it is in.
 */
export function invalidateChatResources(queryClient: QueryClient) {
  const sample = listChatResourcesQueryKey({ path: { public_id: '' } })[0]
  return queryClient.invalidateQueries({
    predicate: (query) => {
      const head = query.queryKey[0]
      return (
        typeof head === 'object' &&
        head !== null &&
        '_id' in head &&
        head._id === sample._id
      )
    },
  })
}
