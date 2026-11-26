// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Whose page a URL is, decided by the URL alone.
 *
 * - `global`: a server-wide page (every project, a database, a setting).
 * - `project`: a project (an API `project_group`, `/p/:slug/*`).
 * - `service`: one service (an API `project`, `/projects/:slug/*`).
 *
 * The sidebar is the same tree at all three; it opens to the project or
 * service shown. A service's views are listed under it there; a project's
 * are the one row of tabs of its page.
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
  | 'variables'
  | 'settings'

/** A page of a section that holds several, shown as a tab above them. */
export interface GroupNavTab {
  title: string
  icon: LucideIcon
  /** Sub-path below `/p/:slug`. */
  url: string
}

export interface GroupNavSection {
  id: GroupSectionId
  title: string
  icon: LucideIcon
  /** Sub-path below `/p/:slug`; empty for the overview. */
  url: string
  /** Other sub-paths that belong to this section. */
  aliases?: string[]
  /** The section's pages, when it has more than one. One of them is `url`. */
  tabs?: GroupNavTab[]
  /**
   * Set for a view that holds nothing until a service uses it: what is
   * missing, said beside it under "Add" while it is not a tab.
   */
  missing?: string
}

/**
 * A project's views, in the order of the tabs of its page and in the shape
 * of a service's: what it is, the data its services have, what it owns,
 * then how it is configured.
 */
export const GROUP_NAV_SECTIONS: GroupNavSection[] = [
  { id: 'overview', title: 'Overview', icon: Home, url: '' },
  {
    id: 'deployments',
    title: 'Deployments',
    icon: GitBranch,
    url: 'deployments',
  },
  { id: 'logs', title: 'Logs', icon: ScrollText, url: 'logs' },
  { id: 'errors', title: 'Errors', icon: AlertOctagon, url: 'errors' },
  {
    id: 'traces',
    title: 'Traces',
    icon: GitFork,
    url: 'traces',
    missing: 'No service sends traces yet.',
  },
  {
    id: 'analytics',
    title: 'Analytics',
    icon: BarChart3,
    url: 'analytics',
    missing: 'No service sends page views yet.',
  },
  {
    id: 'domains',
    title: 'Domains',
    icon: Globe,
    url: 'domains',
    missing: 'No custom domain yet.',
  },
  {
    id: 'databases',
    title: 'Databases',
    icon: Database,
    url: 'databases',
    missing: 'No database linked yet.',
    tabs: [
      { title: 'Databases', icon: Database, url: 'databases' },
      { title: 'Backups', icon: DatabaseBackup, url: 'backups' },
    ],
  },
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
]

/** Every sub-path prefix that belongs to `section`. */
function sectionPrefixes(section: GroupNavSection): string[] {
  return [
    section.url,
    ...(section.aliases ?? []),
    ...(section.tabs ?? []).map((tab) => tab.url),
  ].filter(Boolean)
}

/** The project section a `/p/:slug/<route>` sub-path belongs to. */
export function resolveGroupSection(route: string): GroupSectionId {
  const normalized = trimSlashes(route)
  for (const section of GROUP_NAV_SECTIONS) {
    for (const prefix of sectionPrefixes(section)) {
      if (normalized === prefix || normalized.startsWith(`${prefix}/`))
        return section.id
    }
  }
  return 'overview'
}

export interface GroupNavLocation {
  /** The view the page belongs to. */
  section: GroupNavSection
  /** The view's tab the page is, when the page is a tab's own. */
  tab?: GroupNavTab
}

/** Where a `/p/:slug/<route>` sub-path sits among the project's views. */
export function resolveGroupLocation(route: string): GroupNavLocation {
  const normalized = trimSlashes(route)
  const id = resolveGroupSection(normalized)
  const section =
    GROUP_NAV_SECTIONS.find((candidate) => candidate.id === id) ??
    GROUP_NAV_SECTIONS[0]
  return {
    section,
    tab: section.tabs?.find((tab) => tab.url === normalized),
  }
}

/** URL of a project section. */
export function groupSectionHref(slug: string, section: GroupNavSection) {
  const root = `/p/${encodeURIComponent(slug)}`
  return section.url ? `${root}/${section.url}` : root
}

/** URL of a sub-path below a project. */
export function groupPathHref(slug: string, path: string) {
  return `/p/${encodeURIComponent(slug)}/${path}`
}

/**
 * The server-wide list each scoped project page links to, so an admin who
 * needs everything on the server has one explicit way there. `label` is the
 * button text, kept short; `title` is its tooltip and the palette entry.
 */
export const SERVER_WIDE_LISTS = {
  databases: {
    label: 'All databases',
    title: 'Every database on this server',
    url: '/storage',
  },
  domains: {
    label: 'All domains',
    title: 'Every domain on this server',
    url: '/domains',
  },
  backups: {
    label: 'All backups',
    title: 'Every backup on this server',
    url: '/backups',
  },
  logs: { label: 'All logs', title: 'Logs of every service', url: '/logs' },
  errors: {
    label: 'All errors',
    title: 'Errors of every service',
    url: '/errors',
  },
  traces: {
    label: 'All traces',
    title: 'Traces of every service',
    url: '/traces',
  },
  analytics: {
    label: 'All services',
    title: 'Analytics of every service',
    url: '/analytics',
  },
} as const

export type ServerWideList =
  (typeof SERVER_WIDE_LISTS)[keyof typeof SERVER_WIDE_LISTS]

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
 * every view of a project is also reachable from the palette.
 */
export function groupSectionDestinations(group: {
  id: number
  name: string
  slug: string
}) {
  const destination = (key: string, title: string, path: string) => ({
    id: `group:${group.id}:${key}`,
    title: `${group.name} · ${title}`,
    description: `${title} of project ${group.name}`,
    url: groupPathHref(group.slug, path),
    keywords: ['project', group.name, group.slug, title, key],
  })
  return GROUP_NAV_SECTIONS.filter((section) => section.url).flatMap(
    (section) => [
      destination(section.id, section.title, section.url),
      // A tab that is not the view's landing page, named with it.
      ...(section.tabs ?? [])
        .filter((tab) => tab.url !== section.url)
        .map((tab) =>
          destination(tab.url, `${section.title} · ${tab.title}`, tab.url)
        ),
    ]
  )
}
