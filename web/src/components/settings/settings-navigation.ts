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
  Gauge,
  HardDrive,
  Key,
  KeyRound,
  Monitor,
  Network,
  Puzzle,
  Server,
  Settings2,
  Shield,
  Users,
  UsersRound,
  Waypoints,
  type LucideIcon,
} from 'lucide-react'

export interface SettingsNavigationItem {
  title: string
  url: string
  icon: LucideIcon
  featureKey?: string
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
      {
        title: 'Worker Nodes',
        url: '/settings/nodes',
        icon: Network,
        featureKey: 'multi-node-worker-join',
      },
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
      { title: 'Security Headers', url: '/settings/security', icon: Shield },
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
