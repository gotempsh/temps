// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Activity,
  BarChart3,
  Boxes,
  Cpu,
  Database,
  DatabaseBackup,
  Folder,
  Gauge,
  GitBranch,
  GitFork,
  Globe,
  Mail,
  Network,
  Puzzle,
  Radio,
  ScrollText,
  ShieldAlert,
  Sparkles,
  type LucideIcon,
} from 'lucide-react'

import { isPlatformToolsRoute } from '@/lib/platform-navigation'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'

/**
 * Console route for managing installed plugins.
 *
 * Like Worker Nodes, the page keeps its `/settings/...` URL for existing links
 * and bookmarks but is surfaced in the main navigation, so the sidebar must not
 * swap to the settings navigation on it (see `isSettingsSidebarRoute`).
 */
export const PLUGINS_MANAGEMENT_URL = '/settings/plugins'

function isPathOrChild(pathname: string, base: string): boolean {
  return pathname === base || pathname.startsWith(`${base}/`)
}

/** `/settings/plugins` and `/settings/plugins/install`. */
export function isPluginsManagementRoute(pathname: string): boolean {
  return isPathOrChild(pathname, PLUGINS_MANAGEMENT_URL)
}

/**
 * Whether the sidebar swaps to the settings navigation for this path.
 *
 * Worker Nodes and Plugins live under `/settings` but are main-navigation
 * pages; swapping would hide the entry that is currently active.
 */
export function isSettingsSidebarRoute(pathname: string): boolean {
  return (
    pathname.startsWith('/settings') &&
    !pathname.startsWith(WORKER_NODES_URL) &&
    !isPluginsManagementRoute(pathname)
  )
}

export interface PlatformNavItem {
  title: string
  url: string
  icon: LucideIcon
  activeWhen?: (pathname: string) => boolean
  featureKey?: string
}

export interface PlatformNavGroup {
  label: string
  items: PlatformNavItem[]
}

// The fresh-install sidebar stays focused on daily work, grouped by intent so
// the short list is easy to scan. Every other capability remains visible on
// /tools and in Cmd+K.
export const primaryPlatformGroups: PlatformNavGroup[] = [
  {
    label: 'Applications',
    items: [
      // Experimental: the badge and page header carry the maturity promise
      // (see `featureKeyForPath`), so the entry stays discoverable without
      // presenting a prototype as a finished product.
      {
        title: 'AI workspace',
        url: '/ai-first',
        icon: Sparkles,
        featureKey: 'ai-chat',
      },
      {
        title: 'Projects',
        url: '/projects',
        icon: Folder,
        // A project (group) overview lives at /p/:slug.
        activeWhen: (path: string) =>
          path === '/projects' ||
          path.startsWith('/projects/') ||
          path.startsWith('/p/'),
      },
      { title: 'Git connections', url: '/git-providers', icon: GitBranch },
      { title: 'Domains', url: '/domains', icon: Globe },
      {
        title: 'Email',
        url: '/email',
        icon: Mail,
        activeWhen: (pathname) => isPathOrChild(pathname, '/email'),
      },
      // Lives under the /settings/nodes URL for historical reasons, but it is
      // a build-and-deliver capability: without a worker node a control plane
      // that runs no local workloads cannot build or deploy anything. See
      // WORKER_NODES_URL below for why the sidebar does not treat it as a
      // settings route.
      {
        title: 'Worker nodes',
        url: WORKER_NODES_URL,
        icon: Network,
        featureKey: 'multi-node-worker-join',
      },
    ],
  },
  {
    label: 'Storage',
    items: [
      { title: 'Databases', url: '/storage', icon: Database },
      { title: 'Backups', url: '/backups', icon: DatabaseBackup },
    ],
  },
  {
    label: 'Observability',
    items: [
      { title: 'Analytics', url: '/analytics', icon: BarChart3 },
      { title: 'Traces', url: '/traces', icon: GitFork },
      { title: 'Logs', url: '/logs', icon: ScrollText },
      { title: 'Errors', url: '/errors', icon: ShieldAlert },
      {
        title: 'Server metrics',
        url: '/monitoring/server',
        icon: Cpu,
        activeWhen: (pathname) => pathname.startsWith('/monitoring/server'),
      },
      {
        title: 'Monitoring',
        url: '/monitoring/alerts',
        icon: Gauge,
        activeWhen: (pathname) =>
          pathname.startsWith('/monitoring') &&
          !pathname.startsWith('/monitoring/server'),
        featureKey: 'alerts-metric-alerts',
      },
      { title: 'Proxy', url: '/proxy', icon: Activity },
      {
        title: 'Status pages',
        url: '/status-pages',
        icon: Radio,
        featureKey: 'status-pages',
      },
    ],
  },
  {
    label: 'Administration',
    items: [
      {
        title: 'All tools',
        url: '/tools',
        icon: Boxes,
        activeWhen: isPlatformToolsRoute,
      },
      // Manages which plugins are installed. Distinct from the "Installed
      // plugins" group (`NavPlugins`), which lists the pages those plugins
      // contribute under /plugins/:pluginName. This page lives under
      // /settings/plugins, so the two never share a URL prefix.
      {
        title: 'Plugins',
        url: PLUGINS_MANAGEMENT_URL,
        icon: Puzzle,
        activeWhen: isPluginsManagementRoute,
        featureKey: 'plugin-system',
      },
    ],
  },
]
