// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  NOTIFICATION_CHANNELS_URL,
  isSettingsArea,
} from '@/lib/server-navigation'
import { WORKER_NODES_URL } from '@/lib/worker-nodes'

/**
 * Console route for managing installed plugins: Settings › Extensions.
 * Distinct from the pages those plugins add, which live under
 * `/plugins/:pluginName` and keep their own sidebar entries.
 */
export const PLUGINS_MANAGEMENT_URL = '/settings/plugins'

/** The page behind the Settings entry: every settings page, by group. */
export const SETTINGS_OVERVIEW_URL = '/settings/overview'

/** Who did what, listed in Settings › Access beside who may do what. */
export const AUDIT_LOGS_URL = '/audit-logs'

/** What is left to set up on this server, listed in Settings until done. */
export const SETUP_URL = '/setup'

/**
 * The Settings menu's entry for the setup checklist: listed with its
 * progress while steps are left, gone once they are done or dismissed.
 */
export function setupMenuEntry(setup: {
  visible: boolean
  completedCount: number
  totalCount: number
}): { title: string; url: string; detail: string } | null {
  if (!setup.visible) return null
  return {
    title: 'Setup',
    url: SETUP_URL,
    detail: `${setup.completedCount}/${setup.totalCount}`,
  }
}

/** Connecting an outside agent: a tab of Settings › AI › Connect. */
export const MCP_SERVER_URL = '/settings/mcp-server'

function isPathOrChild(pathname: string, base: string): boolean {
  return pathname === base || pathname.startsWith(`${base}/`)
}

/** `/settings/plugins` and any page under it. */
export function isPluginsManagementRoute(pathname: string): boolean {
  return isPathOrChild(pathname, PLUGINS_MANAGEMENT_URL)
}

/**
 * Whether the sidebar swaps to the settings navigation for this path.
 *
 * Servers (worker nodes) and notification channels keep their `/settings`
 * URLs for existing links, but are a branch of the tree and a tab of
 * Alerts: swapping would hide where the user is. The reverse holds for the
 * audit log, the setup checklist and what the platform connects to (Git,
 * email, model providers): settings pages at URLs outside `/settings`.
 */
export function isSettingsSidebarRoute(pathname: string): boolean {
  if (isPathOrChild(pathname, AUDIT_LOGS_URL)) return true
  if (pathname === SETUP_URL) return true
  if (isSettingsArea(pathname)) return true
  return (
    isPathOrChild(pathname, '/settings') &&
    !isPathOrChild(pathname, WORKER_NODES_URL) &&
    !isPathOrChild(pathname, NOTIFICATION_CHANNELS_URL)
  )
}
