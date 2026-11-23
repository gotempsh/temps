// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Which sidebar the console shows, decided by the URL alone.
 *
 * - `global`: server-wide administration (nodes, users, every database on
 *   the server). Shown only outside a project.
 * - `project`: a project (an API `project_group`, `/p/:slug/*`). The sidebar
 *   is the project's own: its overview, its services and its resources.
 * - `service`: one service (an API `project`, `/projects/:slug/*`). When the
 *   service belongs to a project the sidebar stays that project's, with the
 *   service open as a level inside it; a standalone service keeps its own.
 */

import {
  AlertOctagon,
  BarChart3,
  Database,
  DatabaseBackup,
  GitBranch,
  GitFork,
  Globe,
  Home,
  ScrollText,
  Settings,
  Variable,
  type LucideIcon,
} from 'lucide-react'

export type ConsoleScope =
  | { kind: 'global' }
  | { kind: 'project'; groupSlug: string; route: string }
  | { kind: 'service'; serviceSlug: string; route: string }

/** `/projects/<x>` paths that are pages of their own, not a service. */
const SERVICE_PATH_EXCEPTIONS = new Set(['new', 'import-wizard', 'import'])

function trimSlashes(value: string): string {
  return value.replace(/^\/+|\/+$/g, '')
}

export function resolveConsoleScope(pathname: string): ConsoleScope {
  const group = pathname.match(/^\/p\/([^/]+)(?:\/(.*))?$/)
  if (group) {
    return {
      kind: 'project',
      groupSlug: decodeURIComponent(group[1]),
      route: trimSlashes(group[2] ?? ''),
    }
  }
  const service = pathname.match(/^\/projects\/([^/]+)(?:\/(.*))?$/)
  if (service && !SERVICE_PATH_EXCEPTIONS.has(service[1])) {
    return {
      kind: 'service',
      serviceSlug: decodeURIComponent(service[1]),
      route: trimSlashes(service[2] ?? ''),
    }
  }
  return { kind: 'global' }
}

export type GroupSectionId =
  | 'overview'
  | 'deployments'
  | 'logs'
  | 'errors'
  | 'traces'
  | 'analytics'
  | 'domains'
  | 'databases'
  | 'backups'
  | 'variables'
  | 'settings'

export interface GroupNavSection {
  id: GroupSectionId
  title: string
  icon: LucideIcon
  /** Sub-path below `/p/:slug`; empty for the overview. */
  url: string
  /** Other sub-paths that belong to this section. */
  aliases?: string[]
}

export interface GroupNavGroup {
  /** Omitted for the ungrouped first entry (Overview). */
  label?: string
  sections: GroupNavSection[]
}

/**
 * A project's own menu. The project's services are listed between Overview
 * and Observe by the sidebar itself, since they come from the API.
 */
export const GROUP_NAV_GROUPS: GroupNavGroup[] = [
  {
    sections: [{ id: 'overview', title: 'Overview', icon: Home, url: '' }],
  },
  {
    label: 'Observe',
    sections: [
      {
        id: 'deployments',
        title: 'Deployments',
        icon: GitBranch,
        url: 'deployments',
      },
      { id: 'logs', title: 'Logs', icon: ScrollText, url: 'logs' },
      { id: 'errors', title: 'Errors', icon: AlertOctagon, url: 'errors' },
      { id: 'traces', title: 'Traces', icon: GitFork, url: 'traces' },
      {
        id: 'analytics',
        title: 'Analytics',
        icon: BarChart3,
        url: 'analytics',
      },
    ],
  },
  {
    label: 'Resources',
    sections: [
      { id: 'domains', title: 'Domains', icon: Globe, url: 'domains' },
      { id: 'databases', title: 'Databases', icon: Database, url: 'databases' },
      {
        id: 'backups',
        title: 'Backups',
        icon: DatabaseBackup,
        url: 'backups',
      },
    ],
  },
  {
    label: 'Configure',
    sections: [
      {
        id: 'variables',
        title: 'Shared variables',
        icon: Variable,
        url: 'variables',
      },
      {
        id: 'settings',
        title: 'Settings',
        icon: Settings,
        url: 'settings',
        aliases: ['repository'],
      },
    ],
  },
]

export const GROUP_NAV_SECTIONS: GroupNavSection[] = GROUP_NAV_GROUPS.flatMap(
  (group) => group.sections
)

/** The project section a `/p/:slug/<route>` sub-path belongs to. */
export function resolveGroupSection(route: string): GroupSectionId {
  const normalized = trimSlashes(route)
  for (const section of GROUP_NAV_SECTIONS) {
    if (!section.url) continue
    for (const prefix of [section.url, ...(section.aliases ?? [])]) {
      if (normalized === prefix || normalized.startsWith(`${prefix}/`))
        return section.id
    }
  }
  return 'overview'
}

/** URL of a project section. */
export function groupSectionHref(slug: string, section: GroupNavSection) {
  const root = `/p/${encodeURIComponent(slug)}`
  return section.url ? `${root}/${section.url}` : root
}

/**
 * The server-wide list each scoped project page links to, so an admin who
 * needs everything on the server has one explicit way there.
 */
export const SERVER_WIDE_LISTS = {
  databases: { title: 'All databases on this server', url: '/storage' },
  domains: { title: 'All domains on this server', url: '/domains' },
  backups: { title: 'All backups on this server', url: '/backups' },
  logs: { title: 'All logs on this server', url: '/logs' },
  errors: { title: 'All errors on this server', url: '/errors' },
  traces: { title: 'All traces on this server', url: '/traces' },
  analytics: { title: 'Analytics for every service', url: '/analytics' },
} as const

/** Former `?tab=` values of a project's overview, now pages of their own. */
const LEGACY_GROUP_TABS: Record<string, string> = {
  variables: 'variables',
  settings: 'settings',
}

/**
 * Where an old `/p/:slug?tab=variables|settings` link now lives, keeping its
 * other parameters (`section=` of the settings page); `null` when `search`
 * carries no such tab.
 */
export function legacyGroupTabRedirect(
  slug: string,
  search: string,
  hash = ''
): string | null {
  const params = new URLSearchParams(search)
  const tab = params.get('tab')
  if (tab === null) return null
  params.delete('tab')
  const target = LEGACY_GROUP_TABS[tab]
  const rest = params.toString()
  const root = `/p/${encodeURIComponent(slug)}`
  const path = target ? `${root}/${target}` : root
  return `${rest ? `${path}?${rest}` : path}${hash}`
}

/**
 * ⌘K destinations for a project's own pages ("Storefront · Databases"), so
 * every entry of the project sidebar is also reachable from the palette.
 */
export function groupSectionDestinations(group: {
  id: number
  name: string
  slug: string
}) {
  return GROUP_NAV_SECTIONS.filter((section) => section.url).map((section) => ({
    id: `group:${group.id}:${section.id}`,
    title: `${group.name} · ${section.title}`,
    description: `${section.title} of project ${group.name}`,
    url: groupSectionHref(group.slug, section),
    keywords: ['project', group.name, group.slug, section.title, section.id],
  }))
}
