// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const MONITORING_SECTIONS = [
  { id: 'alerts', label: 'Alerts' },
  { id: 'rules', label: 'Alert rules' },
  { id: 'alarms', label: 'Alarms' },
  { id: 'notifications', label: 'Notifications' },
] as const

export function monitoringSectionLabel(sectionId: string): string | undefined {
  return MONITORING_SECTIONS.find((section) => section.id === sectionId)?.label
}
