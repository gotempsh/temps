// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared schedule preset options used by both CreateBackupSchedule and
 * EditBackupSchedule pages. Extracted here so the constant and its type are
 * defined exactly once.
 */

export interface ScheduleOption {
  label: string
  value: string
  description: string
  customizable?: boolean
}

export const scheduleOptions: ScheduleOption[] = [
  {
    label: 'Every 12 hours',
    value: '0 0 */12 * * *',
    description: 'Runs at 00:00 and 12:00',
  },
  {
    label: 'Daily',
    value: '0 0 0 * * *',
    description: 'Runs every day at midnight',
  },
  {
    label: 'Weekly',
    value: '0 0 0 * * 0',
    description: 'Runs every Sunday at midnight',
  },
  {
    label: 'Monthly',
    value: '0 0 0 1 * *',
    description: 'Runs on the first day of every month at midnight',
  },
  {
    label: 'Custom',
    value: 'custom',
    description: 'Specify a custom cron expression',
    customizable: true,
  },
]
