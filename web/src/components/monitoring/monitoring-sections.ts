// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlarmClock,
  BellRing,
  ListChecks,
  SlidersHorizontal,
} from 'lucide-react'

export const MONITORING_SECTIONS = [
  { id: 'alerts', label: 'Alerts', icon: BellRing },
  { id: 'rules', label: 'Alert rules', icon: ListChecks },
  { id: 'alarms', label: 'Alarms', icon: AlarmClock },
  // Distinct from Settings → Notifications (providers and routes): this
  // section only holds instance-wide delivery preferences and the digest.
  {
    id: 'notifications',
    label: 'Delivery preferences',
    icon: SlidersHorizontal,
  },
] as const

export function monitoringSectionLabel(sectionId: string): string | undefined {
  // Server remains reachable from its dedicated sidebar entry and existing links.
  if (sectionId === 'server') return 'Server'
  return MONITORING_SECTIONS.find((section) => section.id === sectionId)?.label
}
