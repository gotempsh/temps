// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useConsoleExtensions } from '@temps-sdk/console-kit'
import { useMemo } from 'react'
import { AUDIT_LOGS_URL } from '@/components/dashboard/primary-navigation'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import {
  mergeSettingsNavigationGroups,
  settingsNavigationGroups,
  type SettingsNavigationGroup,
} from './settings-navigation'

/** `groups` without the audit log, for a role that cannot read it. */
export function withoutRestrictedSettings(
  groups: readonly SettingsNavigationGroup[],
  canViewAuditLogs: boolean
): SettingsNavigationGroup[] {
  return groups
    .map((group) => ({
      ...group,
      items: group.items.filter(
        (item) => canViewAuditLogs || item.url !== AUDIT_LOGS_URL
      ),
    }))
    .filter((group) => group.items.length > 0)
}

/**
 * The settings groups a user may open: the built-in ones, what extensions
 * add, and the audit log only for the roles that can read it.
 */
export function useSettingsGroups(): SettingsNavigationGroup[] {
  const { settingsNavItems } = useConsoleExtensions()
  const canViewAuditLogs = useCanViewAuditLogs()
  return useMemo(
    () =>
      withoutRestrictedSettings(
        mergeSettingsNavigationGroups(
          settingsNavigationGroups,
          settingsNavItems
        ),
        canViewAuditLogs
      ),
    [settingsNavItems, canViewAuditLogs]
  )
}
