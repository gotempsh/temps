// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  AUDIT_LOGS_URL,
  PLUGINS_MANAGEMENT_URL,
  SETTINGS_OVERVIEW_URL,
  isPluginsManagementRoute,
  isSettingsSidebarRoute,
  setupMenuEntry,
} from './primary-navigation'

describe('plugin management route', () => {
  test('covers the list and the install page', () => {
    expect(isPluginsManagementRoute(PLUGINS_MANAGEMENT_URL)).toBe(true)
    expect(isPluginsManagementRoute('/settings/plugins/install')).toBe(true)
  })

  test('is not the pages an installed plugin adds', () => {
    expect(isPluginsManagementRoute('/plugins/some-plugin')).toBe(false)
    expect(isPluginsManagementRoute('/plugins/some-plugin/settings')).toBe(
      false
    )
  })

  test('does not match a similar prefix', () => {
    expect(isPluginsManagementRoute('/settings/plugins-extra')).toBe(false)
  })
})

describe('settings sidebar swap', () => {
  test('swaps for settings pages, their overview and extensions', () => {
    expect(isSettingsSidebarRoute('/settings')).toBe(true)
    expect(isSettingsSidebarRoute(SETTINGS_OVERVIEW_URL)).toBe(true)
    expect(isSettingsSidebarRoute('/settings/security')).toBe(true)
    expect(isSettingsSidebarRoute(PLUGINS_MANAGEMENT_URL)).toBe(true)
    expect(isSettingsSidebarRoute('/settings/plugins/install')).toBe(true)
  })

  test('swaps for the audit log, a settings page outside /settings', () => {
    expect(isSettingsSidebarRoute(AUDIT_LOGS_URL)).toBe(true)
    expect(isSettingsSidebarRoute('/audit-logs/12')).toBe(true)
    expect(isSettingsSidebarRoute('/audit-logs-export')).toBe(false)
  })

  test('swaps for the setup checklist, which Settings lists until it is done', () => {
    expect(isSettingsSidebarRoute('/setup')).toBe(true)
    expect(isSettingsSidebarRoute('/setup-wizard')).toBe(false)
  })

  test('Settings lists Setup with its progress, until done or dismissed', () => {
    expect(
      setupMenuEntry({ visible: true, completedCount: 3, totalCount: 9 })
    ).toEqual({ title: 'Setup', url: '/setup', detail: '3/9' })
    expect(
      setupMenuEntry({ visible: false, completedCount: 9, totalCount: 9 })
    ).toBeNull()
  })

  test('keeps the tree on servers and alert channels, which live under /settings', () => {
    for (const path of [
      '/settings/nodes',
      '/settings/nodes/4',
      '/settings/notifications',
      '/settings/notifications/routes/new',
    ])
      expect(isSettingsSidebarRoute(path)).toBe(false)
    // A similar prefix is still a settings page.
    expect(isSettingsSidebarRoute('/settings/nodes-extra')).toBe(true)
  })

  test('swaps for what is set up once, at URLs outside /settings', () => {
    for (const path of [
      '/git-providers',
      '/email/providers/new',
      '/status-pages/3',
      '/workflows',
      '/proxy-logs',
      '/ai-gateway/usage',
      '/setup/ai',
      '/settings/mcp-server',
      '/sandboxes',
      '/vault',
    ])
      expect(isSettingsSidebarRoute(path)).toBe(true)
  })

  test('does not swap on what the tree holds, or on the assistant', () => {
    for (const path of [
      '/projects',
      '/p/shop/logs',
      '/projects/web/deployments',
      '/logs',
      '/monitoring/alarms',
      '/storage/4',
      '/domains',
      '/monitoring/server',
      '/chat',
    ])
      expect(isSettingsSidebarRoute(path)).toBe(false)
  })
})
