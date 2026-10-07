// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Notification severity an error alert rule's priority is delivered at.
 * Mirrors `Notification::effective_severity` in temps-notifications: error
 * alert notifications carry a priority but no explicit severity, and routes
 * match on severity.
 */
export function errorRulePrioritySeverity(priority: string): string {
  switch (priority) {
    case 'Low':
      return 'info'
    case 'Normal':
      return 'warning'
    case 'Critical':
      return 'critical'
    default:
      return 'error'
  }
}
