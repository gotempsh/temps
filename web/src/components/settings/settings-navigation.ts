// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Activity,
  ArrowUpCircle,
  BarChart3,
  Bell,
  Bot,
  Boxes,
  Clock,
  Cloud,
  Gauge,
  HardDrive,
  Key,
  KeyRound,
  Monitor,
  Puzzle,
  Server,
  Settings2,
  Shield,
  Users,
  UsersRound,
  Waypoints,
} from 'lucide-react'
import type { ComponentType, ReactNode } from 'react'
import { createElement, Fragment } from 'react'
import type { ConsoleSettingsNavItem } from '@temps-sdk/console-kit'

/**
 * Anything renderable as `<Icon className=... />`. Built-in entries use
 * lucide components; extension entries arrive as a `ReactNode` and are
 * adapted by `mergeSettingsNavigationGroups`.
 */
export type SettingsNavigationIcon = ComponentType<{ className?: string }>

export interface SettingsNavigationItem {
  /** Stable key for rendering. Built-in entries are keyed by url. */
  id?: string
  title: string
  url: string
  icon: SettingsNavigationIcon
  featureKey?: string
  /** Extra Cmd+K search terms. */
  keywords?: string[]
}

export interface SettingsNavigationGroup {
  label: string
  items: SettingsNavigationItem[]
}

/**
 * Canonical instance-settings navigation.
 *
 * Both the Settings sidebar and Cmd+K consume this registry so a settings
 * page cannot be visible in one surface and silently absent from the other.
 */
export const settingsNavigationGroups: SettingsNavigationGroup[] = [
  {
    label: 'General',
    items: [
      { title: 'Platform', url: '/settings', icon: Settings2 },
      { title: 'Version', url: '/settings/version', icon: ArrowUpCircle },
      { title: 'Notifications', url: '/settings/notifications', icon: Bell },
      { title: 'Temps Cloud', url: '/settings/cloud', icon: Cloud },
    ],
  },
  {
    label: 'Access',
    items: [
      { title: 'Users', url: '/settings/users', icon: Users },
      {
        title: 'Teams',
        url: '/settings/teams',
        icon: UsersRound,
        featureKey: 'teams',
      },
      { title: 'Authentication', url: '/settings/auth', icon: KeyRound },
      { title: 'API Keys', url: '/settings/keys', icon: Key },
    ],
  },
  {
    label: 'Infrastructure',
    items: [
      { title: 'Load Balancer', url: '/settings/load-balancer', icon: Server },
      {
        title: 'Docker Registry',
        url: '/settings/docker-registry',
        icon: Boxes,
      },
      { title: 'Build Limits', url: '/settings/build-limits', icon: Gauge },
      {
        title: 'Request Timeouts',
        url: '/settings/request-timeouts',
        icon: Clock,
      },
      // Worker Nodes deliberately is NOT here: it is a main-navigation page
      // under "Build & deliver" (see components/dashboard/Sidebar.tsx). It
      // keeps the /settings/nodes URL, and the sidebar excludes that route
      // from the settings swap so the page renders with the normal shell.
      {
        title: 'Traefik Discovery',
        url: '/settings/traefik-discovery',
        icon: Waypoints,
      },
      {
        title: 'Plugins',
        url: '/settings/plugins',
        icon: Puzzle,
        featureKey: 'plugin-system',
      },
      { title: 'MCP Server', url: '/settings/mcp-server', icon: Bot },
    ],
  },
  {
    label: 'Security',
    items: [
      { title: 'Security', url: '/settings/security', icon: Shield },
      { title: 'Rate Limiting', url: '/settings/rate-limiting', icon: Monitor },
      {
        title: 'Disk Monitoring',
        url: '/settings/disk-monitoring',
        icon: HardDrive,
      },
      {
        title: 'Metrics Monitoring',
        url: '/settings/metrics-monitoring',
        icon: BarChart3,
      },
      {
        title: 'OTel Pipeline',
        url: '/settings/otel-pipeline',
        icon: Activity,
      },
    ],
  },
]

/**
 * A component that renders a fixed node regardless of props. Lets a
 * `ReactNode` icon from an extension satisfy the `SettingsNavigationIcon`
 * component contract the sidebar and Cmd+K render with. Cached per node so
 * repeated merges hand back the same component and React does not remount
 * the icon on every render.
 */
const nodeIconCache = new WeakMap<object, SettingsNavigationIcon>()
function nodeIcon(node: ReactNode): SettingsNavigationIcon {
  if (node !== null && typeof node === 'object') {
    const cached = nodeIconCache.get(node)
    if (cached) return cached
    const Icon = makeNodeIcon(node)
    nodeIconCache.set(node, Icon)
    return Icon
  }
  return makeNodeIcon(node)
}
function makeNodeIcon(node: ReactNode): SettingsNavigationIcon {
  const NodeIcon: SettingsNavigationIcon = () =>
    createElement(Fragment, null, node)
  NodeIcon.displayName = 'SettingsExtensionIcon'
  return NodeIcon
}

/**
 * Merge extension-provided Settings links into the canonical groups.
 *
 * Items whose `group` matches a built-in label are appended to that group
 * after its own entries; any other label becomes a new group after the
 * built-in ones, in first-seen order. Items whose path is not under
 * `/settings/` are dropped, since the sidebar would render them with the
 * wrong nav. Never mutates `groups`.
 */
export function mergeSettingsNavigationGroups(
  groups: readonly SettingsNavigationGroup[],
  extensions: readonly ConsoleSettingsNavItem[] | undefined
): SettingsNavigationGroup[] {
  if (!extensions || extensions.length === 0) return [...groups]
  const merged: SettingsNavigationGroup[] = groups.map((g) => ({
    ...g,
    items: [...g.items],
  }))
  const byLabel = new Map(merged.map((g) => [g.label, g]))
  for (const ext of extensions) {
    if (!ext.path.startsWith('/settings/')) continue
    let group = byLabel.get(ext.group)
    if (!group) {
      group = { label: ext.group, items: [] }
      byLabel.set(ext.group, group)
      merged.push(group)
    }
    group.items.push({
      id: ext.id,
      title: ext.label,
      url: ext.path,
      icon: nodeIcon(ext.icon),
      keywords: ext.keywords,
    })
  }
  return merged
}
