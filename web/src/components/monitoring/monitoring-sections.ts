// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlarmClock,
  BellRing,
  ListChecks,
  SlidersHorizontal,
} from 'lucide-react'

/**
 * The sections of the Alerts area this page serves, in the order of its
 * tabs. Channels (notification providers and routes) is the fifth tab and a
 * page of its own; see `ALERTS` in the server navigation.
 */
export const MONITORING_SECTIONS = [
  { id: 'alarms', label: 'Alarms', icon: AlarmClock },
  { id: 'rules', label: 'Rules', icon: ListChecks },
  // Which built-in events (a failed deployment, an expiring certificate)
  // raise an alert at all.
  { id: 'alerts', label: 'Events', icon: BellRing },
  // Instance-wide delivery preferences and the digest. Where notifications
  // go is the Channels tab.
  { id: 'notifications', label: 'Preferences', icon: SlidersHorizontal },
] as const

/** The section a bare `/monitoring` opens: what is firing. */
export const DEFAULT_MONITORING_SECTION = 'alarms'

export function monitoringSectionLabel(sectionId: string): string | undefined {
  // Server remains reachable from its dedicated sidebar entry and existing links.
  if (sectionId === 'server') return 'Server'
  return MONITORING_SECTIONS.find((section) => section.id === sectionId)?.label
}
