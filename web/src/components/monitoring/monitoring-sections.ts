// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const MONITORING_SECTIONS = [
  { id: 'alerts', label: 'Alerts' },
  { id: 'rules', label: 'Alert rules' },
  { id: 'alarms', label: 'Alarms' },
  // Distinct from Settings → Notifications (providers and routes): this
  // section only holds instance-wide delivery preferences and the digest.
  { id: 'notifications', label: 'Delivery preferences' },
] as const

export function monitoringSectionLabel(sectionId: string): string | undefined {
  // Server remains reachable from its dedicated sidebar entry and existing links.
  if (sectionId === 'server') return 'Server'
  return MONITORING_SECTIONS.find((section) => section.id === sectionId)?.label
}
